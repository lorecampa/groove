mod support;
use groove_core::{ErrorCode, LeaseId, Pool, PoolOptions};
use support::Repo;

// These cases assume only their own explicit lease owners exist. A concurrent
// test's fork temporarily inherits every CLOEXEC descriptor until exec, correctly
// keeping another fixture's flock alive after its Rust handle is dropped. Isolate
// these fixtures; explicit cross-process inheritance is covered by concurrency
// and CLI git_child tests, which intentionally retain and verify those owners.
static PROCESS_FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn pool(repo: &Repo) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}

#[test]
fn protection_survives_drop_and_only_matching_explicit_release_clears_it() {
    let _fixture = PROCESS_FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo);
    let mut lease = pool.acquire(Default::default()).unwrap();
    lease.protect_from_reuse().unwrap();
    let info = lease.info().clone();
    std::fs::write(
        info.worktree_path.join("tracked.txt"),
        "preserve until release",
    )
    .unwrap();
    assert_eq!(
        pool.release_protected(&info.slot_id, info.lease_id)
            .unwrap_err()
            .code,
        ErrorCode::InvalidLease
    );
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, info.lease_id);
    // Ordinary unprotected API behavior remains unchanged; explicit release clears a hold.
    lease.release().unwrap();
    assert!(pool.protected_leases().unwrap().is_empty());
    let next = pool.acquire(Default::default()).unwrap();
    next.protect_from_reuse().unwrap();
    let next_info = next.info().clone();
    std::fs::write(
        next_info.worktree_path.join("tracked.txt"),
        "abandoned result",
    )
    .unwrap();
    drop(next);
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    assert_eq!(
        std::fs::read_to_string(next_info.worktree_path.join("tracked.txt")).unwrap(),
        "abandoned result"
    );
    assert_eq!(
        pool.release_protected(&next_info.slot_id, LeaseId::generate())
            .unwrap_err()
            .code,
        ErrorCode::LeaseMismatch
    );
    pool.release_protected(&next_info.slot_id, next_info.lease_id)
        .unwrap();
    let successor = pool.acquire(Default::default()).unwrap();
    assert_ne!(successor.info().lease_id, next_info.lease_id);
    assert_eq!(
        std::fs::read_to_string(next_info.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn malformed_protection_blocks_reuse_and_failed_release_retains_ownership() {
    let _fixture = PROCESS_FIXTURE.lock().unwrap();
    let repo = Repo::new();
    let pool = pool(&repo);
    let mut lease = pool.acquire(Default::default()).unwrap();
    lease.protect_from_reuse().unwrap();
    let info = lease.info().clone();
    let path = repo
        .state()
        .join(format!("slots/{}.protected.json", info.slot_id));
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(&path, "broken").unwrap();
    assert_eq!(
        pool.protected_leases().unwrap_err().code,
        ErrorCode::ProtocolError
    );
    assert!(lease.release().is_err());
    assert!(lease.is_active());
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolExhausted
    );
    drop(lease);
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        ErrorCode::PoolUnhealthy
    );
    std::fs::write(&path, saved).unwrap();
    pool.release_protected(&info.slot_id, info.lease_id)
        .unwrap();
    assert!(pool.acquire(Default::default()).is_ok());
}
