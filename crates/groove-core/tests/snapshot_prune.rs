mod support;
use groove_core::snapshot::{PruneOptions, PruneRetentionReason, SaveOptions, Snapshots};
use std::{fs, time::Duration};
use support::Repo;

fn service(repo: &Repo) -> Snapshots {
    Snapshots::open(&repo.path, Duration::from_secs(30)).unwrap()
}
fn options(apply: bool, keep_last: usize) -> PruneOptions {
    PruneOptions {
        older_than: Duration::ZERO,
        keep_last,
        apply,
    }
}
#[test]
fn preview_preserves_recoverable_bytes_and_apply_keeps_newest() {
    if !support::in_subprocess("preview_preserves_recoverable_bytes_and_apply_keeps_newest") {
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    fs::write(repo.path.join("tracked.txt"), "first saved bytes\n").unwrap();
    let first = snapshots.save(SaveOptions::default()).unwrap();
    fs::write(repo.path.join("tracked.txt"), "second saved bytes\n").unwrap();
    let second = snapshots.save(SaveOptions::default()).unwrap();
    let preview = snapshots.prune(options(false, 1)).unwrap();
    assert!(!preview.applied);
    assert_eq!(preview.selected.as_slice(), std::slice::from_ref(&first.id));
    assert!(preview.deleted.is_empty());
    assert_eq!(preview.retained[0].id, second.id);
    assert_eq!(preview.retained[0].reason, PruneRetentionReason::KeepLast);
    let recovery = repo.temp.path().join("recovery");
    snapshots.restore(&first.id, Some(&recovery)).unwrap();
    assert_eq!(
        fs::read(recovery.join("tracked.txt")).unwrap(),
        b"first saved bytes\n"
    );
    let applied = snapshots.prune(options(true, 1)).unwrap();
    assert_eq!(applied.deleted, [first.id]);
    assert_eq!(snapshots.list().unwrap()[0].id, second.id);
    assert!(snapshots
        .prune(options(true, 1))
        .unwrap()
        .deleted
        .is_empty());
}
#[test]
fn newest_window_is_per_original_git_directory() {
    if !support::in_subprocess("newest_window_is_per_original_git_directory") {
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    let a1 = snapshots.save(SaveOptions::default()).unwrap();
    let a2 = snapshots.save(SaveOptions::default()).unwrap();
    let linked = repo.temp.path().join("linked");
    repo.git(&[
        "worktree",
        "add",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    let other = Snapshots::open(&linked, Duration::from_secs(30)).unwrap();
    let b1 = other.save(SaveOptions::default()).unwrap();
    let b2 = other.save(SaveOptions::default()).unwrap();
    let result = snapshots.prune(options(true, 1)).unwrap();
    assert_eq!(result.deleted, [b1.id, a1.id]);
    let remaining: Vec<_> = snapshots
        .list()
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(remaining, [b2.id, a2.id]);
}
#[test]
fn excessive_age_retains_snapshots_and_corrupt_inventory_fails_closed() {
    if !support::in_subprocess("excessive_age_retains_snapshots_and_corrupt_inventory_fails_closed")
    {
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    let saved = snapshots.save(SaveOptions::default()).unwrap();
    let mut policy = options(true, 0);
    policy.older_than = Duration::MAX;
    let result = snapshots.prune(policy).unwrap();
    assert!(result.selected.is_empty());
    assert_eq!(result.retained[0].reason, PruneRetentionReason::TooRecent);
    let reference = format!("refs/groove/snapshots/{}", uuid::Uuid::new_v4());
    repo.git(&["update-ref", &reference, "HEAD"]);
    assert!(snapshots.prune(options(true, 0)).is_err());
    assert_eq!(snapshots.show(&saved.id).unwrap().id, saved.id);
}
#[test]
fn protected_workspace_preserves_unpinned_published_checkpoint() {
    if !support::in_subprocess("protected_workspace_preserves_unpinned_published_checkpoint") {
        return;
    }
    let repo = Repo::new();
    groove_core::Pool::init(
        &repo.path,
        groove_core::PoolOptions {
            capacity: 1,
            worktree_root: Some(repo.temp.path().join("pool")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = groove_core::Pool::open(&repo.path).unwrap();
    let mut lease = pool.acquire(Default::default()).unwrap();
    lease.protect_from_reuse().unwrap();
    let snapshots = Snapshots::open(&lease.info().worktree_path, Duration::from_secs(30)).unwrap();
    let saved = snapshots.save(SaveOptions::default()).unwrap();
    assert!(!saved.pinned);
    let result = snapshots.prune(options(true, 0)).unwrap();
    assert!(result.deleted.is_empty());
    assert_eq!(result.retained[0].id, saved.id);
    assert_eq!(
        result.retained[0].reason,
        PruneRetentionReason::ProtectedWorkspace
    );
    // Git registrations may use relative backlinks. Their spelling must not
    // make the same durably protected source eligible for pruning.
    let admin = std::path::Path::new(&saved.workspace);
    let target = lease.info().worktree_path.join(".git");
    let common = admin
        .ancestors()
        .find(|base| target.starts_with(base))
        .unwrap();
    let mut relative = std::path::PathBuf::new();
    for _ in admin.strip_prefix(common).unwrap().components() {
        relative.push("..");
    }
    relative.push(target.strip_prefix(common).unwrap());
    fs::write(admin.join("gitdir"), format!("{}\n", relative.display())).unwrap();
    assert_eq!(
        snapshots.prune(options(true, 0)).unwrap().retained[0].reason,
        PruneRetentionReason::ProtectedWorkspace
    );
    lease.release().unwrap();
    assert_eq!(
        snapshots.prune(options(true, 0)).unwrap().deleted,
        [saved.id]
    );
}

#[test]
fn pins_count_inside_the_newest_window_and_survive_without_a_window() {
    if !support::in_subprocess("pins_count_inside_the_newest_window_and_survive_without_a_window") {
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    let old = snapshots.save(SaveOptions::default()).unwrap();
    let newest = snapshots.save(SaveOptions::default()).unwrap();
    let oid = repo.git(&["rev-parse", &format!("refs/groove/snapshots/{}", newest.id)]);
    repo.git(&[
        "update-ref",
        &format!("refs/groove/snapshot-pins/{}", newest.id),
        &oid,
    ]);
    let result = snapshots.prune(options(true, 1)).unwrap();
    assert_eq!(result.deleted, [old.id]);
    assert_eq!(result.retained[0].id, newest.id);
    assert_eq!(result.retained[0].reason, PruneRetentionReason::Pinned);
    assert!(snapshots
        .prune(options(true, 0))
        .unwrap()
        .deleted
        .is_empty());
    assert!(snapshots.show(&newest.id).unwrap().pinned);
}

#[test]
fn inconsistent_protection_paths_fail_before_deleting_any_snapshot() {
    if !support::in_subprocess("inconsistent_protection_paths_fail_before_deleting_any_snapshot") {
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    let saved = snapshots.save(Default::default()).unwrap();
    groove_core::Pool::init(
        &repo.path,
        groove_core::PoolOptions {
            capacity: 1,
            worktree_root: Some(repo.temp.path().join("pool")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = groove_core::Pool::open(&repo.path).unwrap();
    let reserved = pool.reserve(Default::default()).unwrap();
    let result_file = reserved.worktree_path.join("precious.txt");
    fs::write(&result_file, b"preserve reserved work\n").unwrap();
    let path = repo
        .state()
        .join("slots")
        .join(format!("{}.protected.json", reserved.slot_id));
    let original = fs::read(&path).unwrap();
    let mut protection: serde_json::Value = serde_json::from_slice(&original).unwrap();
    protection["lease"]["worktree_path"] = serde_json::json!(repo.temp.path().join("wrong-source"));
    fs::write(&path, serde_json::to_vec(&protection).unwrap()).unwrap();
    let error = snapshots.prune(options(true, 0)).unwrap_err();
    assert_eq!(error.code, groove_core::ErrorCode::InvalidArgument);
    assert_eq!(snapshots.show(&saved.id).unwrap().id, saved.id);
    assert_eq!(fs::read(&result_file).unwrap(), b"preserve reserved work\n");
    assert_eq!(
        fs::read(&path).unwrap(),
        serde_json::to_vec(&protection).unwrap()
    );
    // A dangling protection symlink is malformed state, not an absent reservation.
    fs::remove_file(&path).unwrap();
    let missing = repo.temp.path().join("missing-protection");
    std::os::unix::fs::symlink(&missing, &path).unwrap();
    let error = snapshots.prune(options(true, 0)).unwrap_err();
    assert_eq!(error.code, groove_core::ErrorCode::InvalidArgument);
    assert_eq!(snapshots.show(&saved.id).unwrap().id, saved.id);
    assert_eq!(fs::read_link(&path).unwrap(), missing);
    assert_eq!(fs::read(&result_file).unwrap(), b"preserve reserved work\n");
    fs::remove_file(&path).unwrap();
    fs::write(&path, original).unwrap();
    pool.release_lease(reserved.lease_id).unwrap();
}
