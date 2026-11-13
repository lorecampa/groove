mod support;

use groove_core::{ErrorCode, Pool, PoolOptions, PoolUpdate};
use std::{fs, time::Duration};
use support::Repo;

#[test]
fn targeted_inspection_handles_growth_missing_workspaces_and_git_failures() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let first = "wt-0001".parse().unwrap();
    let second = "wt-0002".parse().unwrap();
    let timeout = Duration::from_secs(5);
    assert!(pool.inspect_slot(&first).unwrap().git.is_none());
    assert!(pool
        .inspect_slot_detailed(&first, timeout)
        .unwrap()
        .git
        .is_none());
    assert_eq!(
        pool.inspect_slot(&second).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        pool.inspect_slot_detailed(&first, Duration::ZERO)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );

    Pool::open(&r.path)
        .unwrap()
        .configure(PoolUpdate {
            capacity: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pool.inspect_slot(&second).unwrap().slot_id, second);

    let lease = pool.reserve(Default::default()).unwrap();
    fs::write(lease.worktree_path.join("tracked.txt"), "edited\n").unwrap();
    fs::write(lease.worktree_path.join("new.txt"), "new\n").unwrap();
    let facts = pool
        .inspect_slot_detailed(&lease.slot_id, timeout)
        .unwrap()
        .git
        .unwrap();
    assert!(facts.modified && facts.untracked);
    assert!(!facts.staged);
    assert!(facts.error.is_none());
    assert!(pool.inspect_slot(&lease.slot_id).unwrap().git.is_none());

    // A missing registration must be an explicit error, never a clean checkout.
    fs::remove_file(lease.worktree_path.join(".git")).unwrap();
    assert!(pool
        .inspect_slot_detailed(&lease.slot_id, timeout)
        .unwrap()
        .git
        .unwrap()
        .error
        .is_some());
}
