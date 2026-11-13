mod support;
use groove_core::{Pool, PoolOptions, ReadySettings};
use serde_json::Value;
use std::{
    fs,
    time::{Duration, Instant},
};
use support::{assert_success, Repo};

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
fn cli_defaults_prepare_and_replenish_without_a_public_worker_command() {
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
