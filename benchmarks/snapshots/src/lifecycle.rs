//! Focused workspace reservation workloads and independent result checks.
use crate::{
    evidence::Evidence,
    harness::{command, fingerprint},
    profiles::{Case, Cleanup, Workload},
    statistics::Observation,
    util::{require, require_reused, Error, Result, Runner},
};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn call(runner: &Runner, binary: &Path, repo: &Path, args: &[&str]) -> Result<(Value, Value)> {
    command(
        runner,
        binary,
        repo,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        false,
    )
}
/// Controlled pool workloads own materialization; automatic workers would race
/// their measurements and fixture teardown. Legacy CLIs were already lazy.
pub(crate) fn init_pool(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    pool: Option<&str>,
    args: &[&str],
) -> Result<(Value, Value)> {
    // Capability discovery is outside the measured initialization command and
    // lets older baseline binaries keep their existing lazy initialization.
    let output = runner.run(binary, ["--json", "pool", "init", "--help"], repo)?;
    let help: Value = serde_json::from_slice(&output.stdout)?;
    require(
        help["api_version"] == 2 && help["ok"] == true,
        "Invalid CLI init help response",
    )?;
    let text = help["result"]["text"]
        .as_str()
        .ok_or_else(|| Error::Invalid("Missing CLI init help text".into()))?;
    let ready = text.split_whitespace().any(|word| word == "--ready-target");
    let idle = text.split_whitespace().any(|word| word == "--idle-timeout");
    require(
        ready == idle,
        "CLI exposes an unsupported partial ready-pool policy",
    )?;
    let mut args = args.to_vec();
    if ready {
        args.extend(["--ready-target", "0", "--idle-timeout", "never"]);
    }
    let result = match pool {
        Some(pool) => named(runner, binary, repo, pool, &args)?,
        None => call(runner, binary, repo, &args)?,
    };
    if ready {
        require(
            result.0["ready_target"] == 0 && result.0["idle_timeout"] == "never",
            "Benchmark pool did not disable automatic checkout work",
        )?;
    }
    Ok(result)
}
fn path(lease: &Value) -> Result<PathBuf> {
    lease["workspace_path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| Error::Invalid("Missing acquired worktree".into()))
}
fn release(runner: &Runner, binary: &Path, repo: &Path, task: &str, force: bool) -> Result<Value> {
    let mut args = vec!["release", "--task", task];
    if force {
        args.push("--force");
    }
    Ok(call(runner, binary, repo, &args)?.1)
}
fn pristine(runner: &Runner, repo: &Path, lease: &Value) -> Result<()> {
    let workspace = path(lease)?;
    require(
        runner
            .git(&workspace, ["status", "--porcelain=v1", "-z"])?
            .is_empty(),
        "Acquired checkout is dirty",
    )?;
    require(
        runner.git_text(&workspace, &["rev-parse", "HEAD"])?
            == runner.git_text(repo, &["rev-parse", "HEAD"])?,
        "Acquired checkout has wrong HEAD",
    )?;
    require(
        fs::read(workspace.join(crate::fixture::spec().path(0)))?
            == fs::read(repo.join(crate::fixture::spec().path(0)))?,
        "Acquired checkout has wrong contents",
    )?;
    let symbolic = runner.capture("git", ["symbolic-ref", "-q", "HEAD"], &workspace)?;
    require(
        symbolic.status.code() == Some(1) && symbolic.stdout.is_empty(),
        "Acquired checkout is not detached",
    )
}
fn payload(bytes: usize, seed: u64) -> Vec<u8> {
    let mut state = seed;
    (0..bytes)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
pub(crate) fn prepare(runner: &Runner, repo: &Path, case: &Case) -> Result<()> {
    if let Workload::WorkspaceBuild { units, .. } = case.workload {
        return crate::build::prepare(runner, repo, units);
    }
    let artifacts = match case.workload {
        Workload::WorkspaceDeletion {
            artifact_files,
            artifact_file_bytes,
            artifact_binary_bytes,
            ..
        } if artifact_files > 0 => {
            Some((artifact_files, artifact_file_bytes, artifact_binary_bytes))
        }
        _ => None,
    };
    if let Some((artifact_files, artifact_file_bytes, artifact_binary_bytes)) = artifacts {
        let source = repo.join("artifact-source");
        fs::create_dir(&source)?;
        let mut manifest = Vec::new();
        for i in 0..artifact_files {
            let bytes = payload(artifact_file_bytes, i as u64 + 17);
            manifest.extend_from_slice(crate::util::hash(&bytes).as_bytes());
            fs::write(source.join(format!("f{i:04}.bin")), bytes)?;
        }
        let bytes = payload(artifact_binary_bytes, 12345);
        manifest.extend_from_slice(crate::util::hash(&bytes).as_bytes());
        fs::write(source.join("large.bin"), bytes)?;
        fs::write(repo.join("artifact-input"), crate::util::hash(&manifest))?;
        runner.git(repo, ["add", "-A"])?;
        runner.git(repo, ["commit", "-qm", "deletion-artifacts-v1"])?;
        runner.git(repo, ["repack", "-ad", "--depth=50", "--window=10"])?;
    }
    Ok(())
}
fn artifact_contents(repo: &Path, lease: &Value, expected_files: usize) -> Result<()> {
    let target = path(lease)?.join("ignored");
    let source = repo.join("artifact-source");
    require(
        fs::read_dir(&target)?.count() == expected_files,
        "Seeded artifact entry count differs",
    )?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        require(
            fs::read(entry.path())? == fs::read(target.join(entry.file_name()))?,
            "Seeded artifact contents differ from independent source",
        )?;
    }
    Ok(())
}
fn empty(runner: &Runner, binary: &Path, repo: &Path) -> Result<()> {
    let (list, _) = call(runner, binary, repo, &["list"])?;
    require(
        list["reservations"].as_array().is_some_and(Vec::is_empty),
        "Workload left active reservations",
    )
}
pub(crate) fn measure(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &Case,
) -> Result<Observation> {
    match case.workload {
        Workload::WorkspaceBuild { strategy, units } => {
            return crate::build::measure(runner, binary, repo, case, strategy, units)
        }
        Workload::MonorepoReuse {
            cleanup,
            ignored_files,
            ignored_file_bytes,
        } => {
            return crate::monorepo::measure(
                runner,
                binary,
                repo,
                case,
                cleanup,
                ignored_files,
                ignored_file_bytes,
            )
        }
        Workload::WorkspaceMultiPool {
            pools,
            workers_per_pool,
        } => return multi_pool(runner, binary, repo, case, pools, workers_per_pool),
        Workload::WorkspaceDeletion { .. } => return deletion(runner, binary, repo, case),
        _ => (),
    }
    let before = fingerprint(runner, repo, false)?;
    let cleanup = if let Workload::WorkspaceReuse { cleanup } = case.workload {
        cleanup
    } else {
        Cleanup::Pristine
    };
    init_pool(
        runner,
        binary,
        repo,
        None,
        &[
            "pool",
            "init",
            "--capacity",
            &case.capacity().to_string(),
            "--cleanup",
            cleanup.name(),
        ],
    )?;
    let mut evidence = Evidence::default();
    match case.workload {
        Workload::WorkspaceReuse { cleanup } => {
            let (lease, time) = call(runner, binary, repo, &["acquire", "--task", "single"])?;
            evidence.record("first_acquire", time)?;
            pristine(runner, repo, &lease)?;
            release(runner, binary, repo, "single", false)?;
            let (lease, time) = call(runner, binary, repo, &["acquire", "--task", "single"])?;
            evidence.record("warm_acquire", time)?;
            require_reused(&lease, "warm_acquire")?;
            pristine(runner, repo, &lease)?;
            evidence.record("release", release(runner, binary, repo, "single", false)?)?;
            let (lease, _) = call(runner, binary, repo, &["acquire", "--task", "dirty"])?;
            let workspace = path(&lease)?;
            let dirty = workspace.join(crate::fixture::spec().path(0));
            if case.contents == crate::profiles::Contents::ConvertedText {
                // This has the same normalized blob but is physically wrong
                // for the checkout. Reuse must restore CRLF, not retain LF.
                let bytes: Vec<_> = fs::read(&dirty)?
                    .into_iter()
                    .filter(|b| *b != b'\r')
                    .collect();
                fs::write(dirty, bytes)?;
            } else {
                fs::write(dirty, b"dirty")?;
            }
            fs::write(workspace.join("untracked"), b"dirty")?;
            fs::create_dir(workspace.join("ignored"))?;
            for i in 0..128 {
                fs::write(workspace.join(format!("ignored/residue-{i}")), b"cache")?;
            }
            release(runner, binary, repo, "dirty", true)?;
            let (lease, time) = call(runner, binary, repo, &["acquire", "--task", "clean"])?;
            evidence.record("dirty_acquire", time)?;
            require_reused(&lease, "dirty_acquire")?;
            pristine(runner, repo, &lease)?;
            require(
                !path(&lease)?.join("untracked").exists(),
                "Untracked residue survived sanitation",
            )?;
            require(
                path(&lease)?.join("ignored").exists() == (cleanup == Cleanup::Warm),
                "Ignored residue violates cleanup policy",
            )?;
            if cleanup == Cleanup::Warm {
                require(
                    fs::read(path(&lease)?.join("ignored/residue-0"))? == b"cache",
                    "Warm cleanup corrupted ignored residue",
                )?;
            }
            release(runner, binary, repo, "clean", false)?;
            evidence.checked("pristine_checkout");
            evidence.checked("cleanup_policy");
        }
        Workload::WorkspaceConcurrency { workers } => {
            concurrent(runner, binary, repo, workers, &mut evidence)?
        }
        _ => return Err(Error::Invalid("Not a workspace profile case".into())),
    }
    empty(runner, binary, repo)?;
    evidence.checked("released");
    require(
        fingerprint(runner, repo, false)? == before,
        "Workspace workload changed source checkout/index",
    )?;
    evidence.checked("source_preserved");
    crate::evidence::observation(case, evidence)
}

fn parallel<T: Send>(workers: usize, job: impl Fn(usize) -> Result<T> + Sync) -> Result<Vec<T>> {
    std::thread::scope(|scope| {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mut starts = Vec::new();
        let mut handles = Vec::new();
        let job = &job;
        for i in 0..workers {
            let (start_tx, start_rx) = std::sync::mpsc::sync_channel(1);
            let ready = ready_tx.clone();
            let handle = std::thread::Builder::new().spawn_scoped(scope, move || {
                ready.send(())?;
                drop(ready);
                start_rx.recv_timeout(Duration::from_secs(30))?;
                job(i)
            })?;
            starts.push(start_tx);
            handles.push(handle);
        }
        drop(ready_tx);
        // Scope exit drops start senders on every startup error. Waiting peers
        // observe disconnection before the scope joins them, rather than hanging.
        for _ in 0..workers {
            ready_rx.recv_timeout(Duration::from_secs(30))?;
        }
        for start in starts {
            start.send(())?;
        }
        handles
            .into_iter()
            .map(|h| h.join().map_err(|_| Error::WorkerPanic)?)
            .collect()
    })
}
fn concurrent(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    workers: usize,
    evidence: &mut Evidence,
) -> Result<()> {
    for i in 0..workers {
        call(
            runner,
            binary,
            repo,
            &["acquire", "--task", &format!("prime-{i}")],
        )?;
    }
    for i in 0..workers {
        release(runner, binary, repo, &format!("prime-{i}"), false)?;
    }
    let start = Instant::now();
    let acquired = parallel(workers, |i| {
        call(
            runner,
            binary,
            repo,
            &[
                "acquire",
                "--task",
                &format!("worker-{i}"),
                "--wait",
                "60000ms",
            ],
        )
    })?;
    let acquire_seconds = start.elapsed().as_secs_f64();
    let ids: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["reservation_id"].as_str())
        .collect();
    let paths: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["workspace_path"].as_str())
        .collect();
    require(
        ids.len() == workers
            && !ids.contains(&None)
            && paths.len() == workers
            && !paths.contains(&None),
        "Concurrent workers shared a reservation/workspace",
    )?;
    for (_, time) in acquired {
        evidence.record("acquire", time)?;
    }
    evidence.checked("unique_reservations");
    evidence.record(
        "full_pool_rejection",
        crate::harness::reject_full_pool(runner, binary, repo)?,
    )?;
    evidence.checked("full_pool_rejection");
    let start = Instant::now();
    let completed = parallel(workers, |i| {
        let task = format!("worker-{i}");
        release(runner, binary, repo, &task, false)
    })?;
    evidence.batch_seconds = Some(acquire_seconds + start.elapsed().as_secs_f64());
    for released in completed {
        evidence.record("release", released)?;
    }
    evidence.checked("successful_workers");
    Ok(())
}

fn named(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    pool: &str,
    args: &[&str],
) -> Result<(Value, Value)> {
    let mut selected = vec!["--pool", pool];
    selected.extend_from_slice(args);
    call(runner, binary, repo, &selected)
}

fn multi_pool(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &Case,
    pools: usize,
    workers: usize,
) -> Result<Observation> {
    let before = fingerprint(runner, repo, false)?;
    let mut evidence = Evidence::default();
    let names: Vec<_> = (0..pools).map(|i| format!("bench-{i}")).collect();
    let mut roots = Vec::new();
    for pool in &names {
        let (initialized, timing) = init_pool(
            runner,
            binary,
            repo,
            Some(pool),
            &["pool", "init", "--capacity", &workers.to_string()],
        )?;
        roots.push(PathBuf::from(
            initialized["workspace_root"]
                .as_str()
                .ok_or_else(|| Error::Invalid("Missing pool root".into()))?,
        ));
        evidence.record("pool_init", timing)?;
    }
    let total = pools * workers;
    let first = parallel(total, |i| {
        named(
            runner,
            binary,
            repo,
            &names[i / workers],
            &["acquire", "--task", &format!("prime-{}", i % workers)],
        )
    })?;
    for (i, (lease, timing)) in first.into_iter().enumerate() {
        pristine(runner, repo, &lease)?;
        evidence.record("first_acquire", timing)?;
        named(
            runner,
            binary,
            repo,
            &names[i / workers],
            &["release", "--task", &format!("prime-{}", i % workers)],
        )?;
    }
    let started = Instant::now();
    let acquired = parallel(total, |i| {
        named(
            runner,
            binary,
            repo,
            &names[i / workers],
            &[
                "acquire",
                "--task",
                &format!("worker-{}", i % workers),
                "--idempotency-key",
                &format!("request-{}", i % workers),
                "--wait",
                "60000ms",
            ],
        )
    })?;
    let acquire_seconds = started.elapsed().as_secs_f64();
    let ids: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["reservation_id"].as_str())
        .collect();
    let paths: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["workspace_path"].as_str())
        .collect();
    require(
        ids.len() == total
            && paths.len() == total
            && !ids.contains(&None)
            && !paths.contains(&None),
        "Pools shared a reservation or checkout",
    )?;
    for (i, (lease, timing)) in acquired.into_iter().enumerate() {
        require(
            path(&lease)?.parent() == Some(roots[i / workers].as_path()),
            "Reservation escaped its pool root",
        )?;
        pristine(runner, repo, &lease)?;
        evidence.record("acquire", timing)?;
    }
    evidence.checked("unique_reservations");
    evidence.checked("pool_isolation");
    for pool in &names {
        evidence.record(
            "full_pool_rejection",
            crate::harness::reject(
                runner,
                binary,
                repo,
                &["--pool", pool, "acquire", "--task", "overflow"],
                "POOL_EXHAUSTED",
                3,
            )?,
        )?;
    }
    evidence.checked("full_pool_rejection");
    let (catalog, timing) = call(runner, binary, repo, &["pool", "list"])?;
    let entries = catalog["pools"]
        .as_array()
        .ok_or_else(|| Error::Invalid("Missing pool catalog".into()))?;
    require(
        entries.len() == pools
            && entries
                .iter()
                .all(|p| p["phase"] == "ready" && p["capacity"] == workers),
        "Pool catalog differs from fixture",
    )?;
    let observed: BTreeSet<_> = entries.iter().filter_map(|p| p["name"].as_str()).collect();
    require(
        observed == names.iter().map(String::as_str).collect(),
        "Pool catalog lost or invented a namespace",
    )?;
    evidence.record("pool_catalog", timing)?;
    evidence.checked("catalog");
    let started = Instant::now();
    let completed = parallel(total, |i| {
        let pool = &names[i / workers];
        let task = format!("worker-{}", i % workers);
        let (_, released) = named(runner, binary, repo, pool, &["release", "--task", &task])?;
        Ok(released)
    })?;
    evidence.batch_seconds = Some(acquire_seconds + started.elapsed().as_secs_f64());
    for released in completed {
        evidence.record("release", released)?;
    }
    evidence.checked("successful_workers");
    for pool in &names {
        let (list, _) = named(runner, binary, repo, pool, &["list"])?;
        require(
            list["reservations"].as_array().is_some_and(Vec::is_empty),
            "Named pool retained an active reservation",
        )?;
    }
    evidence.checked("released");
    require(
        fingerprint(runner, repo, false)? == before,
        "Multi-pool workload changed source checkout/index",
    )?;
    evidence.checked("source_preserved");
    crate::evidence::observation(case, evidence)
}

fn lock_identities(state: &Path) -> Result<Vec<(PathBuf, u64, u64)>> {
    use std::os::unix::fs::MetadataExt;
    let mut result = Vec::new();
    for directory in ["locks", "requests", "artifacts"] {
        let root = state.join(directory);
        if !root.exists() {
            continue;
        }
        for item in fs::read_dir(root)? {
            let path = item?.path();
            if path.extension().is_some_and(|s| s == "lock") {
                let metadata = fs::symlink_metadata(&path)?;
                require(metadata.is_file(), "Unexpected permanent lock type")?;
                result.push((path, metadata.dev(), metadata.ino()));
            }
        }
    }
    result.sort();
    Ok(result)
}

fn deletion(runner: &Runner, binary: &Path, repo: &Path, case: &Case) -> Result<Observation> {
    let Workload::WorkspaceDeletion {
        slots,
        materialized,
        dirty,
        external,
        artifact_files,
        ..
    } = case.workload
    else {
        return Err(Error::Invalid("Not a deletion case".into()));
    };
    let before = fingerprint(runner, repo, false)?;
    let parent = repo
        .parent()
        .ok_or_else(|| Error::Invalid("Fixture has no parent".into()))?;
    let external_root = parent.join("external-worktrees");
    let mut init = vec!["pool", "init", "--capacity"];
    let capacity = slots.to_string();
    init.push(&capacity);
    let external_text = external_root
        .to_str()
        .ok_or_else(|| Error::Invalid("Non-UTF-8 fixture root".into()))?;
    if external {
        init.extend(["--workspace-root", external_text]);
    }
    let (initialized, _) = init_pool(runner, binary, repo, None, &init)?;
    let root = PathBuf::from(
        initialized["workspace_root"]
            .as_str()
            .ok_or_else(|| Error::Invalid("Missing checkout root".into()))?,
    );
    let state = PathBuf::from(
        initialized["state_dir"]
            .as_str()
            .ok_or_else(|| Error::Invalid("Missing pool state".into()))?,
    );
    init_pool(
        runner,
        binary,
        repo,
        Some("survivor"),
        &["pool", "init", "--capacity", "1"],
    )?;
    let (sibling, _) = named(
        runner,
        binary,
        repo,
        "survivor",
        &["acquire", "--task", "slot-0"],
    )?;
    let sibling_file = path(&sibling)?.join("sibling-result");
    fs::write(&sibling_file, b"preserved sibling")?;
    let mut leases = Vec::new();
    if materialized {
        for i in 0..slots {
            let (lease, _) = call(
                runner,
                binary,
                repo,
                &[
                    "acquire",
                    "--task",
                    &format!("slot-{i}"),
                    "--idempotency-key",
                    &format!("delete-{i}"),
                ],
            )?;
            pristine(runner, repo, &lease)?;
            if artifact_files > 0 {
                let target = path(&lease)?.join("ignored");
                fs::create_dir_all(&target)?;
                let legacy = repo.join(".git/groove/artifacts").join("a".repeat(64));
                fs::create_dir_all(&legacy)?;
                for entry in fs::read_dir(repo.join("artifact-source"))? {
                    let entry = entry?;
                    fs::copy(entry.path(), target.join(entry.file_name()))?;
                    fs::copy(entry.path(), legacy.join(entry.file_name()))?;
                }
                artifact_contents(repo, &lease, artifact_files + 1)?;
            }
            leases.push(lease);
        }
    }
    let selected = crate::fixture::spec().path(0);
    let original = fs::read(repo.join(&selected))?;
    let (saved, receipt) = if let Some(first) = leases.first() {
        fs::write(path(first)?.join(&selected), b"saved task result")?;
        let (finished, _) = call(
            runner,
            binary,
            repo,
            &["release", "--save", "--task", "slot-0"],
        )?;
        require(
            finished["snapshot"]["pinned"] == true,
            "Finished result was not pinned",
        )?;
        fs::write(path(first)?.join(&selected), &original)?;
        for i in 1..slots {
            release(runner, binary, repo, &format!("slot-{i}"), false)?;
        }
        (
            finished["snapshot"].clone(),
            first["reservation_id"].as_str().map(String::from),
        )
    } else {
        let (saved, _) = call(runner, binary, repo, &["snapshot", "save"])?;
        (saved["snapshot"].clone(), None)
    };
    for lease in &leases {
        if dirty {
            let workspace = path(lease)?;
            fs::write(workspace.join(&selected), b"discarded tracked edit")?;
            fs::write(workspace.join("discarded-untracked"), b"discarded")?;
            fs::create_dir_all(workspace.join("ignored"))?;
            for i in 0..128 {
                fs::write(workspace.join(format!("ignored/residue-{i}")), b"discarded")?;
            }
        }
    }
    let marker = root.join("retained-root-file");
    fs::write(&marker, b"keep root neighbor")?;
    let locks = lock_identities(&state)?;
    let refs = runner.git(repo, ["show-ref"])?;
    let mut evidence = Evidence::default();
    evidence.record(
        "ambiguous_rejection",
        crate::harness::reject(
            runner,
            binary,
            repo,
            &["pool", "destroy"],
            "INVALID_ARGUMENT",
            2,
        )?,
    )?;
    let mut args = vec!["pool", "destroy"];
    if dirty {
        args.push("--force");
    }
    let mut preview_args = args.clone();
    preview_args.push("--dry-run");
    let (preview, timing) = named(runner, binary, repo, "default", &preview_args)?;
    evidence.record("destroy_preview", timing)?;
    require(
        preview["destruction"]["deleted"] == false && lock_identities(&state)? == locks,
        "Preview changed deletion state or locks",
    )?;
    for lease in &leases {
        let workspace = path(lease)?;
        require(workspace.is_dir(), "Preview removed a checkout")?;
        require(
            fs::read(workspace.join(&selected))?
                == if dirty {
                    b"discarded tracked edit".as_slice()
                } else {
                    original.as_slice()
                },
            "Preview changed tracked checkout data",
        )?;
        if dirty {
            require(
                fs::read(workspace.join("discarded-untracked"))? == b"discarded",
                "Preview removed untracked data",
            )?;
            for i in 0..128 {
                require(
                    fs::read(workspace.join(format!("ignored/residue-{i}")))? == b"discarded",
                    "Preview removed ignored data",
                )?;
            }
        }
        if artifact_files > 0 {
            artifact_contents(
                repo,
                lease,
                artifact_files + 1 + if dirty { 128 } else { 0 },
            )?;
        }
    }
    require(
        fs::read(&marker)? == b"keep root neighbor",
        "Preview removed unrelated root data",
    )?;
    evidence.checked("preview_preserved");
    let id = preview["destruction"]["pool_id"]
        .as_str()
        .ok_or_else(|| Error::Invalid("Missing pool generation".into()))?;
    args.extend(["--pool-id", id]);
    let (deleted, timing) = named(runner, binary, repo, "default", &args)?;
    require(
        deleted["destruction"]["deleted"] == true,
        "Deletion did not complete",
    )?;
    evidence.record("destroy", timing)?;
    let (retried, timing) = named(runner, binary, repo, "default", &args)?;
    require(
        retried["destruction"]["already_deleted"] == true,
        "Deletion retry was not stable",
    )?;
    evidence.record("destroy_retry", timing)?;
    for i in 1..=slots {
        require(
            !root.join(format!("wt-{i:04}")).exists(),
            "Deleted checkout remains",
        )?;
    }
    require(
        fs::read(&marker)? == b"keep root neighbor",
        "Deletion removed unrelated root data",
    )?;
    if let Some(entries) = deleted["destruction"]["artifacts"].as_array() {
        for entry in entries {
            require(
                !Path::new(
                    entry
                        .as_str()
                        .ok_or_else(|| Error::Invalid("Invalid artifact path".into()))?,
                )
                .exists(),
                "Deleted artifact remains",
            )?;
        }
        if artifact_files > 0 {
            require(
                !entries.is_empty(),
                "Cached deletion did not remove artifacts",
            )?;
        }
    } else {
        return Err(Error::Invalid("Missing deletion artifact report".into()));
    }
    evidence.checked("workspaces_removed");
    let native = runner.git_text(repo, &["worktree", "list", "--porcelain", "-z"])?;
    for slot in 1..=slots {
        let expected = format!("worktree {}", root.join(format!("wt-{slot:04}")).display());
        require(
            !native.split('\0').any(|field| field == expected),
            "Deleted native registration remains",
        )?;
    }
    evidence.checked("registrations_removed");
    require(
        lock_identities(&state)? == locks,
        "Deletion replaced or removed a permanent lock",
    )?;
    let replacement = parent.join("replacement-worktrees");
    let replacement_text = replacement
        .to_str()
        .ok_or_else(|| Error::Invalid("Non-UTF-8 fixture path".into()))?;
    let (reinitialized, timing) = init_pool(
        runner,
        binary,
        repo,
        Some("default"),
        &[
            "pool",
            "init",
            "--capacity",
            "1",
            "--workspace-root",
            replacement_text,
        ],
    )?;
    require(
        reinitialized["workspace_root"] == replacement_text && lock_identities(&state)? == locks,
        "Reinit lost permanent lock identities or used the wrong root",
    )?;
    evidence.record("reinitialize", timing)?;
    evidence.checked("permanent_locks");
    crate::harness::reject(
        runner,
        binary,
        repo,
        &["--pool", "default", "pool", "destroy", "--pool-id", id],
        "CONFIG_CONFLICT",
        4,
    )?;
    if let Some(lease_id) = &receipt {
        let (finished, _) = named(
            runner,
            binary,
            repo,
            "default",
            &["release", "--save", "--reservation-id", lease_id],
        )?;
        require(
            finished["snapshot"]["id"] == saved["id"],
            "Finish receipt was lost",
        )?;
        crate::harness::reject(
            runner,
            binary,
            repo,
            &[
                "acquire",
                "--task",
                "slot-0",
                "--idempotency-key",
                "delete-0",
            ],
            "INVALID_RESERVATION",
            5,
        )?;
    }
    require(
        runner.git(repo, ["show-ref"])? == refs,
        "Deletion/reinit changed shared refs",
    )?;
    let snapshot_id = saved["id"]
        .as_str()
        .ok_or_else(|| Error::Invalid("Missing saved snapshot".into()))?;
    let (shown, _) = call(runner, binary, repo, &["snapshot", "show", snapshot_id])?;
    require(
        shown["snapshot"] == saved,
        "Saved snapshot metadata changed",
    )?;
    let restored = parent.join("recovered-result");
    let restored_text = restored
        .to_str()
        .ok_or_else(|| Error::Invalid("Non-UTF-8 recovery path".into()))?;
    call(
        runner,
        binary,
        repo,
        &["snapshot", "restore", snapshot_id, "--to", restored_text],
    )?;
    require(
        fs::read(restored.join(&selected))?
            == if materialized {
                b"saved task result".to_vec()
            } else {
                original
            },
        "Retained snapshot did not recover the original result",
    )?;
    evidence.checked("retained_results");
    let (shown, _) = named(
        runner,
        binary,
        repo,
        "survivor",
        &["show", "--task", "slot-0"],
    )?;
    require(
        shown["reservation"]["reservation_id"] == sibling["reservation_id"]
            && fs::read(&sibling_file)? == b"preserved sibling",
        "Deletion/reinit damaged a sibling reservation/result",
    )?;
    named(
        runner,
        binary,
        repo,
        "survivor",
        &["release", "--task", "slot-0", "--force"],
    )?;
    evidence.checked("sibling_preserved");
    let (fresh, _) = call(runner, binary, repo, &["acquire", "--task", "replacement"])?;
    pristine(runner, repo, &fresh)?;
    release(runner, binary, repo, "replacement", false)?;
    evidence.checked("reinitialized");
    empty(runner, binary, repo)?;
    let (sibling, _) = named(runner, binary, repo, "survivor", &["list"])?;
    require(
        sibling["reservations"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "Sibling reservation remained active",
    )?;
    evidence.checked("released");
    require(
        fingerprint(runner, repo, false)? == before,
        "Deletion workload changed source checkout/index",
    )?;
    evidence.checked("source_preserved");
    crate::evidence::observation(case, evidence)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initialization_disables_background_work_and_accepts_legacy_lazy_baselines() {
        use std::os::unix::fs::PermissionsExt;
        const CHILD: &str = "GROOVE_BENCH_INIT_PEER_ROOT";
        const TEST: &str = "lifecycle::tests::initialization_disables_background_work_and_accepts_legacy_lazy_baselines";
        let Some(root) = std::env::var_os(CHILD) else {
            // Keep executable-file writes outside unrelated fixture forks.
            let root = tempfile::tempdir().unwrap();
            let mut runner = Runner::new().isolated(root.path()).unwrap();
            runner.timeout = Duration::from_secs(20);
            runner
                .env
                .insert(CHILD.into(), root.path().as_os_str().into());
            runner
                .run(
                    std::env::current_exe().unwrap(),
                    ["--exact", TEST],
                    root.path(),
                )
                .unwrap();
            assert_eq!(fs::read(root.path().join("entered")).unwrap(), b"entered");
            return;
        };
        let root = Path::new(&root);
        fs::write(root.join("entered"), b"entered").unwrap();
        let runner = Runner::new().isolated(root).unwrap();
        for mode in ["legacy", "current", "ignored-policy"] {
            let help = serde_json::json!({"api_version":2,"ok":true,"request_id":null,"result":{"text":
                if mode == "legacy" { "Usage: groove pool init --capacity N" }
                else { "Usage: groove pool init --capacity N --ready-target N --idle-timeout D" }
            }});
            let response = serde_json::json!({"api_version":2,"ok":true,"request_id":null,"result":
                if mode == "legacy" { serde_json::json!({}) }
                else { serde_json::json!({"ready_target":if mode == "current" {0} else {1},"idle_timeout":"never"}) }
            });
            let policy = if mode == "legacy" {
                "test \"$ready\" = absent; test \"$idle\" = absent"
            } else {
                "test \"$ready\" = 0; test \"$idle\" = never"
            };
            let script = r#"#!/bin/bash
set -eu
for argument in "$@"; do
  if [[ $argument == --help ]]; then
    cat <<'HELP_RESPONSE'
@HELP@
HELP_RESPONSE
    exit 0
  fi
done
ready=absent
idle=absent
while (( $# )); do
  case $1 in
    --ready-target) shift; ready=$1;;
    --idle-timeout) shift; idle=$1;;
  esac
  shift
done
@POLICY@
cat <<'INIT_RESPONSE'
@RESPONSE@
INIT_RESPONSE
"#
            .replace("@HELP@", &help.to_string())
            .replace("@POLICY@", policy)
            .replace("@RESPONSE@", &response.to_string());
            let binary = root.join(mode);
            fs::write(&binary, script).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
            let result = init_pool(
                &runner,
                &binary,
                root,
                Some("bench"),
                &["pool", "init", "--capacity", "2"],
            );
            if mode == "ignored-policy" {
                assert!(
                    matches!(result, Err(Error::Invalid(message)) if message.contains("did not disable"))
                );
            } else {
                assert!(result.is_ok(), "{mode}: {result:?}");
            }
        }
    }
    #[test]
    fn concurrent_peers_all_run_and_failures_or_panics_propagate() {
        let peers: BTreeSet<_> = parallel(4, Ok).unwrap().into_iter().collect();
        assert_eq!(peers, (0..4).collect());
        let failed = parallel(4, |i| {
            if i == 1 {
                Err(Error::Invalid("peer failed".into()))
            } else {
                Ok(i)
            }
        });
        assert!(matches!(failed,Err(Error::Invalid(message)) if message=="peer failed"));
        let panicked = parallel(4, |i| {
            assert_ne!(i, 1, "intentional peer panic");
            Ok(i)
        });
        assert!(matches!(panicked, Err(Error::WorkerPanic)));
    }
}
