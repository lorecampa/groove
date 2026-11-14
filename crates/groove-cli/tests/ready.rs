mod support;
use groove_core::{Pool, PoolOptions, ReadySettings};
use serde_json::Value;
use std::{
    fs,
    time::{Duration, Instant},
};
use support::{assert_success, Repo};
#[test]
fn worker_startup_failures_are_recorded_in_configured_logs() {
    let repo = Repo::new();
    let logs = repo.temp.path().join("logs");
    let output = support::command(&repo.path, support::groove_binary())
        .arg("--internal-ready-worker")
        .env("GROOVE_INTERNAL_READY_WORKER", "1")
        .env("GROOVE_LOG_ENABLED", "true")
        .env("GROOVE_LOG_DIR", &logs)
        .env("XDG_CONFIG_HOME", repo.temp.path().join("config"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    let events: Vec<Value> = fs::read_dir(logs)
        .unwrap()
        .flat_map(|entry| {
            fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect();
    assert!(
        events
            .iter()
            .any(|event| event.to_string().contains("ready.failed")
                && event.to_string().contains("INVALID_ARGUMENT")),
        "{events:?}"
    );
}

fn cli(repo: &Repo, args: &[&str]) -> Value {
    let output = support::groove_in(&repo.path).args(args).output().unwrap();
    assert_success(&output);
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    value["result"].clone()
}
fn wait_ready(pool: &Pool, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let report = pool.ready_report().unwrap();
        if !report.running && report.ready == count && report.attempted_at_unix_ms > 0 {
            return;
        }
        assert!(Instant::now() < deadline, "ready deadline: {report:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn background_workers_do_not_retain_external_lock_descriptors() {
    if !support::in_subprocess("background_workers_do_not_retain_external_lock_descriptors") {
        return;
    }
    use std::os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, process::CommandExt},
    };
    let repo = Repo::new();
    cli(
        &repo,
        &[
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    );
    let pool = Pool::open(&repo.path).unwrap();
    let real_git = support::command(&repo.path, "sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let bin = repo.temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let script = bin.join("git");
    fs::write(
        &script,
        r#"#!/bin/bash
if [[ ${GROOVE_INTERNAL_READY_WORKER:-} == 1 && ! -e "$GROOVE_WORKER_ENTERED" ]]; then
  printf entered > "$GROOVE_WORKER_ENTERED"
  for ((i=0; i<1500; i++)); do
    [[ -e "$GROOVE_WORKER_RESUME" ]] && break
    sleep 0.01
  done
fi
exec "$GROOVE_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let entered = repo.temp.path().join("entered");
    let resume = repo.temp.path().join("resume");
    struct Resume(std::path::PathBuf);
    impl Drop for Resume {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, b"go");
        }
    }
    let unblock = Resume(resume.clone());
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let lock_path = repo.temp.path().join("external.lock");
    let lock = fs::File::create(&lock_path).unwrap();
    let fd = lock.as_raw_fd();
    // SAFETY: flock borrows the live file; fcntl in the child only clears CLOEXEC.
    assert_eq!(unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) }, 0);
    let mut command = support::groove_in(&repo.path);
    command
        .args(["config", "set", "--ready-target", "1"])
        .env("PATH", path)
        .env("GROOVE_REAL_GIT", real_git.trim())
        .env("GROOVE_WORKER_ENTERED", &entered)
        .env("GROOVE_WORKER_RESUME", &resume);
    // SAFETY: only async-signal-safe fcntl executes after fork; lock stays owned until wait.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = support::TestChild::spawn(&mut command);
    assert_success(&child.wait_with_output().unwrap());
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    while !entered.exists() {
        assert!(
            Instant::now() < deadline,
            "worker did not enter Git barrier"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(lock);
    let observer = fs::File::open(&lock_path).unwrap();
    // SAFETY: the independently opened descriptor remains live through the syscall.
    let acquired = unsafe { libc::flock(observer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    drop(unblock);
    wait_ready(&pool, 1);
    assert!(
        acquired,
        "detached worker retained the caller's external lock"
    );
}

#[test]
fn cli_defaults_prepare_and_replenish_without_a_public_worker_command() {
    if !support::in_subprocess("cli_defaults_prepare_and_replenish_without_a_public_worker_command")
    {
        return;
    }
    let repo = Repo::new();
    let initialized = cli(&repo, &["init", "--capacity", "3"]);
    assert_eq!(initialized["ready_target"], 1);
    assert_eq!(initialized["idle_timeout"], "7d");
    let pool = Pool::open(&repo.path).unwrap();
    wait_ready(&pool, 1);
    let first = cli(&repo, &["acquire", "--task", "one"]);
    wait_ready(&pool, 1);
    // Confirm an actual second checkout exists, rather than trusting a stale report.
    let deadline = Instant::now() + Duration::from_secs(20);
    while fs::read_dir(&pool.settings().unwrap().worktree_root)
        .unwrap()
        .count()
        != 2
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let second = cli(&repo, &["acquire", "--task", "two"]);
    assert_ne!(first["slot_id"], second["slot_id"]);
    wait_ready(&pool, 1);
    assert_eq!(pool.protected_leases().unwrap().len(), 2);
    assert!(cli(&repo, &["show"])["ready"].is_object());
    let help = cli(&repo, &["--help"]);
    assert!(!help["text"].as_str().unwrap().contains("reconcile"));
    // Consume the last spare so any remaining worker has no preparation to do.
    cli(&repo, &["acquire", "--task", "three"]);
    wait_ready(&pool, 0);
}
#[test]
fn global_defaults_are_copied_and_invalid_targets_preserve_pool_settings() {
    if !support::in_subprocess(
        "global_defaults_are_copied_and_invalid_targets_preserve_pool_settings",
    ) {
        return;
    }
    let repo = Repo::new();
    cli(
        &repo,
        &[
            "config",
            "global",
            "set",
            "pool_defaults.ready_target=2",
            "pool_defaults.idle_timeout=never",
        ],
    );
    let output = support::groove_in(&repo.path)
        .args(["init", "--capacity", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!repo.state().join("config.json").exists());
    cli(&repo, &["init", "--capacity", "2", "--ready-target", "0"]);
    cli(
        &repo,
        &[
            "config",
            "global",
            "set",
            "pool_defaults.ready_target=1",
            "pool_defaults.idle_timeout=1h",
        ],
    );
    let settings = cli(&repo, &["config", "show"]);
    assert_eq!(settings["pool"]["ready_target"], 0);
    assert_eq!(settings["pool"]["idle_timeout"], "never");
    let before = fs::read(repo.state().join("config.json")).unwrap();
    let output = support::groove_in(&repo.path)
        .args(["config", "set", "--ready-target", "3"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(fs::read(repo.state().join("config.json")).unwrap(), before);
    cli(
        &repo,
        &["config", "set", "--capacity", "3", "--ready-target", "3"],
    );
    wait_ready(&Pool::open(&repo.path).unwrap(), 3);
}

#[cfg(feature = "test-hooks")]
fn worker(
    repo: &Repo,
    pool: &Pool,
    checkpoint: &str,
) -> (support::TestChild, std::os::unix::net::UnixListener) {
    let generation = pool.request_ready().unwrap().unwrap();
    let socket = repo.temp.path().join("ready.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut command = support::command(&repo.path, support::groove_binary());
    command
        .args(["--internal-ready-worker"])
        .arg(pool.common_git_dir())
        .arg("default")
        .arg(generation.to_string())
        .arg("5000")
        .env("GROOVE_INTERNAL_READY_WORKER", "1")
        .env("GROOVE_TEST_CHECKPOINT", checkpoint)
        .env("GROOVE_TEST_SOCKET", socket);
    (support::TestChild::spawn(&mut command), listener)
}
#[cfg(feature = "test-hooks")]
fn initialized(repo: &Repo) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            ready: Some(ReadySettings::default()),
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}
#[cfg(feature = "test-hooks")]
#[test]
fn killed_preparer_recovers_and_concurrent_worker_cannot_duplicate_preparation() {
    if !support::in_subprocess(
        "killed_preparer_recovers_and_concurrent_worker_cannot_duplicate_preparation",
    ) {
        return;
    }
    let repo = Repo::new();
    let pool = initialized(&repo);
    let (mut child, listener) = worker(&repo, &pool, "ready-before-preparation");
    let stream = child
        .checkpoint(
            &listener,
            "ready-before-preparation",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    assert!(pool.ready_report().unwrap().running);
    let generation = pool.request_ready().unwrap().unwrap();
    pool.run_ready(generation).unwrap(); // coalesces, never takes the busy slot
    assert!(pool.protected_leases().unwrap().is_empty());
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    drop(stream);
    pool.run_ready(generation).unwrap();
    assert_eq!(pool.ready_report().unwrap().ready, 1);
    assert_eq!(
        fs::read_dir(pool.settings().unwrap().worktree_root)
            .unwrap()
            .count(),
        1
    );
    let lease = pool.reserve(Default::default()).unwrap();
    assert_eq!(
        fs::read_to_string(lease.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}
#[cfg(feature = "test-hooks")]
#[test]
fn killed_eviction_resumes_without_replacing_locks_or_losing_capacity() {
    if !support::in_subprocess("killed_eviction_resumes_without_replacing_locks_or_losing_capacity")
    {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let repo = Repo::new();
    let pool = initialized(&repo);
    let generation = pool.request_ready().unwrap().unwrap();
    pool.run_ready(generation).unwrap();
    let slot = repo.state().join("slots/wt-0001.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&slot).unwrap()).unwrap();
    record["idle_since_unix_ms"] = 1.into();
    fs::write(&slot, serde_json::to_vec(&record).unwrap()).unwrap();
    pool.configure(groove_core::PoolUpdate {
        ready_target: Some(0),
        idle_timeout: Some("1s".parse().unwrap()),
        ..Default::default()
    })
    .unwrap();
    let lock = repo.state().join("locks/wt-0001.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    let (mut child, listener) = worker(&repo, &pool, "owned-registrations-removed");
    let stream = child
        .checkpoint(
            &listener,
            "owned-registrations-removed",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    drop(stream);
    pool.run_ready(generation).unwrap();
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
    assert!(!repo.slot(1).exists());
    assert_eq!(pool.settings().unwrap().capacity, 2);
    pool.reserve(Default::default()).unwrap();
}

#[cfg(feature = "test-hooks")]
#[test]
fn request_during_worker_exit_is_processed_before_owner_exits() {
    if !support::in_subprocess("request_during_worker_exit_is_processed_before_owner_exits") {
        return;
    }
    use std::io::Write;
    let repo = Repo::new();
    let pool = initialized(&repo);
    let (mut child, listener) = worker(&repo, &pool, "ready-pass-completed");
    let mut stream = child
        .checkpoint(&listener, "ready-pass-completed", support::PROCESS_TIMEOUT)
        .unwrap();
    pool.configure(groove_core::PoolUpdate {
        ready_target: Some(2),
        ..Default::default()
    })
    .unwrap();
    pool.request_ready().unwrap();
    stream.write_all(b"continue\n").unwrap();
    drop(stream);
    let mut stream = child
        .checkpoint(&listener, "ready-pass-completed", support::PROCESS_TIMEOUT)
        .unwrap();
    assert_eq!(pool.ready_report().unwrap().ready, 2);
    stream.write_all(b"continue\n").unwrap();
    drop(stream);
    assert!(child.wait().unwrap().success());
}
#[cfg(feature = "test-hooks")]
#[test]
fn released_retry_during_preparation_never_unlocks_the_worker() {
    if !support::in_subprocess("released_retry_during_preparation_never_unlocks_the_worker") {
        return;
    }
    let repo = Repo::new();
    let pool = initialized(&repo);
    let lease = pool.reserve(Default::default()).unwrap();
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
    let (mut child, listener) = worker(&repo, &pool, "ready-before-preparation");
    let stream = child
        .checkpoint(
            &listener,
            "ready-before-preparation",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
    assert!(pool.ready_report().unwrap().running);
    assert!(pool.inspect_slot(&lease.slot_id).unwrap().lock_held);
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
}

#[cfg(feature = "test-hooks")]
#[test]
fn destruction_refuses_a_live_preparer_then_preserves_its_permanent_lock() {
    if !support::in_subprocess(
        "destruction_refuses_a_live_preparer_then_preserves_its_permanent_lock",
    ) {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let repo = Repo::new();
    let pool = initialized(&repo);
    let lock = repo.state().join("locks/ready.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    let (mut child, listener) = worker(&repo, &pool, "ready-before-preparation");
    let stream = child
        .checkpoint(
            &listener,
            "ready-before-preparation",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    assert_eq!(
        Pool::destroy(&repo.path, Default::default())
            .unwrap_err()
            .code,
        groove_core::ErrorCode::ConfigConflict
    );
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    Pool::destroy(
        &repo.path,
        groove_core::DestroyOptions {
            discard_changes: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
}

#[cfg(feature = "test-hooks")]
#[test]
fn recovery_replenishes_after_target_increases_during_interrupted_eviction() {
    if !support::in_subprocess(
        "recovery_replenishes_after_target_increases_during_interrupted_eviction",
    ) {
        return;
    }
    let repo = Repo::new();
    let pool = initialized(&repo);
    let generation = pool.request_ready().unwrap().unwrap();
    pool.run_ready(generation).unwrap();
    let slot = repo.state().join("slots/wt-0001.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&slot).unwrap()).unwrap();
    record["idle_since_unix_ms"] = 1.into();
    fs::write(&slot, serde_json::to_vec(&record).unwrap()).unwrap();
    pool.configure(groove_core::PoolUpdate {
        ready_target: Some(0),
        idle_timeout: Some("1s".parse().unwrap()),
        ..Default::default()
    })
    .unwrap();
    let (mut child, listener) = worker(&repo, &pool, "owned-registrations-removed");
    let stream = child
        .checkpoint(
            &listener,
            "owned-registrations-removed",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    pool.configure(groove_core::PoolUpdate {
        ready_target: Some(1),
        ..Default::default()
    })
    .unwrap();
    pool.request_ready().unwrap();
    pool.run_ready(generation).unwrap();
    assert_eq!(pool.ready_report().unwrap().ready, 1);
    assert_eq!(
        fs::read_to_string(repo.slot(1).join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert!(
        !repo.slot(2).exists(),
        "recovery should reuse the interrupted slot"
    );
}
