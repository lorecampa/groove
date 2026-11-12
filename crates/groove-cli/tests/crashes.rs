#![cfg(feature = "test-hooks")]
mod support;
use groove_core::{Pool, PoolOptions};
use std::{
    os::unix::{fs::MetadataExt, net::UnixListener},
    process::{Command, Stdio},
};
use support::Repo;

fn checkpoint_command(r: &Repo, name: &str, args: &[&str]) -> (Command, UnixListener) {
    let socket = r.temp.path().join("checkpoint.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&r.path);
    command
        .arg("--repo")
        .arg(&r.path)
        .args(args)
        .env("GROOVE_TEST_CHECKPOINT", name)
        .env("GROOVE_TEST_SOCKET", socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (command, listener)
}
fn kill_at_checkpoint(command: &mut Command, listener: UnixListener, name: &str) {
    let mut child = support::TestChild::spawn(command);
    let stream = child
        .checkpoint(&listener, name, support::PROCESS_TIMEOUT)
        .unwrap();
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    drop(stream);
}
#[test]
fn initialization_crash_boundaries_resume_without_replacing_locks() {
    for name in [
        "metadata-synced",
        "metadata-renamed",
        "before-ready-manifest",
    ] {
        let r = Repo::new();
        let (mut command, listener) = checkpoint_command(&r, name, &["init", "--capacity", "1"]);
        kill_at_checkpoint(&mut command, listener, name);
        let lock = r.state().join("locks/wt-0001.lock");
        let old = std::fs::metadata(&lock).ok().map(|m| m.ino());
        Pool::init(
            &r.path,
            PoolOptions {
                resume: r.state().join("initializing.json").exists(),
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        if let Some(old) = old {
            assert_eq!(std::fs::metadata(&lock).unwrap().ino(), old);
        }
        assert!(Pool::open(&r.path)
            .unwrap()
            .acquire(Default::default())
            .is_ok());
    }
}

#[test]
fn pool_growth_crash_boundaries_preserve_reservations_and_lock_inodes() {
    for name in ["pool-slot-added", "before-pool-update", "pool-updated"] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.reserve(Default::default()).unwrap();
        std::fs::write(
            lease.worktree_path.join("tracked.txt"),
            "protected result\n",
        )
        .unwrap();
        let lock = r.state().join("locks/wt-0001.lock");
        let inode = std::fs::metadata(&lock).unwrap().ino();
        let (mut command, listener) = checkpoint_command(
            &r,
            name,
            &["config", "set", "--capacity", "3", "--cleanup", "warm"],
        );
        kill_at_checkpoint(&mut command, listener, name);
        assert_eq!(
            pool.settings().unwrap().capacity,
            if name == "pool-updated" { 3 } else { 1 }
        );
        pool.configure(groove_core::PoolUpdate {
            capacity: Some(3),
            cleanup_policy: Some(groove_core::CleanupPolicy::Warm),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
        assert_eq!(
            std::fs::read(lease.worktree_path.join("tracked.txt")).unwrap(),
            b"protected result\n"
        );
        assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
        let one = pool.acquire(Default::default()).unwrap();
        let two = pool.acquire(Default::default()).unwrap();
        assert_ne!(one.info().slot_id, two.info().slot_id);
        assert_eq!(
            pool.acquire(Default::default()).err().unwrap().code,
            groove_core::ErrorCode::PoolExhausted
        );
    }
}

#[test]
fn setup_crash_boundaries_never_infer_readiness_or_replace_locks() {
    use groove_core::{PreparationOptions, PreparationStatus};
    for name in [
        "setup-recorded",
        "setup-step-completed",
        "before-cache-publish",
        "cache-published",
        "before-setup-ready",
        "setup-completed",
    ] {
        let repo = Repo::new();
        std::fs::write(
            repo.path.join(".groove.toml"),
            r#"
schema_version=1
[caches.deps]
paths=["ignored/deps"]
inputs=["tracked.txt"]
key="fixture"
[setup.dev]
cache="deps"
[[setup.dev.steps]]
name="dependencies"
command=["bash","-c","mkdir -p ignored/deps; printf artifact > ignored/deps/file"]
when="cache_miss"
"#,
        )
        .unwrap();
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "project"]);
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&repo.path).unwrap();
        let lease = pool.reserve(Default::default()).unwrap();
        let lock = repo.state().join("locks/wt-0001.execution.lock");
        let inode = std::fs::metadata(&lock).unwrap().ino();
        let (mut command, listener) = checkpoint_command(
            &repo,
            name,
            &[
                "exec",
                "--lease-id",
                &lease.lease_id.to_string(),
                "--setup",
                "dev",
                "--grace-ms",
                "0",
            ],
        );
        kill_at_checkpoint(&mut command, listener, name);
        let observed = pool.preparation(lease.lease_id).unwrap().unwrap();
        assert_eq!(
            observed.status,
            if name == "setup-completed" {
                PreparationStatus::Ready
            } else {
                PreparationStatus::Preparing
            }
        );
        assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
        assert!(pool.acquire(Default::default()).is_err());
        assert_eq!(std::fs::metadata(&lock).unwrap().ino(), inode);
        if observed.status != PreparationStatus::Ready {
            assert!(pool
                .prepare(lease.lease_id, "dev", PreparationOptions::default())
                .is_err());
        }
        // Other test threads can be between fork and exec with inherited CLOEXEC
        // descriptors. Observe exclusion closing; never clear records or bypass locks.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let ready = loop {
            match pool.prepare(
                lease.lease_id,
                "dev",
                PreparationOptions {
                    retry: true,
                    ..Default::default()
                },
            ) {
                Ok(ready) => break ready,
                Err(error)
                    if error.code == groove_core::ErrorCode::InvalidLease
                        && error.message
                            == "Managed execution is active; stop it before releasing"
                        && std::time::Instant::now() < deadline =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(error) => panic!("{name}: {error:?}"),
            }
        };
        assert_eq!(ready.status, PreparationStatus::Ready);
        assert_eq!(
            std::fs::read(lease.worktree_path.join("ignored/deps/file")).unwrap(),
            b"artifact"
        );
        assert_eq!(std::fs::metadata(lock).unwrap().ino(), inode);
    }
}

#[test]
fn acquisition_crash_boundaries_never_hand_out_an_unverified_worktree() {
    for name in [
        "before-normalization",
        "old-worktree-removed",
        "worktree-created",
        "verified",
        "metadata-synced",
        "metadata-renamed",
        "lease-recorded",
    ] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        if name == "before-normalization" {
            drop(pool.acquire(Default::default()).unwrap());
        }
        let ino = std::fs::metadata(r.state().join("locks/wt-0001.lock"))
            .unwrap()
            .ino();
        let (mut command, listener) = checkpoint_command(&r, name, &["acquire", "--base", "HEAD"]);
        kill_at_checkpoint(&mut command, listener, name);
        let lease = pool.acquire(Default::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(lease.info().worktree_path.join("tracked.txt")).unwrap(),
            "baseline\n"
        );
        assert_eq!(
            std::fs::metadata(r.state().join("locks/wt-0001.lock"))
                .unwrap()
                .ino(),
            ino
        );
    }
}

#[test]
fn durable_shell_reservations_survive_sigkill_and_release_retries_are_generation_safe() {
    for checkpoint in [
        "reuse-protected",
        "metadata-renamed",
        "release-recorded",
        "release-unlocked",
    ] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        if checkpoint == "reuse-protected" {
            let (mut command, listener) = checkpoint_command(&r, checkpoint, &["acquire"]);
            kill_at_checkpoint(&mut command, listener, checkpoint);
        } else {
            let output = support::groove()
                .arg("--repo")
                .arg(&r.path)
                .arg("acquire")
                .output()
                .unwrap();
            support::assert_success(&output);
        }
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.protected_leases().unwrap().remove(0);
        if checkpoint != "reuse-protected" {
            let (mut command, listener) = checkpoint_command(
                &r,
                checkpoint,
                &["release", "--lease-id", &lease.lease_id.to_string()],
            );
            kill_at_checkpoint(&mut command, listener, checkpoint);
        } else {
            assert!(pool.acquire(Default::default()).is_err());
        }
        // Use independent CLI processes throughout. Taking locks in this
        // multithreaded harness can transiently lend them to another test's fork
        // before CLOEXEC runs; that is not the ordinary one-shot CLI lifecycle.
        let release = |id: groove_core::LeaseId| {
            support::groove()
                .arg("--repo")
                .arg(&r.path)
                .args(["release", "--lease-id", &id.to_string()])
                .output()
                .unwrap()
        };
        support::assert_success(&release(lease.lease_id));
        support::assert_success(&release(lease.lease_id));
        let acquired = support::groove()
            .arg("--repo")
            .arg(&r.path)
            .arg("acquire")
            .output()
            .unwrap();
        support::assert_success(&acquired);
        let next = pool.protected_leases().unwrap().remove(0);
        assert!(!release(lease.lease_id).status.success());
        support::assert_success(&release(next.lease_id));
    }
}

#[test]
fn durable_request_retry_recovers_protection_published_before_its_journal() {
    for checkpoint in ["reservation-protected", "reservation-journaled"] {
        let r = Repo::new();
        let cli = |args: &[&str]| support::groove_in(&r.path).args(args).output().unwrap();
        support::assert_success(&cli(&["init", "--capacity", "2"]));
        let (mut command, listener) =
            checkpoint_command(&r, checkpoint, &["acquire", "--request-key", "lost-task"]);
        kill_at_checkpoint(&mut command, listener, checkpoint);
        let original = Pool::open(&r.path)
            .unwrap()
            .protected_leases()
            .unwrap()
            .remove(0);
        let retry = support::assert_success(&cli(&["acquire", "--request-key", "lost-task"]));
        assert_eq!(retry["result"]["lease_id"], original.lease_id.to_string());
        assert_eq!(
            Pool::open(&r.path)
                .unwrap()
                .protected_leases()
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn finish_crash_retries_preserve_checkpoint_and_never_release_a_successor() {
    for checkpoint in ["finish-snapshot-saved", "finish-released"] {
        let r = Repo::new();
        let cli = |args: &[&str]| support::groove_in(&r.path).args(args).output().unwrap();
        support::assert_success(&cli(&["init", "--capacity", "1"]));
        let lease = support::assert_success(&cli(&["acquire"]))["result"].clone();
        let id = lease["lease_id"].as_str().unwrap();
        std::fs::write(r.slot(1).join("tracked.txt"), "checkpoint results").unwrap();
        let (mut command, listener) =
            checkpoint_command(&r, checkpoint, &["finish", "--lease-id", id]);
        kill_at_checkpoint(&mut command, listener, checkpoint);
        let receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(r.state().join(format!("slots/finish-{id}.json"))).unwrap(),
        )
        .unwrap();
        let successor = if checkpoint == "finish-released" {
            Some(support::assert_success(&cli(&["acquire"]))["result"].clone())
        } else {
            assert!(!cli(&["acquire"]).status.success());
            None
        };
        let retry = support::assert_success(&cli(&["finish", "--lease-id", id]));
        assert_eq!(retry["result"]["snapshot"]["id"], receipt["snapshot"]["id"]);
        let recovered = r.temp.path().join("recovered");
        support::assert_success(&cli(&[
            "snapshot",
            "restore",
            receipt["snapshot"]["id"].as_str().unwrap(),
            "--to",
            recovered.to_str().unwrap(),
        ]));
        assert_eq!(
            std::fs::read_to_string(recovered.join("tracked.txt")).unwrap(),
            "checkpoint results"
        );
        if let Some(successor) = successor {
            assert!(!cli(&["acquire"]).status.success());
            support::assert_success(&cli(&[
                "release",
                "--lease-id",
                successor["lease_id"].as_str().unwrap(),
            ]));
        }
    }
}

#[test]
fn history_recovery_keeps_unknown_and_known_outcomes_distinct() {
    use groove_core::{CleanupStatus, HistoryFilter, RunOutcome};
    use std::time::Duration;
    for (point, expected) in [
        ("execution-recorded", RunOutcome::Recovered),
        ("run-completed", RunOutcome::Completed),
    ] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.reserve(Default::default()).unwrap();
        let id = lease.lease_id.to_string();
        let (mut command, listener) = checkpoint_command(
            &r,
            point,
            &[
                "exec",
                "--lease-id",
                &id,
                "--",
                "bash",
                "-c",
                "printf result; exit 9",
            ],
        );
        let mut child = support::TestChild::spawn(&mut command);
        let stream = child
            .checkpoint(&listener, point, support::PROCESS_TIMEOUT)
            .unwrap();
        let preview = pool.clean_logs(Duration::ZERO, false).unwrap();
        assert_eq!(preview.entries.len(), 1);
        assert_eq!(preview.entries[0].status, CleanupStatus::Skipped);
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());
        drop(stream);
        let runs = pool.history(&HistoryFilter::default()).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(
            runs[0].outcome,
            if point == "execution-recorded" {
                RunOutcome::Unresolved
            } else {
                RunOutcome::Completed
            }
        );
        assert_eq!(
            pool.clean_logs(Duration::ZERO, false).unwrap().entries[0].status,
            CleanupStatus::Skipped
        );
        assert!(runs[0].stdout_path.exists());
        assert!(pool.history_delete(runs[0].run_id).is_err());
        pool.recover_execution(lease.lease_id).unwrap();
        let recovered = pool.history_show(runs[0].run_id).unwrap();
        assert_eq!(recovered.outcome, expected);
        assert_eq!(
            recovered.exit_code,
            if expected == RunOutcome::Completed {
                Some(9)
            } else {
                None
            }
        );
        assert_eq!(
            pool.clean_logs(Duration::ZERO, false).unwrap().entries[0].status,
            CleanupStatus::Removed
        );
        assert_eq!(
            pool.lease_info(lease.lease_id).unwrap().lease_id,
            lease.lease_id
        );
    }
}

#[test]
fn interrupted_log_cleanup_retries_without_forgetting_the_outcome() {
    use groove_core::{CleanupStatus, ExecOptions, LogState};
    use std::time::Duration;
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    let run = pool
        .exec(
            lease.lease_id,
            ExecOptions {
                command: ["bash", "-c", "printf output; printf error >&2"]
                    .map(Into::into)
                    .to_vec(),
                ..Default::default()
            },
        )
        .unwrap();
    let id = run.run.run_id.parse().unwrap();
    // Capture every permanent lock, independent of lock naming conventions.
    let inodes: Vec<_> = std::fs::read_dir(r.state().join("locks"))
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let inode = std::fs::metadata(&path).unwrap().ino();
            (path, inode)
        })
        .collect();
    let (mut command, listener) = checkpoint_command(
        &r,
        "log-removed",
        &["storage", "clean", "--logs", "--older-than", "0s"],
    );
    kill_at_checkpoint(&mut command, listener, "log-removed");
    assert!(!run.run.stdout_path.exists());
    assert!(run.run.stderr_path.exists());
    assert_eq!(pool.history_show(id).unwrap().logs, LogState::Removing);
    assert_eq!(
        pool.clean_logs(Duration::ZERO, false).unwrap().entries[0].status,
        CleanupStatus::Removed
    );
    let record = pool.history_show(id).unwrap();
    assert_eq!(record.logs, LogState::Removed);
    assert_eq!(record.exit_code, Some(0));
    assert!(!run.run.stderr_path.exists());
    for (path, inode) in inodes {
        assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
    }
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().lease_id,
        lease.lease_id
    );
}

#[test]
fn recovery_reconstructs_missing_history_without_guessing_origin_or_exit() {
    use groove_core::{HistoryFilter, RunOrigin, RunOutcome};
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    let (mut command, listener) = checkpoint_command(
        &r,
        "execution-protected",
        &[
            "exec",
            "--lease-id",
            &lease.lease_id.to_string(),
            "--",
            "true",
        ],
    );
    kill_at_checkpoint(&mut command, listener, "execution-protected");
    assert!(pool.history(&HistoryFilter::default()).unwrap().is_empty());
    assert_eq!(pool.storage_usage().unwrap().untracked_runs, 1);
    assert!(pool
        .clean_logs(std::time::Duration::ZERO, false)
        .unwrap()
        .entries
        .is_empty());
    pool.recover_execution(lease.lease_id).unwrap();
    let runs = pool.history(&HistoryFilter::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::Recovered);
    assert!(matches!(runs[0].origin, RunOrigin::Unknown));
    assert!(runs[0].exit_code.is_none());
    assert!(runs[0].duration_ms.is_none());
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().lease_id,
        lease.lease_id
    );
}

#[test]
fn checkpoint_finish_ignores_parent_git_filters() {
    let fixture = tempfile::tempdir().unwrap();
    let config = fixture.path().join("global.gitconfig");
    std::fs::write(&config, "[filter \"fixture\"]\nclean = cat\nsmudge = cat\n").unwrap();
    // Use a child process: changing the test runner's global environment would
    // interfere with concurrently executing tests. The selected scenario verifies
    // preserved snapshots and generation-safe retry through actual CLI crashes.
    let output = support::TestChild::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "finish_crash_retries_preserve_checkpoint_and_never_release_a_successor",
                "--nocapture",
            ])
            .env("GIT_CONFIG_GLOBAL", config)
            .env("GIT_CONFIG_NOSYSTEM", "1"),
    )
    .wait_with_output()
    .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
}

#[test]
fn verification_rejects_writes_racing_in_place_cleanup() {
    // Another fixture's fork could briefly keep this fixture's just-dropped lease.
    if !support::in_subprocess("verification_rejects_writes_racing_in_place_cleanup") {
        return;
    }
    // A straggling writer (for example an agent that was never stopped) can touch
    // a released checkout after Git cleaned it. Verification, not cleanup, is
    // what keeps that contamination from being handed to the next task.
    for contamination in ["untracked", "modified"] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        drop(pool.acquire(Default::default()).unwrap());
        let name = "normalized-in-place";
        let (mut command, listener) = checkpoint_command(&r, name, &["acquire", "--task", "next"]);
        let mut child = support::TestChild::spawn(&mut command);
        let stream = child
            .checkpoint(&listener, name, support::PROCESS_TIMEOUT)
            .unwrap();
        match contamination {
            "untracked" => std::fs::write(r.slot(1).join("stray.txt"), "late write").unwrap(),
            _ => std::fs::write(r.slot(1).join("tracked.txt"), "late write").unwrap(),
        }
        drop(stream);
        let output = child.wait_with_output().unwrap();
        let response = support::assert_success(&output);
        assert_eq!(response["result"]["recovery"], "rebuilt", "{contamination}");
        assert!(!r.slot(1).join("stray.txt").exists(), "{contamination}");
        assert_eq!(
            std::fs::read_to_string(r.slot(1).join("tracked.txt")).unwrap(),
            "baseline\n",
            "{contamination}"
        );
    }
}
