mod support;
use groove_core::{CleanupPolicy, Pool, PoolOptions};
use std::{
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{symlink, MetadataExt},
    },
};
use support::Repo;

#[test]
fn init_idempotent_conflicts_and_permanent_inodes() {
    let r = Repo::new();
    let options = PoolOptions {
        capacity: 2,
        ..Default::default()
    };
    assert!(Pool::init(&r.path, options.clone()).unwrap().created);
    assert!(!r.slot(1).exists());
    let lock = r.state().join("locks/wt-0001.lock");
    let ino = fs::metadata(&lock).unwrap().ino();
    assert_eq!(
        Pool::init(&r.path, options).unwrap_err().code,
        groove_core::ErrorCode::ConfigConflict
    );
    assert_eq!(fs::metadata(lock).unwrap().ino(), ino);
    assert_eq!(
        Pool::init(&r.path, PoolOptions::default())
            .unwrap_err()
            .code,
        groove_core::ErrorCode::ConfigConflict
    );
    let linked = r.temp.path().join("linked");
    r.git(&["worktree", "add", "--detach", linked.to_str().unwrap()]);
    assert_eq!(Pool::open(linked).unwrap().capacity(), 2);
}
#[test]
fn bare_repo_and_external_root() {
    let r = Repo::new();
    let bare = r.temp.path().join("bare.git");
    r.git(&[
        "clone",
        "--bare",
        r.path.to_str().unwrap(),
        bare.to_str().unwrap(),
    ]);
    let root = r.temp.path().join("external");
    Pool::init(
        &bare,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(root.clone()),
            cleanup_policy: CleanupPolicy::Warm,
            ..Default::default()
        },
    )
    .unwrap();
    let p = Pool::open(&bare).unwrap();
    let l = p.acquire(Default::default()).unwrap();
    assert!(l.info().worktree_path.starts_with(root));
}
#[test]
fn rejects_symlink_unknown_schema_and_missing_locks() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let lock = r.state().join("locks/wt-0001.lock");
    fs::remove_file(&lock).unwrap();
    symlink(r.path.join("tracked.txt"), &lock).unwrap();
    let p = Pool::open(&r.path).unwrap();
    assert!(p.acquire(Default::default()).is_err());
    assert_eq!(
        fs::read_to_string(r.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    let config = r.state().join("config.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
    value["schema_version"] = 999.into();
    fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        Pool::open(&r.path).unwrap_err().code,
        groove_core::ErrorCode::UnsupportedConfiguration
    );
}
#[test]
fn rejects_non_utf8_managed_roots() {
    let r = Repo::new();
    let root = r
        .temp
        .path()
        .join(std::ffi::OsString::from_vec(b"bad\xff".to_vec()));
    let error = Pool::init(
        &r.path,
        PoolOptions {
            worktree_root: Some(root),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, groove_core::ErrorCode::UnsupportedConfiguration);
}

#[test]
fn invalid_root_does_not_poison_initialization_and_unknown_slot_version_is_preserved() {
    let r = Repo::new();
    assert!(Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(r.path.join(".git")),
            ..Default::default()
        }
    )
    .is_err());
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let record = r.state().join("slots/wt-0001.json");
    fs::write(&record, "{\"schema_version\":2,\"future\":true}").unwrap();
    let pool = Pool::open(&r.path).unwrap();
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::UnsupportedConfiguration
    );
    assert_eq!(
        fs::read_to_string(record).unwrap(),
        "{\"schema_version\":2,\"future\":true}"
    );
}
