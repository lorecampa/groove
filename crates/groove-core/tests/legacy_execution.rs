mod support;

use groove_core::{
    snapshot::SaveOptions, DestroyOptions, ErrorCode, Pool, PoolOptions, SlotStatus,
};
use std::fs;
use support::Repo;

#[test]
fn legacy_execution_records_preserve_results_and_reservations() {
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
    let result = lease.worktree_path.join("tracked.txt");
    fs::write(&result, b"keep task result").unwrap();
    // Even an unreadable old record is protection, never permission to reuse.
    let record = repo
        .state()
        .join(format!("slots/{}.execution.json", lease.slot_id));
    fs::write(&record, b"legacy record").unwrap();
    assert_eq!(
        pool.release_lease(lease.lease_id).unwrap_err().code,
        ErrorCode::InvalidLease
    );
    assert_eq!(
        pool.finish(lease.lease_id, SaveOptions::default())
            .unwrap_err()
            .code,
        ErrorCode::InvalidLease
    );
    assert_eq!(
        Pool::destroy(
            &repo.path,
            DestroyOptions {
                discard_changes: true,
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        ErrorCode::ConfigConflict
    );
    assert_eq!(
        pool.inspect_slot(&lease.slot_id).unwrap().status,
        SlotStatus::Unhealthy
    );
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().lease_id,
        lease.lease_id
    );
    assert_eq!(fs::read(&record).unwrap(), b"legacy record");
    assert_eq!(fs::read(&result).unwrap(), b"keep task result");
    assert_eq!(
        pool.acquire(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
}
