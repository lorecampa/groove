mod support;

use groove_core::{Pool, PoolOptions};
use std::{
    fs,
    os::unix::fs::{symlink, DirBuilderExt},
};
use support::Repo;

#[test]
fn disk_usage_counts_legacy_files_without_following_record_paths_or_symlinks() {
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
    let before = pool.storage_usage().unwrap().workspace_bytes;
    fs::write(
        lease.worktree_path.join("ignored-payload"),
        b"workspace bytes",
    )
    .unwrap();
    let external = repo.temp.path().join("external");
    fs::write(&external, b"external bytes must not be counted").unwrap();
    let logs = repo.state().join("task-logs");
    fs::DirBuilder::new().mode(0o700).create(&logs).unwrap();
    let run = logs.join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&run).unwrap();
    let metadata = br#"{"stdout_path":"/outside/pool"}"#;
    fs::write(run.join("run.json"), metadata).unwrap();
    symlink(&external, run.join("stdout.log")).unwrap();
    fs::write(run.join("stderr.log"), b"stderr").unwrap();
    let unknown = repo.state().join("task-logs/untracked");
    fs::create_dir(&unknown).unwrap();
    fs::write(unknown.join("data"), b"unknown").unwrap();
    let usage = pool.storage_usage().unwrap();
    assert_eq!(usage.workspace_bytes, before + 15);
    assert_eq!(usage.runs, 1);
    assert_eq!(usage.history_bytes, metadata.len() as u64);
    assert_eq!(usage.log_bytes, 6);
    assert_eq!(usage.untracked_runs, 1);
    assert_eq!(usage.untracked_log_bytes, 7);
    assert_eq!(fs::read(run.join("run.json")).unwrap(), metadata);
    assert_eq!(
        fs::read(&external).unwrap(),
        b"external bytes must not be counted"
    );
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().lease_id,
        lease.lease_id
    );
}
