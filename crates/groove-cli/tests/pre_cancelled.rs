mod support;

use groove_core::{ErrorCode, ExecOptions, Pool, PoolOptions, RunOutcome, Termination};
use std::{
    fs,
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};
use support::Repo;

#[test]
fn already_cancelled_execution_records_completion_without_launching_and_keeps_reservation() {
    if !support::in_subprocess(
        "already_cancelled_execution_records_completion_without_launching_and_keeps_reservation",
    ) {
        return;
    }
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    let commands = [
        // Deterministically distinguishes cancellation from even an attempted spawn.
        vec![repo.temp.path().join("nonexistent-worker").into_os_string()],
        vec![
            "bash".into(),
            "-c".into(),
            "printf launched > should-not-exist; printf output; printf error >&2".into(),
        ],
    ];
    for command in commands {
        let result = pool
            .exec(
                lease.lease_id,
                ExecOptions {
                    command,
                    cancellation: Some(Arc::new(AtomicBool::new(true))),
                    grace: Duration::ZERO,
                    ..Default::default()
                },
            )
            .expect("observed cancellation must not try spawning the requested program");
        assert_eq!(result.termination, Termination::Cancelled);
        assert!(!result.succeeded());
        assert_eq!(result.shell_status(), 143);
        assert_eq!(result.cancel_signal, Some(libc::SIGTERM));
        assert_eq!(result.exit_code, None);
        assert_eq!(result.signal, None, "there was no child to signal");
        assert_eq!(result.run.leader_pid, None);
        assert_eq!(result.lease_id, lease.lease_id);
        assert!(!lease.worktree_path.join("should-not-exist").exists());
        assert!(fs::read(&result.run.stdout_path).unwrap().is_empty());
        assert!(fs::read(&result.run.stderr_path).unwrap().is_empty());

        let history = pool
            .history_show(result.run.run_id.parse().unwrap())
            .unwrap();
        assert_eq!(history.outcome, RunOutcome::Completed);
        assert_eq!(history.termination, Some(Termination::Cancelled));
        assert_eq!(history.cancel_signal, result.cancel_signal);
        assert_eq!(history.exit_code, None);
        assert_eq!(history.signal, None);
        assert!(history.failure.is_none());
        assert!(history.ended_at_unix_ms.is_some());
        assert!(history.duration_ms.is_some());
        assert_eq!(history.lease_id, lease.lease_id);

        let output = support::groove_in(&repo.path)
            .args(["history", "show", &result.run.run_id])
            .env("GROOVE_LOG_ENABLED", "false")
            .output()
            .unwrap();
        let response = support::assert_success(&output);
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
        jsonschema::validator_for(&schema)
            .unwrap()
            .validate(&response)
            .unwrap();
        assert!(
            output.stderr.is_empty(),
            "history show produced unexpected stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(response["ok"], true);
        assert_eq!(response["result"]["run"]["outcome"], "completed");
        assert_eq!(response["result"]["run"]["termination"], "cancelled");

        assert!(!repo
            .state()
            .join(format!("slots/{}.execution.json", lease.slot_id))
            .exists());
        let protected = pool.protected_leases().unwrap();
        assert_eq!(protected.len(), 1);
        assert_eq!(protected[0].lease_id, lease.lease_id);
        assert_eq!(
            pool.reserve(Default::default()).unwrap_err().code,
            ErrorCode::PoolExhausted
        );
    }

    // Cancellation cleared execution protection, so a subsequent command needs no recovery.
    let resumed = pool
        .exec(
            lease.lease_id,
            ExecOptions {
                command: vec!["bash".into(), "-c".into(), "printf resumed".into()],
                grace: Duration::ZERO,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(resumed.succeeded());
    assert!(resumed.run.leader_pid.is_some());
    assert_eq!(fs::read(resumed.run.stdout_path).unwrap(), b"resumed");
    pool.release_lease(lease.lease_id).unwrap();
}

#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_cancelled_completion_retains_protection_and_recovery_preserves_history() {
    use groove_core::HistoryFilter;
    use std::{os::unix::net::UnixListener, process::Command};

    const NAME: &str =
        "interrupted_cancelled_completion_retains_protection_and_recovery_preserves_history";
    if let Some(path) = std::env::var_os("GROOVE_PRE_CANCELLED_FIXTURE") {
        let pool = Pool::open(path).unwrap();
        let lease = pool.protected_leases().unwrap().remove(0);
        pool.exec(
            lease.lease_id,
            ExecOptions {
                command: vec!["must-not-launch".into()],
                cancellation: Some(Arc::new(AtomicBool::new(true))),
                ..Default::default()
            },
        )
        .unwrap();
        return;
    }
    if !support::in_subprocess(NAME) {
        return;
    }
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    let socket = repo.temp.path().join("completion.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = support::TestChild::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--nocapture"])
            .env("GROOVE_PRE_CANCELLED_FIXTURE", &repo.path)
            .env("GROOVE_TEST_CHECKPOINT", "run-completed")
            .env("GROOVE_TEST_SOCKET", socket),
    );
    let stream = child
        .checkpoint(&listener, "run-completed", support::PROCESS_TIMEOUT)
        .unwrap();
    let execution_path = repo
        .state()
        .join(format!("slots/{}.execution.json", lease.slot_id));
    let execution: groove_core::ExecutionInfo =
        serde_json::from_slice(&fs::read(&execution_path).unwrap()).unwrap();
    assert_eq!(execution.leader_pid, None);
    let runs = pool.history(&HistoryFilter::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::Completed);
    assert_eq!(runs[0].termination, Some(Termination::Cancelled));
    assert_eq!(runs[0].cancel_signal, Some(libc::SIGTERM));
    assert!(fs::read(&runs[0].stdout_path).unwrap().is_empty());
    assert!(fs::read(&runs[0].stderr_path).unwrap().is_empty());
    assert_eq!(
        pool.release_lease(lease.lease_id).unwrap_err().code,
        ErrorCode::InvalidLease
    );
    assert_eq!(
        pool.recover_execution(lease.lease_id).unwrap_err().code,
        ErrorCode::InvalidLease
    );

    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    drop(stream);
    assert!(execution_path.exists());
    pool.recover_execution(lease.lease_id).unwrap();
    let recovered = pool.history_show(runs[0].run_id).unwrap();
    assert_eq!(recovered.outcome, RunOutcome::Completed);
    assert_eq!(recovered.termination, Some(Termination::Cancelled));
    assert_eq!(recovered.cancel_signal, Some(libc::SIGTERM));
    assert_eq!(recovered.exit_code, None);
    assert_eq!(recovered.signal, None);
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
    assert!(!execution_path.exists());
    pool.release_lease(lease.lease_id).unwrap();
}
