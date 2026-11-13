#![cfg(feature = "test-hooks")]

mod support;

use groove_core::{
    ErrorCode, ExecOptions, HistoryFilter, NamedCommand, Pool, PoolOptions, PreparationOptions,
    PreparationStatus, RunOutcome, Termination,
};
use std::{
    fs,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use support::{Repo, TestChild, PROCESS_TIMEOUT};

const PROJECT: &str = r#"
schema_version=1
[caches.deps]
paths=["ignored/deps"]
inputs=["tracked.txt"]
key="setup-cancellation"
[setup.dev]
cache="deps"
[[setup.dev.steps]]
name="dependencies"
command=["bash", "-c", "mkdir -p ignored/deps; printf artifact > ignored/deps/file; printf first > first-ran"]
[[setup.dev.steps]]
name="later"
command=["bash", "-c", "printf later > later-ran"]
[recipes.check]
command=["bash", "-c", "printf checked > recipe-ran"]
requires_setup="dev"
"#;

fn cancel_at(name: &str, checkpoint: &str, completed_steps: usize) {
    // Run preparation in a child whose checkpoint environment is local to its
    // Command. A separate bounded channel sets the PUBLIC cancellation flag
    // and acknowledges the store before the parent releases the checkpoint.
    // A stop acknowledgement alone can precede the controller's flag store.
    if let Some(path) = std::env::var_os("GROOVE_SETUP_CANCELLATION_FIXTURE") {
        let pool = Pool::open(path).unwrap();
        let lease = pool.protected_leases().unwrap().remove(0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let mut control =
            UnixStream::connect(std::env::var_os("GROOVE_SETUP_CANCELLATION_CONTROL").unwrap())
                .unwrap();
        control.set_read_timeout(Some(PROCESS_TIMEOUT)).unwrap();
        control.set_write_timeout(Some(PROCESS_TIMEOUT)).unwrap();
        let controller = std::thread::spawn(move || {
            let mut request = [0];
            control.read_exact(&mut request).unwrap();
            assert_eq!(request, [b'c']);
            flag.store(true, Ordering::Release);
            control.write_all(b"a").unwrap();
        });
        let result = pool
            .prepare(
                lease.lease_id,
                "dev",
                PreparationOptions {
                    timeout: PROCESS_TIMEOUT,
                    execution: ExecOptions {
                        cancellation: Some(cancelled.clone()),
                        grace: Duration::ZERO,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        controller.join().unwrap();
        assert!(cancelled.load(Ordering::Acquire));
        assert_eq!(result.status, PreparationStatus::Failed);
        assert_eq!(result.cancelled, Some(libc::SIGTERM));
        return;
    }
    if !support::in_subprocess(name) {
        return;
    }

    let repo = Repo::new();
    fs::write(repo.path.join(".groove.toml"), PROJECT).unwrap();
    repo.git(&["add", ".groove.toml"]);
    repo.git(&["commit", "-qm", "setup cancellation fixture"]);
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
    fs::write(lease.worktree_path.join("task-result"), "keep this result").unwrap();
    let registrations = repo.git(&["worktree", "list", "--porcelain", "-z"]);
    let checkpoint_path = repo.temp.path().join("checkpoint.sock");
    let listener = UnixListener::bind(&checkpoint_path).unwrap();
    let control_path = repo.temp.path().join("cancel.sock");
    let control_listener = UnixListener::bind(&control_path).unwrap();
    let mut child = TestChild::spawn(
        support::command(&repo.path, std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("GROOVE_SETUP_CANCELLATION_FIXTURE", &repo.path)
            .env("GROOVE_SETUP_CANCELLATION_CONTROL", control_path)
            .env("GROOVE_TEST_CHECKPOINT", checkpoint)
            .env("GROOVE_TEST_SOCKET", checkpoint_path),
    );
    let mut control = support::accept_child(
        &control_listener,
        std::slice::from_mut(&mut child),
        PROCESS_TIMEOUT,
    )
    .unwrap();
    let mut paused = child
        .checkpoint(&listener, checkpoint, PROCESS_TIMEOUT)
        .unwrap();
    assert_eq!(
        pool.preparation(lease.lease_id).unwrap().unwrap().status,
        PreparationStatus::Preparing
    );
    // At run-completed the worker has actually exited and its zero status is
    // durable; cancellation must still prevent setup readiness and later steps.
    let before = pool.history(&HistoryFilter::default()).unwrap();
    assert_eq!(before.len(), completed_steps);
    assert!(before.iter().all(|run| {
        run.outcome == RunOutcome::Completed
            && run.termination == Some(Termination::Exited)
            && run.exit_code == Some(0)
    }));
    control.write_all(b"c").unwrap();
    let mut acknowledged = [0];
    control.read_exact(&mut acknowledged).unwrap();
    assert_eq!(acknowledged, [b'a']);
    paused.write_all(b"x").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{checkpoint}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let failed = pool.preparation(lease.lease_id).unwrap().unwrap();
    assert_eq!(failed.status, PreparationStatus::Failed);
    assert_eq!(failed.cancelled, Some(libc::SIGTERM));
    assert!(failed.error.is_some());
    assert!(failed.completed_at_unix_ms.is_some());
    assert_eq!(failed.lease_id, lease.lease_id);
    assert_eq!(
        fs::read(lease.worktree_path.join("task-result")).unwrap(),
        b"keep this result"
    );
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
    assert_eq!(
        pool.reserve(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
    assert_eq!(
        pool.prepare(lease.lease_id, "dev", Default::default())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument,
        "cancellation must require an explicit setup retry"
    );
    assert_eq!(
        pool.exec_named(
            lease.lease_id,
            NamedCommand::Recipe("check"),
            &[],
            Default::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument,
        "observed cancellation must not satisfy requires_setup"
    );
    assert!(!lease.worktree_path.join("recipe-ran").exists());
    assert_eq!(
        repo.git(&["worktree", "list", "--porcelain", "-z"]),
        registrations
    );

    let history = pool.history(&HistoryFilter::default()).unwrap();
    assert_eq!(history.len(), completed_steps.max(1));
    assert_eq!(failed.steps.len(), history.len());
    if completed_steps == 0 {
        let execution = failed.steps[0].execution.as_ref().unwrap();
        assert_eq!(execution.termination, Termination::Cancelled);
        assert_eq!(execution.run.leader_pid, None);
        assert_eq!(execution.exit_code, None);
        assert_eq!(history[0].outcome, RunOutcome::Completed);
        assert_eq!(history[0].termination, Some(Termination::Cancelled));
        assert!(!lease.worktree_path.join("first-ran").exists());
        assert!(!lease.worktree_path.join("ignored/deps/file").exists());
    } else {
        assert!(failed
            .steps
            .iter()
            .all(|step| step.execution.as_ref().unwrap().succeeded()));
        assert_eq!(
            fs::read(lease.worktree_path.join("ignored/deps/file")).unwrap(),
            b"artifact"
        );
    }
    assert_eq!(
        lease.worktree_path.join("later-ran").exists(),
        completed_steps == 2
    );
}

#[test]
fn observed_cancellation_before_setup_step_never_launches_the_step_or_marks_ready() {
    cancel_at(
        "observed_cancellation_before_setup_step_never_launches_the_step_or_marks_ready",
        "before-setup-step",
        0,
    );
}

#[test]
fn observed_cancellation_before_cache_publication_never_marks_setup_ready() {
    cancel_at(
        "observed_cancellation_before_cache_publication_never_marks_setup_ready",
        "before-cache-publish",
        2,
    );
}

#[test]
fn observed_cancellation_after_successful_worker_exit_stops_later_setup_steps() {
    cancel_at(
        "observed_cancellation_after_successful_worker_exit_stops_later_setup_steps",
        "run-completed",
        1,
    );
}
