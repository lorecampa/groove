mod support;

use groove_core::{snapshot::Snapshots, ErrorCode};
use std::{
    fs,
    os::unix::{ffi::OsStrExt, fs::symlink, fs::PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};
use support::Repo;

fn service(path: &Path) -> Snapshots {
    Snapshots::open(path, Duration::from_secs(30)).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct Workspace {
    head: String,
    index: Vec<u8>,
    config: Vec<u8>,
    files: Vec<(PathBuf, u32, Vec<u8>)>,
}

impl Workspace {
    fn read(repo: &Repo) -> Self {
        fn walk(root: &Path, path: &Path, files: &mut Vec<(PathBuf, u32, Vec<u8>)>) {
            for entry in fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path == root.join(".git") {
                    continue;
                }
                let metadata = fs::symlink_metadata(&path).unwrap();
                let bytes = if metadata.is_symlink() {
                    fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_bytes()
                        .to_vec()
                } else if metadata.is_file() {
                    fs::read(&path).unwrap()
                } else {
                    Vec::new()
                };
                files.push((
                    path.strip_prefix(root).unwrap().to_owned(),
                    metadata.permissions().mode(),
                    bytes,
                ));
                if metadata.is_dir() {
                    walk(root, &path, files);
                }
            }
        }
        let mut files = Vec::new();
        walk(&repo.path, &repo.path, &mut files);
        files.sort();
        Self {
            head: repo.git(&["rev-parse", "HEAD"]),
            index: fs::read(repo.path.join(".git/index")).unwrap(),
            config: fs::read(repo.path.join(".git/config")).unwrap(),
            files,
        }
    }
}

fn assert_applies(repo: &Repo, before: &str, after: &str, patch: &str) {
    assert!(!patch.is_empty());
    assert!(!patch.contains('\u{1b}'), "patch contains terminal escapes");
    let scratch = tempfile::tempdir_in(repo.temp.path()).unwrap();
    let index = scratch.path().join("index");
    let patch_file = scratch.path().join("export.patch");
    // Apply exactly the exported bytes, without fixing prefixes or newlines.
    fs::write(&patch_file, patch).unwrap();
    for args in [
        vec!["read-tree", before],
        vec!["apply", "--cached", patch_file.to_str().unwrap()],
        vec!["write-tree"],
    ] {
        let output = support::command(&repo.path, "git")
            .env("GIT_INDEX_FILE", &index)
            .args(&args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}\n{patch}",
            String::from_utf8_lossy(&output.stderr)
        );
        if args[0] == "write-tree" {
            assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), after);
        }
    }
}

fn exercise_patch(format: &str, config: &[(&str, &str)]) {
    let repo = Repo::format(format);
    fs::create_dir(repo.path.join("nested")).unwrap();
    for (name, bytes) in [
        ("binary", b"before\0\xff".as_slice()),
        ("crlf.txt", b"before\r\nsecond\r\n"),
        ("deleted", b"delete me\n"),
        ("file-to-dir", b"file\n"),
        ("nested/space name.txt", b"nested before\n"),
        (":literal", b"colon before\n"),
        ("executable", b"same bytes\n"),
    ] {
        fs::write(repo.path.join(name), bytes).unwrap();
    }
    fs::create_dir(repo.path.join("dir-to-file")).unwrap();
    fs::write(repo.path.join("dir-to-file/child"), "child\n").unwrap();
    symlink("tracked.txt", repo.path.join("link")).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "patch baseline"]);

    // User ignore rules must survive export's formatting overrides.
    let excludes = repo.temp.path().join("user-ignore");
    fs::write(&excludes, "user-ignored\n").unwrap();
    repo.git(&["config", "core.excludesFile", excludes.to_str().unwrap()]);
    for (key, value) in config {
        repo.git(&["config", key, value]);
    }
    let helper = repo.temp.path().join("diff-helper");
    fs::write(
        &helper,
        "#!/bin/sh\n: > \"$0.ran\"\nprintf 'converted\\n'\n",
    )
    .unwrap();
    let command = format!("sh '{}'", helper.display());
    for key in ["diff.external", "diff.audit.command", "diff.audit.textconv"] {
        repo.git(&["config", key, &command]);
    }
    fs::write(repo.path.join(".git/info/attributes"), "*.txt diff=audit\n").unwrap();

    // Start discovery below the root to exercise diff.relative preferences.
    let snapshots = service(&repo.path.join("nested"));
    let before = snapshots.save(Default::default()).unwrap();
    fs::write(repo.path.join("tracked.txt"), "staged\n").unwrap();
    repo.git(&["add", "tracked.txt"]);
    for (name, bytes) in [
        ("tracked.txt", b"working\n".as_slice()),
        ("binary", b"after\0\xfe"),
        ("crlf.txt", b"after\r\nsecond\r\nlast\r\n"),
        ("nested/space name.txt", b"nested after\n"),
        (":literal", b"colon after\n"),
        ("untracked", b"new file\n"),
        ("user-ignored", b"private bytes\n"),
    ] {
        fs::write(repo.path.join(name), bytes).unwrap();
    }
    fs::remove_file(repo.path.join("deleted")).unwrap();
    fs::remove_file(repo.path.join("file-to-dir")).unwrap();
    fs::create_dir(repo.path.join("file-to-dir")).unwrap();
    fs::write(repo.path.join("file-to-dir/child"), "new child\n").unwrap();
    fs::remove_dir_all(repo.path.join("dir-to-file")).unwrap();
    fs::write(repo.path.join("dir-to-file"), "now a file\n").unwrap();
    fs::set_permissions(
        repo.path.join("executable"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fs::remove_file(repo.path.join("link")).unwrap();
    symlink("nested/space name.txt", repo.path.join("link")).unwrap();
    let after = snapshots.save(Default::default()).unwrap();
    assert!(!repo
        .git(&["ls-tree", "-r", "--name-only", &after.work_tree])
        .lines()
        .any(|path| path == "user-ignored"));
    assert_eq!(
        repo.git(&["show", &format!("{}:tracked.txt", after.index_tree)]),
        "staged"
    );
    let original = Workspace::read(&repo);
    for other in [Some(after.id.as_str()), None] {
        let patch = snapshots.diff(&before.id, other).unwrap();
        assert_applies(&repo, &before.work_tree, &after.work_tree, &patch);
        assert_eq!(Workspace::read(&repo), original);
        assert!(
            !helper.with_extension("ran").exists(),
            "external helper ran"
        );
    }
    assert_eq!(snapshots.list().unwrap().len(), 2);
}

#[test]
fn exported_patches_reconstruct_trees_under_hostile_display_preferences() {
    for config in [
        vec![("color.ui", "always")],
        vec![("diff.noprefix", "true")],
        vec![("diff.mnemonicprefix", "true")],
        vec![("diff.srcprefix", "old/"), ("diff.dstprefix", "new/")],
        vec![("diff.relative", "true")],
        vec![("diff.context", "0")],
    ] {
        exercise_patch("sha1", &config);
    }
}

#[test]
fn sha256_patches_reconstruct_trees_under_combined_hostile_preferences() {
    exercise_patch(
        "sha256",
        &[
            ("color.ui", "always"),
            ("diff.noprefix", "true"),
            ("diff.mnemonicprefix", "true"),
            ("diff.srcprefix", "old/"),
            ("diff.dstprefix", "new/"),
            ("diff.relative", "true"),
            ("diff.context", "0"),
        ],
    );
}

#[test]
fn unchanged_snapshots_export_an_empty_patch() {
    let repo = Repo::new();
    repo.git(&["config", "color.ui", "always"]);
    let snapshots = service(&repo.path);
    let before = snapshots.save(Default::default()).unwrap();
    let after = snapshots.save(Default::default()).unwrap();
    let original = Workspace::read(&repo);
    for other in [Some(after.id.as_str()), None] {
        assert_eq!(snapshots.diff(&before.id, other).unwrap(), "");
        assert_eq!(Workspace::read(&repo), original);
    }
}

#[test]
fn current_diff_rejects_intent_to_add_under_hostile_display_preferences() {
    let repo = Repo::new();
    let snapshots = service(&repo.path);
    let before = snapshots.save(Default::default()).unwrap();
    repo.git(&["config", "color.ui", "always"]);
    repo.git(&["config", "diff.noprefix", "true"]);
    repo.git(&["config", "diff.relative", "true"]);
    fs::write(repo.path.join("intent.txt"), "not staged yet\n").unwrap();
    repo.git(&["add", "--intent-to-add", "intent.txt"]);
    let original = Workspace::read(&repo);
    let error = snapshots.diff(&before.id, None).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(error.to_string().contains("intent-to-add"), "{error}");
    assert_eq!(Workspace::read(&repo), original);
    assert_eq!(snapshots.list().unwrap().len(), 1);
}
