mod support;
use groove_core::{CleanupPolicy, ErrorCode, Pool, PoolOptions, PoolUpdate};
use std::{fs, os::unix::fs::MetadataExt};
use support::Repo;

#[test]
fn growth_preserves_active_writers_and_old_handles_observe_new_slots() {
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let old = Pool::open(&repo.path).unwrap();
    let first = old.acquire(Default::default()).unwrap();
    let first_path = first.info().worktree_path.clone();
    fs::write(first_path.join("tracked.txt"), "live writer\n").unwrap();
    let inode = fs::metadata(repo.state().join("locks/wt-0001.lock"))
        .unwrap()
        .ino();
    let result = old
        .configure(PoolUpdate {
            capacity: Some(3),
            cleanup_policy: Some(CleanupPolicy::Warm),
            ..Default::default()
        })
        .unwrap();
    assert!(result.updated);
    assert_eq!(old.settings().unwrap().capacity, 3);
    assert_eq!(
        fs::metadata(repo.state().join("locks/wt-0001.lock"))
            .unwrap()
            .ino(),
        inode
    );
    assert_eq!(
        fs::read(first_path.join("tracked.txt")).unwrap(),
        b"live writer\n"
    );
    let second = old.acquire(Default::default()).unwrap();
    let third = old.acquire(Default::default()).unwrap();
    assert_ne!(second.info().slot_id, first.info().slot_id);
    assert_ne!(second.info().slot_id, third.info().slot_id);
    assert_eq!(second.info().cleanup_policy, CleanupPolicy::Warm);
    assert_eq!(old.inspect().unwrap().slots.len(), 3);
    assert_eq!(
        old.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    let info = second.into_protected().unwrap();
    old.release_protected(&info.slot_id, info.lease_id).unwrap();
    drop(third);
    drop(first);
    assert!(
        !old.configure(PoolUpdate {
            capacity: Some(3),
            ..Default::default()
        })
        .unwrap()
        .updated
    );
}

#[test]
fn invalid_combined_updates_do_not_change_manifest_or_locks() {
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    let before = fs::read(repo.state().join("config.json")).unwrap();
    let error = pool
        .configure(PoolUpdate {
            capacity: Some(1),
            cleanup_policy: Some(CleanupPolicy::Warm),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert_eq!(fs::read(repo.state().join("config.json")).unwrap(), before);
    assert_eq!(
        pool.settings().unwrap().cleanup_policy,
        CleanupPolicy::Pristine
    );
    assert_eq!(
        pool.configure(PoolUpdate {
            capacity: Some(0),
            ..Default::default()
        })
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn changed_cleanup_applies_only_on_the_next_acquisition() {
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            cleanup_policy: CleanupPolicy::Warm,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "cache").unwrap();
    pool.configure(PoolUpdate {
        cleanup_policy: Some(CleanupPolicy::Pristine),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(lease.info().cleanup_policy, CleanupPolicy::Warm);
    assert!(path.join("ignored/cache").exists());
    drop(lease);
    let lease = pool.acquire(Default::default()).unwrap();
    assert_eq!(lease.info().cleanup_policy, CleanupPolicy::Pristine);
    assert!(!path.join("ignored/cache").exists());
}

#[test]
fn settings_owner_helper() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let Ok(repo) = std::env::var("GROOVE_SETTINGS_REPO") else {
        return;
    };
    let mut socket =
        UnixStream::connect(std::env::var_os("GROOVE_SETTINGS_SOCKET").unwrap()).unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let old = Pool::open(repo).unwrap();
    let first = old.acquire(Default::default()).unwrap();
    fs::write(
        first.info().worktree_path.join("tracked.txt"),
        "child is working\n",
    )
    .unwrap();
    socket.write_all(b"owned").unwrap();
    let mut signal = [0];
    socket.read_exact(&mut signal).unwrap();
    let _second = old.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read(first.info().worktree_path.join("tracked.txt")).unwrap(),
        b"child is working\n"
    );
    socket.write_all(b"grown").unwrap();
    socket.read_exact(&mut signal).unwrap();
}

#[test]
fn live_process_keeps_ownership_and_refreshes_after_growth() {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let socket_path = repo.temp.path().join("settings.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "settings_owner_helper", "--nocapture"])
        .env("GROOVE_SETTINGS_REPO", &repo.path)
        .env("GROOVE_SETTINGS_SOCKET", &socket_path)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut child = support::TestChild::spawn(&mut command);
    let mut socket = support::accept_child(
        &listener,
        std::slice::from_mut(&mut child),
        support::PROCESS_TIMEOUT,
    )
    .unwrap();
    socket
        .set_read_timeout(Some(std::time::Duration::from_secs(20)))
        .unwrap();
    let mut message = [0; 5];
    socket.read_exact(&mut message).unwrap();
    assert_eq!(&message, b"owned");
    let pool = Pool::open(&repo.path).unwrap();
    pool.configure(PoolUpdate {
        capacity: Some(2),
        ..Default::default()
    })
    .unwrap();
    socket.write_all(b"g").unwrap();
    socket.read_exact(&mut message).unwrap();
    assert_eq!(&message, b"grown");
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    socket.write_all(b"x").unwrap();
    assert!(child.wait().unwrap().success());
}
