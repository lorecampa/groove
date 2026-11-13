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
        let root = r.state().join("worktrees");
        let (mut command, listener) = checkpoint_command(
            &r,
            name,
            &[
                "init",
                "--capacity",
                "1",
                "--worktree-root",
                root.to_str().unwrap(),
            ],
        );
        kill_at_checkpoint(&mut command, listener, name);
        let lock = r.state().join("locks/wt-0001.lock");
        let old = std::fs::metadata(&lock).ok().map(|m| m.ino());
        Pool::init(
            &r.path,
            PoolOptions {
                resume: r.state().join("initializing.json").exists(),
                capacity: 1,
                worktree_root: Some(support::worktree_root(&r.path, "default")),
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
fn deletion_crashes_resume_with_the_same_policy_and_preserve_results_and_inodes() {
    if !support::in_subprocess(
        "deletion_crashes_resume_with_the_same_policy_and_preserve_results_and_inodes",
    ) {
        return;
    }
    use groove_core::{
        snapshot::{SaveOptions, Snapshots},
        DestroyOptions, ErrorCode,
    };
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    for name in [
        "pool-deleting",
        "pool-slot-removed",
        "pool-artifact-removed",
        "pool-deleted",
    ] {
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
        let a = pool.acquire(Default::default()).unwrap();
        let b = pool.acquire(Default::default()).unwrap();
        drop(a);
        drop(b);
        let cache = repo.state().join("artifacts");
        fs::create_dir(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
        let data = cache.join("b".repeat(64));
        fs::create_dir(&data).unwrap();
        fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(data.join("bytes"), "cache").unwrap();
        let snapshots = Snapshots::open(&repo.path, Duration::from_secs(10)).unwrap();
        let snapshot = snapshots.save(SaveOptions::default()).unwrap();
        let inodes: Vec<_> = fs::read_dir(repo.state().join("locks"))
            .unwrap()
            .map(|e| {
                let path = e.unwrap().path();
                let ino = fs::metadata(&path).unwrap().ino();
                (path, ino)
            })
            .collect();
        let (mut command, listener) =
            checkpoint_command(&repo, name, &["destroy", "--discard-changes"]);
        kill_at_checkpoint(&mut command, listener, name);
        assert_eq!(
            Pool::open(&repo.path).unwrap_err().code,
            if name == "pool-deleted" {
                ErrorCode::NotInitialized
            } else {
                ErrorCode::ConfigConflict
            }
        );
        if name != "pool-deleted" {
            assert_eq!(
                Pool::init(
                    &repo.path,
                    PoolOptions {
                        capacity: 2,
                        worktree_root: Some(support::worktree_root(&repo.path, "default")),
                        ..Default::default()
                    }
                )
                .unwrap_err()
                .code,
                ErrorCode::ConfigConflict
            );
            assert_eq!(
                Pool::destroy(&repo.path, Default::default())
                    .unwrap_err()
                    .code,
                ErrorCode::ConfigConflict
            );
        }
        let before = fs::read(repo.state().join("lifecycle.json")).unwrap();
        let preview = Pool::destroy(
            &repo.path,
            DestroyOptions {
                dry_run: true,
                discard_changes: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            fs::read(repo.state().join("lifecycle.json")).unwrap(),
            before
        );
        let result = Pool::destroy(
            &repo.path,
            DestroyOptions {
                discard_changes: true,
                pool_id: preview.pool_id,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.deleted);
        assert!(!repo.slot(1).exists() && !repo.slot(2).exists() && !data.exists());
        for (path, ino) in inodes {
            assert_eq!(fs::metadata(path).unwrap().ino(), ino);
        }
        assert_eq!(snapshots.show(&snapshot.id).unwrap().id, snapshot.id);
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(support::worktree_root(&repo.path, "default")),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(Pool::open(&repo.path)
            .unwrap()
            .acquire(Default::default())
            .is_ok());
    }
}

#[test]
fn deletion_excludes_new_operations_and_covers_finish_metadata_after_unlock() {
    if !support::in_subprocess(
        "deletion_excludes_new_operations_and_covers_finish_metadata_after_unlock",
    ) {
        return;
    }
    use groove_core::{DestroyOptions, ErrorCode, PoolUpdate};
    use std::io::Write;
    for point in [
        "before-normalization",
        "before-pool-update",
        "finish-released",
    ] {
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
        drop(pool.acquire(Default::default()).unwrap());
        let id = if point == "finish-released" {
            Some(
                pool.reserve(Default::default())
                    .unwrap()
                    .lease_id
                    .to_string(),
            )
        } else {
            None
        };
        let args = match point {
            "before-normalization" => vec!["acquire"],
            "before-pool-update" => vec!["config", "set", "--capacity", "2"],
            _ => vec!["release", "--save", "--lease-id", id.as_deref().unwrap()],
        };
        let (mut command, listener) = checkpoint_command(&repo, point, &args);
        let mut child = support::TestChild::spawn(&mut command);
        let mut stream = child
            .checkpoint(&listener, point, support::PROCESS_TIMEOUT)
            .unwrap();
        let rejected = Pool::destroy(
            &repo.path,
            DestroyOptions {
                discard_changes: true,
                ..Default::default()
            },
        );
        stream.write_all(b"g").unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(rejected.unwrap_err().code, ErrorCode::ConfigConflict);
        // Reservation creation may have succeeded in the acquire case.
        for lease in pool.protected_leases().unwrap() {
            pool.release_lease(lease.lease_id).unwrap();
        }
        let (mut command, listener) =
            checkpoint_command_after_removing_socket(&repo, "pool-deleting", &["destroy"]);
        let mut child = support::TestChild::spawn(&mut command);
        let mut stream = child
            .checkpoint(&listener, "pool-deleting", support::PROCESS_TIMEOUT)
            .unwrap();
        let acquire = pool.acquire(Default::default());
        let configure = pool.configure(PoolUpdate {
            capacity: Some(3),
            ..Default::default()
        });
        stream.write_all(b"g").unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(acquire.unwrap_err().code, ErrorCode::ConfigConflict);
        assert_eq!(configure.unwrap_err().code, ErrorCode::ConfigConflict);
        assert!(!repo.slot(1).exists());
    }
}

fn checkpoint_command_after_removing_socket(
    repo: &Repo,
    name: &str,
    args: &[&str],
) -> (Command, UnixListener) {
    std::fs::remove_file(repo.temp.path().join("checkpoint.sock")).unwrap();
    checkpoint_command(repo, name, args)
}

#[test]
fn interrupted_initialization_can_be_cancelled_and_reinitialized() {
    if !support::in_subprocess("interrupted_initialization_can_be_cancelled_and_reinitialized") {
        return;
    }
    for name in ["metadata-renamed", "before-ready-manifest"] {
        let repo = Repo::new();
        let (mut command, listener) = checkpoint_command(&repo, name, &["init", "--capacity", "2"]);
        kill_at_checkpoint(&mut command, listener, name);
        let result = Pool::destroy(&repo.path, Default::default()).unwrap();
        assert!(result.deleted);
        assert!(!repo.state().join("initializing.json").exists());
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(support::worktree_root(&repo.path, "default")),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(Pool::open(&repo.path)
            .unwrap()
            .reserve(Default::default())
            .is_ok());
    }
}

#[test]
fn deletion_retries_after_unregistering_and_after_a_reported_io_failure() {
    if !support::in_subprocess(
        "deletion_retries_after_unregistering_and_after_a_reported_io_failure",
    ) {
        return;
    }
    use std::{fs, io::Write};
    for point in ["owned-registrations-removed", "pool-slot-removed"] {
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
        let p = Pool::open(&repo.path).unwrap();
        drop(p.acquire(Default::default()).unwrap());
        let (mut command, listener) = checkpoint_command(&repo, point, &["destroy"]);
        let mut child = support::TestChild::spawn(&mut command);
        let mut stream = child
            .checkpoint(&listener, point, support::PROCESS_TIMEOUT)
            .unwrap();
        if point == "owned-registrations-removed" {
            assert!(repo.slot(1).exists());
            child.kill().unwrap();
            child.wait().unwrap();
        } else {
            // Inject a durable publication failure after workspace removal.
            fs::write(repo.state().join("retired"), "obstruction").unwrap();
            stream.write_all(b"g").unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(!output.status.success());
            let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["error"]["code"], "UNSUPPORTED_CONFIGURATION");
            fs::remove_file(repo.state().join("retired")).unwrap();
        }
        assert_eq!(
            Pool::open(&repo.path).unwrap_err().code,
            groove_core::ErrorCode::ConfigConflict
        );
        let result = Pool::destroy(&repo.path, Default::default()).unwrap();
        assert!(result.deleted && result.resumed);
        assert!(!repo.slot(1).exists());
    }
}

#[test]
fn reinit_intent_crashes_can_be_cancelled_without_overwriting_retired_identity() {
    if !support::in_subprocess(
        "reinit_intent_crashes_can_be_cancelled_without_overwriting_retired_identity",
    ) {
        return;
    }
    use groove_core::{DestroyOptions, ErrorCode};
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
    let old = Pool::destroy(&repo.path, Default::default()).unwrap();
    let archive = repo
        .state()
        .join("retired")
        .join(format!("{}.json", old.pool_id.unwrap()));
    let original = std::fs::read(&archive).unwrap();
    let (mut command, listener) =
        checkpoint_command(&repo, "metadata-renamed", &["init", "--capacity", "1"]);
    kill_at_checkpoint(&mut command, listener, "metadata-renamed");
    let preview = Pool::destroy(
        &repo.path,
        DestroyOptions {
            dry_run: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(preview.pool_id, None);
    assert!(!preview.deleted);
    assert_eq!(
        Pool::destroy(
            &repo.path,
            DestroyOptions {
                pool_id: old.pool_id,
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ConfigConflict
    );
    let cancelled = Pool::destroy(&repo.path, Default::default()).unwrap();
    assert_ne!(cancelled.pool_id, old.pool_id);
    assert!(cancelled.deleted);
    assert_eq!(std::fs::read(archive).unwrap(), original);
    assert!(!repo.state().join("initializing.json").exists());
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(Pool::open(&repo.path)
        .unwrap()
        .acquire(Default::default())
        .is_ok());
}

#[test]
fn interrupted_named_initialization_keeps_its_root_claim_and_sibling_usable() {
    if !support::in_subprocess(
        "interrupted_named_initialization_keeps_its_root_claim_and_sibling_usable",
    ) {
        return;
    }
    use groove_core::{PoolName, PoolPhase, DEFAULT_GIT_TIMEOUT};
    let repo = Repo::new();
    let backend: PoolName = "backend".parse().unwrap();
    let frontend: PoolName = "frontend".parse().unwrap();
    Pool::init_named(
        &repo.path,
        &backend,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, backend.as_str())),
            ..Default::default()
        },
    )
    .unwrap();
    let back = Pool::open_named(&repo.path, &backend).unwrap();
    drop(back.acquire(Default::default()).unwrap());
    let root = repo.temp.path().join("frontend-root");
    let (mut command, listener) = checkpoint_command(
        &repo,
        "before-ready-manifest",
        &[
            "--pool",
            "frontend",
            "init",
            "--capacity",
            "1",
            "--worktree-root",
            root.to_str().unwrap(),
        ],
    );
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(&listener, "before-ready-manifest", support::PROCESS_TIMEOUT)
        .unwrap();
    // Healthy normalization needs only the sibling's own ownership descriptor.
    let healthy = back.acquire(Default::default());
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    assert_eq!(
        healthy.unwrap().info().recovery,
        groove_core::RecoveryOutcome::Sanitized
    );
    let catalog = Pool::list_pools(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    assert_eq!(
        catalog.iter().find(|p| p.name == frontend).unwrap().phase,
        PoolPhase::Initializing
    );
    let listed = support::groove_in(&repo.path)
        .arg("pools")
        .output()
        .unwrap();
    let listed = support::assert_success(&listed);
    assert!(listed["result"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|pool| pool["name"] == "frontend" && pool["phase"] == "initializing"));
    let sizes = support::groove_in(&repo.path)
        .args(["pools", "--disk-usage"])
        .output()
        .unwrap();
    let sizes = support::assert_success(&sizes);
    let interrupted = sizes["result"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["name"] == "frontend")
        .unwrap();
    assert_eq!(interrupted["phase"], "initializing");
    assert_eq!(interrupted.get("storage"), Some(&serde_json::Value::Null));
    let lock = repo.state().join("pools/frontend/locks/wt-0001.lock");
    let inode = std::fs::metadata(&lock).unwrap().ino();
    assert_eq!(
        Pool::init_named(
            &repo.path,
            &"competitor".parse().unwrap(),
            PoolOptions {
                capacity: 1,
                worktree_root: Some(root.clone()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        groove_core::ErrorCode::ConfigConflict
    );
    Pool::init_named(
        &repo.path,
        &frontend,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(root),
            resume: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(std::fs::metadata(lock).unwrap().ino(), inode);
    assert!(Pool::open_named(&repo.path, &frontend)
        .unwrap()
        .acquire(Default::default())
        .is_ok());
    assert!(back.acquire(Default::default()).is_ok());
}

#[test]
fn interrupted_named_deletion_never_disables_a_siblings_workers_or_results() {
    if !support::in_subprocess(
        "interrupted_named_deletion_never_disables_a_siblings_workers_or_results",
    ) {
        return;
    }
    use groove_core::{DestroyOptions, ExecOptions, PoolName, PoolPhase, DEFAULT_GIT_TIMEOUT};
    let repo = Repo::new();
    let frontend: PoolName = "frontend".parse().unwrap();
    let backend: PoolName = "backend".parse().unwrap();
    for pool in [&frontend, &backend] {
        Pool::init_named(
            &repo.path,
            pool,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(support::worktree_root(&repo.path, pool.as_str())),
                ..Default::default()
            },
        )
        .unwrap();
    }
    let front = Pool::open_named(&repo.path, &frontend).unwrap();
    drop(front.acquire(Default::default()).unwrap());
    let back = Pool::open_named(&repo.path, &backend).unwrap();
    let b = back.reserve(Default::default()).unwrap();
    std::fs::write(b.worktree_path.join("tracked.txt"), "backend result").unwrap();
    let (mut command, listener) =
        checkpoint_command(&repo, "pool-deleting", &["--pool", "frontend", "destroy"]);
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(&listener, "pool-deleting", support::PROCESS_TIMEOUT)
        .unwrap();
    let ran = back.exec(
        b.lease_id,
        ExecOptions {
            command: vec!["true".into()],
            ..Default::default()
        },
    );
    let catalog = Pool::list_pools(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    assert!(ran.unwrap().succeeded());
    assert_eq!(
        catalog.iter().find(|p| p.name == frontend).unwrap().phase,
        PoolPhase::Deleting
    );
    let listed = support::groove_in(&repo.path)
        .arg("pools")
        .output()
        .unwrap();
    let listed = support::assert_success(&listed);
    assert!(listed["result"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|pool| pool["name"] == "frontend" && pool["phase"] == "deleting"));
    assert_eq!(
        catalog.iter().find(|p| p.name == backend).unwrap().phase,
        PoolPhase::Ready
    );
    let finished = back.finish(b.lease_id, Default::default()).unwrap();
    let sizes = support::groove_in(&repo.path)
        .args(["pools", "--disk-usage"])
        .output()
        .unwrap();
    let sizes = support::assert_success(&sizes);
    let interrupted = sizes["result"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pool| pool["name"] == "frontend")
        .unwrap();
    assert_eq!(interrupted["phase"], "deleting");
    assert_eq!(interrupted.get("storage"), Some(&serde_json::Value::Null));
    let resumed = Pool::destroy_named(&repo.path, &frontend, DestroyOptions::default()).unwrap();
    assert!(resumed.deleted && resumed.resumed);
    let snapshots =
        groove_core::snapshot::Snapshots::open(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    assert!(snapshots.show(&finished.snapshot.id).unwrap().pinned);
    assert_eq!(
        std::fs::read_to_string(b.worktree_path.join("tracked.txt")).unwrap(),
        "backend result"
    );
    assert!(back.acquire(Default::default()).is_ok());
}

#[test]
fn snapshot_recovery_serializes_new_namespace_claims_until_materialization_finishes() {
    if !support::in_subprocess(
        "snapshot_recovery_serializes_new_namespace_claims_until_materialization_finishes",
    ) {
        return;
    }
    use groove_core::{snapshot::Snapshots, PoolName, DEFAULT_GIT_TIMEOUT};
    use std::{io::Write, time::Duration};
    let repo = Repo::new();
    let frontend: PoolName = "frontend".parse().unwrap();
    let backend: PoolName = "backend".parse().unwrap();
    Pool::init_named(
        &repo.path,
        &frontend,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, frontend.as_str())),
            ..Default::default()
        },
    )
    .unwrap();
    let snapshots = Snapshots::open(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    let saved = snapshots.save(Default::default()).unwrap();
    let destination = repo.temp.path().join("recovered");
    let (mut command, listener) = checkpoint_command(
        &repo,
        "snapshot-restored-files",
        &[
            "snapshot",
            "restore",
            &saved.id,
            "--to",
            destination.to_str().unwrap(),
        ],
    );
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = child
        .checkpoint(
            &listener,
            "snapshot-restored-files",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    let blocked = Pool::init_named(
        &repo.path,
        &backend,
        PoolOptions {
            capacity: 1,
            git_timeout: Duration::from_secs(2),
            worktree_root: Some(support::worktree_root(&repo.path, backend.as_str())),
            ..Default::default()
        },
    );
    let catalog = Pool::list_pools(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    stream.write_all(b"g").unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(blocked.unwrap_err().code, groove_core::ErrorCode::IoError);
    assert_eq!(
        catalog.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
        vec![frontend]
    );
    assert_eq!(
        std::fs::read_to_string(destination.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    Pool::init_named(
        &repo.path,
        &backend,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, backend.as_str())),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(Pool::open_named(&repo.path, &backend)
        .unwrap()
        .acquire(Default::default())
        .is_ok());
}

#[test]
fn pool_growth_crash_boundaries_preserve_reservations_and_lock_inodes() {
    if !support::in_subprocess("pool_growth_crash_boundaries_preserve_reservations_and_lock_inodes")
    {
        return;
    }
    for (name, reinitialized) in [
        "metadata-synced",
        "metadata-renamed",
        "pool-slot-added",
        "before-pool-update",
        "pool-updated",
    ]
    .into_iter()
    .flat_map(|name| [(name, false), (name, true)])
    {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: if reinitialized { 2 } else { 1 },
                worktree_root: Some(support::worktree_root(&r.path, "default")),
                ..Default::default()
            },
        )
        .unwrap();
        if reinitialized {
            let old = Pool::open(&r.path).unwrap();
            let leases = [
                old.acquire(Default::default()).unwrap(),
                old.acquire(Default::default()).unwrap(),
            ];
            drop(leases);
            Pool::destroy(&r.path, Default::default()).unwrap();
            Pool::init(
                &r.path,
                PoolOptions {
                    capacity: 1,
                    worktree_root: Some(r.temp.path().join("replacement-root")),
                    ..Default::default()
                },
            )
            .unwrap();
        }
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
        let new_locks: Vec<_> = ["wt-0002.lock", "wt-0002.execution.lock"]
            .into_iter()
            .map(|name| {
                let path = r.state().join("locks").join(name);
                let inode = std::fs::metadata(&path).unwrap().ino();
                (path, inode)
            })
            .collect();
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
        for (path, inode) in new_locks {
            assert_eq!(std::fs::metadata(path).unwrap().ino(), inode);
        }
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
                worktree_root: Some(support::worktree_root(&repo.path, "default")),
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
                        && error.retryable
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
                worktree_root: Some(support::worktree_root(&r.path, "default")),
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
                worktree_root: Some(support::worktree_root(&r.path, "default")),
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
            checkpoint_command(&r, checkpoint, &["release", "--save", "--lease-id", id]);
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
        let retry = support::assert_success(&cli(&["release", "--save", "--lease-id", id]));
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
    // Unrelated fixture forks can inherit the execution guard used by recovery
    // until exec, so dropping it need not make immediate cleanup possible.
    if !support::in_subprocess("history_recovery_keeps_unknown_and_known_outcomes_distinct") {
        return;
    }
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
                worktree_root: Some(support::worktree_root(&r.path, "default")),
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
        let cleanup = pool.clean_logs(Duration::ZERO, false).unwrap();
        assert_eq!(
            cleanup.entries[0].status,
            CleanupStatus::Removed,
            "{:?}",
            cleanup.entries[0]
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
            worktree_root: Some(support::worktree_root(&r.path, "default")),
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
            worktree_root: Some(support::worktree_root(&r.path, "default")),
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
                worktree_root: Some(support::worktree_root(&r.path, "default")),
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

#[test]
fn maintenance_contention_is_retryable_and_preserves_finish_results() {
    if !support::in_subprocess("maintenance_contention_is_retryable_and_preserves_finish_results") {
        return;
    }
    use std::{fs, io::Write};
    use support::assert_success;
    for dry_run in [true, false] {
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
        let id = lease.lease_id.to_string();
        assert_success(
            &support::groove_in(&repo.path)
                .args(["exec", "--lease-id", &id, "--", "true"])
                .output()
                .unwrap(),
        );
        fs::write(lease.worktree_path.join("result.txt"), "preserved result\n").unwrap();
        let args = if dry_run {
            vec![
                "storage",
                "clean",
                "--logs",
                "--older-than",
                "0s",
                "--dry-run",
            ]
        } else {
            vec!["storage", "clean", "--logs", "--older-than", "0s"]
        };
        let (mut command, listener) = checkpoint_command(&repo, "log-cleanup-locked", &args);
        let mut cleaner = support::TestChild::spawn(&mut command);
        let mut stream = cleaner
            .checkpoint(&listener, "log-cleanup-locked", support::PROCESS_TIMEOUT)
            .unwrap();
        let inode = fs::metadata(repo.state().join("locks/wt-0001.execution.lock"))
            .unwrap()
            .ino();
        for operation in [
            vec!["release", "--save", "--lease-id", &id],
            vec!["exec", "--lease-id", &id, "--", "true"],
        ] {
            let out = support::groove_in(&repo.path)
                .args(operation)
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(5));
            let response: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
            assert_eq!(response["error"]["code"], "INVALID_LEASE");
            assert_eq!(response["error"]["retryable"], true);
            assert_eq!(
                pool.lease_info(lease.lease_id).unwrap().lease_id,
                lease.lease_id
            );
            assert!(pool
                .inspect()
                .unwrap()
                .slots
                .iter()
                .all(|slot| slot.execution.is_none()));
            assert_eq!(
                fs::read(lease.worktree_path.join("result.txt")).unwrap(),
                b"preserved result\n"
            );
        }
        stream.write_all(b"g").unwrap();
        assert!(cleaner.wait().unwrap().success());
        let result = assert_success(
            &support::groove_in(&repo.path)
                .args(["release", "--save", "--lease-id", &id])
                .output()
                .unwrap(),
        );
        let snapshot = result["result"]["snapshot"]["id"].as_str().unwrap();
        let recovered = repo.temp.path().join("recovered");
        assert_success(
            &support::groove_in(&repo.path)
                .args(["snapshot", "restore", snapshot, "--to"])
                .arg(&recovered)
                .output()
                .unwrap(),
        );
        assert_eq!(
            fs::read(recovered.join("result.txt")).unwrap(),
            b"preserved result\n"
        );
        assert!(pool.protected_leases().unwrap().is_empty());
        assert_eq!(
            fs::metadata(repo.state().join("locks/wt-0001.execution.lock"))
                .unwrap()
                .ino(),
            inode
        );
    }
}

#[test]
fn finish_retries_saved_snapshot_after_maintenance_blocks_release() {
    if !support::in_subprocess("finish_retries_saved_snapshot_after_maintenance_blocks_release") {
        return;
    }
    use std::{fs, io::Write};
    use support::assert_success;
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
    let id = lease.lease_id.to_string();
    assert_success(
        &support::groove_in(&repo.path)
            .args(["exec", "--lease-id", &id, "--", "true"])
            .output()
            .unwrap(),
    );
    fs::write(lease.worktree_path.join("tracked.txt"), "staged\n").unwrap();
    support::git(&lease.worktree_path, &["add", "tracked.txt"]);
    fs::write(lease.worktree_path.join("tracked.txt"), "working\n").unwrap();
    let (mut command, listener) = checkpoint_command(
        &repo,
        "finish-before-release",
        &["release", "--save", "--lease-id", &id],
    );
    let mut finisher = support::TestChild::spawn(&mut command);
    let mut finish_stream = finisher
        .checkpoint(&listener, "finish-before-release", support::PROCESS_TIMEOUT)
        .unwrap();
    let socket = repo.temp.path().join("cleanup.sock");
    let cleanup_listener = UnixListener::bind(&socket).unwrap();
    let mut cleaner = support::TestChild::spawn(
        support::groove_in(&repo.path)
            .args([
                "storage",
                "clean",
                "--logs",
                "--older-than",
                "0s",
                "--dry-run",
            ])
            .env("GROOVE_TEST_CHECKPOINT", "log-cleanup-locked")
            .env("GROOVE_TEST_SOCKET", socket),
    );
    let mut cleanup_stream = cleaner
        .checkpoint(
            &cleanup_listener,
            "log-cleanup-locked",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    finish_stream.write_all(b"g").unwrap();
    let failed = finisher.wait_with_output().unwrap();
    assert_eq!(failed.status.code(), Some(5));
    let failure: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failure["error"]["retryable"], true);
    let snapshot = failure["error"]["details"]["snapshot_id"].as_str().unwrap();
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().lease_id,
        lease.lease_id
    );
    cleanup_stream.write_all(b"g").unwrap();
    assert!(cleaner.wait().unwrap().success());
    let retried = assert_success(
        &support::groove_in(&repo.path)
            .args(["release", "--save", "--lease-id", &id])
            .output()
            .unwrap(),
    );
    assert_eq!(retried["result"]["snapshot"]["id"], snapshot);
    assert!(pool.protected_leases().unwrap().is_empty());
    let recovered = repo.temp.path().join("recovered");
    assert_success(
        &support::groove_in(&repo.path)
            .args(["snapshot", "restore", snapshot, "--to"])
            .arg(&recovered)
            .output()
            .unwrap(),
    );
    assert_eq!(
        fs::read(recovered.join("tracked.txt")).unwrap(),
        b"working\n"
    );
    assert_eq!(
        support::git(&recovered, &["show", ":tracked.txt"]),
        "staged"
    );
}
