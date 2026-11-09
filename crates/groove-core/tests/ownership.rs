mod support;
use groove_core::{Pool, PoolOptions};
use std::{fs::OpenOptions, os::fd::AsFd};
use support::Repo;

#[test]
fn exhaustion_drop_release_and_stale_generation() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let p = Pool::open(&r.path).unwrap();
    let lease = p.acquire(Default::default()).unwrap();
    let info = lease.info().clone();
    let (info2, fd) = lease.into_descriptor().unwrap();
    assert_eq!(info.lease_id, info2.lease_id);
    assert_eq!(
        p.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::PoolExhausted
    );
    let independent = OpenOptions::new()
        .read(true)
        .write(true)
        .open(r.state().join("locks/wt-0001.lock"))
        .unwrap();
    assert_eq!(
        p.release_fd(&info.slot_id, info.lease_id, independent.as_fd())
            .unwrap_err()
            .code,
        groove_core::ErrorCode::InvalidLease
    );
    let wrong = groove_core::LeaseId::generate();
    assert_eq!(
        p.release_fd(&info.slot_id, wrong, fd.as_fd())
            .unwrap_err()
            .code,
        groove_core::ErrorCode::LeaseMismatch
    );
    assert!(p.acquire(Default::default()).is_err());
    p.release_fd(&info.slot_id, info.lease_id, fd.as_fd())
        .unwrap();
    let mut next = p.acquire(Default::default()).unwrap();
    assert_ne!(info.lease_id, next.info().lease_id);
    assert_eq!(
        p.release_fd(&info.slot_id, info.lease_id, fd.as_fd())
            .unwrap_err()
            .code,
        groove_core::ErrorCode::LeaseMismatch
    );
    assert!(p.acquire(Default::default()).is_err());
    next.release().unwrap();
    assert!(!next.is_active());
    assert!(next.release().is_err());
    drop(p.acquire(Default::default()).unwrap());
    assert!(p.acquire(Default::default()).is_ok());
}
#[test]
fn duplicate_keeps_lock_until_last_close() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let p = Pool::open(&r.path).unwrap();
    let (_, fd) = p
        .acquire(Default::default())
        .unwrap()
        .into_descriptor()
        .unwrap();
    let duplicate = fd.try_clone().unwrap();
    drop(fd);
    assert!(p.acquire(Default::default()).is_err());
    drop(duplicate);
    assert!(p.acquire(Default::default()).is_ok());
}

#[test]
fn unwinding_closes_lease_and_failed_release_retains_it() {
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
