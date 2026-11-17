mod support;
use groove_core::{ErrorCode, Pool, PoolOptions, ProcessUse};
use std::{
    fs,
    os::unix::process::CommandExt,
    time::{Duration, Instant},
};
use support::{Repo, TestChild};

fn refuses_workspace_user(usage: ProcessUse) {
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
    let ready = repo.temp.path().join("ready");
    let mut command = support::command(repo.temp.path(), "bash");
    command.process_group(0);
    match usage {
        ProcessUse::WorkingDirectory => {
            command
                .current_dir(&lease.worktree_path)
                .args(["-c", "printf ready > \"$1\"; exec sleep 30", "fixture"])
                .arg(&ready);
        }
        ProcessUse::Executable => {
            let program = lease.worktree_path.join("workspace-sleep");
            // Use our Cargo-built executable. Copied Apple system binaries can
            // lose their execution privileges and be killed before exec completes.
            fs::copy(std::env::current_exe().unwrap(), &program).unwrap();
            command = support::command(repo.temp.path(), program);
            command
                .process_group(0)
                .args(["--exact", "workspace_executable_helper", "--nocapture"])
                .env("GROOVE_EXECUTABLE_HELPER_READY", &ready);
        }
        ProcessUse::OpenFile => {
            command
                .args([
                    "-c",
                    "exec 3<> \"$1\"; printf ready > \"$2\"; exec sleep 30",
                    "fixture",
                ])
                .arg(lease.worktree_path.join("tracked.txt"))
                .arg(&ready);
        }
        _ => panic!("add a fixture for a new process-use variant"),
    }
    let mut child = TestChild::spawn(&mut command);
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    loop {
        let processes = pool.workspace_processes(lease.lease_id).unwrap();
        if ready.exists()
            && processes
                .iter()
                .any(|p| p.pid == child.id() && p.uses == usage)
        {
            break;
        }
        if child.try_wait().unwrap().is_some() {
            let output = child.wait_with_output().unwrap();
            panic!(
                "workspace user exited before {usage:?} readiness: status={} stdout={} stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
        }
        assert!(
            Instant::now() < deadline,
            "{usage:?} process not found: {processes:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let error = pool
        .ensure_quiescent(lease.lease_id, false, Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::WorkspaceInUse);
    assert!(error
        .details
        .processes
        .iter()
        .any(|process| process.pid == child.id() && process.uses == usage));
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
    assert_eq!(
        fs::read(lease.worktree_path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(pool.workspace_processes(lease.lease_id).unwrap().is_empty());
    pool.ensure_quiescent(lease.lease_id, false, Duration::from_secs(5))
        .unwrap();
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
}

#[test]
fn workspace_executable_helper() {
    let Some(ready) = std::env::var_os("GROOVE_EXECUTABLE_HELPER_READY") else {
        return;
    };
    fs::write(ready, "ready").unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[test]
fn working_directory_user_blocks_handoff_until_stopped() {
    if !support::in_subprocess("working_directory_user_blocks_handoff_until_stopped") {
        return;
    }
    refuses_workspace_user(ProcessUse::WorkingDirectory);
}

#[test]
fn workspace_executable_blocks_handoff_until_stopped() {
    // Another fixture's fork can retain the executable copy's write descriptor
    // until exec, making this otherwise-ready program fail with ETXTBSY.
    if !support::in_subprocess("workspace_executable_blocks_handoff_until_stopped") {
        return;
    }
    refuses_workspace_user(ProcessUse::Executable);
}

#[test]
fn open_workspace_file_blocks_handoff_until_stopped() {
    if !support::in_subprocess("open_workspace_file_blocks_handoff_until_stopped") {
        return;
    }
    refuses_workspace_user(ProcessUse::OpenFile);
}

#[test]
fn read_only_observers_outside_the_workspace_do_not_block_handoff_or_deletion() {
    if !support::in_subprocess(
        "read_only_observers_outside_the_workspace_do_not_block_handoff_or_deletion",
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
    let ready = repo.temp.path().join("reader-ready");
    let mut command = support::command(repo.temp.path(), "bash");
    command
        .process_group(0)
        .args([
            "-c",
            "exec 3< \"$1\"; printf ready > \"$2\"; exec sleep 30",
            "fixture",
        ])
        .arg(lease.worktree_path.join("tracked.txt"))
        .arg(&ready);
    let mut child = TestChild::spawn(&mut command);
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    while !ready.exists() {
        assert!(child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    pool.ensure_quiescent(lease.lease_id, true, Duration::from_secs(5))
        .unwrap();
    assert_eq!(
        fs::read(lease.worktree_path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
    Pool::destroy(&repo.path, Default::default()).unwrap();
    assert!(!lease.worktree_path.exists());
    child.kill().unwrap();
    child.wait().unwrap();
}
