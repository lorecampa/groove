mod support;
use groove_core::{
    snapshot::{SaveOptions, Snapshots},
    CleanupPolicy, DestroyOptions, ErrorCode, ExecOptions, HistoryFilter, Pool, PoolName,
    PoolOptions, PoolPhase, PoolUpdate, ReservationOptions, DEFAULT_GIT_TIMEOUT,
};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt},
};
use support::Repo;

fn name(value: &str) -> PoolName {
    value.parse().unwrap()
}
fn create(repo: &Repo, value: &str) -> Pool {
    let name = name(value);
    Pool::init_named(
        &repo.path,
        &name,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open_named(&repo.path, &name).unwrap()
}
fn options() -> ReservationOptions {
    ReservationOptions {
        task: Some("same-task".into()),
        request_key: Some("same-key".into()),
        ..Default::default()
    }
}
fn remove(repo: &Repo, value: &str) -> groove_core::DestroyResult {
    Pool::destroy_named(
        &repo.path,
        &name(value),
        DestroyOptions {
            discard_changes: true,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn pools_isolate_capacity_labels_request_keys_settings_and_history() {
    if !support::in_subprocess("pools_isolate_capacity_labels_request_keys_settings_and_history") {
        return;
    }
    let repo = Repo::new();
    let front = create(&repo, "frontend");
    let back = create(&repo, "backend");
    let default = create(&repo, "default");
    let f = front.reserve(options()).unwrap();
    let b = back.reserve(options()).unwrap();
    let d = default.reserve(options()).unwrap();
    assert_eq!(f.slot_id, b.slot_id);
    assert_eq!(b.slot_id, d.slot_id);
    assert_ne!(f.worktree_path, b.worktree_path);
    assert_ne!(b.worktree_path, d.worktree_path);
    assert_eq!(front.reserve(options()).unwrap().lease_id, f.lease_id);
    assert_eq!(back.reserve(options()).unwrap().lease_id, b.lease_id);
    assert_eq!(
        back.acquire(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
    assert_eq!(
        front.release_lease(b.lease_id).unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
    front
        .configure(PoolUpdate {
            capacity: Some(2),
            cleanup_policy: Some(CleanupPolicy::Warm),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(back.settings().unwrap().capacity, 1);
    assert_eq!(
        back.settings().unwrap().cleanup_policy,
        CleanupPolicy::Pristine
    );
    for (p, id, expected) in [
        (&front, f.lease_id, "frontend"),
        (&back, b.lease_id, "backend"),
        (&default, d.lease_id, "default"),
    ] {
        p.exec(
            id,
            ExecOptions {
                command: vec![
                    "sh".into(),
                    "-c".into(),
                    "printf %s \"$GROOVE_POOL\"".into(),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        let history = p.history(&HistoryFilter::default()).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(
            fs::read_to_string(&history[0].stdout_path).unwrap(),
            expected
        );
    }
    front.release_lease(f.lease_id).unwrap();
    assert_eq!(
        front.reserve(options()).unwrap_err().code,
        ErrorCode::InvalidLease
    );
    assert_eq!(back.reserve(options()).unwrap().lease_id, b.lease_id);
    remove(&repo, "frontend");
    assert_eq!(
        back.lease_by_task("same-task").unwrap().lease_id,
        b.lease_id
    );
    assert_eq!(
        default.lease_by_task("same-task").unwrap().lease_id,
        d.lease_id
    );
    let retained =
        Pool::open_retained_named(&repo.path, &name("frontend"), DEFAULT_GIT_TIMEOUT).unwrap();
    retained
        .clean_logs(std::time::Duration::ZERO, false)
        .unwrap();
    let history = back.history(&HistoryFilter::default()).unwrap();
    assert_eq!(
        fs::read_to_string(&history[0].stdout_path).unwrap(),
        "backend"
    );
    assert_eq!(back.settings().unwrap().pool_name, name("backend"));
}

#[test]
fn ambiguous_destruction_and_wrong_generations_preserve_every_pool() {
    if !support::in_subprocess("ambiguous_destruction_and_wrong_generations_preserve_every_pool") {
        return;
    }
    let repo = Repo::new();
    let default = create(&repo, "default");
    let front = create(&repo, "frontend");
    let back = create(&repo, "backend");
    drop(default.acquire(Default::default()).unwrap());
    let f = front.reserve(options()).unwrap();
    let b = back.reserve(options()).unwrap();
    fs::write(b.worktree_path.join("result"), "backend result").unwrap();
    assert_eq!(
        Pool::destroy(&repo.path, Default::default())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert!(repo.slot(1).exists());
    assert_eq!(
        front.lease_by_task("same-task").unwrap().lease_id,
        f.lease_id
    );
    let old_default = remove(&repo, "default");
    assert_eq!(
        Pool::destroy_named(
            &repo.path,
            &name("backend"),
            DestroyOptions {
                pool_id: old_default.pool_id,
                discard_changes: true,
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ConfigConflict
    );
    front.release_lease(f.lease_id).unwrap();
    remove(&repo, "frontend");
    assert_eq!(
        fs::read_to_string(b.worktree_path.join("result")).unwrap(),
        "backend result"
    );
    assert_eq!(back.reserve(options()).unwrap().lease_id, b.lease_id);
    let catalog = Pool::list_pools(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    assert_eq!(
        catalog
            .iter()
            .map(|p| (p.name.as_str(), p.phase))
            .collect::<Vec<_>>(),
        vec![
            ("backend", PoolPhase::Ready),
            ("default", PoolPhase::Deleted),
            ("frontend", PoolPhase::Deleted)
        ]
    );
}

#[test]
fn checkout_root_claims_include_deleted_pools_and_reject_symlink_aliases() {
    if !support::in_subprocess(
        "checkout_root_claims_include_deleted_pools_and_reject_symlink_aliases",
    ) {
        return;
    }
    let repo = Repo::new();
    let area = repo.temp.path().join("external");
    let root = area.join("front-checkouts");
    let front = name("frontend");
    let back = name("backend");
    Pool::init_named(
        &repo.path,
        &front,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(root.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    for candidate in [root.clone(), root.join("nested"), area] {
        let error = Pool::init_named(
            &repo.path,
            &back,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(candidate),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::ConfigConflict);
        assert_eq!(Pool::open_named(&repo.path, &front).unwrap().capacity(), 1);
    }
    let alias = repo.temp.path().join("alias");
    symlink(&root, &alias).unwrap();
    assert_eq!(
        Pool::init_named(
            &repo.path,
            &back,
            PoolOptions {
                worktree_root: Some(alias),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::UnsupportedConfiguration
    );
    remove(&repo, "frontend");
    assert_eq!(
        Pool::init_named(
            &repo.path,
            &back,
            PoolOptions {
                worktree_root: Some(root.clone()),
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ConfigConflict
    );
    Pool::init_named(
        &repo.path,
        &front,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(repo.temp.path().join("new-front")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::init_named(
        &repo.path,
        &back,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(root),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(Pool::open_named(&repo.path, &back)
        .unwrap()
        .acquire(Default::default())
        .is_ok());
}

#[test]
fn named_snapshots_share_results_and_reject_stale_handles_and_all_pool_destinations() {
    if !support::in_subprocess(
        "named_snapshots_share_results_and_reject_stale_handles_and_all_pool_destinations",
    ) {
        return;
    }
    let repo = Repo::new();
    let front = create(&repo, "frontend");
    let back = create(&repo, "backend");
    let f = front.acquire(Default::default()).unwrap();
    let snapshots = Snapshots::open(&f.info().worktree_path, DEFAULT_GIT_TIMEOUT).unwrap();
    let saved = snapshots.save(Default::default()).unwrap();
    drop(f);
    remove(&repo, "frontend");
    let replacement = create(&repo, "frontend");
    let f = replacement.reserve(Default::default()).unwrap();
    let b = back.reserve(Default::default()).unwrap();
    fs::write(b.worktree_path.join("tracked.txt"), "keep backend").unwrap();
    assert_eq!(
        snapshots.restore(&saved.id, None).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    let shared = Snapshots::open(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    assert_eq!(shared.show(&saved.id).unwrap().id, saved.id);
    for pool in [&back, &replacement] {
        let destination = pool.settings().unwrap().worktree_root.join("recovery");
        assert_eq!(
            shared
                .restore(&saved.id, Some(&destination))
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert!(!destination.exists());
    }
    let finished = replacement
        .finish(f.lease_id, SaveOptions::default())
        .unwrap();
    assert!(shared.show(&finished.snapshot.id).unwrap().pinned);
    assert_eq!(
        fs::read_to_string(b.worktree_path.join("tracked.txt")).unwrap(),
        "keep backend"
    );
}

#[test]
fn caches_are_pool_specific_even_with_identical_compatibility_keys() {
    if !support::in_subprocess("caches_are_pool_specific_even_with_identical_compatibility_keys") {
        return;
    }
    let repo = Repo::new();
    fs::write(repo.path.join(".groove.toml"), "schema_version=1\n[caches.deps]\npaths=['ignored/deps']\ninputs=['tracked.txt']\nkey='same-toolchain'\n[setup.dev]\ncache='deps'\n[[setup.dev.steps]]\nname='seed'\ncommand=['sh','-c','mkdir -p ignored/deps; printf %s \"$GROOVE_POOL\" > ignored/deps/value']\nwhen='cache_miss'\n").unwrap();
    repo.git(&["add", ".groove.toml"]);
    repo.git(&["commit", "-qm", "pool cache fixture"]);
    let front = create(&repo, "frontend");
    let back = create(&repo, "backend");
    let mut key = None;
    for (pool, expected) in [(&front, "frontend"), (&back, "backend")] {
        let first = pool.reserve(Default::default()).unwrap();
        let setup = pool
            .prepare(first.lease_id, "dev", Default::default())
            .unwrap();
        assert_eq!(setup.status, groove_core::PreparationStatus::Ready);
        assert!(!setup.cache_hit);
        if let Some(key) = &key {
            assert_eq!(setup.cache_key.as_ref(), Some(key));
        } else {
            key = setup.cache_key;
        }
        assert_eq!(
            fs::read_to_string(first.worktree_path.join("ignored/deps/value")).unwrap(),
            expected
        );
        pool.release_lease(first.lease_id).unwrap();
        let next = pool.reserve(Default::default()).unwrap();
        assert!(
            pool.prepare(next.lease_id, "dev", Default::default())
                .unwrap()
                .cache_hit
        );
        assert_eq!(
            fs::read_to_string(next.worktree_path.join("ignored/deps/value")).unwrap(),
            expected
        );
        pool.release_lease(next.lease_id).unwrap();
    }
    remove(&repo, "frontend");
    let b = back.reserve(Default::default()).unwrap();
    assert!(
        back.prepare(b.lease_id, "dev", Default::default())
            .unwrap()
            .cache_hit
    );
    assert_eq!(
        fs::read_to_string(b.worktree_path.join("ignored/deps/value")).unwrap(),
        "backend"
    );
}

#[test]
fn legacy_default_layout_and_linked_worktree_discovery_keep_their_identity() {
    if !support::in_subprocess(
        "legacy_default_layout_and_linked_worktree_discovery_keep_their_identity",
    ) {
        return;
    }
    let repo = Repo::new();
    let default = create(&repo, "default");
    let d = default.reserve(options()).unwrap();
    let path = repo.state().join("locks/wt-0001.lock");
    let inode = fs::metadata(&path).unwrap().ino();
    // Simulate the earlier ready format: no lifecycle record, same permanent locks.
    fs::remove_file(repo.state().join("lifecycle.json")).unwrap();
    let default = Pool::open(&repo.path).unwrap();
    let front = create(&repo, "frontend");
    assert_eq!(fs::metadata(path).unwrap().ino(), inode);
    assert_eq!(default.reserve(options()).unwrap().lease_id, d.lease_id);
    let linked = repo.temp.path().join("linked");
    repo.git(&["worktree", "add", "--detach", linked.to_str().unwrap()]);
    fs::write(linked.join("tracked.txt"), "linked head").unwrap();
    support::git(&linked, &["commit", "-qam", "linked commit"]);
    let from_linked = Pool::open_named(&linked, &name("frontend")).unwrap();
    let lease = from_linked.reserve(options()).unwrap();
    assert_eq!(
        lease.base_oid.as_str(),
        support::git(&linked, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        front.lease_by_task("same-task").unwrap().lease_id,
        lease.lease_id
    );
    assert_eq!(
        Pool::list_pools(&linked, DEFAULT_GIT_TIMEOUT)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn a_missing_repository_admin_lock_is_never_recreated_by_another_pool() {
    if !support::in_subprocess("a_missing_repository_admin_lock_is_never_recreated_by_another_pool")
    {
        return;
    }
    let repo = Repo::new();
    let front = create(&repo, "frontend");
    let f = front.reserve(options()).unwrap();
    let path = repo.state().join("locks/admin.lock");
    fs::remove_file(&path).unwrap();
    assert_eq!(
        Pool::init_named(&repo.path, &name("backend"), Default::default())
            .unwrap_err()
            .code,
        ErrorCode::IoError
    );
    assert!(!path.exists());
    assert!(f.worktree_path.exists());
    let protection = front.settings().unwrap_err();
    assert_eq!(protection.code, ErrorCode::IoError);
    assert_eq!(
        fs::read_to_string(f.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn recovery_resolves_parent_aliases_and_still_refuses_pool_roots() {
    if !support::in_subprocess("recovery_resolves_parent_aliases_and_still_refuses_pool_roots") {
        return;
    }
    let repo = Repo::new();
    let snapshots = Snapshots::open(&repo.path, DEFAULT_GIT_TIMEOUT).unwrap();
    let saved = snapshots.save(Default::default()).unwrap();
    let area = repo.temp.path().join("recovery-area");
    fs::create_dir(&area).unwrap();
    let alias = repo.temp.path().join("recovery-alias");
    symlink(&area, &alias).unwrap();
    let restored = snapshots
        .restore(&saved.id, Some(&alias.join("result")))
        .unwrap();
    assert_eq!(
        restored.path,
        fs::canonicalize(area.join("result")).unwrap()
    );
    assert_eq!(
        fs::read_to_string(restored.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    let front = create(&repo, "frontend");
    let root = front.settings().unwrap().worktree_root;
    let alias = repo.temp.path().join("pool-alias");
    symlink(&root, &alias).unwrap();
    assert_eq!(
        snapshots
            .restore(&saved.id, Some(&alias.join("result")))
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert!(!root.join("result").exists());
}

#[test]
fn root_spellings_cannot_alias_another_pools_directory() {
    if !support::in_subprocess("root_spellings_cannot_alias_another_pools_directory") {
        return;
    }
    let repo = Repo::new();
    let upper = repo.temp.path().join("External");
    Pool::init_named(
        &repo.path,
        &name("frontend"),
        PoolOptions {
            capacity: 1,
            worktree_root: Some(upper.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let lower = repo.temp.path().join("external");
    let aliases = lower.exists();
    let result = Pool::init_named(
        &repo.path,
        &name("backend"),
        PoolOptions {
            capacity: 1,
            worktree_root: Some(lower.clone()),
            ..Default::default()
        },
    );
    if aliases {
        assert_eq!(result.unwrap_err().code, ErrorCode::ConfigConflict);
    } else {
        assert_eq!(result.unwrap().worktree_root, lower);
        let back = Pool::open_named(&repo.path, &name("backend")).unwrap();
        let lease = back.reserve(Default::default()).unwrap();
        fs::write(lease.worktree_path.join("result"), "distinct root").unwrap();
        remove(&repo, "frontend");
        assert_eq!(
            fs::read_to_string(lease.worktree_path.join("result")).unwrap(),
            "distinct root"
        );
    }
}

#[test]
fn missing_published_metadata_never_relinquishes_an_existing_root_claim() {
    if !support::in_subprocess(
        "missing_published_metadata_never_relinquishes_an_existing_root_claim",
    ) {
        return;
    }
    let repo = Repo::new();
    let front = create(&repo, "frontend");
    let root = front.settings().unwrap().worktree_root;
    let f = front.reserve(options()).unwrap();
    fs::write(f.worktree_path.join("result"), "keep result").unwrap();
    let manifest = repo.state().join("pools/frontend/config.json");
    let original = fs::read(&manifest).unwrap();
    fs::remove_file(&manifest).unwrap();
    let other = Pool::init_named(
        &repo.path,
        &name("backend"),
        PoolOptions {
            worktree_root: Some(root),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(other.code, ErrorCode::ConfigConflict);
    assert_eq!(
        Pool::init_named(&repo.path, &name("frontend"), Default::default())
            .unwrap_err()
            .code,
        ErrorCode::ConfigConflict
    );
    assert!(!repo
        .state()
        .join("pools/backend/initializing.json")
        .exists());
    assert_eq!(
        fs::read_to_string(f.worktree_path.join("result")).unwrap(),
        "keep result"
    );
    fs::write(manifest, original).unwrap();
    assert_eq!(
        Pool::open_named(&repo.path, &name("frontend"))
            .unwrap()
            .lease_by_task("same-task")
            .unwrap()
            .lease_id,
        f.lease_id
    );
}
