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
    if let Workload::WorkspaceSetup {
        artifact_files,
        artifact_file_bytes,
        artifact_binary_bytes,
    } = case.workload
    {
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
