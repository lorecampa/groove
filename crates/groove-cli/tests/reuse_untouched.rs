mod support;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use support::Repo;

fn cli(repo: &Repo, args: &[&str]) -> Value {
    let output = support::groove_in(&repo.path).args(args).output().unwrap();
    support::assert_success(&output)["result"].clone()
}
fn init(repo: &Repo) {
    cli(
        repo,
        &[
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    );
}
fn acquire(repo: &Repo) -> Value {
    cli(repo, &["acquire"])
}
fn release(repo: &Repo, lease: &Value) {
    cli(
        repo,
        &[
            "release",
            "--force",
            "--lease-id",
            lease["lease_id"].as_str().unwrap(),
        ],
    );
}
fn tree(root: &Path) -> BTreeMap<PathBuf, (u32, u64, Vec<u8>)> {
    fn walk(root: &Path, path: &Path, output: &mut BTreeMap<PathBuf, (u32, u64, Vec<u8>)>) {
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                walk(root, &path, output);
            } else {
                output.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (
                        metadata.mode() & 0o7777,
                        metadata.nlink(),
                        fs::read(&path).unwrap(),
                    ),
                );
            }
        }
    }
    let mut output = BTreeMap::new();
    walk(root, root, &mut output);
    output
}

#[test]
#[cfg(target_os = "linux")]
fn verification_markers_on_another_filesystem_force_a_full_rewrite() {
    if !Path::new("/dev/shm").is_dir() {
        eprintln!("cross-filesystem regression requires /dev/shm");
        return;
    }
    let repo = Repo::new();
    let root = tempfile::tempdir_in("/dev/shm").unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    if fs::metadata(root.path()).unwrap().dev() == fs::metadata(&repo.path).unwrap().dev() {
        eprintln!("cross-filesystem regression requires distinct devices");
        return;
    }
    cli(
        &repo,
        &[
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
            "--worktree-root",
            root.path().to_str().unwrap(),
        ],
    );
    let lease = acquire(&repo);
    let file = repo.slot(1).join("tracked.txt");
    let before = fs::metadata(&file).unwrap();
    release(&repo, &lease);
    acquire(&repo);
    let after = fs::metadata(&file).unwrap();
    assert_ne!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(fs::read(file).unwrap(), b"baseline\n");
}

#[test]
fn untouched_files_keep_their_inode_and_mtime_across_reservations() {
    let repo = Repo::new();
    init(&repo);
    let lease = acquire(&repo);
    let file = repo.slot(1).join("tracked.txt");
    let before = fs::metadata(&file).unwrap();
    release(&repo, &lease);
    acquire(&repo);
    let after = fs::metadata(file).unwrap();
    assert_eq!(
        (after.ino(), after.mtime(), after.mtime_nsec()),
        (before.ino(), before.mtime(), before.mtime_nsec())
    );
}

fn set_attribute(path: &Path) {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new("user.groove-test").unwrap();
    let value = b"must be removed by checkout";
    #[cfg(target_os = "linux")]
    // SAFETY: the path/name are NUL-terminated and value covers its stated length.
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: the path/name are NUL-terminated and value covers its stated length.
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    assert_eq!(result, 0, "{}", std::io::Error::last_os_error());
}

fn assert_attribute_removed(path: &Path) {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    #[cfg(target_os = "linux")]
    // SAFETY: path/key are terminated strings; a null buffer queries the size.
    let result = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"user.groove-test".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: path/key are terminated strings; a null buffer queries the size.
    let result = unsafe {
        libc::getxattr(
            path.as_ptr(),
            c"user.groove-test".as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        )
    };
    assert_eq!(result, -1);
    #[cfg(target_os = "linux")]
    let absent = libc::ENODATA;
    #[cfg(target_os = "macos")]
    let absent = libc::ENOATTR;
    assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(absent));
}

fn contaminate(root: &Path, case: &str, outside: &Path) {
    let file = root.join("tracked.txt");
    match case {
        "content" => fs::write(file, b"edited\n").unwrap(),
        "chmod" => fs::set_permissions(file, fs::Permissions::from_mode(0o777)).unwrap(),
        "xattr" => set_attribute(&file),
        "hardlink" => {
            fs::hard_link(&file, outside).unwrap();
        }
        "utimes" => fs::File::open(file)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1234)),
            )
            .unwrap(),
        "directory-swap" => {
            fs::rename(root.join("one"), root.join("swap")).unwrap();
            fs::rename(root.join("two"), root.join("one")).unwrap();
            fs::rename(root.join("swap"), root.join("two")).unwrap();
        }
        "staged" => {
            fs::write(file, b"staged edit\n").unwrap();
            support::git(root, &["add", "tracked.txt"]);
        }
        "removed-index" => {
            support::git(root, &["rm", "--cached", "tracked.txt"]);
        }
        "assume-unchanged" => {
            fs::write(file, b"hidden edit\n").unwrap();
            support::git(root, &["update-index", "--assume-unchanged", "tracked.txt"]);
        }
        _ => unreachable!(),
    }
}

#[test]
fn metadata_content_index_and_directory_changes_match_a_full_rewrite() {
    for case in [
        "content",
        "chmod",
        "xattr",
        "hardlink",
        "utimes",
        "directory-swap",
        "staged",
        "removed-index",
        "assume-unchanged",
    ] {
        let repo = Repo::new();
        for (dir, bytes) in [("one", b"one\n".as_slice()), ("two", b"two\n".as_slice())] {
            fs::create_dir(repo.path.join(dir)).unwrap();
            fs::write(repo.path.join(dir).join("file.txt"), bytes).unwrap();
        }
        support::git(&repo.path, &["add", "."]);
        support::git(&repo.path, &["commit", "-qm", "directory fixtures"]);
        let oracle = repo.temp.path().join("oracle");
        support::git(
            &repo.path,
            &[
                "worktree",
                "add",
                "--detach",
                oracle.to_str().unwrap(),
                "HEAD",
            ],
        );
        init(&repo);
        let lease = acquire(&repo);
        let workspace = repo.slot(1);
        contaminate(
            &workspace,
            case,
            &repo.temp.path().join("outside-workspace"),
        );
        contaminate(&oracle, case, &repo.temp.path().join("outside-oracle"));
        release(&repo, &lease);
        let index = support::git(&oracle, &["rev-parse", "--git-path", "index"]);
        fs::remove_file(index).unwrap();
        support::git(&oracle, &["checkout", "--detach", "--force", "HEAD", "--"]);
        support::git(&oracle, &["clean", "-ffdx", "--"]);
        acquire(&repo);
        assert_eq!(tree(&workspace), tree(&oracle), "{case}");
        assert_eq!(
            support::git(&workspace, &["write-tree"]),
            support::git(&oracle, &["write-tree"]),
            "{case}"
        );
        if case == "xattr" {
            assert_attribute_removed(&workspace.join("tracked.txt"));
        }
    }
}

#[test]
fn changing_attribute_rules_forces_the_full_rewrite_path() {
    let repo = Repo::new();
    fs::write(repo.path.join("converted.x"), b"line\n").unwrap();
    support::git(&repo.path, &["add", "."]);
    support::git(&repo.path, &["commit", "-qm", "before attributes"]);
    init(&repo);
    let lease = acquire(&repo);
    let before = fs::metadata(repo.slot(1).join("tracked.txt")).unwrap();
    release(&repo, &lease);
    fs::write(repo.path.join(".gitattributes"), b"*.x text eol=crlf\n").unwrap();
    support::git(&repo.path, &["add", ".gitattributes"]);
    support::git(&repo.path, &["commit", "-qm", "new conversion"]);
    let revision = support::git(&repo.path, &["rev-parse", "HEAD"]);
    cli(&repo, &["acquire", "--base", &revision]);
    assert_eq!(
        fs::read(repo.slot(1).join("converted.x")).unwrap(),
        b"line\r\n"
    );
    assert_ne!(
        fs::metadata(repo.slot(1).join("tracked.txt"))
            .unwrap()
            .ino(),
        before.ino()
    );
}

#[cfg(feature = "test-hooks")]
#[test]
fn interruption_after_marker_removal_forces_a_full_rewrite() {
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    init(&repo);
    let lease = acquire(&repo);
    release(&repo, &lease);
    let path = repo.slot(1).join("tracked.txt");
    let before = fs::metadata(&path).unwrap();
    let socket = repo.temp.path().join("reuse.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = support::TestChild::spawn(
        support::groove_in(&repo.path)
            .arg("acquire")
            .env("GROOVE_TEST_CHECKPOINT", "reuse-marker-removed")
            .env("GROOVE_TEST_SOCKET", socket),
    );
    let stream = child
        .checkpoint(&listener, "reuse-marker-removed", support::PROCESS_TIMEOUT)
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    acquire(&repo);
    assert_ne!(fs::metadata(path).unwrap().ino(), before.ino());
}

#[test]
fn legacy_and_interrupted_checkouts_cannot_seed_dirty_directory_metadata() {
    for legacy in [true, false] {
        let repo = Repo::new();
        init(&repo);
        let lease = acquire(&repo);
        let workspace = repo.slot(1);
        let expected = fs::metadata(&workspace).unwrap().mode() & 0o7777;
        let admin = PathBuf::from(support::git(
            &workspace,
            &["rev-parse", "--absolute-git-dir"],
        ));
        release(&repo, &lease);
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o777)).unwrap();
        if legacy {
            fs::remove_file(admin.join("groove-directories.json")).unwrap();
            fs::write(admin.join("groove-verified"), b"").unwrap();
        } else {
            fs::remove_file(admin.join("groove-verified")).unwrap();
        }
        acquire(&repo);
        assert_eq!(fs::metadata(&workspace).unwrap().mode() & 0o7777, expected);
        assert_eq!(
            fs::read(workspace.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
    }
}
