mod support;
use groove_core::{AcquireOptions, ErrorCode, Pool, PoolOptions, ReservationOptions, SlotStatus};
use std::{
    fs,
    sync::{Arc, Barrier, Mutex},
    time::{Duration, Instant},
};
use support::Repo;
static FIXTURE: Mutex<()> = Mutex::new(());

fn pool(repo: &Repo, capacity: usize) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}
fn options(key: &str) -> ReservationOptions {
    ReservationOptions {
        request_key: Some(key.into()),
        task: Some("review task".into()),
        ..Default::default()
    }
}

#[test]
fn durable_occupancy_is_retryable_and_wait_rescans_after_release() {
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let first = pool.reserve(options("first")).unwrap();
    let error = pool.acquire(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::PoolExhausted);
    assert!(error.retryable);
    let error = pool
        .acquire(AcquireOptions {
            wait: Duration::from_millis(100),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AcquireTimeout);
    assert!(error.retryable);
    let releaser = pool.clone();
    let id = first.lease_id;
    let worker = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        releaser.release_lease(id).unwrap();
    });
    let start = Instant::now();
    let second = pool
        .acquire(AcquireOptions {
            wait: Duration::from_secs(5),
            ..Default::default()
        })
        .unwrap();
    assert!(start.elapsed() >= Duration::from_millis(100));
    assert_eq!(second.info().slot_id, first.slot_id);
    assert_ne!(second.info().lease_id, first.lease_id);
    worker.join().unwrap();
}

#[test]
fn task_keys_survive_restart_reject_conflicts_and_never_reassign_completed_requests() {
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let first = pool.reserve(options("operation-1")).unwrap();
    fs::write(first.worktree_path.join("tracked.txt"), "saved task state").unwrap();
    let restarted = Pool::open(&repo.path).unwrap();
    let again = restarted.reserve(options("operation-1")).unwrap();
    assert_eq!(first.lease_id, again.lease_id);
    assert_eq!(
        fs::read_to_string(first.worktree_path.join("tracked.txt")).unwrap(),
        "saved task state"
    );
    assert_eq!(again.task.as_deref(), Some("review task"));
    assert!(again.acquired_at_unix_ms.is_some());
    let mut conflict = options("operation-1");
    conflict.acquire.base = "HEAD~0".into();
    assert_eq!(
        restarted.reserve(conflict).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    restarted.release_lease(first.lease_id).unwrap();
    restarted.release_lease(first.lease_id).unwrap();
    let second = restarted.reserve(options("operation-2")).unwrap();
    assert_ne!(first.lease_id, second.lease_id);
    assert_eq!(
        restarted.release_lease(first.lease_id).unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
    assert_eq!(
        restarted.reserve(options("operation-1")).unwrap_err().code,
        ErrorCode::InvalidLease
    );
    assert_eq!(
        restarted.protected_leases().unwrap()[0].lease_id,
        second.lease_id
    );
    restarted.release_lease(second.lease_id).unwrap();
}

#[test]
fn concurrent_retries_allocate_exactly_one_reservation() {
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 4);
    let barrier = Arc::new(Barrier::new(5));
    let workers: Vec<_> = (0..5)
        .map(|_| {
            let pool = pool.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                pool.reserve(options("one-logical-task")).unwrap().lease_id
            })
        })
        .collect();
    let ids: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
    assert_eq!(pool.protected_leases().unwrap().len(), 1);
    pool.release_lease(ids[0]).unwrap();
}

#[test]
fn inspection_includes_free_reserved_locked_and_quarantined_slots() {
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 3);
    let reserved = pool.reserve(options("task")).unwrap();
    let active = pool.acquire(Default::default()).unwrap();
    let report = pool.inspect().unwrap();
    assert_eq!(report.capacity, 3);
    assert_eq!(report.slots.len(), 3);
    assert!(report
        .slots
        .iter()
        .any(|s| s.status == SlotStatus::Available));
    assert!(report.slots.iter().any(|s| s.status == SlotStatus::Reserved
        && s.lease.as_ref().unwrap().lease_id == reserved.lease_id));
    assert!(report.slots.iter().any(|s| s.status == SlotStatus::Locked
        && s.lock_held
        && s.slot_id == active.info().slot_id));
    let free = report
        .slots
        .iter()
        .find(|s| s.status == SlotStatus::Available)
        .unwrap();
    fs::write(
        repo.state().join(format!("slots/{}.json", free.slot_id)),
        r#"{"schema_version":1,"phase":"quarantined","lease":null,"failure":"fixture failure"}"#,
    )
    .unwrap();
    let report = pool.inspect().unwrap();
    let failed = report
        .slots
        .iter()
        .find(|s| s.slot_id == free.slot_id)
        .unwrap();
    assert_eq!(failed.status, SlotStatus::Quarantined);
    assert_eq!(failed.failure.as_deref(), Some("fixture failure"));
    drop(active);
    pool.release_lease(reserved.lease_id).unwrap();
}

#[test]
fn finish_pins_results_survives_reuse_and_is_idempotent() {
    if !support::in_subprocess("finish_pins_results_survives_reuse_and_is_idempotent") {
        return;
    }
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let first = pool.reserve(options("first")).unwrap();
    fs::write(
        first.worktree_path.join("tracked.txt"),
        "results to preserve",
    )
    .unwrap();
    let result = pool.finish(first.lease_id, Default::default()).unwrap();
    assert!(result.snapshot.pinned);
    assert!(pool.protected_leases().unwrap().is_empty());
    let next = pool.reserve(options("next")).unwrap();
    let again = pool.finish(first.lease_id, Default::default()).unwrap();
    assert_eq!(result.snapshot.id, again.snapshot.id);
    assert_eq!(again.release.slot_id, first.slot_id);
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, next.lease_id);
    let snapshots =
        groove_core::snapshot::Snapshots::open(&repo.path, Duration::from_secs(30)).unwrap();
    let recovered = repo.temp.path().join("recovered");
    snapshots
        .restore(&result.snapshot.id, Some(&recovered))
        .unwrap();
    assert_eq!(
        fs::read_to_string(recovered.join("tracked.txt")).unwrap(),
        "results to preserve"
    );
    pool.release_lease(next.lease_id).unwrap();
}

#[test]
fn failed_checkpoint_keeps_results_and_reservation_protected() {
    if !support::in_subprocess("failed_checkpoint_keeps_results_and_reservation_protected") {
        return;
    }
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let info = pool.reserve(options("task")).unwrap();
    fs::write(
        info.worktree_path.join("tracked.txt"),
        "larger than the capture limit",
    )
    .unwrap();
    assert_eq!(
        pool.finish(
            info.lease_id,
            groove_core::snapshot::SaveOptions {
                max_file_bytes: 1,
                ..Default::default()
            }
        )
        .unwrap_err()
        .code,
        groove_core::ErrorCode::InvalidArgument
    );
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, info.lease_id);
    assert_eq!(
        pool.acquire(Default::default()).unwrap_err().code,
        groove_core::ErrorCode::PoolExhausted
    );
    assert_eq!(
        fs::read_to_string(info.worktree_path.join("tracked.txt")).unwrap(),
        "larger than the capture limit"
    );
    pool.release_lease(info.lease_id).unwrap();
}

#[test]
fn unrelated_corrupt_slot_does_not_prevent_releasing_a_valid_reservation() {
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 2);
    let info = pool.reserve(options("task")).unwrap();
    let other = pool
        .inspect()
        .unwrap()
        .slots
        .into_iter()
        .find(|slot| slot.slot_id != info.slot_id)
        .unwrap();
    fs::write(
        repo.state().join(format!("slots/{}.json", other.slot_id)),
        "invalid state",
    )
    .unwrap();
    assert!(pool
        .inspect()
        .unwrap()
        .slots
        .into_iter()
        .any(|slot| slot.status == SlotStatus::Unhealthy));
    assert!(pool.release_lease(info.lease_id).unwrap().released);
}

#[test]
fn manifest_keeps_one_schema_and_stable_execution_lock_identities() {
    let _fixture = FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let read_config = || {
        serde_json::from_slice::<serde_json::Value>(
            &fs::read(repo.state().join("config.json")).unwrap(),
        )
        .unwrap()
    };
    let initial = read_config();
    assert_eq!(initial["schema_version"], 1);
    let identity = initial["execution_locks"]["wt-0001"].clone();
    assert!(identity.is_object());
    let mut lease = pool.acquire(Default::default()).unwrap();
    lease.protect_from_reuse().unwrap();
    lease.release().unwrap();
    let next = pool.reserve(options("next")).unwrap();
    pool.release_lease(next.lease_id).unwrap();
    assert_eq!(read_config(), initial);
}
