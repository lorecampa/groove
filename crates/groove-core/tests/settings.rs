mod support;
use groove_core::{
    snapshot::{SaveOptions, Snapshots},
    CleanupPolicy, DestroyOptions, ErrorCode, Pool, PoolName, PoolOptions, PoolUpdate,
    ReservationOptions,
};
use std::{fs, os::unix::fs::MetadataExt};
use support::Repo;

#[test]
fn growth_reuses_retired_slots_without_losing_results_or_releasing_successors() {
    if !support::in_subprocess(
        "growth_reuses_retired_slots_without_losing_results_or_releasing_successors",
    ) {
        return;
    }
    for name in [PoolName::default(), "frontend".parse().unwrap()] {
        let repo = Repo::new();
        let initialized = Pool::init_named(
            &repo.path,
            &name,
            PoolOptions {
                capacity: 2,
                worktree_root: Some(support::worktree_root(&repo.path, name.as_str())),
                ..Default::default()
            },
        )
        .unwrap();
        let old = Pool::open_named(&repo.path, &name).unwrap();
        let leases: Vec<_> = (0..2)
            .map(|i| {
                old.reserve(ReservationOptions {
                    task: Some(format!("old-{i}")),
                    request_key: Some(format!("old-key-{i}")),
                    ..Default::default()
                })
                .unwrap()
            })
            .collect();
        let retired = leases.iter().find(|l| l.slot_id.number() == 2).unwrap();
        fs::write(
            retired.worktree_path.join("tracked.txt"),
            b"retained result",
        )
        .unwrap();
        let finished = old
            .finish(retired.lease_id, SaveOptions::default())
            .unwrap();
        for lease in &leases {
            if lease.lease_id != retired.lease_id {
                old.release_lease(lease.lease_id).unwrap();
            }
        }
        let locks: Vec<_> = fs::read_dir(initialized.state_dir.join("locks"))
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let metadata = fs::metadata(&path).unwrap();
                (path, metadata.dev(), metadata.ino())
            })
            .collect();
        Pool::destroy_named(
            &repo.path,
            &name,
            DestroyOptions {
                discard_changes: true,
                ..Default::default()
            },
        )
        .unwrap();
        // Cover both default-root reuse and a named pool switching to an external root.
        let replacement_root = Some(if name == PoolName::default() {
            initialized.worktree_root.clone()
        } else {
            repo.temp.path().join("replacement-workspaces")
        });
        Pool::init_named(
            &repo.path,
            &name,
            PoolOptions {
                capacity: 1,
                worktree_root: replacement_root,
                ..Default::default()
            },
        )
        .unwrap();
        let current = Pool::open_named(&repo.path, &name).unwrap();
        let active = current
            .reserve(ReservationOptions {
                task: Some("current".into()),
                ..Default::default()
            })
            .unwrap();
        fs::write(active.worktree_path.join("tracked.txt"), b"current result").unwrap();
        current
            .configure(PoolUpdate {
                capacity: Some(3),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(current.settings().unwrap().capacity, 3);
        assert_eq!(
            current.protected_leases().unwrap()[0].lease_id,
            active.lease_id
        );
        assert_eq!(
            fs::read(active.worktree_path.join("tracked.txt")).unwrap(),
            b"current result"
        );
        let fresh: Vec<_> = (0..2)
            .map(|_| current.reserve(Default::default()).unwrap())
            .collect();
        assert!(fresh.iter().any(|l| l.slot_id.number() == 2));
        assert!(fresh.iter().any(|l| l.slot_id.number() == 3));
        for lease in &fresh {
            assert_ne!(lease.lease_id, retired.lease_id);
            assert_eq!(
                fs::read(lease.worktree_path.join("tracked.txt")).unwrap(),
                b"baseline\n"
            );
        }
        let replay = current
            .finish(retired.lease_id, SaveOptions::default())
            .unwrap();
        assert_eq!(replay.snapshot.id, finished.snapshot.id);
        assert_eq!(current.protected_leases().unwrap().len(), 3);
        assert_eq!(
            current
                .reserve(ReservationOptions {
                    task: retired.task.clone(),
                    request_key: retired.request_key.clone(),
                    ..Default::default()
                })
                .unwrap_err()
                .code,
            ErrorCode::InvalidLease
        );
        let recovery = repo.temp.path().join("recovered-result");
        let snapshots = Snapshots::open(&repo.path, groove_core::DEFAULT_GIT_TIMEOUT).unwrap();
        assert!(snapshots.show(&finished.snapshot.id).unwrap().pinned);
        snapshots
            .restore(&finished.snapshot.id, Some(&recovery))
            .unwrap();
        assert_eq!(
            fs::read(recovery.join("tracked.txt")).unwrap(),
            b"retained result"
        );
        for (path, dev, ino) in locks {
            let metadata = fs::metadata(path).unwrap();
            assert_eq!((metadata.dev(), metadata.ino()), (dev, ino));
        }
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
    }
}

#[test]
fn growth_uses_bounded_descriptors_and_preserves_a_live_lease() {
    if !support::in_subprocess("growth_uses_bounded_descriptors_and_preserves_a_live_lease") {
        return;
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit is initialized, writable storage of the syscall's exact type.
    let read = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    assert_eq!(read, 0);
    assert!(
        limit.rlim_max >= 64,
        "fixture requires 64 allowed descriptors"
    );
    limit.rlim_cur = 64;
    // SAFETY: only this isolated test process's soft descriptor limit changes.
    let set = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
    assert_eq!(set, 0);
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
    let mut owner = pool.acquire(Default::default()).unwrap();
    fs::write(
        owner.info().worktree_path.join("tracked.txt"),
        b"live result",
    )
    .unwrap();
    let path = repo.state().join("locks/wt-0001.lock");
    let inode = fs::metadata(&path).unwrap().ino();
    pool.configure(PoolUpdate {
        capacity: Some(128),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(pool.settings().unwrap().capacity, 128);
    assert_eq!(pool.inspect().unwrap().slots.len(), 128);
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
    assert_eq!(
        fs::read(owner.info().worktree_path.join("tracked.txt")).unwrap(),
        b"live result"
    );
    let mut fresh = pool.acquire(Default::default()).unwrap();
    assert_ne!(fresh.info().slot_id, owner.info().slot_id);
    assert_eq!(
        fs::read(fresh.info().worktree_path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    fresh.release().unwrap();
    owner.release().unwrap();
}

#[test]
fn growth_preserves_active_writers_and_old_handles_observe_new_slots() {
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
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
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
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
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
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
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

fn smaller_replacement(repo: &Repo) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::destroy(&repo.path, DestroyOptions::default()).unwrap();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}

#[test]
fn growth_rejects_protected_corrupt_or_foreign_retired_infrastructure() {
    if !support::in_subprocess("growth_rejects_protected_corrupt_or_foreign_retired_infrastructure")
    {
        return;
    }
    for case in [
        "unpublished",
        "protection",
        "execution",
        "record",
        "slot-mismatch",
        "foreign-lease-path",
        "checkout",
        "missing-lock",
    ] {
        let repo = Repo::new();
        let pool = smaller_replacement(&repo);
        let active = pool.reserve(Default::default()).unwrap();
        fs::write(
            active.worktree_path.join("tracked.txt"),
            b"protected result",
        )
        .unwrap();
        let config = fs::read(repo.state().join("config.json")).unwrap();
        let record = repo.state().join("slots/wt-0002.json");
        // Deliberate corruption arranges rejected updates; no recovery removes protection.
        let (evidence, expected) = match case {
            "unpublished" => {
                let path = repo.state().join("slots/wt-0003.json");
                fs::write(&path, br#"{"schema_version":1,"phase":"quarantined","lease":null,"failure":"unexpected"}"#).unwrap();
                (path, ErrorCode::ConfigConflict)
            }
            "protection" | "execution" => {
                let suffix = if case == "protection" {
                    "protected"
                } else {
                    "execution"
                };
                let path = repo.state().join(format!("slots/wt-0002.{suffix}.json"));
                fs::write(&path, b"malformed protection is still protection").unwrap();
                (path, ErrorCode::ConfigConflict)
            }
            "record" => {
                fs::write(&record, b"not valid metadata").unwrap();
                (record.clone(), ErrorCode::ProtocolError)
            }
            "slot-mismatch" | "foreign-lease-path" => {
                let mut value: serde_json::Value =
                    serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
                value["phase"] = "released".into();
                value["lease"] = serde_json::to_value(&active).unwrap();
                if case == "foreign-lease-path" {
                    value["lease"]["slot_id"] = "wt-0002".into();
                    value["lease"]["worktree_path"] =
                        repo.temp.path().join("foreign").to_str().unwrap().into();
                }
                fs::write(&record, serde_json::to_vec(&value).unwrap()).unwrap();
                (record.clone(), ErrorCode::ConfigConflict)
            }
            "checkout" => {
                fs::create_dir(repo.slot(2)).unwrap();
                let path = repo.slot(2).join("keep");
                fs::write(&path, b"unrecognized result").unwrap();
                (path, ErrorCode::ConfigConflict)
            }
            "missing-lock" => {
                fs::remove_file(repo.state().join("locks/wt-0002.lock")).unwrap();
                (record.clone(), ErrorCode::IoError)
            }
            _ => unreachable!(),
        };
        let evidence_bytes = fs::read(&evidence).unwrap();
        let record_bytes = fs::read(&record).unwrap();
        let error = pool
            .configure(PoolUpdate {
                capacity: Some(3),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(error.code, expected, "{case}: {error}");
        assert_eq!(fs::read(&evidence).unwrap(), evidence_bytes, "{case}");
        assert_eq!(fs::read(&record).unwrap(), record_bytes, "{case}");
        assert_eq!(
            fs::read(repo.state().join("config.json")).unwrap(),
            config,
            "{case}"
        );
        assert_eq!(pool.settings().unwrap().capacity, 1);
        assert_eq!(
            pool.lease_info(active.lease_id).unwrap().lease_id,
            active.lease_id
        );
        assert_eq!(
            fs::read(active.worktree_path.join("tracked.txt")).unwrap(),
            b"protected result"
        );
        assert_eq!(
            pool.acquire(Default::default()).unwrap_err().code,
            ErrorCode::PoolExhausted
        );
        if case == "missing-lock" {
            assert!(!repo.state().join("locks/wt-0002.lock").exists());
        }
    }
}

#[test]
fn growth_lock_owner_helper() {
    use std::{
        io::{Read, Write},
        os::{fd::AsRawFd, unix::net::UnixStream},
    };
    let Some(path) = std::env::var_os("GROOVE_GROWTH_LOCK") else {
        return;
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    // SAFETY: file owns the fixture's permanent lock descriptor throughout the handshake.
    let locked = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(locked, 0);
    let mut socket =
        UnixStream::connect(std::env::var_os("GROOVE_GROWTH_SOCKET").unwrap()).unwrap();
    socket
        .set_read_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    socket.write_all(b"owned").unwrap();
    socket.read_exact(&mut [0]).unwrap();
    drop(file);
}

#[test]
fn growth_refuses_retired_slot_and_execution_owners_without_changing_state() {
    if !support::in_subprocess(
        "growth_refuses_retired_slot_and_execution_owners_without_changing_state",
    ) {
        return;
    }
    use std::{
        io::{Read, Write},
        os::unix::net::UnixListener,
        process::Command,
    };
    for suffix in ["lock", "execution.lock"] {
        let repo = Repo::new();
        let pool = smaller_replacement(&repo);
        let active = pool.reserve(Default::default()).unwrap();
        fs::write(active.worktree_path.join("tracked.txt"), b"active result").unwrap();
        let socket_path = repo.temp.path().join("growth.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let lock = repo.state().join(format!("locks/wt-0002.{suffix}"));
        let inode = fs::metadata(&lock).unwrap().ino();
        let config = fs::read(repo.state().join("config.json")).unwrap();
        let record = fs::read(repo.state().join("slots/wt-0002.json")).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "growth_lock_owner_helper", "--nocapture"])
            .env("GROOVE_GROWTH_LOCK", &lock)
            .env("GROOVE_GROWTH_SOCKET", &socket_path);
        let mut child = support::TestChild::spawn(&mut command);
        let mut socket = support::accept_child(
            &listener,
            std::slice::from_mut(&mut child),
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
        socket
            .set_read_timeout(Some(support::PROCESS_TIMEOUT))
            .unwrap();
        let mut ready = [0; 5];
        socket.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"owned");
        assert_eq!(
            pool.configure(PoolUpdate {
                capacity: Some(2),
                ..Default::default()
            })
            .unwrap_err()
            .code,
            ErrorCode::ConfigConflict
        );
        assert_eq!(fs::read(repo.state().join("config.json")).unwrap(), config);
        assert_eq!(
            fs::read(repo.state().join("slots/wt-0002.json")).unwrap(),
            record
        );
        assert_eq!(fs::metadata(&lock).unwrap().ino(), inode);
        assert_eq!(
            pool.lease_info(active.lease_id).unwrap().lease_id,
            active.lease_id
        );
        assert_eq!(
            fs::read(active.worktree_path.join("tracked.txt")).unwrap(),
            b"active result"
        );
        socket.write_all(b"x").unwrap();
        assert!(child.wait().unwrap().success());
        pool.configure(PoolUpdate {
            capacity: Some(2),
            ..Default::default()
        })
        .unwrap();
        let fresh = pool.reserve(Default::default()).unwrap();
        assert_eq!(fresh.slot_id.number(), 2);
        assert_eq!(
            fs::read(fresh.worktree_path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
    }
}
