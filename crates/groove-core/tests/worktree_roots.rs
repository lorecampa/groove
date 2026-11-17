mod support;

use groove_core::{ErrorCode, Pool, PoolOptions};
use std::fs;
use support::Repo;

#[test]
fn external_defaults_separate_clones_and_pools_but_agree_from_linked_callers() {
    let first = Repo::new();
    let second = Repo::new();
    let storage = support::tempdir();
    let options = PoolOptions {
        capacity: 1,
        worktree_base: Some(storage.path().to_owned()),
        ..Default::default()
    };
    let a = Pool::init(&first.path, options.clone()).unwrap();
    let b = Pool::init(&second.path, options.clone()).unwrap();
    assert_ne!(
        a.worktree_root, b.worktree_root,
        "same-name clones must not share slots"
    );
    assert!(a.worktree_root.starts_with(storage.path()));
    assert!(!a.worktree_root.starts_with(&first.path));
    let linked = first.temp.path().join("linked");
    first.git(&["worktree", "add", "--detach", linked.to_str().unwrap()]);
    let named = Pool::init_named(&linked, &"frontend".parse().unwrap(), options).unwrap();
    assert_eq!(a.worktree_root.parent(), named.worktree_root.parent());
    assert_eq!(named.worktree_root.file_name().unwrap(), "frontend");
    assert_eq!(a.worktree_root.file_name().unwrap(), "default");
    let pool = Pool::open(&linked).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    assert!(lease.info().worktree_path.starts_with(&a.worktree_root));
    assert_eq!(
        fs::read_to_string(lease.info().worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn an_exact_root_overrides_the_base_and_relative_bases_are_rejected() {
    let repo = Repo::new();
    let invalid = Pool::init(
        &repo.path,
        PoolOptions {
            worktree_base: Some("relative".into()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(invalid.code, ErrorCode::InvalidArgument);
    let exact = repo.temp.path().join("chosen-exact-root");
    let result = Pool::init(
        &repo.path,
        PoolOptions {
            worktree_root: Some(exact.clone()),
            worktree_base: Some("ignored-relative-base".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.worktree_root, exact);
}
