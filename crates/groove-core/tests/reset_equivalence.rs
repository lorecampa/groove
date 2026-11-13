mod support;
use groove_core::{CleanupPolicy, Pool, PoolOptions};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
};
use support::{git, Repo};

// Compare observable checkout state without asking Git to normalize the bytes.
fn physical_tree(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
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
                    metadata.permissions().mode() & 0o111,
                    fs::read(&path).unwrap(),
                )
            };
            out.insert(relative, value);
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
                    capacity: 1,
                    cleanup_policy: policy,
                    ..Default::default()
                },
            );
            initialized.unwrap();
            let p = Pool::open(&r.path).unwrap();
            if key.starts_with("filter.") {
                assert_eq!(
                    p.acquire(Default::default()).unwrap_err().code,
                    groove_core::ErrorCode::UnsupportedConfiguration
                );
                assert_eq!(fs::read(r.path.join("tracked.txt")).unwrap(), b"baseline\n");
                continue;
            }
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
