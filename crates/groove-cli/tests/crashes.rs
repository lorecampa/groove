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
                ready: Some(Default::default()),
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
fn deletion_crash_at_pool_deleting_preserves_results_and_inodes() {
    if support::in_subprocess("deletion_crash_at_pool_deleting_preserves_results_and_inodes") {
        assert_deletion_crash_preserves_results_and_inodes("pool-deleting");
    }
}

#[test]
fn deletion_crash_at_slot_removal_preserves_results_and_inodes() {
    if support::in_subprocess("deletion_crash_at_slot_removal_preserves_results_and_inodes") {
        assert_deletion_crash_preserves_results_and_inodes("pool-slot-removed");
    }
}

#[test]
fn deletion_crash_at_artifact_removal_preserves_results_and_inodes() {
    if support::in_subprocess("deletion_crash_at_artifact_removal_preserves_results_and_inodes") {
        assert_deletion_crash_preserves_results_and_inodes("pool-artifact-removed");
    }
}

#[test]
fn deletion_crash_at_pool_deleted_preserves_results_and_inodes() {
    if support::in_subprocess("deletion_crash_at_pool_deleted_preserves_results_and_inodes") {
        assert_deletion_crash_preserves_results_and_inodes("pool-deleted");
    }
}

fn assert_deletion_crash_preserves_results_and_inodes(name: &str) {
    use groove_core::{
        snapshot::{SaveOptions, Snapshots},
        DestroyOptions, ErrorCode,
    };
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
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
            ready: Some(Default::default()),
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
    use groove_core::{DestroyOptions, PoolName, PoolPhase, DEFAULT_GIT_TIMEOUT};
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
    let catalog = Pool::list_pools(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    let sibling = back.lease_info(b.lease_id).unwrap();
    assert_eq!(sibling.lease_id, b.lease_id);
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
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

macro_rules! pool_growth_crash_test {
    ($test:ident, $checkpoint:literal, $reinitialized:literal) => {
        #[test]
        fn $test() {
            if support::in_subprocess(stringify!($test)) {
                pool_growth_crash_preserves_reservations_and_lock_inodes(
                    $checkpoint,
                    $reinitialized,
                );
            }
        }
    };
}

pool_growth_crash_test!(growth_metadata_synced, "metadata-synced", false);
pool_growth_crash_test!(growth_metadata_renamed, "metadata-renamed", false);
pool_growth_crash_test!(growth_slot_added, "pool-slot-added", false);
pool_growth_crash_test!(growth_before_pool_update, "before-pool-update", false);
pool_growth_crash_test!(growth_pool_updated, "pool-updated", false);
pool_growth_crash_test!(
    reinitialized_growth_metadata_synced,
    "metadata-synced",
    true
);
pool_growth_crash_test!(
    reinitialized_growth_metadata_renamed,
    "metadata-renamed",
    true
);
pool_growth_crash_test!(reinitialized_growth_slot_added, "pool-slot-added", true);
pool_growth_crash_test!(
    reinitialized_growth_before_pool_update,
    "before-pool-update",
    true
);
pool_growth_crash_test!(reinitialized_growth_pool_updated, "pool-updated", true);

fn pool_growth_crash_preserves_reservations_and_lock_inodes(name: &str, reinitialized: bool) {
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
            let output = support::groove_in(&r.path)
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
            assert_eq!(
                pool.acquire(Default::default()).unwrap_err().code,
                groove_core::ErrorCode::PoolExhausted
            );
        }
        // Use independent CLI processes throughout. Taking locks in this
        // multithreaded harness can transiently lend them to another test's fork
        // before CLOEXEC runs; that is not the ordinary one-shot CLI lifecycle.
        let release = |id: groove_core::LeaseId| {
            support::groove_in(&r.path)
                .arg("--repo")
                .arg(&r.path)
                .args(["release", "--lease-id", &id.to_string()])
                .output()
                .unwrap()
        };
        support::assert_success(&release(lease.lease_id));
        support::assert_success(&release(lease.lease_id));
        let acquired = support::groove_in(&r.path)
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
fn saved_release_retires_every_pin_superseded_by_interrupted_retries() {
    superseded_pins(false);
}

#[test]
fn pin_retirement_recovers_after_release_and_slot_reuse() {
    superseded_pins(true);
}

fn superseded_pins(interrupt_retirement: bool) {
    let repo = Repo::new();
    let cli = |args: &[&str]| support::groove_in(&repo.path).args(args).output().unwrap();
    support::assert_success(&cli(&["init", "--capacity", "1"]));
    let lease = support::assert_success(&cli(&["acquire"]))["result"].clone();
    let id = lease["lease_id"].as_str().unwrap();
    let mut superseded = Vec::new();
    for attempt in 0..3 {
        if attempt > 0 {
            std::fs::remove_file(repo.temp.path().join("checkpoint.sock")).unwrap();
        }
        std::fs::write(
            repo.slot(1).join("tracked.txt"),
            format!("attempt {attempt}\n"),
        )
        .unwrap();
        let (mut command, listener) = checkpoint_command(
            &repo,
            "finish-snapshot-saved",
            &["release", "--save", "--lease-id", id],
        );
        kill_at_checkpoint(&mut command, listener, "finish-snapshot-saved");
        let receipt: serde_json::Value = serde_json::from_slice(
            &std::fs::read(repo.state().join(format!("slots/finish-{id}.json"))).unwrap(),
        )
        .unwrap();
        superseded.push(receipt["snapshot"]["id"].as_str().unwrap().to_owned());
    }
    for old in &superseded {
        let snapshot = support::assert_success(&cli(&["snapshot", "show", old]));
        assert_eq!(snapshot["result"]["snapshot"]["pinned"], true);
    }
    std::fs::write(repo.slot(1).join("tracked.txt"), b"final bytes\n").unwrap();
    let successor = if interrupt_retirement {
        let (mut command, listener) = checkpoint_command_after_removing_socket(
            &repo,
            "finish-before-pin-retirement",
            &["release", "--save", "--lease-id", id],
        );
        kill_at_checkpoint(&mut command, listener, "finish-before-pin-retirement");
        let successor = support::assert_success(&cli(&["acquire"]))["result"].clone();
        std::fs::write(repo.slot(1).join("successor.txt"), b"preserve successor").unwrap();
        Some(successor)
    } else {
        None
    };
    let finished = support::assert_success(&cli(&["release", "--save", "--lease-id", id]));
    assert_eq!(finished["result"]["snapshot"]["pinned"], true);
    for old in &superseded {
        let snapshot = support::assert_success(&cli(&["snapshot", "show", old]));
        assert_eq!(snapshot["result"]["snapshot"]["pinned"], false, "{old}");
    }
    let recovered = repo.temp.path().join("final-recovery");
    support::assert_success(&cli(&[
        "snapshot",
        "restore",
        finished["result"]["snapshot"]["id"].as_str().unwrap(),
        "--to",
        recovered.to_str().unwrap(),
    ]));
    assert_eq!(
        std::fs::read(recovered.join("tracked.txt")).unwrap(),
        b"final bytes\n"
    );
    if let Some(successor) = successor {
        assert_eq!(
            std::fs::read(repo.slot(1).join("successor.txt")).unwrap(),
            b"preserve successor"
        );
        assert_eq!(
            Pool::open(&repo.path).unwrap().protected_leases().unwrap()[0]
                .lease_id
                .to_string(),
            successor["lease_id"].as_str().unwrap()
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
        if successor.is_some() {
            assert_eq!(retry["result"]["snapshot"]["id"], receipt["snapshot"]["id"]);
        }
        let recovered = r.temp.path().join("recovered");
        support::assert_success(&cli(&[
            "snapshot",
            "restore",
            retry["result"]["snapshot"]["id"].as_str().unwrap(),
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

fn retry_unreleased_finish(checkpoint: &str, delete_snapshot: bool, fail_capture: bool) {
    let r = Repo::new();
    let cli = |args: &[&str]| support::groove_in(&r.path).args(args).output().unwrap();
    support::assert_success(&cli(&["init", "--capacity", "1"]));
    let lease = support::assert_success(&cli(&["acquire"]))["result"].clone();
    let id = lease["lease_id"].as_str().unwrap();
    std::fs::write(r.slot(1).join("tracked.txt"), "first checkpoint").unwrap();
    let (mut command, listener) =
        checkpoint_command(&r, checkpoint, &["release", "--save", "--lease-id", id]);
    kill_at_checkpoint(&mut command, listener, checkpoint);
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(r.state().join(format!("slots/finish-{id}.json"))).unwrap(),
    )
    .unwrap();
    if delete_snapshot {
        support::assert_success(&cli(&[
            "snapshot",
            "delete",
            receipt["snapshot"]["id"].as_str().unwrap(),
            "--force",
        ]));
    }
    std::fs::write(r.slot(1).join("tracked.txt"), "work after crash").unwrap();
    support::git(&r.slot(1), &["add", "tracked.txt"]);
    std::fs::write(r.slot(1).join("tracked.txt"), "unstaged after crash").unwrap();
    std::fs::write(r.slot(1).join("new.txt"), "new work").unwrap();
    if fail_capture {
        let failure = cli(&[
            "release",
            "--save",
            "--lease-id",
            id,
            "--max-file-bytes",
            "1",
        ]);
        assert!(!failure.status.success());
        let failed: serde_json::Value = serde_json::from_slice(&failure.stdout).unwrap();
        assert_eq!(failed["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(
            Pool::open(&r.path).unwrap().protected_leases().unwrap()[0]
                .lease_id
                .to_string(),
            id
        );
        let exhausted = cli(&["acquire"]);
        assert!(!exhausted.status.success());
        assert_eq!(
            std::fs::read(r.slot(1).join("new.txt")).unwrap(),
            b"new work"
        );
    }
    let retry = support::assert_success(&cli(&["release", "--save", "--lease-id", id]));
    assert_ne!(retry["result"]["snapshot"]["id"], receipt["snapshot"]["id"]);
    assert_eq!(retry["result"]["snapshot"]["pinned"], true);
    // Reuse destroys the checkout; recovery must depend on the new snapshot alone.
    support::assert_success(&cli(&["acquire"]));
    let recovered = r.temp.path().join("recovered");
    support::assert_success(&cli(&[
        "snapshot",
        "restore",
        retry["result"]["snapshot"]["id"].as_str().unwrap(),
        "--to",
        recovered.to_str().unwrap(),
    ]));
    assert_eq!(
        std::fs::read(recovered.join("tracked.txt")).unwrap(),
        b"unstaged after crash"
    );
    assert_eq!(
        support::git(&recovered, &["show", ":tracked.txt"]),
        "work after crash"
    );
    assert_eq!(
        std::fs::read(recovered.join("new.txt")).unwrap(),
        b"new work"
    );
}

#[test]
fn finish_retry_saves_edits_after_snapshot_receipt() {
    retry_unreleased_finish("finish-snapshot-saved", false, false);
}

#[test]
fn finish_retry_saves_edits_before_release() {
    retry_unreleased_finish("finish-before-release", false, false);
}

#[test]
fn finish_retry_saves_edits_after_partial_release() {
    retry_unreleased_finish("release-recorded", false, false);
    retry_unreleased_finish("release-recorded", true, false);
    retry_unreleased_finish("release-recorded", false, true);
}

#[test]
fn finish_retry_replaces_deleted_snapshot() {
    retry_unreleased_finish("finish-before-release", true, false);
}

#[test]
fn finish_retry_capture_failure_preserves_reservation() {
    retry_unreleased_finish("finish-before-release", false, true);
}

#[test]
fn completed_finish_retry_rejects_deleted_snapshot_and_preserves_successor() {
    let r = Repo::new();
    let cli = |args: &[&str]| support::groove_in(&r.path).args(args).output().unwrap();
    support::assert_success(&cli(&["init", "--capacity", "1"]));
    let lease = support::assert_success(&cli(&["acquire"]))["result"].clone();
    let id = lease["lease_id"].as_str().unwrap();
    let finished = support::assert_success(&cli(&["release", "--save", "--lease-id", id]));
    let snapshot = finished["result"]["snapshot"]["id"].as_str().unwrap();
    support::assert_success(&cli(&["snapshot", "delete", snapshot, "--force"]));
    let successor = support::assert_success(&cli(&["acquire"]))["result"].clone();
    std::fs::write(r.slot(1).join("new.txt"), "successor work").unwrap();
    let retry = cli(&["release", "--save", "--lease-id", id]);
    assert!(!retry.status.success());
    let error: serde_json::Value = serde_json::from_slice(&retry.stdout).unwrap();
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        error["error"]["message"],
        format!("Snapshot not found: {snapshot}")
    );
    assert_eq!(error["error"]["details"]["snapshot_id"], snapshot);
    assert_eq!(error["error"]["details"]["released"], true);
    assert_eq!(
        Pool::open(&r.path).unwrap().protected_leases().unwrap()[0]
            .lease_id
            .to_string(),
        successor["lease_id"]
    );
    assert_eq!(
        std::fs::read(r.slot(1).join("new.txt")).unwrap(),
        b"successor work"
    );
    assert!(!cli(&["acquire"]).status.success());
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
