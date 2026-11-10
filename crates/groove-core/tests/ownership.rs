mod support;
use groove_core::{ErrorCode, Pool, PoolOptions};
use support::Repo;

fn single_slot_pool(r: &Repo) -> Pool {
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&r.path).unwrap()
}

#[test]
fn exhaustion_drop_release_and_stale_generation() {
    if !support::in_subprocess("exhaustion_drop_release_and_stale_generation") {
        return;
    }
    let r = Repo::new();
    let p = single_slot_pool(&r);
    let mut lease = p.acquire(Default::default()).unwrap();
    let info = lease.info().clone();
    assert_eq!(
        p.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    // A live in-process owner cannot be released through the durable path.
    assert_eq!(
        p.release_protected(&info.slot_id, info.lease_id)
            .unwrap_err()
            .code,
        ErrorCode::InvalidLease
    );
    lease.release().unwrap();
    assert!(!lease.is_active());
    assert!(lease.release().is_err());
    let mut next = p.acquire(Default::default()).unwrap();
    assert_ne!(info.lease_id, next.info().lease_id);
    next.release().unwrap();
    drop(p.acquire(Default::default()).unwrap());
    assert!(p.acquire(Default::default()).is_ok());
}

#[test]
fn stale_reservation_ids_never_release_a_successor() {
    if !support::in_subprocess("stale_reservation_ids_never_release_a_successor") {
        return;
    }
    let r = Repo::new();
    let p = single_slot_pool(&r);
    let first = p.reserve(Default::default()).unwrap();
    p.release_lease(first.lease_id).unwrap();
    // Retrying the same release succeeds while its generation is current.
    p.release_lease(first.lease_id).unwrap();
    let second = p.reserve(Default::default()).unwrap();
    let result = second.worktree_path.join("tracked.txt");
    std::fs::write(&result, "successor result").unwrap();
    assert_eq!(
        p.release_lease(first.lease_id).unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
    assert_eq!(p.protected_leases().unwrap().len(), 1);
    assert_eq!(p.protected_leases().unwrap()[0].lease_id, second.lease_id);
    assert_eq!(std::fs::read(&result).unwrap(), b"successor result");
    assert_eq!(
        p.release_lease(groove_core::LeaseId::generate())
            .unwrap_err()
            .code,
        ErrorCode::LeaseMismatch
    );
    assert_eq!(std::fs::read(result).unwrap(), b"successor result");
    p.release_lease(second.lease_id).unwrap();
}

#[test]
fn unwinding_closes_lease_and_failed_release_retains_it() {
    if !support::in_subprocess("unwinding_closes_lease_and_failed_release_retains_it") {
        return;
    }
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let result = std::panic::catch_unwind(|| {
        let _lease = pool.acquire(Default::default()).unwrap();
        panic!("simulated task failure");
    });
    assert!(result.is_err());
    let mut lease = pool.acquire(Default::default()).unwrap();
    let record = r.state().join("slots/wt-0001.json");
    let saved = std::fs::read(&record).unwrap();
    std::fs::write(&record, "malformed").unwrap();
    assert!(lease.release().is_err());
    assert!(lease.is_active());
    assert!(pool.acquire(Default::default()).is_err());
    std::fs::write(record, saved).unwrap();
    lease.release().unwrap();
}
