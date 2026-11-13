mod support;
use groove_core::{AcquireOptions, CleanupPolicy, Pool, PoolOptions, RecoveryOutcome};
use std::{
    fs,
    os::unix::{
        ffi::OsStringExt,
        fs::{symlink, MetadataExt},
    },
};
use support::{git, Repo};

fn pool(r: &Repo, policy: CleanupPolicy) -> Pool {
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            cleanup_policy: policy,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&r.path).unwrap()
}
#[test]
fn resets_staged_unstaged_ignored_nested_and_preserves_refs() {
    let r = Repo::new();
    let p = pool(&r, CleanupPolicy::Pristine);
    let lease = p.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    git(&path, &["branch", "agent-result"]);
    fs::write(path.join("tracked.txt"), "staged").unwrap();
    git(&path, &["add", "."]);
    fs::write(path.join("tracked.txt"), "unstaged").unwrap();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "cache").unwrap();
    fs::create_dir(path.join("nested")).unwrap();
    git(&path.join("nested"), &["init", "-q"]);
    fs::write(
        path.join(std::ffi::OsString::from_vec(if cfg!(target_os = "macos") {
            // APFS requires valid UTF-8 names; still exercise spaces, Unicode
            // and a newline without dropping the cleanup/recovery assertions.
            b"untracked \xc3\xa8\n".to_vec()
        } else {
            b"untracked\xff\n".to_vec()
        })),
        "bytes",
    )
    .unwrap();
    drop(lease);
    let next = p.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert!(!path.join("ignored").exists());
    assert!(!path.join("nested").exists());
    assert!(git(&path, &["status", "--porcelain", "--ignored"]).is_empty());
    assert_eq!(
        r.git(&["rev-parse", "agent-result"]),
        next.info().base_oid.as_str()
    );
    assert_eq!(
        fs::read_to_string(r.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}
#[test]
fn warm_preserves_ignored_but_resets_tracked() {
    let r = Repo::new();
    let p = pool(&r, CleanupPolicy::Warm);
    let l = p.acquire(Default::default()).unwrap();
    let path = l.info().worktree_path.clone();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "warm").unwrap();
    fs::write(path.join("untracked"), "remove").unwrap();
    fs::write(path.join("tracked.txt"), "dirty").unwrap();
    drop(l);
    let _next = p.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("ignored/cache")).unwrap(),
        "warm"
    );
    assert!(!path.join("untracked").exists());
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}
#[test]
fn reuse_replaces_hidden_same_size_edits() {
    for policy in [CleanupPolicy::Warm, CleanupPolicy::Pristine] {
        let r = Repo::new();
        let p = pool(&r, policy);
        let lease = p.acquire(Default::default()).unwrap();
        let path = lease.info().worktree_path.clone();
        let file = path.join("tracked.txt");
        let original = fs::metadata(&file).unwrap();
        git(
            &path,
            &["update-index", "--assume-unchanged", "tracked.txt"],
        );
        fs::write(&file, "tampered\n").unwrap();
        fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(original.modified().unwrap()))
            .unwrap();
        drop(lease);
        let _lease = p.acquire(Default::default()).unwrap();
        assert_eq!(fs::read(&file).unwrap(), b"baseline\n");
        assert!(git(&path, &["status", "--porcelain"]).is_empty());
    }
}
#[test]
fn commit_switch_reapplies_attributes_to_unchanged_blobs() {
    let r = Repo::new();
    fs::write(r.path.join(".gitattributes"), "*.txt text eol=lf\n").unwrap();
    r.git(&["add", ".gitattributes"]);
    r.git(&["commit", "-qm", "LF checkout"]);
    let old = r.git(&["rev-parse", "HEAD"]);
    fs::write(r.path.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
    r.git(&["add", ".gitattributes"]);
    r.git(&["commit", "-qm", "CRLF checkout"]);
    let p = pool(&r, CleanupPolicy::Warm);
    let lease = p
        .acquire(AcquireOptions {
            base: old.clone(),
            ..Default::default()
        })
        .unwrap();
    let path = lease.info().worktree_path.clone();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\n");
    drop(lease);
    let lease = p.acquire(Default::default()).unwrap();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\r\n");
    drop(lease);
    let _lease = p
        .acquire(AcquireOptions {
            base: old,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\n");
}

#[test]
fn reuse_reapplies_checkout_conversions_from_working_and_committed_attributes() {
    for (original, dirty, expected, tampered) in [
        (
            "*.txt crlf=input\n",
            "*.txt crlf=input\n",
            b"baseline\n".as_slice(),
            b"baseline\r\n".as_slice(),
        ),
        (
            "*.txt -text\n",
            "*.txt text eol=crlf\n",
            b"baseline\n".as_slice(),
            b"baseline\r\n".as_slice(),
        ),
        (
            "*.txt text eol=crlf\n",
            "*.txt -text\n",
            b"baseline\r\n".as_slice(),
            b"baseline\n".as_slice(),
        ),
    ] {
        let r = Repo::new();
        fs::write(r.path.join(".gitattributes"), original).unwrap();
        r.git(&["add", ".gitattributes"]);
        r.git(&["commit", "-qm", "checkout attributes"]);
        let p = pool(&r, CleanupPolicy::Warm);
        let lease = p.acquire(Default::default()).unwrap();
        let path = lease.info().worktree_path.clone();
        assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), expected);
        fs::write(path.join(".gitattributes"), dirty).unwrap();
        fs::write(path.join("tracked.txt"), tampered).unwrap();
        drop(lease);
        let _lease = p.acquire(Default::default()).unwrap();
        assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), expected);
        assert_eq!(
            fs::read_to_string(path.join(".gitattributes")).unwrap(),
            original
        );
    }
}

#[test]
fn reuse_honors_ignored_and_info_attributes_and_repository_checkout_settings() {
    use std::os::unix::fs::PermissionsExt;
    for setting in [
        "ignored-attributes",
        "info-attributes",
        "autocrlf",
        "filemode",
    ] {
        let r = Repo::new();
        if setting == "ignored-attributes" {
            fs::write(r.path.join(".gitignore"), "ignored/\n.gitattributes\n").unwrap();
            r.git(&["add", ".gitignore"]);
            r.git(&["commit", "-qm", "ignore local attributes"]);
        } else if setting == "info-attributes" {
            fs::create_dir_all(r.path.join(".git/info")).unwrap();
            fs::write(r.path.join(".git/info/attributes"), "*.txt text eol=crlf\n").unwrap();
        } else if setting == "autocrlf" {
            r.git(&["config", "core.autocrlf", "true"]);
        } else {
            r.git(&["config", "core.filemode", "false"]);
        }
        let p = pool(&r, CleanupPolicy::Warm);
        let lease = p.acquire(Default::default()).unwrap();
        let path = lease.info().worktree_path.clone();
        let file = path.join("tracked.txt");
        if setting == "ignored-attributes" {
            fs::write(path.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
        }
        if setting == "filemode" {
            fs::set_permissions(&file, fs::Permissions::from_mode(0o755)).unwrap();
        } else {
            fs::write(&file, b"baseline\n").unwrap();
        }
        drop(lease);
        let _lease = p.acquire(Default::default()).unwrap();
        if setting == "filemode" {
            assert_eq!(fs::metadata(file).unwrap().permissions().mode() & 0o111, 0);
        } else {
            assert_eq!(fs::read(file).unwrap(), b"baseline\r\n", "{setting}");
        }
    }
}

#[test]
fn warm_commit_switch_removes_old_tracked_paths_that_become_ignored() {
    let r = Repo::new();
    fs::create_dir(r.path.join("obsolete")).unwrap();
    fs::write(r.path.join("obsolete/file"), "old tracked content\n").unwrap();
    r.git(&["add", "obsolete/file"]);
    r.git(&["commit", "-qm", "old tracked path"]);
    let old = r.git(&["rev-parse", "HEAD"]);
    r.git(&["rm", "obsolete/file"]);
    fs::write(r.path.join(".gitignore"), "ignored/\nobsolete/\n").unwrap();
    r.git(&["add", ".gitignore"]);
    r.git(&["commit", "-qm", "remove and ignore old path"]);
    let p = pool(&r, CleanupPolicy::Warm);
    let lease = p
        .acquire(AcquireOptions {
            base: old.clone(),
            ..Default::default()
        })
        .unwrap();
    let path = lease.info().worktree_path.clone();
    fs::create_dir(path.join("ignored")).unwrap();
    fs::write(path.join("ignored/cache"), "keep cache\n").unwrap();
    drop(lease);
    let lease = p.acquire(Default::default()).unwrap();
    assert!(!path.join("obsolete/file").exists());
    assert_eq!(
        fs::read(path.join("ignored/cache")).unwrap(),
        b"keep cache\n"
    );
    drop(lease);
    let _lease = p
        .acquire(AcquireOptions {
            base: old,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        fs::read(path.join("obsolete/file")).unwrap(),
        b"old tracked content\n"
    );
    assert_eq!(
        fs::read(path.join("ignored/cache")).unwrap(),
        b"keep cache\n"
    );
}
#[test]
fn repairs_index_and_interrupted_operations_without_changing_lock_inode() {
    let r = Repo::new();
    let p = pool(&r, CleanupPolicy::Pristine);
    let l = p.acquire(Default::default()).unwrap();
    let path = l.info().worktree_path.clone();
    let admin = std::path::PathBuf::from(git(&path, &["rev-parse", "--absolute-git-dir"]));
    let lock = r.state().join("locks/wt-0001.lock");
    let ino = fs::metadata(&lock).unwrap().ino();
    fs::write(admin.join("index"), "broken").unwrap();
    fs::create_dir(admin.join("rebase-merge")).unwrap();
    drop(l);
    let next = p.acquire(Default::default()).unwrap();
    assert_eq!(next.info().recovery, RecoveryOutcome::Rebuilt);
    assert_eq!(fs::metadata(lock).unwrap().ino(), ino);
    assert!(git(&path, &["status", "--porcelain"]).is_empty());
}
#[test]
fn removes_assume_unchanged_flags_and_corrects_wrong_commit() {
    let r = Repo::new();
    let p = pool(&r, CleanupPolicy::Pristine);
    let l = p.acquire(Default::default()).unwrap();
    let path = l.info().worktree_path.clone();
    git(
        &path,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    fs::write(path.join("tracked.txt"), "hidden contamination").unwrap();
    drop(l);
    let l = p.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    drop(l);
    fs::write(r.path.join("tracked.txt"), "new base").unwrap();
    r.git(&["commit", "-qam", "new"]);
    let next = p
        .acquire(AcquireOptions {
            base: "HEAD".into(),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(next.info().base_oid.as_str(), r.git(&["rev-parse", "HEAD"]));
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "new base"
    );
}
#[test]
fn symlinked_slot_quarantines_without_touching_primary_or_other_slots() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 2,
            ..Default::default()
        },
    )
    .unwrap();
    symlink(&r.path, r.slot(1)).unwrap();
    let p = Pool::open(&r.path).unwrap();
    let l = p.acquire(Default::default()).unwrap();
    assert_eq!(l.info().slot_id, groove_core::SlotId::new(2).unwrap());
    assert_eq!(
        fs::read_to_string(r.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}
#[test]
fn sha256_and_unsupported_revisions_submodules_and_sparse_checkout() {
    let r = Repo::format("sha256");
    let p = pool(&r, CleanupPolicy::Pristine);
    assert_eq!(
        p.acquire(Default::default())
            .unwrap()
            .info()
            .base_oid
            .as_str()
            .len(),
        64
    );
    assert_eq!(
        p.acquire(AcquireOptions {
            base: "--help".into(),
            ..Default::default()
        })
        .err()
        .unwrap()
        .code,
        groove_core::ErrorCode::InvalidRevision
    );
    r.git(&["config", "core.sparseCheckout", "true"]);
    assert_eq!(
        p.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::UnsupportedConfiguration
    );
    r.git(&["config", "core.sparseCheckout", "false"]);
    let oid = r.git(&["rev-parse", "HEAD"]);
    r.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{oid},submodule"),
    ]);
    r.git(&["commit", "-qm", "gitlink"]);
    assert_eq!(
        p.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::UnsupportedConfiguration
    );
}

#[test]
fn empty_commit_deleted_tracked_file_and_tracked_symlink_are_supported() {
    let r = Repo::new();
    symlink("tracked.txt", r.path.join("tracked-link")).unwrap();
    r.git(&["add", "."]);
    r.git(&["commit", "-qm", "symlink"]);
    let p = pool(&r, CleanupPolicy::Pristine);
    let lease = p.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    fs::remove_file(path.join("tracked.txt")).unwrap();
    drop(lease);
    let lease = p.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read_link(path.join("tracked-link")).unwrap(),
        std::path::PathBuf::from("tracked.txt")
    );
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    drop(lease);
    r.git(&["rm", "-q", "-r", "."]);
    r.git(&["commit", "-qm", "empty tree"]);
    let lease = p.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read_dir(lease.info().worktree_path.clone())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn forged_git_pointer_and_missing_registration_never_touch_primary() {
    let r = Repo::new();
    let pool = pool(&r, CleanupPolicy::Pristine);
    let lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    drop(lease);
    fs::write(
        path.join(".git"),
        format!("gitdir: {}\n", r.path.join(".git").display()),
    )
    .unwrap();
    let next = pool.acquire(Default::default()).unwrap();
    assert_eq!(next.info().recovery, RecoveryOutcome::Rebuilt);
    assert_eq!(
        fs::read_to_string(r.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert!(r.git(&["status", "--porcelain"]).is_empty());
    drop(next);
    fs::remove_dir_all(&path).unwrap();
    assert!(pool.acquire(Default::default()).is_ok());
}
#[test]
fn all_broken_slots_report_bounded_failure_and_later_recover() {
    let r = Repo::new();
    let pool = pool(&r, CleanupPolicy::Pristine);
    symlink(&r.path, r.slot(1)).unwrap();
    let error = pool.acquire(Default::default()).err().unwrap();
    assert_eq!(error.code, groove_core::ErrorCode::PoolUnhealthy);
    assert_eq!(
        error.details.capacity.as_ref().unwrap().failed_slots.len(),
        1
    );
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(r.state().join("slots/wt-0001.json")).unwrap()).unwrap();
    assert_eq!(record["phase"], "quarantined");
    fs::remove_file(r.slot(1)).unwrap();
    assert!(pool.acquire(Default::default()).is_ok());
}

#[test]
fn large_tree_acquires_and_rejects_submodules_beyond_diagnostic_limit() {
    use std::{io::Write, process::Stdio};
    let r = Repo::new();
    let blob = r.git(&["rev-parse", "HEAD:tracked.txt"]);
    let old_commit = r.git(&["rev-parse", "HEAD"]);
    let mut entries = String::new();
    for i in 0..18_000 {
        entries.push_str(&format!("100644 blob {blob}\tfile-{i:05}.txt\n"));
    }
    let write_tree = |entries: &str| {
        let mut child = support::command(&r.path, "git")
            .arg("mktree")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(entries.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    let tree = write_tree(&entries);
    let commit = r.git(&["commit-tree", &tree, "-m", "large tree"]);
    r.git(&["update-ref", "HEAD", &commit]);
    assert!(r.git(&["ls-tree", "-r", "-z", "HEAD"]).len() > 1024 * 1024);
    let pool = pool(&r, CleanupPolicy::Pristine);
    let lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    assert_eq!(fs::read_dir(&path).unwrap().count(), 18_001);
    assert_eq!(
        fs::read_to_string(path.join("file-17999.txt")).unwrap(),
        "baseline\n"
    );
    drop(lease);

    // A gitlink at the end must still be detected after the diagnostic buffer fills.
    entries.push_str(&format!("160000 commit {old_commit}\tzz-submodule\n"));
    let tree = write_tree(&entries);
    let commit = r.git(&["commit-tree", &tree, "-m", "late submodule"]);
    r.git(&["update-ref", "HEAD", &commit]);
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::UnsupportedConfiguration
    );
    assert!(!path.join("zz-submodule").exists());
}
