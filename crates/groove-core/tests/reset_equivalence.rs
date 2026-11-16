mod support;
use groove_core::{CleanupPolicy, Pool, PoolOptions};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};
use support::{git, Repo};

// Compare observable checkout state without asking Git to normalize the bytes.
type PhysicalTree = BTreeMap<PathBuf, (u32, u64, Vec<u8>)>;
fn physical_tree(root: &Path) -> PhysicalTree {
    fn visit(root: &Path, dir: &Path, out: &mut PhysicalTree) {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == ".git" {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(root).unwrap().to_owned();
            let metadata = fs::symlink_metadata(&path).unwrap();
            let value = if metadata.is_symlink() {
                use std::os::unix::ffi::OsStrExt;
                (
                    0o120000,
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec(),
                )
            } else if metadata.is_dir() {
                visit(root, &path, out);
                (0o040000, Vec::new())
            } else {
                (
                    metadata.permissions().mode() & 0o7777,
                    fs::read(&path).unwrap(),
                )
            };
            out.insert(relative, (value.0, metadata.nlink(), value.1));
        }
    }
    let mut tree = BTreeMap::new();
    visit(root, root, &mut tree);
    tree
}

#[test]
fn reused_checkout_matches_full_rewrite_for_configuration_and_path_contamination() {
    for (key, value, attrs) in [
        ("core.ignorestat", "true", ""),
        ("core.trustctime", "false", ""),
        ("core.checkstat", "minimal", ""),
        ("core.filemode", "false", ""),
        ("core.symlinks", "false", ""),
        ("core.autocrlf", "true", ""),
        ("core.autocrlf", "input", ""),
        ("core.eol", "crlf", ""),
        ("core.splitindex", "true", ""),
        (
            "filter.fixture.clean",
            "sed s/SMUDGED/CLEAN/g",
            "*.txt filter=fixture\n",
        ),
        ("core.ignorestat", "false", "*.txt ident\n"),
        ("core.ignorestat", "false", "*.txt text eol=crlf\n"),
        ("core.ignorestat", "false", "*.txt crlf=input\n"),
        (
            "core.ignorestat",
            "false",
            "*.txt working-tree-encoding=UTF-16LE\n",
        ),
    ] {
        for policy in [CleanupPolicy::Pristine, CleanupPolicy::Warm] {
            eprintln!("{key}={value} attributes={attrs:?} cleanup={policy:?}");
            let r = Repo::new();
            fs::write(r.path.join(".gitattributes"), attrs).unwrap();
            fs::write(r.path.join("ident.txt"), "$Id$\n").unwrap();
            fs::write(r.path.join("to-directory"), "file\n").unwrap();
            fs::create_dir(r.path.join("to-file")).unwrap();
            fs::write(r.path.join("to-file/child"), "child\n").unwrap();
            symlink("tracked.txt", r.path.join("link")).unwrap();
            // Encoding attributes apply to checkout; blobs start as UTF-8.
            git(
                &r.path,
                &[
                    "-c",
                    "core.attributesFile=/dev/null",
                    "add",
                    "--",
                    "to-directory",
                    "to-file",
                    "link",
                ],
            );
            if attrs.contains("encoding") {
                fs::write(r.path.join(".gitattributes"), "").unwrap();
                git(&r.path, &["add", "ident.txt"]);
                fs::write(r.path.join(".gitattributes"), attrs).unwrap();
                git(&r.path, &["add", ".gitattributes"]);
            } else {
                git(&r.path, &["add", "."]);
            }
            git(&r.path, &["commit", "-qm", "physical checkout fixture"]);
            git(&r.path, &["config", key, value]);
            if key.starts_with("filter.") {
                git(
                    &r.path,
                    &["config", "filter.fixture.smudge", "sed s/CLEAN/SMUDGED/g"],
                );
            }
            let oracle = r.temp.path().join("oracle");
            git(
                &r.path,
                &[
                    "worktree",
                    "add",
                    "--detach",
                    oracle.to_str().unwrap(),
                    "HEAD",
                ],
            );
            let initialized = Pool::init(
                &r.path,
                PoolOptions {
                    worktree_root: Some(support::worktree_root(&r.path, "default")),
                    capacity: 1,
                    cleanup_policy: policy,
                    ..Default::default()
                },
            );
            if key.starts_with("filter.") {
                assert_eq!(
                    initialized.unwrap_err().code,
                    groove_core::ErrorCode::UnsupportedConfiguration
                );
                assert_eq!(fs::read(r.path.join("tracked.txt")).unwrap(), b"baseline\n");
                continue;
            }
            initialized.unwrap();
            let p = Pool::open(&r.path).unwrap();
            let lease = p.acquire(Default::default()).unwrap();
            let workspace = lease.info().worktree_path.clone();
            for root in [&workspace, &oracle] {
                // Changes that can normalize to the original blob must still
                // recover the same physical bytes as a complete rewrite.
                if attrs.contains("ident") {
                    fs::write(root.join("ident.txt"), "$Id: wrong-expansion $\n").unwrap();
                } else if attrs.contains("eol=crlf") {
                    fs::write(root.join("ident.txt"), "$Id$\n").unwrap();
                } else if attrs.contains("crlf=input") {
                    fs::write(root.join("ident.txt"), "$Id$\r\n").unwrap();
                }
                fs::write(root.join("tracked.txt"), b"tampered\n").unwrap();
                git(root, &["update-index", "--assume-unchanged", "tracked.txt"]);
                fs::remove_file(root.join("to-directory")).unwrap();
                fs::create_dir(root.join("to-directory")).unwrap();
                fs::write(root.join("to-directory/junk"), "junk").unwrap();
                fs::remove_dir_all(root.join("to-file")).unwrap();
                fs::write(root.join("to-file"), "wrong type").unwrap();
                fs::remove_file(root.join("link")).unwrap();
                fs::write(root.join("link"), "wrong target").unwrap();
                fs::set_permissions(root.join("ident.txt"), fs::Permissions::from_mode(0o755))
                    .unwrap();
                fs::create_dir(root.join("ignored")).unwrap();
                fs::write(root.join("ignored/cache"), "cache").unwrap();
                fs::write(root.join("untracked"), "remove").unwrap();
            }
            drop(lease);
            // Original preparation: discard the index, forced checkout, cleanup.
            let index = git(&oracle, &["rev-parse", "--git-path", "index"]);
            fs::remove_file(index).unwrap();
            git(&oracle, &["checkout", "--detach", "--force", "HEAD", "--"]);
            git(
                &oracle,
                &[
                    "clean",
                    if policy == CleanupPolicy::Pristine {
                        "-ffdx"
                    } else {
                        "-ffd"
                    },
                    "--",
                ],
            );
            let _next = p.acquire(Default::default()).unwrap();
            assert_eq!(
                physical_tree(&workspace),
                physical_tree(&oracle),
                "{key}={value} attrs={attrs:?} {policy:?}"
            );
        }
    }
}

#[test]
fn reuse_breaks_external_hardlinks() {
    for cleanup in [CleanupPolicy::Pristine, CleanupPolicy::Warm] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                worktree_root: Some(support::worktree_root(&r.path, "default")),
                capacity: 1,
                cleanup_policy: cleanup,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.acquire(Default::default()).unwrap();
        let workspace = lease.info().worktree_path.clone();
        let outside = r.temp.path().join("outside.txt");
        fs::write(&outside, b"baseline\n").unwrap();
        fs::remove_file(workspace.join("tracked.txt")).unwrap();
        fs::hard_link(&outside, workspace.join("tracked.txt")).unwrap();
        drop(lease);
        let _next = pool.acquire(Default::default()).unwrap();
        let text = fs::metadata(workspace.join("tracked.txt")).unwrap();
        assert_eq!(text.nlink(), 1, "{cleanup:?}");
        assert_ne!(text.ino(), fs::metadata(&outside).unwrap().ino());
        fs::write(workspace.join("tracked.txt"), b"next reservation\n").unwrap();
        assert_eq!(fs::read(outside).unwrap(), b"baseline\n");
    }
}

#[test]
fn orphaned_slot_head_preserves_warm_cache_during_normalization() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            capacity: 1,
            cleanup_policy: CleanupPolicy::Warm,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    let workspace = lease.info().worktree_path.clone();
    git(&workspace, &["checkout", "--orphan", "scratch"]);
    fs::create_dir(workspace.join("ignored")).unwrap();
    fs::write(workspace.join("ignored/cache"), "keep\n").unwrap();
    drop(lease);
    let next = pool.acquire(Default::default()).unwrap();
    assert_eq!(
        next.info().recovery,
        groove_core::RecoveryOutcome::Sanitized
    );
    assert_eq!(
        fs::read(workspace.join("ignored/cache")).unwrap(),
        b"keep\n"
    );
    assert_eq!(
        fs::read(workspace.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
}

#[test]
fn reuse_restores_read_only_and_special_permission_bits() {
    for cleanup in [CleanupPolicy::Pristine, CleanupPolicy::Warm] {
        let r = Repo::new();
        fs::write(r.path.join("run.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(r.path.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        git(&r.path, &["add", "run.sh"]);
        git(&r.path, &["commit", "-qm", "executable"]);
        Pool::init(
            &r.path,
            PoolOptions {
                worktree_root: Some(support::worktree_root(&r.path, "default")),
                capacity: 1,
                cleanup_policy: cleanup,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.acquire(Default::default()).unwrap();
        let workspace = lease.info().worktree_path.clone();
        let expected = ["tracked.txt", "run.sh"].map(|p| {
            fs::metadata(workspace.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        });
        fs::set_permissions(
            workspace.join("tracked.txt"),
            fs::Permissions::from_mode(0o444),
        )
        .unwrap();
        fs::set_permissions(workspace.join("run.sh"), fs::Permissions::from_mode(0o4755)).unwrap();
        drop(lease);
        let _next = pool.acquire(Default::default()).unwrap();
        let actual = ["tracked.txt", "run.sh"].map(|p| {
            fs::metadata(workspace.join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        });
        assert_eq!(actual, expected, "{cleanup:?}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn reuse_removes_extended_attributes_from_tracked_files() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    let file = CString::new(
        lease
            .info()
            .worktree_path
            .join("tracked.txt")
            .as_os_str()
            .as_bytes(),
    )
    .unwrap();
    let value = b"contamination";
    // SAFETY: file/name are terminated strings and value is readable for its length.
    let result = unsafe {
        libc::setxattr(
            file.as_ptr(),
            c"user.groove-reset".as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        assert_eq!(error.raw_os_error(), Some(libc::EOPNOTSUPP), "{error}");
        eprintln!("skipping xattr assertion: fixture filesystem does not support xattrs");
        return;
    }
    drop(lease);
    let _next = pool.acquire(Default::default()).unwrap();
    // SAFETY: file/name are terminated strings; a null buffer queries the size.
    let result = unsafe {
        libc::getxattr(
            file.as_ptr(),
            c"user.groove-reset".as_ptr(),
            std::ptr::null_mut(),
            0,
        )
    };
    assert_eq!(result, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENODATA)
    );
}

#[cfg(target_os = "macos")]
#[test]
fn reuse_restores_case_only_tracked_renames_on_case_insensitive_storage() {
    let r = Repo::new();
    if !r.path.join("TRACKED.TXT").exists() {
        eprintln!("skipping case-insensitive assertion on a case-sensitive filesystem");
        return;
    }
    Pool::init(
        &r.path,
        PoolOptions {
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    let workspace = lease.info().worktree_path.clone();
    fs::rename(workspace.join("tracked.txt"), workspace.join("TRACKED.TXT")).unwrap();
    drop(lease);
    let _next = pool.acquire(Default::default()).unwrap();
    let names: Vec<_> = fs::read_dir(workspace)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(names.iter().any(|n| n == "tracked.txt"));
    assert!(!names.iter().any(|n| n == "TRACKED.TXT"));
}

#[test]
fn warm_reuse_rewrites_backdated_tracked_files_without_touching_cached_output() {
    use std::time::{Duration, UNIX_EPOCH};
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            capacity: 1,
            cleanup_policy: CleanupPolicy::Warm,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    let workspace = lease.info().worktree_path.clone();
    let tracked = workspace.join("tracked.txt");
    let backdated = UNIX_EPOCH + Duration::from_secs(60);
    fs::File::options()
        .write(true)
        .open(&tracked)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(backdated))
        .unwrap();
    fs::create_dir(workspace.join("ignored")).unwrap();
    let cache = workspace.join("ignored/output");
    fs::write(&cache, b"unverified cache from another input\n").unwrap();
    let cache_time = backdated + Duration::from_secs(1);
    fs::File::options()
        .write(true)
        .open(&cache)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(cache_time))
        .unwrap();
    drop(lease);
    let _next = pool.acquire(Default::default()).unwrap();
    assert_eq!(fs::read(&tracked).unwrap(), b"baseline\n");
    assert!(fs::metadata(tracked).unwrap().modified().unwrap() > cache_time);
    assert_eq!(
        fs::read(&cache).unwrap(),
        b"unverified cache from another input\n"
    );
    assert_eq!(fs::metadata(cache).unwrap().modified().unwrap(), cache_time);
}

#[test]
fn reuse_tracks_the_verified_base_when_the_task_soft_resets_head() {
    let r = Repo::new();
    let old = r.git(&["rev-parse", "HEAD"]);
    fs::write(r.path.join(".gitattributes"), b"*.txt text eol=crlf\n").unwrap();
    r.git(&["add", ".gitattributes"]);
    r.git(&["commit", "-qm", "new conversion rules"]);
    let new = r.git(&["rev-parse", "HEAD"]);
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
    let lease = pool
        .acquire(groove_core::AcquireOptions {
            base: old.trim().into(),
            ..Default::default()
        })
        .unwrap();
    let path = lease.info().worktree_path.clone();
    git(&path, &["reset", "--soft", new.trim()]);
    drop(lease);
    let _next = pool.acquire(Default::default()).unwrap();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\r\n");
    assert!(git(&path, &["status", "--porcelain"]).is_empty());
}

#[test]
fn reuse_detects_changed_external_conversion_rules() {
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
    let lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    drop(lease);
    fs::write(
        r.path.join(".git/info/attributes"),
        b"*.txt text eol=crlf\n",
    )
    .unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\r\n");
    drop(lease);
    fs::write(r.path.join(".git/info/attributes"), b"*.txt text eol=lf\n").unwrap();
    let _next = pool.acquire(Default::default()).unwrap();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\n");
}

#[test]
fn directory_permissions_are_fresh_after_reuse() {
    let r = Repo::new();
    fs::create_dir(r.path.join("tracked-dir")).unwrap();
    fs::write(r.path.join("tracked-dir/file"), b"baseline").unwrap();
    r.git(&["add", "."]);
    r.git(&["commit", "-qm", "nested directory"]);
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
    let lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.join("tracked-dir");
    let expected = fs::metadata(&path).unwrap().mode() & 0o7777;
    drop(lease);
    for mode in [0o777, 0o700] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let lease = pool.acquire(Default::default()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o7777, expected);
        assert_eq!(fs::read(path.join("file")).unwrap(), b"baseline");
        drop(lease);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn directory_extended_attributes_do_not_carry_into_the_next_lease() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
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
    let lease = pool.acquire(Default::default()).unwrap();
    let path = CString::new(lease.info().worktree_path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path/key are terminated strings and the value covers its length.
    let result = unsafe {
        libc::setxattr(
            path.as_ptr(),
            c"user.groove-dir".as_ptr(),
            b"dirty".as_ptr().cast(),
            5,
            0,
        )
    };
    if result != 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOTSUP)
        );
        return;
    }
    drop(lease);
    let _next = pool.acquire(Default::default()).unwrap();
    assert_eq!(
        // SAFETY: path/key are terminated strings; a null buffer queries the size.
        unsafe {
            libc::getxattr(
                path.as_ptr(),
                c"user.groove-dir".as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ENODATA)
    );
}

#[test]
fn reuse_detects_edits_inside_the_configured_attributes_file() {
    let r = Repo::new();
    let attributes = r.temp.path().join("external-attributes");
    fs::write(&attributes, b"*.txt text eol=lf\n").unwrap();
    r.git(&[
        "config",
        "core.attributesFile",
        attributes.to_str().unwrap(),
    ]);
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
    let lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    drop(lease);
    fs::write(&attributes, b"*.txt text eol=crlf\n").unwrap();
    let _next = pool.acquire(Default::default()).unwrap();
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\r\n");
}
