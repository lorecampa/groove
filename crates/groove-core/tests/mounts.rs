#![cfg(target_os = "linux")]
mod support;

use groove_core::{DestroyOptions, ErrorCode, Pool, PoolOptions};
use std::{fs, os::unix::fs::MetadataExt, path::Path, process::Command};

#[test]
#[ignore = "requires Linux user/mount namespaces and mount/umount utilities"]
fn bind_mounts_preserve_external_files_during_reuse_rebuild_and_destroy() {
    const CHILD: &str = "GROOVE_BIND_MOUNT_FIXTURE";
    if let Some(path) = std::env::var_os(CHILD) {
        exercise(Path::new(&path));
        return;
    }
    // The parent owns fixture cleanup. The child's mount namespace is gone
    // before TempDir can recurse, including when an assertion panics.
    let repo = support::Repo::new();
    let output = support::TestChild::spawn(
        Command::new("unshare")
            .args([
                "--user",
                "--map-root-user",
                "--mount",
                "--propagation",
                "private",
            ])
            .arg(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "bind_mounts_preserve_external_files_during_reuse_rebuild_and_destroy",
                "--nocapture",
            ])
            .env(CHILD, &repo.path),
    )
    .wait_with_output()
    .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn exercise(repo: &Path) {
    Pool::init(
        repo,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(repo, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(repo).unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    pool.release_lease(lease.lease_id).unwrap();
    let source = repo.parent().unwrap().join("external");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("precious"), "outside pool").unwrap();
    let target = lease.worktree_path.join("ignored/bind");
    fs::create_dir_all(&target).unwrap();
    assert!(Command::new("mount")
        .arg("--bind")
        .arg(&source)
        .arg(&target)
        .status()
        .unwrap()
        .success());
    assert_eq!(
        fs::metadata(&source).unwrap().dev(),
        fs::metadata(&target).unwrap().dev()
    );
    for damaged in [false, true] {
        if damaged {
            fs::remove_file(lease.worktree_path.join(".git")).unwrap();
        }
        assert_eq!(
            pool.acquire(Default::default()).unwrap_err().code,
            ErrorCode::PoolUnhealthy
        );
        assert_eq!(fs::read(source.join("precious")).unwrap(), b"outside pool");
        assert_eq!(
            Pool::destroy(
                repo,
                DestroyOptions {
                    discard_changes: true,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::ConfigConflict
        );
        assert_eq!(fs::read(source.join("precious")).unwrap(), b"outside pool");
    }
    assert!(Command::new("umount")
        .arg(&target)
        .status()
        .unwrap()
        .success());
    let next = pool.acquire(Default::default()).unwrap();
    assert_ne!(next.info().lease_id, lease.lease_id);
    assert_eq!(fs::read(source.join("precious")).unwrap(), b"outside pool");
}
