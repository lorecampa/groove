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
fn ambiguous_task_labels_require_a_lease_id() {
    let repo = Repo::new();
    let pool = pool(&repo, 2);
    for _ in 0..2 {
        pool.reserve(task("same")).unwrap();
    }
    assert_eq!(
        pool.lease_by_task("same").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        pool.lease_by_task("missing").unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
}
