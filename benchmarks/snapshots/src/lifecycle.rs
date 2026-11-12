//! Focused workspace workloads with explicit setup and independent result checks.
use crate::{
    evidence::Evidence,
    harness::{command, fingerprint},
    profiles::{Case, Cleanup, Workload},
    statistics::Observation,
    util::{require, Error, Result, Runner},
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
fn path(lease: &Value) -> Result<PathBuf> {
    lease["worktree_path"]
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
    let artifacts = match case.workload {
        Workload::WorkspaceSetup {
            artifact_files,
            artifact_file_bytes,
            artifact_binary_bytes,
        } => Some((artifact_files, artifact_file_bytes, artifact_binary_bytes)),
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
        fs::write(
            repo.join(".groove.toml"),
            r#"schema_version = 1
[caches.artifact]
paths = ["ignored"]
inputs = ["artifact-input"]
key = "workspace-setup-v1"
mode = "copy"
[setup.dev]
cache = "artifact"
[[setup.dev.steps]]
name = "artifact"
command = ["/bin/bash", "-c", "mkdir -p ignored; cp -p artifact-source/* ignored/"]
when = "cache_miss"
"#,
        )?;
        runner.git(repo, ["add", "-A"])?;
        runner.git(repo, ["commit", "-qm", "setup-profile-v1"])?;
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
        list["leases"].as_array().is_some_and(Vec::is_empty),
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
    call(
        runner,
        binary,
        repo,
        &[
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
            pristine(runner, repo, &lease)?;
            evidence.record("release", release(runner, binary, repo, "single", false)?)?;
            let (lease, _) = call(runner, binary, repo, &["acquire", "--task", "dirty"])?;
            let workspace = path(&lease)?;
            fs::write(workspace.join(crate::fixture::spec().path(0)), b"dirty")?;
            fs::write(workspace.join("untracked"), b"dirty")?;
            fs::create_dir(workspace.join("ignored"))?;
            for i in 0..128 {
                fs::write(workspace.join(format!("ignored/residue-{i}")), b"cache")?;
            }
            release(runner, binary, repo, "dirty", true)?;
            let (lease, time) = call(runner, binary, repo, &["acquire", "--task", "clean"])?;
            evidence.record("dirty_acquire", time)?;
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
        Workload::WorkspaceSetup {
            artifact_files,
            artifact_file_bytes,
            artifact_binary_bytes,
        } => {
            for (task, hit, operation) in
                [("miss", false, "setup_miss"), ("hit", true, "setup_hit")]
            {
                let (lease, _) = call(runner, binary, repo, &["acquire", "--task", task])?;
                let (prepared, time) = call(
                    runner,
                    binary,
                    repo,
                    &["exec", "--task", task, "--setup", "dev"],
                )?;
                require(
                    prepared["preparation"]["status"] == "ready"
                        && prepared["preparation"]["cache_hit"] == hit,
                    "Unexpected setup/cache outcome",
                )?;
                require(
                    prepared["preparation"]["steps"][0]["skipped"] == hit,
                    "Cache-hit step was not skipped or cache-miss step did not execute",
                )?;
                if hit {
                    require(
                        prepared["preparation"]["seeded_files"] == (artifact_files + 1)
                            && prepared["preparation"]["seeded_bytes"]
                                == (artifact_files * artifact_file_bytes + artifact_binary_bytes),
                        "Cache seeding counters differ from fixture",
                    )?;
                }
                artifact_contents(repo, &lease, artifact_files + 1)?;
                evidence.record(operation, time)?;
                release(runner, binary, repo, task, false)?;
            }
            evidence.checked("cache_miss");
            evidence.checked("cache_hit");
            evidence.checked("artifact_contents");
            evidence.checked("step_skipped");
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
                "--wait-ms",
                "60000",
            ],
        )
    })?;
    let acquire_seconds = start.elapsed().as_secs_f64();
    let ids: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["lease_id"].as_str())
        .collect();
    let paths: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["worktree_path"].as_str())
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
        let (executed, time) = call(
            runner,
            binary,
            repo,
            &["exec", "--task", &task, "--", "/usr/bin/true"],
        )?;
        require(
            executed["execution"]["termination"] == "exited"
                && executed["execution"]["exit_code"] == 0,
            "Managed worker did not succeed",
        )?;
        Ok((time, release(runner, binary, repo, &task, false)?))
    })?;
    evidence.batch_seconds = Some(acquire_seconds + start.elapsed().as_secs_f64());
    for (executed, released) in completed {
        evidence.record("exec", executed)?;
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
        let (initialized, timing) = named(
            runner,
            binary,
            repo,
            pool,
            &["init", "--capacity", &workers.to_string()],
        )?;
        roots.push(PathBuf::from(
            initialized["worktree_root"]
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
                "--request-key",
                &format!("request-{}", i % workers),
                "--wait-ms",
                "60000",
            ],
        )
    })?;
    let acquire_seconds = started.elapsed().as_secs_f64();
    let ids: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["lease_id"].as_str())
        .collect();
    let paths: BTreeSet<_> = acquired
        .iter()
        .map(|(l, _)| l["worktree_path"].as_str())
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
    let (catalog, timing) = call(runner, binary, repo, &["pools"])?;
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
        let (executed, time) = named(
            runner,
            binary,
            repo,
            pool,
            &[
                "exec",
                "--task",
                &task,
                "--",
                "/bin/sh",
                "-c",
                "printf %s \"$GROOVE_POOL\"",
            ],
        )?;
        let (_, released) = named(runner, binary, repo, pool, &["release", "--task", &task])?;
        Ok((executed, time, released))
    })?;
    evidence.batch_seconds = Some(acquire_seconds + started.elapsed().as_secs_f64());
    for (i, (executed, time, released)) in completed.into_iter().enumerate() {
        require(
            executed["execution"]["termination"] == "exited"
                && executed["execution"]["exit_code"] == 0,
            "Named-pool worker failed",
        )?;
        let log = executed["execution"]["run"]["stdout_path"]
            .as_str()
            .ok_or_else(|| Error::Invalid("Missing worker log".into()))?;
        require(
            fs::read_to_string(log)? == names[i / workers],
            "Worker received another pool's environment",
        )?;
        evidence.record("exec", time)?;
        evidence.record("release", released)?;
    }
    evidence.checked("successful_workers");
    for pool in &names {
        let (list, _) = named(runner, binary, repo, pool, &["list"])?;
        require(
            list["leases"].as_array().is_some_and(Vec::is_empty),
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
    let mut init = vec!["init", "--capacity"];
    let capacity = slots.to_string();
    init.push(&capacity);
    let external_text = external_root
        .to_str()
        .ok_or_else(|| Error::Invalid("Non-UTF-8 fixture root".into()))?;
    if external {
        init.extend(["--worktree-root", external_text]);
    }
    let (initialized, _) = call(runner, binary, repo, &init)?;
    let root = PathBuf::from(
        initialized["worktree_root"]
            .as_str()
            .ok_or_else(|| Error::Invalid("Missing checkout root".into()))?,
    );
    let state = PathBuf::from(
        initialized["state_dir"]
            .as_str()
            .ok_or_else(|| Error::Invalid("Missing pool state".into()))?,
    );
    named(
        runner,
        binary,
        repo,
        "survivor",
        &["init", "--capacity", "1"],
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
                    "--request-key",
                    &format!("delete-{i}"),
                ],
            )?;
            pristine(runner, repo, &lease)?;
            if artifact_files > 0 {
                let (setup, _) = call(
                    runner,
                    binary,
                    repo,
                    &["exec", "--task", &format!("slot-{i}"), "--setup", "dev"],
                )?;
                require(
                    setup["preparation"]["status"] == "ready",
                    "Deletion cache setup failed",
                )?;
                artifact_contents(repo, &lease, artifact_files + 1)?;
            }
            leases.push(lease);
        }
    }
    let selected = crate::fixture::spec().path(0);
    let original = fs::read(repo.join(&selected))?;
    let (saved, history, receipt) = if let Some(first) = leases.first() {
        fs::write(path(first)?.join(&selected), b"saved task result")?;
        call(
            runner,
            binary,
            repo,
            &[
                "exec",
                "--task",
                "slot-0",
                "--",
                "/bin/sh",
                "-c",
                "printf retained-log",
            ],
        )?;
        let (finished, _) = call(runner, binary, repo, &["finish", "--task", "slot-0"])?;
        require(
            finished["snapshot"]["pinned"] == true,
            "Finished result was not pinned",
        )?;
        let (history, _) = call(runner, binary, repo, &["history"])?;
        fs::write(path(first)?.join(&selected), &original)?;
        for i in 1..slots {
            release(runner, binary, repo, &format!("slot-{i}"), false)?;
        }
        (
            finished["snapshot"].clone(),
            Some(history),
            first["lease_id"].as_str().map(String::from),
        )
    } else {
        let (saved, _) = call(runner, binary, repo, &["snapshot", "save"])?;
        (saved["snapshot"].clone(), None, None)
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
        crate::harness::reject(runner, binary, repo, &["destroy"], "INVALID_ARGUMENT", 2)?,
    )?;
    let mut args = vec!["destroy"];
    if dirty {
        args.push("--discard-changes");
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
    let (reinitialized, timing) = named(
        runner,
        binary,
        repo,
        "default",
        &[
            "init",
            "--capacity",
            "1",
            "--worktree-root",
            replacement_text,
        ],
    )?;
    require(
        reinitialized["worktree_root"] == replacement_text && lock_identities(&state)? == locks,
        "Reinit lost permanent lock identities or used the wrong root",
    )?;
    evidence.record("reinitialize", timing)?;
    evidence.checked("permanent_locks");
    crate::harness::reject(
        runner,
        binary,
        repo,
        &["--pool", "default", "destroy", "--pool-id", id],
        "CONFIG_CONFLICT",
        4,
    )?;
    if let Some(lease_id) = &receipt {
        let (finished, _) = named(
            runner,
            binary,
            repo,
            "default",
            &["finish", "--lease-id", lease_id],
        )?;
        require(
            finished["snapshot"]["id"] == saved["id"],
            "Finish receipt was lost",
        )?;
        crate::harness::reject(
            runner,
            binary,
            repo,
            &["acquire", "--task", "slot-0", "--request-key", "delete-0"],
            "INVALID_LEASE",
            5,
        )?;
    }
    if let Some(history) = history {
        let (retained, _) = named(runner, binary, repo, "default", &["history"])?;
        require(
            retained == history,
            "Deletion/reinit changed retained history",
        )?;
        let runs = retained["runs"]
            .as_array()
            .ok_or_else(|| Error::Invalid("Missing retained runs".into()))?;
        require(
            runs.iter().any(|r| {
                r["stdout_path"]
                    .as_str()
                    .is_some_and(|p| fs::read(p).is_ok_and(|bytes| bytes == b"retained-log"))
            }),
            "Retained log contents were lost",
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
        shown["lease"]["lease_id"] == sibling["lease_id"]
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
        sibling["leases"].as_array().is_some_and(Vec::is_empty),
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
