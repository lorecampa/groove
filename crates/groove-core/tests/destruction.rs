//! Deletion must preserve results and exclusion even on rejection and interruption.
mod support;
use groove_core::{
    snapshot::{SaveOptions, Snapshots},
    AcquireOptions, DestroyOptions, ErrorCode, ExecOptions, HistoryFilter, Pool, PoolOptions,
    PoolUpdate, ReservationOptions,
};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    time::Duration,
};
use support::Repo;

fn pool(repo: &Repo, capacity: usize) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}
fn destroy(repo: &Repo, discard_changes: bool) -> groove_core::Result<groove_core::DestroyResult> {
    Pool::destroy(
        &repo.path,
        DestroyOptions {
            discard_changes,
            ..Default::default()
        },
    )
}

#[test]
fn deletion_retains_results_history_and_permanent_locks_across_smaller_reinit() {
    if !support::in_subprocess(
        "deletion_retains_results_history_and_permanent_locks_across_smaller_reinit",
    ) {
        return;
    }
    let repo = Repo::new();
    let old = pool(&repo, 2);
    let first = old.acquire(Default::default()).unwrap();
    let reserved = old
        .reserve(ReservationOptions {
            request_key: Some("completed".into()),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        reserved.slot_id.number(),
        if first.info().slot_id.number() == 1 {
            2
        } else {
            1
        }
    );
    // Put history on both slots so one remains outside a smaller replacement pool.
    let run = old
        .exec(
            reserved.lease_id,
            ExecOptions {
                command: vec!["sh".into(), "-c".into(), "printf saved-log".into()],
                ..Default::default()
            },
        )
        .unwrap();
    assert!(run.succeeded());
    fs::write(reserved.worktree_path.join("tracked.txt"), "saved result").unwrap();
    let finish = old
        .finish(reserved.lease_id, SaveOptions::default())
        .unwrap();
    let slot = first.info().slot_id.clone();
    let second = first.into_protected().unwrap();
    old.exec(
        second.lease_id,
        ExecOptions {
            command: vec!["true".into()],
            ..Default::default()
        },
    )
    .unwrap();
    old.release_lease(second.lease_id).unwrap();
    let inodes: Vec<_> = fs::read_dir(repo.state().join("locks"))
        .unwrap()
        .map(|e| {
            let path = e.unwrap().path();
            let ino = fs::metadata(&path).unwrap().ino();
            (path, ino)
        })
        .collect();
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
    assert!(!preview.deleted);
    assert_eq!(
        fs::read(repo.state().join("lifecycle.json")).unwrap(),
        before
    );
    assert!(repo.slot(1).exists() || repo.slot(2).exists());
    let result = destroy(&repo, true).unwrap();
    assert!(result.deleted && !result.already_deleted);
    assert!(!repo.slot(1).exists() && !repo.slot(2).exists());
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::NotInitialized
    );
    assert!(destroy(&repo, true).unwrap().already_deleted);
    for (path, ino) in &inodes {
        assert_eq!(fs::metadata(path).unwrap().ino(), *ino);
    }
    let retained = Pool::open_retained(&repo.path, Duration::from_secs(10)).unwrap();
    let histories = retained.history(&HistoryFilter::default()).unwrap();
    assert_eq!(histories.len(), 2);
    assert!(histories
        .iter()
        .any(|h| fs::read_to_string(&h.stdout_path).unwrap() == "saved-log"));
    assert_eq!(
        retained
            .finish(reserved.lease_id, SaveOptions::default())
            .unwrap()
            .snapshot
            .id,
        finish.snapshot.id
    );
    let snapshots = Snapshots::open(&repo.path, Duration::from_secs(10)).unwrap();
    assert!(snapshots.show(&finish.snapshot.id).unwrap().pinned);
    let restored = repo.temp.path().join("restored");
    snapshots
        .restore(&finish.snapshot.id, Some(&restored))
        .unwrap();
    assert_eq!(
        fs::read_to_string(restored.join("tracked.txt")).unwrap(),
        "saved result"
    );
    let new = pool(&repo, 1);
    assert_eq!(
        old.acquire(Default::default()).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        old.configure(PoolUpdate {
            capacity: Some(3),
            ..Default::default()
        })
        .unwrap_err()
        .code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        Pool::destroy(
            &repo.path,
            DestroyOptions {
                pool_id: result.pool_id,
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        new.reserve(ReservationOptions {
            request_key: Some("completed".into()),
            ..Default::default()
        })
        .unwrap_err()
        .code,
        ErrorCode::InvalidLease
    );
    let retained = Pool::open_retained(&repo.path, Duration::from_secs(10)).unwrap();
    let cleanup = retained.clean_logs(Duration::ZERO, false).unwrap();
    assert!(
        cleanup
            .entries
            .iter()
            .all(|e| e.status == groove_core::CleanupStatus::Removed),
        "{cleanup:?}"
    );
    for h in histories {
        retained.history_delete(h.run_id).unwrap();
    }
    assert!(retained
        .history(&HistoryFilter::default())
        .unwrap()
        .is_empty());
    assert_eq!(new.capacity(), 1);
    assert!(repo.state().join(format!("locks/{slot}.lock")).exists());
}

#[test]
fn live_descriptors_and_durable_reservations_cannot_be_discarded() {
    if !support::in_subprocess("live_descriptors_and_durable_reservations_cannot_be_discarded") {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 1);
    let lease = p.acquire(Default::default()).unwrap();
    fs::write(lease.info().worktree_path.join("result"), "keep").unwrap();
    assert_eq!(
        destroy(&repo, true).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    let info = lease.into_protected().unwrap();
    assert_eq!(
        destroy(&repo, true).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(p.lease_info(info.lease_id).unwrap().lease_id, info.lease_id);
    assert_eq!(
        fs::read_to_string(info.worktree_path.join("result")).unwrap(),
        "keep"
    );
    p.release_lease(info.lease_id).unwrap();
    assert!(destroy(&repo, true).unwrap().deleted);
}

#[test]
fn released_edits_and_ignored_files_need_explicit_discard() {
    if !support::in_subprocess("released_edits_and_ignored_files_need_explicit_discard") {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 1);
    let mut lease = p.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "valuable cache").unwrap();
    lease.release().unwrap();
    assert_eq!(
        destroy(&repo, false).unwrap_err().code,
        ErrorCode::UnsavedChanges
    );
    assert_eq!(
        fs::read_to_string(path.join("ignored/cache")).unwrap(),
        "valuable cache"
    );
    assert!(p
        .acquire(AcquireOptions {
            base: "HEAD".into(),
            ..Default::default()
        })
        .is_ok());
    assert!(destroy(&repo, false).unwrap().deleted);
}

#[test]
fn symlinked_paths_and_corrupt_protection_fail_without_removing_any_slot() {
    if !support::in_subprocess(
        "symlinked_paths_and_corrupt_protection_fail_without_removing_any_slot",
    ) {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 2);
    let a = p.acquire(Default::default()).unwrap();
    let b = p.acquire(Default::default()).unwrap();
    drop(a);
    drop(b);
    let path = repo.state().join("slots/wt-0002.protected.json");
    fs::write(&path, "broken").unwrap();
    assert_eq!(
        destroy(&repo, true).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert!(repo.slot(1).exists() && repo.slot(2).exists());
    fs::remove_file(path).unwrap();
    fs::remove_dir_all(repo.slot(2)).unwrap();
    symlink(&repo.path, repo.slot(2)).unwrap();
    assert_eq!(
        destroy(&repo, true).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert!(repo.slot(1).exists());
    assert_eq!(
        fs::read_to_string(repo.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn external_roots_and_forged_git_pointers_preserve_unrelated_worktrees() {
    if !support::in_subprocess(
        "external_roots_and_forged_git_pointers_preserve_unrelated_worktrees",
    ) {
        return;
    }
    let repo = Repo::new();
    let root = repo.temp.path().join("external");
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(root.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let p = Pool::open(&repo.path).unwrap();
    let lease = p.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    drop(lease);
    let other = repo.temp.path().join("other");
    repo.git(&["worktree", "add", "--detach", other.to_str().unwrap()]);
    fs::write(
        path.join(".git"),
        format!("gitdir: {}\n", repo.path.join(".git").display()),
    )
    .unwrap();
    fs::write(root.join("unrelated"), "keep").unwrap();
    let result = destroy(&repo, true).unwrap();
    assert_eq!(result.worktree_root, root);
    assert_eq!(fs::read_to_string(root.join("unrelated")).unwrap(), "keep");
    assert_eq!(
        support::git(&other, &["rev-parse", "HEAD"]),
        repo.git(&["rev-parse", "HEAD"])
    );
    assert_eq!(
        fs::read_to_string(repo.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert!(!repo
        .git(&["worktree", "list", "--porcelain"])
        .contains(path.to_str().unwrap()));
}

#[test]
fn missing_checkout_root_still_removes_only_owned_native_registrations() {
    if !support::in_subprocess(
        "missing_checkout_root_still_removes_only_owned_native_registrations",
    ) {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 1);
    drop(p.acquire(Default::default()).unwrap());
    fs::remove_dir_all(repo.state().join("worktrees")).unwrap();
    assert!(destroy(&repo, false).unwrap().deleted);
    assert!(!repo
        .git(&["worktree", "list", "--porcelain"])
        .contains("wt-0001"));
    assert!(pool(&repo, 1).acquire(Default::default()).is_ok());
}

#[test]
fn artifact_cleanup_preserves_permanent_locks_and_symlink_targets() {
    if !support::in_subprocess("artifact_cleanup_preserves_permanent_locks_and_symlink_targets") {
        return;
    }
    let repo = Repo::new();
    pool(&repo, 1);
    let root = repo.state().join("artifacts");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let key = "a".repeat(64);
    let lock = root.join(format!("{key}.lock"));
    fs::write(&lock, "").unwrap();
    fs::set_permissions(&lock, fs::Permissions::from_mode(0o600)).unwrap();
    let ino = fs::metadata(&lock).unwrap().ino();
    let data = root.join(&key);
    fs::create_dir(&data).unwrap();
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(repo.path.join("tracked.txt"), data.join("link")).unwrap();
    let unknown = root.join("unexpected");
    fs::write(&unknown, "keep").unwrap();
    assert_eq!(
        destroy(&repo, false).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert!(data.exists());
    fs::remove_file(unknown).unwrap();
    assert_eq!(destroy(&repo, false).unwrap().artifacts, vec![data.clone()]);
    assert!(!data.exists());
    assert_eq!(fs::metadata(lock).unwrap().ino(), ino);
    assert_eq!(
        fs::read_to_string(repo.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn bare_sha256_pool_and_lazy_slots_can_be_deleted() {
    if !support::in_subprocess("bare_sha256_pool_and_lazy_slots_can_be_deleted") {
        return;
    }
    let repo = Repo::format("sha256");
    let bare = repo.temp.path().join("bare.git");
    repo.git(&[
        "clone",
        "--bare",
        repo.path.to_str().unwrap(),
        bare.to_str().unwrap(),
    ]);
    Pool::init(
        &bare,
        PoolOptions {
            capacity: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let p = Pool::open(&bare).unwrap();
    drop(p.acquire(Default::default()).unwrap());
    let result = Pool::destroy(&bare, Default::default()).unwrap();
    assert_eq!(result.slots.len(), 2);
    assert!(result.deleted);
    assert_eq!(
        support::git(&bare, &["rev-parse", "HEAD"]),
        repo.git(&["rev-parse", "HEAD"])
    );
}

#[test]
fn stale_snapshot_handles_cannot_restore_into_a_replacement_pool() {
    if !support::in_subprocess("stale_snapshot_handles_cannot_restore_into_a_replacement_pool") {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 1);
    let lease = p.acquire(Default::default()).unwrap();
    let snapshots = Snapshots::open(&lease.info().worktree_path, Duration::from_secs(10)).unwrap();
    let saved = snapshots.save(SaveOptions::default()).unwrap();
    drop(lease);
    destroy(&repo, false).unwrap();
    let replacement = pool(&repo, 1);
    let lease = replacement.acquire(Default::default()).unwrap();
    fs::write(lease.info().worktree_path.join("tracked.txt"), "successor").unwrap();
    assert_eq!(
        snapshots.restore(&saved.id, None).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        snapshots.save(SaveOptions::default()).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        fs::read_to_string(lease.info().worktree_path.join("tracked.txt")).unwrap(),
        "successor"
    );
}

#[test]
fn reinitialization_never_recreates_a_missing_permanent_lock() {
    if !support::in_subprocess("reinitialization_never_recreates_a_missing_permanent_lock") {
        return;
    }
    let repo = Repo::new();
    pool(&repo, 2);
    destroy(&repo, false).unwrap();
    // Corruption is deliberate test setup, never a recovery action.
    let lock = repo.state().join("locks/wt-0002.lock");
    fs::remove_file(&lock).unwrap();
    let error = Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::IoError);
    assert!(!lock.exists());
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::NotInitialized
    );
}

#[test]
fn hidden_edits_and_unreferenced_commits_are_never_silently_discarded() {
    if !support::in_subprocess("hidden_edits_and_unreferenced_commits_are_never_silently_discarded")
    {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 1);
    let lease = p.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    drop(lease);
    support::git(
        &path,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    fs::write(path.join("tracked.txt"), "hidden result").unwrap();
    assert_eq!(
        destroy(&repo, false).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "hidden result"
    );
    support::git(
        &path,
        &["update-index", "--no-assume-unchanged", "tracked.txt"],
    );
    support::git(&path, &["commit", "-qam", "detached result"]);
    let oid = support::git(&path, &["rev-parse", "HEAD"]);
    assert_eq!(
        destroy(&repo, false).unwrap_err().code,
        ErrorCode::UnsavedChanges
    );
    assert_eq!(support::git(&path, &["rev-parse", "HEAD"]), oid);
    // A repository ref must preserve HEAD; the snapshot's parent does so.
    let snapshots = Snapshots::open(&path, Duration::from_secs(10)).unwrap();
    let saved = snapshots.save(SaveOptions::default()).unwrap();
    assert!(destroy(&repo, false).unwrap().deleted);
    assert_eq!(
        Snapshots::open(&repo.path, Duration::from_secs(10))
            .unwrap()
            .show(&saved.id)
            .unwrap()
            .head
            .unwrap()
            .as_str(),
        oid
    );
}

#[test]
fn prepared_pools_remove_only_validated_stale_setup_endpoints() {
    if !support::in_subprocess("prepared_pools_remove_only_validated_stale_setup_endpoints") {
        return;
    }
    let repo = Repo::new();
    fs::write(
        repo.path.join(".groove.toml"),
        "schema_version=1\n[[setup.dev.steps]]\nname='ready'\ncommand=['true']\n",
    )
    .unwrap();
    repo.git(&["add", ".groove.toml"]);
    repo.git(&["commit", "-qm", "setup"]);
    let p = pool(&repo, 1);
    let lease = p.reserve(Default::default()).unwrap();
    let ready = p
        .prepare(lease.lease_id, "dev", Default::default())
        .unwrap();
    assert_eq!(ready.status, groove_core::PreparationStatus::Ready);
    p.release_lease(lease.lease_id).unwrap();
    let path = repo.state().join("slots/wt-0001.setup.json");
    let original = fs::read(&path).unwrap();
    let mut malformed: serde_json::Value = serde_json::from_slice(&original).unwrap();
    malformed["control_name"] = "foreign-endpoint".into();
    fs::write(&path, serde_json::to_vec(&malformed).unwrap()).unwrap();
    assert_eq!(
        destroy(&repo, true).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert!(repo.slot(1).exists());
    fs::write(&path, original).unwrap();
    assert!(destroy(&repo, false).unwrap().deleted);
    assert_eq!(
        p.history(&HistoryFilter::default()).unwrap_err().code,
        ErrorCode::NotInitialized
    );
    assert_eq!(
        Pool::open_retained(&repo.path, Duration::from_secs(10))
            .unwrap()
            .history(&HistoryFilter::default())
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn missing_lifecycle_locks_fail_without_creating_a_replacement_inode() {
    if !support::in_subprocess("missing_lifecycle_locks_fail_without_creating_a_replacement_inode")
    {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, 1);
    drop(p.acquire(Default::default()).unwrap());
    let path = repo.state().join("locks/lifecycle.lock");
    fs::remove_file(&path).unwrap();
    assert_eq!(destroy(&repo, true).unwrap_err().code, ErrorCode::IoError);
    assert!(!path.exists());
    assert!(repo.slot(1).exists());
    assert_eq!(
        p.acquire(Default::default()).unwrap_err().code,
        ErrorCode::IoError
    );
    assert!(!path.exists());
}

#[test]
fn deleting_many_lazy_slots_uses_bounded_descriptors() {
    if !support::in_subprocess("deleting_many_lazy_slots_uses_bounded_descriptors") {
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
        "fixture requires at least 64 allowed descriptors"
    );
    limit.rlim_cur = 64;
    // SAFETY: limit is valid; only this isolated test process's soft limit changes.
    let set = unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) };
    assert_eq!(set, 0);
    let repo = Repo::new();
    pool(&repo, 64);
    let deleted = destroy(&repo, false).unwrap();
    assert!(deleted.deleted);
    assert_eq!(deleted.slots.len(), 64);
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::NotInitialized
    );
}

#[test]
fn unpublished_archive_files_are_preserved_and_do_not_block_retry() {
    if !support::in_subprocess("unpublished_archive_files_are_preserved_and_do_not_block_retry") {
        return;
    }
    let repo = Repo::new();
    pool(&repo, 1);
    let root = repo.state().join("retired");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let scratch = root.join(".unpublished");
    fs::write(&scratch, "incomplete archive").unwrap();
    assert!(destroy(&repo, false).unwrap().deleted);
    assert_eq!(fs::read_to_string(&scratch).unwrap(), "incomplete archive");
    assert!(pool(&repo, 1).acquire(Default::default()).is_ok());
}
