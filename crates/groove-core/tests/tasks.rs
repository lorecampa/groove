mod support;
use groove_core::{ErrorCode, HeadMode, Pool, PoolOptions, ReservationOptions};
use support::{git, Repo};

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

fn task(label: &str) -> ReservationOptions {
    ReservationOptions {
        task: Some(label.into()),
        ..Default::default()
    }
}

#[test]
fn reservations_are_detached_and_found_by_task_label() {
    let repo = Repo::new();
    let pool = pool(&repo, 2);
    let lease = pool.reserve(task("fix parser")).unwrap();
    assert_eq!(lease.head_mode, HeadMode::Detached);
    assert_eq!(
        git(&lease.worktree_path, &["rev-parse", "HEAD"]),
        lease.base_oid.as_str()
    );
    // No branch is created or checked out.
    assert_eq!(
        git(
            &repo.path,
            &["branch", "--list", "--format=%(refname:short)"]
        ),
        git(&repo.path, &["branch", "--show-current"])
    );
    assert_eq!(
        pool.lease_by_task("fix parser").unwrap().lease_id,
        lease.lease_id
    );
    pool.release_lease(lease.lease_id).unwrap();
    assert_eq!(
        pool.lease_by_task("fix parser").unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
}

#[test]
fn an_active_task_label_cannot_be_reserved_twice() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("an_active_task_label_cannot_be_reserved_twice") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 3);
    let first = pool.reserve(task("same")).unwrap();
    // An agent that retries without a request key must not get a second slot.
    let error = pool.reserve(task("same")).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // The rejection names the existing reservation so the caller can resume it.
    assert_eq!(error.details.lease_id, Some(first.lease_id));
    assert_eq!(
        error.details.worktree_path.as_deref(),
        Some(first.worktree_path.as_path())
    );
    assert_eq!(pool.protected_leases().unwrap().len(), 1);
    assert_eq!(pool.lease_by_task("same").unwrap().lease_id, first.lease_id);
    // Other labels are unaffected, and a released label can be used again.
    pool.reserve(task("other")).unwrap();
    pool.release_lease(first.lease_id).unwrap();
    let again = pool.reserve(task("same")).unwrap();
    assert_ne!(again.lease_id, first.lease_id);
    assert_eq!(pool.lease_by_task("same").unwrap().lease_id, again.lease_id);
}

#[test]
fn concurrent_reservations_of_one_label_admit_exactly_one() {
    let repo = Repo::new();
    let pool = pool(&repo, 4);
    let barrier = std::sync::Barrier::new(4);
    let results: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    pool.reserve(task("race"))
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let admitted = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(admitted, 1, "{results:?}");
    for error in results.iter().filter_map(|r| r.as_ref().err()) {
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{error:?}");
    }
    assert_eq!(pool.protected_leases().unwrap().len(), 1);
}

#[test]
fn labels_with_control_characters_are_rejected_before_allocation() {
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    for label in ["\u{1b}]0;title\u{7}", "two\nlines", "nul\0byte"] {
        let error = pool.reserve(task(label)).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{label:?}");
    }
    let error = pool
        .reserve(ReservationOptions {
            request_key: Some("key\r".into()),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    // Nothing was allocated: the only slot is still free.
    assert!(pool.protected_leases().unwrap().is_empty());
    pool.reserve(task("plain label, unicode è")).unwrap();
}

#[test]
fn ambiguous_legacy_task_labels_require_a_lease_id() {
    let repo = Repo::new();
    let pool = pool(&repo, 2);
    let first = pool.reserve(task("same")).unwrap();
    let second = pool.reserve(task("different")).unwrap();
    // Pools written before labels were unique may already hold duplicates.
    let protection = repo
        .state()
        .join(format!("slots/{}.protected.json", second.slot_id));
    let mut record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&protection).unwrap()).unwrap();
    record["lease"]["task"] = "same".into();
    std::fs::write(&protection, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(
        pool.lease_by_task("same").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        pool.lease_by_task("missing").unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
    // Lease IDs stay unambiguous.
    assert_eq!(
        pool.lease_info(first.lease_id).unwrap().slot_id,
        first.slot_id
    );
}
