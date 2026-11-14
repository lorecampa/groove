mod support;
use groove_core::{ErrorCode, Pool, PoolOptions, PoolUpdate, ReadySettings, ReservationOptions};
use std::fs;
use support::Repo;

fn pool(repo: &Repo, capacity: usize, target: usize) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity,
            ready: Some(ReadySettings {
                ready_target: target,
                ..Default::default()
            }),
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}
fn run(pool: &Pool) {
    let generation = pool.request_ready().unwrap().unwrap();
    pool.run_ready(generation).unwrap();
    let report = pool.ready_report().unwrap();
    assert!(report.error.is_none(), "{report:?}");
}
#[test]
fn reaching_the_ready_target_leaves_excess_released_checkouts_untouched() {
    if !support::in_subprocess(
        "reaching_the_ready_target_leaves_excess_released_checkouts_untouched",
    ) {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 4, 1);
    let leases: Vec<_> = (0..4)
        .map(|_| pool.reserve(Default::default()).unwrap())
        .collect();
    for lease in &leases {
        fs::create_dir(lease.worktree_path.join("ignored")).unwrap();
        fs::write(
            lease.worktree_path.join("ignored/cache"),
            "retained until needed",
        )
        .unwrap();
        pool.release_lease(lease.lease_id).unwrap();
    }
    for _ in 0..2 {
        run(&pool);
        assert_eq!(pool.ready_report().unwrap().ready, 1);
        assert_eq!(
            leases
                .iter()
                .filter(|lease| lease.worktree_path.join("ignored/cache").exists())
                .count(),
            3
        );
    }
    assert_eq!(pool.settings().unwrap().capacity, 4);
    for _ in 0..4 {
        pool.reserve(Default::default()).unwrap();
    }
}

#[test]
fn replenishes_without_expiring_reservations_and_preserves_slot_locks() {
    if !support::in_subprocess("replenishes_without_expiring_reservations_and_preserves_slot_locks")
    {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let repo = Repo::new();
    let pool = pool(&repo, 3, 1);
    let settings = pool.settings().unwrap();
    let lock = settings.state_dir.join("locks/wt-0001.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    run(&pool);
    assert_eq!(pool.ready_report().unwrap().ready, 1);
    let lease = pool.reserve(ReservationOptions::default()).unwrap();
    fs::write(lease.worktree_path.join("important.txt"), "keep me").unwrap();
    run(&pool);
    assert_eq!(pool.ready_report().unwrap().ready, 1);
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
    assert_eq!(
        fs::read_to_string(lease.worktree_path.join("important.txt")).unwrap(),
        "keep me"
    );
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
}
#[test]
fn target_validation_is_atomic_and_defaults_do_not_modify_legacy_pools() {
    let repo = Repo::new();
    let pool = pool(&repo, 2, 1);
    let before = fs::read(pool.settings().unwrap().state_dir.join("config.json")).unwrap();
    let error = pool
        .configure(PoolUpdate {
            ready_target: Some(3),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(
        fs::read(pool.settings().unwrap().state_dir.join("config.json")).unwrap(),
        before
    );
    pool.configure(PoolUpdate {
        capacity: Some(3),
        ready_target: Some(3),
        ..Default::default()
    })
    .unwrap();
    run(&pool);
    assert_eq!(pool.ready_report().unwrap().ready, 3);
}
#[test]
fn preserves_post_release_edits_and_prepares_another_slot() {
    if !support::in_subprocess("preserves_post_release_edits_and_prepares_another_slot") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 2, 1);
    let lease = pool.reserve(ReservationOptions::default()).unwrap();
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
    fs::write(lease.worktree_path.join("after-release.txt"), "unsaved").unwrap();
    run(&pool);
    assert!(pool
        .ready_report()
        .unwrap()
        .failures
        .contains_key(&lease.slot_id));
    assert_eq!(
        fs::read_to_string(lease.worktree_path.join("after-release.txt")).unwrap(),
        "unsaved"
    );
    assert_eq!(pool.ready_report().unwrap().ready, 1);
}
#[test]
fn saved_release_can_be_prepared_and_snapshot_remains_recoverable() {
    if !support::in_subprocess("saved_release_can_be_prepared_and_snapshot_remains_recoverable") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    let lease = pool.reserve(ReservationOptions::default()).unwrap();
    fs::write(lease.worktree_path.join("saved.txt"), "saved result").unwrap();
    let finished = pool.finish(lease.lease_id, Default::default()).unwrap();
    run(&pool);
    assert_eq!(
        pool.ready_report().unwrap().ready,
        1,
        "{:?}",
        pool.ready_report().unwrap()
    );
    assert!(!lease.worktree_path.join("saved.txt").exists());
    let snapshots =
        groove_core::snapshot::Snapshots::open(&repo.path, groove_core::DEFAULT_GIT_TIMEOUT)
            .unwrap();
    let destination = repo.path.parent().unwrap().join("recovered");
    snapshots
        .restore(&finished.snapshot.id, Some(&destination))
        .unwrap();
    assert_eq!(
        fs::read_to_string(destination.join("saved.txt")).unwrap(),
        "saved result"
    );
}
#[test]
fn idle_eviction_keeps_target_and_slot_capacity() {
    if !support::in_subprocess("idle_eviction_keeps_target_and_slot_capacity") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 3, 3);
    run(&pool);
    let settings = pool.settings().unwrap();
    for number in 1..=3 {
        let path = settings
            .state_dir
            .join(format!("slots/wt-{number:04}.json"));
        let mut record: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        record["idle_since_unix_ms"] = 1.into();
        fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    }
    pool.configure(PoolUpdate {
        ready_target: Some(1),
        idle_timeout: Some("1s".parse().unwrap()),
        ..Default::default()
    })
    .unwrap();
    run(&pool);
    assert_eq!(
        fs::read_dir(&settings.worktree_root)
            .unwrap()
            .filter(|entry| entry.as_ref().unwrap().file_name() != ".groove-owner")
            .count(),
        1
    );
    assert_eq!(pool.settings().unwrap().capacity, 3);
    let leases: Vec<_> = (0..3)
        .map(|_| pool.reserve(Default::default()).unwrap())
        .collect();
    assert_eq!(leases.len(), 3);
}
#[test]
fn invalid_base_is_rejected_before_changing_settings_or_reservations() {
    let repo = Repo::new();
    let pool = pool(&repo, 2, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    let config = pool.settings().unwrap().state_dir.join("config.json");
    let before = fs::read(&config).unwrap();
    let error = pool
        .configure(PoolUpdate {
            ready_base: Some("missing-reference".into()),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRevision);
    assert_eq!(fs::read(config).unwrap(), before);
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
}

#[test]
fn a_ready_base_deleted_after_configuration_is_reported_without_touching_reservations() {
    let repo = Repo::new();
    let pool = pool(&repo, 2, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    repo.git(&["branch", "ready-base"]);
    pool.configure(PoolUpdate {
        ready_base: Some("ready-base".into()),
        ..Default::default()
    })
    .unwrap();
    repo.git(&["branch", "-D", "ready-base"]);
    let generation = pool.request_ready().unwrap().unwrap();
    pool.run_ready(generation).unwrap();
    assert!(pool.ready_report().unwrap().error.is_some());
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
}
#[test]
fn idle_durations_reject_overflow_zero_and_unicode_without_panicking() {
    for value in ["0s", "-1d", "1", "1w", "é", "💥", "18446744073709551615d"] {
        assert_eq!(
            value.parse::<groove_core::IdleTimeout>().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        "24h"
            .parse::<groove_core::IdleTimeout>()
            .unwrap()
            .to_string(),
        "1d"
    );
    assert_eq!(
        "never"
            .parse::<groove_core::IdleTimeout>()
            .unwrap()
            .to_string(),
        "never"
    );
}

#[test]
fn legacy_pool_inspection_does_not_enable_background_work() {
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
    let before = fs::read(repo.state().join("config.json")).unwrap();
    assert!(pool.settings().unwrap().ready.is_none());
    assert!(pool.request_ready().unwrap().is_none());
    assert_eq!(fs::read(repo.state().join("config.json")).unwrap(), before);
    assert!(!repo.slot(1).exists());
}
#[test]
fn future_idle_times_do_not_authorize_eviction() {
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    run(&pool);
    let path = repo.state().join("slots/wt-0001.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["idle_since_unix_ms"] = u64::MAX.into();
    fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
    pool.configure(PoolUpdate {
        ready_target: Some(0),
        idle_timeout: Some("1s".parse().unwrap()),
        ..Default::default()
    })
    .unwrap();
    run(&pool);
    assert!(repo.slot(1).join("tracked.txt").exists());
}
#[test]
fn foreign_repository_is_preserved_and_reported() {
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    fs::create_dir(repo.slot(1)).unwrap();
    support::git(&repo.slot(1), &["init", "-q"]);
    fs::write(repo.slot(1).join("foreign.txt"), "foreign work").unwrap();
    run(&pool);
    assert_eq!(pool.ready_report().unwrap().failures.len(), 1);
    assert_eq!(
        fs::read_to_string(repo.slot(1).join("foreign.txt")).unwrap(),
        "foreign work"
    );
    assert!(repo.slot(1).join(".git").is_dir());
}
#[test]
fn saved_release_with_later_staging_changes_is_not_discarded() {
    if !support::in_subprocess("saved_release_with_later_staging_changes_is_not_discarded") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    fs::write(lease.worktree_path.join("tracked.txt"), "saved edit").unwrap();
    pool.finish(lease.lease_id, Default::default()).unwrap();
    support::git(&lease.worktree_path, &["add", "tracked.txt"]);
    run(&pool);
    assert_eq!(pool.ready_report().unwrap().ready, 0);
    assert!(pool
        .ready_report()
        .unwrap()
        .failures
        .contains_key(&lease.slot_id));
    assert_eq!(
        support::git(&lease.worktree_path, &["show", ":tracked.txt"]),
        "saved edit"
    );
}
#[test]
fn failed_pass_backoff_does_not_slide_and_policy_change_retries_immediately() {
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    repo.git(&["branch", "missing"]);
    pool.configure(PoolUpdate {
        ready_base: Some("missing".into()),
        ..Default::default()
    })
    .unwrap();
    repo.git(&["branch", "-D", "missing"]);
    let generation = pool.request_ready().unwrap().unwrap();
    pool.run_ready(generation).unwrap();
    let first = pool.ready_report().unwrap();
    pool.request_ready().unwrap();
    pool.run_ready(generation).unwrap();
    assert_eq!(
        pool.ready_report().unwrap().attempted_at_unix_ms,
        first.attempted_at_unix_ms
    );
    pool.configure(PoolUpdate {
        ready_base: Some(repo.git(&["rev-parse", "HEAD"])),
        ..Default::default()
    })
    .unwrap();
    run(&pool);
    assert_eq!(pool.ready_report().unwrap().ready, 1);
}

#[test]
fn hidden_index_changes_and_new_commits_after_release_are_preserved() {
    if !support::in_subprocess("hidden_index_changes_and_new_commits_after_release_are_preserved") {
        return;
    }
    for commit in [false, true] {
        let repo = Repo::new();
        let pool = pool(&repo, 1, 1);
        let lease = pool.reserve(Default::default()).unwrap();
        pool.release_protected(&lease.slot_id, lease.lease_id)
            .unwrap();
        fs::write(lease.worktree_path.join("tracked.txt"), "new work").unwrap();
        if commit {
            support::git(&lease.worktree_path, &["add", "tracked.txt"]);
            support::git(&lease.worktree_path, &["commit", "-qm", "unexpected"]);
        } else {
            support::git(
                &lease.worktree_path,
                &["update-index", "--assume-unchanged", "tracked.txt"],
            );
        }
        run(&pool);
        assert_eq!(pool.ready_report().unwrap().ready, 0);
        assert_eq!(pool.ready_report().unwrap().failures.len(), 1);
        assert_eq!(
            fs::read_to_string(lease.worktree_path.join("tracked.txt")).unwrap(),
            "new work"
        );
    }
}

#[test]
fn ready_worker_preserves_skip_worktree_edits() {
    if !support::in_subprocess("ready_worker_preserves_skip_worktree_edits") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
    fs::write(lease.worktree_path.join("tracked.txt"), "hidden work").unwrap();
    support::git(
        &lease.worktree_path,
        &["update-index", "--skip-worktree", "tracked.txt"],
    );
    run(&pool);
    let report = pool.ready_report().unwrap();
    assert_eq!(report.ready, 0);
    assert_eq!(report.failures.len(), 1);
    assert_eq!(
        fs::read_to_string(lease.worktree_path.join("tracked.txt")).unwrap(),
        "hidden work"
    );
}

#[test]
fn named_pools_keep_independent_targets_and_ready_checkouts() {
    let repo = Repo::new();
    let first = pool(&repo, 2, 1);
    let name = "frontend".parse().unwrap();
    Pool::init_named(
        &repo.path,
        &name,
        PoolOptions {
            capacity: 3,
            ready: Some(ReadySettings {
                ready_target: 2,
                ..Default::default()
            }),
            worktree_root: Some(support::worktree_root(&repo.path, "frontend")),
            ..Default::default()
        },
    )
    .unwrap();
    let second = Pool::open_named(&repo.path, &name).unwrap();
    run(&first);
    run(&second);
    assert_eq!(first.ready_report().unwrap().ready, 1);
    assert_eq!(second.ready_report().unwrap().ready, 2);
    assert_ne!(
        first.settings().unwrap().worktree_root,
        second.settings().unwrap().worktree_root
    );
    first
        .configure(PoolUpdate {
            ready_target: Some(0),
            idle_timeout: Some("never".parse().unwrap()),
            ..Default::default()
        })
        .unwrap();
    assert!(first.request_ready().unwrap().is_none());
    assert_eq!(second.settings().unwrap().ready.unwrap().ready_target, 2);
}

#[test]
fn external_process_in_idle_checkout_blocks_background_mutation() {
    if !support::in_subprocess("external_process_in_idle_checkout_blocks_background_mutation") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    pool.release_protected(&lease.slot_id, lease.lease_id)
        .unwrap();
    // spawn returns after exec, whose cwd is already set to the workspace.
    let mut command = support::command(&lease.worktree_path, "sleep");
    command.arg("30");
    let mut child = support::TestChild::spawn(&mut command);
    run(&pool);
    let report = pool.ready_report().unwrap();
    assert_eq!(report.ready, 0);
    assert!(report.failures.contains_key(&lease.slot_id));
    assert_eq!(
        fs::read_to_string(lease.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}
#[test]
fn stale_workers_cannot_mutate_a_reinitialized_pool() {
    if !support::in_subprocess("stale_workers_cannot_mutate_a_reinitialized_pool") {
        return;
    }
    let repo = Repo::new();
    let old = pool(&repo, 1, 1);
    run(&old);
    let generation = old.request_ready().unwrap().unwrap();
    let inode = {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(repo.state().join("locks/ready.lock"))
            .unwrap()
            .ino()
    };
    Pool::destroy(&repo.path, Default::default()).unwrap();
    let new = pool(&repo, 1, 1);
    assert_eq!(new.ready_report().unwrap().ready, 0);
    let lease = new.reserve(Default::default()).unwrap();
    fs::write(lease.worktree_path.join("new.txt"), "new generation").unwrap();
    assert_eq!(
        old.run_ready(generation).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        new.run_ready(generation).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        fs::read_to_string(lease.worktree_path.join("new.txt")).unwrap(),
        "new generation"
    );
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        fs::metadata(repo.state().join("locks/ready.lock"))
            .unwrap()
            .ino(),
        inode
    );
}

#[test]
fn corrupt_ready_metadata_does_not_block_another_healthy_slot() {
    if !support::in_subprocess("corrupt_ready_metadata_does_not_block_another_healthy_slot") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 2, 1);
    run(&pool);
    let metadata = repo.state().join("slots/wt-0001.ready.json");
    fs::write(&metadata, "{broken").unwrap();
    let work = repo.slot(1).join("precious.txt");
    fs::write(&work, "preserve").unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    assert_eq!(lease.slot_id.number(), 2);
    assert_eq!(fs::read(&work).unwrap(), b"preserve");
    assert_eq!(fs::read(metadata).unwrap(), b"{broken");
    let error = pool.acquire(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::PoolExhausted);
    assert_eq!(error.details.capacity.unwrap().failed_slots.len(), 1);
}

#[test]
fn unsupported_worker_metadata_is_never_overwritten_or_used_to_discard_files() {
    let repo = Repo::new();
    let pool = pool(&repo, 1, 1);
    run(&pool);
    let work_path = repo.state().join("slots/wt-0001.ready.json");
    let mut work: serde_json::Value =
        serde_json::from_slice(&fs::read(&work_path).unwrap()).unwrap();
    work["schema_version"] = 999.into();
    let bytes = serde_json::to_vec(&work).unwrap();
    fs::write(&work_path, &bytes).unwrap();
    fs::write(repo.slot(1).join("preserved.txt"), "keep").unwrap();
    assert_eq!(
        pool.acquire(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert_eq!(fs::read(&work_path).unwrap(), bytes);
    assert_eq!(
        fs::read_to_string(repo.slot(1).join("preserved.txt")).unwrap(),
        "keep"
    );
    let report_path = repo.state().join("ready-report.json");
    let mut report: serde_json::Value =
        serde_json::from_slice(&fs::read(&report_path).unwrap()).unwrap();
    report["schema_version"] = 999.into();
    let bytes = serde_json::to_vec(&report).unwrap();
    fs::write(&report_path, &bytes).unwrap();
    let generation = pool.request_ready().unwrap().unwrap();
    assert_eq!(
        pool.run_ready(generation).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert_eq!(fs::read(report_path).unwrap(), bytes);
}
