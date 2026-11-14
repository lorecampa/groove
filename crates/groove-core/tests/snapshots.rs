mod support;
use groove_core::snapshot::{SaveOptions, Snapshots};
use groove_core::ErrorCode;
use std::{
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{symlink, PermissionsExt},
    },
    time::Duration,
};
use support::Repo;
fn service(repo: &Repo) -> Snapshots {
    Snapshots::open(&repo.path, Duration::from_secs(30)).unwrap()
}
fn save(s: &Snapshots) -> groove_core::snapshot::Snapshot {
    s.save(SaveOptions::default()).unwrap()
}
#[test]
fn unreadable_untracked_directories_fail_capture_without_publishing() {
    // SAFETY: geteuid only observes the calling process's effective uid.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("requires an unprivileged user to enforce directory permissions");
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    let directory = repo.path.join("unreadable");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("work.txt"), "must survive").unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).unwrap();
    let result = snapshots.save(Default::default());
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap_err().code, ErrorCode::GitError);
    assert!(snapshots.list().unwrap().is_empty());
    assert_eq!(
        fs::read(directory.join("work.txt")).unwrap(),
        b"must survive"
    );
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    let saved = save(&snapshots);
    let recovered = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&recovered)).unwrap();
    assert_eq!(
        fs::read(recovered.join("unreadable/work.txt")).unwrap(),
        b"must survive"
    );
}

#[test]
fn unresolved_merge_metadata_cannot_be_silently_omitted_from_a_snapshot() {
    let repo = Repo::new();
    let base = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "-qb", "merge-source"]);
    fs::write(repo.path.join("merged.txt"), "merged work").unwrap();
    repo.git(&["add", "merged.txt"]);
    repo.git(&["commit", "-qm", "source"]);
    let source = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "--detach", &base]);
    repo.git(&["merge", "--no-ff", "--no-commit", "merge-source"]);
    repo.git(&["branch", "-D", "merge-source"]);
    let merge_head = fs::read(repo.path.join(".git/MERGE_HEAD")).unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let snapshots = service(&repo);
    assert_eq!(
        snapshots.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert!(snapshots.list().unwrap().is_empty());
    assert_eq!(
        fs::read(repo.path.join(".git/MERGE_HEAD")).unwrap(),
        merge_head
    );
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read(repo.path.join("merged.txt")).unwrap(),
        b"merged work"
    );
    repo.git(&["commit", "-qm", "finish merge"]);
    let saved = save(&snapshots);
    let recovered = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&recovered)).unwrap();
    assert_eq!(support::git(&recovered, &["rev-parse", "HEAD^2"]), source);
}

#[test]
fn roundtrip_preserves_index_files_and_head() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("tracked.txt"), "staged\n").unwrap();
    repo.git(&["add", "tracked.txt"]);
    fs::write(repo.path.join("tracked.txt"), "unstaged\n").unwrap();
    fs::write(repo.path.join("new\nfile"), b"binary\0\xff").unwrap();
    fs::create_dir(repo.path.join("ignored")).unwrap();
    fs::write(repo.path.join("ignored/cache"), "keep").unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let head = repo.git(&["rev-parse", "HEAD"]);
    let snap = save(&s);
    assert_eq!(index, fs::read(repo.path.join(".git/index")).unwrap());
    assert_eq!(head, repo.git(&["rev-parse", "HEAD"]));
    fs::write(repo.path.join("tracked.txt"), "later\n").unwrap();
    fs::remove_file(repo.path.join("new\nfile")).unwrap();
    fs::write(repo.path.join("extra"), "later").unwrap();
    let restored = s.restore(&snap.id, None).unwrap();
    assert!(restored.safety_snapshot_id.is_some());
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"unstaged\n"
    );
    assert_eq!(repo.git(&["show", ":tracked.txt"]), "staged");
    assert_eq!(
        fs::read(repo.path.join("new\nfile")).unwrap(),
        b"binary\0\xff"
    );
    assert!(!repo.path.join("extra").exists());
    assert_eq!(fs::read(repo.path.join("ignored/cache")).unwrap(), b"keep");
}
#[test]
fn external_recovery_and_gc_keep_local_history() {
    let repo = Repo::new();
    let s = service(&repo);
    repo.git(&["commit", "--allow-empty", "-qm", "local"]);
    let head = repo.git(&["rev-parse", "HEAD"]);
    fs::write(repo.path.join("new"), "hello").unwrap();
    let snap = save(&s);
    repo.git(&["gc", "-q"]);
    let dest = repo.temp.path().join("recovered");
    s.restore(&snap.id, Some(&dest)).unwrap();
    assert_eq!(fs::read(dest.join("new")).unwrap(), b"hello");
    assert_eq!(support::git(&dest, &["rev-parse", "HEAD"]), head);
    assert!(s.restore(&snap.id, Some(&dest)).is_err());
}
#[test]
fn unborn_roundtrip_and_external_recovery() {
    let temp = tempfile::tempdir().unwrap();
    support::git(
        temp.path(),
        &["init", "-q", "--initial-branch=audit/original"],
    );
    fs::write(temp.path().join("first"), "first").unwrap();
    let s = Snapshots::open(temp.path(), Duration::from_secs(30)).unwrap();
    let snap = save(&s);
    assert!(snap.head.is_none());
    fs::write(temp.path().join("first"), "later").unwrap();
    s.restore(&snap.id, None).unwrap();
    assert_eq!(fs::read(temp.path().join("first")).unwrap(), b"first");
    let outside = tempfile::tempdir().unwrap();
    let dest = outside.path().join("recovered");
    s.restore(&snap.id, Some(&dest)).unwrap();
    assert_eq!(
        support::git(&dest, &["symbolic-ref", "HEAD"]),
        "refs/heads/audit/original"
    );
    assert_eq!(fs::read(dest.join("first")).unwrap(), b"first");
    assert_eq!(support::git(&dest, &["ls-files"]), "");
}
#[test]
fn delete_removes_only_the_selected_snapshot() {
    let repo = Repo::new();
    let s = service(&repo);
    let a = save(&s);
    let b = save(&s);
    assert_eq!(s.list().unwrap().len(), 2);
    s.delete(&a.id, false).unwrap();
    assert!(s.show(&a.id).is_err());
    assert!(s.show(&b.id).is_ok());
    assert!(s.delete(&a.id, false).is_err());
}
#[test]
fn file_modes_deletions_symlinks_and_platform_filenames() {
    let repo = Repo::format("sha256");
    let s = service(&repo);
    fs::remove_file(repo.path.join("tracked.txt")).unwrap();
    let name = std::ffi::OsStr::from_bytes(if cfg!(target_os = "macos") {
        b"odd \xc3\xa8\n"
    } else {
        b"odd\xff"
    });
    fs::write(repo.path.join(name), "exe").unwrap();
    fs::set_permissions(repo.path.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    symlink("absent", repo.path.join("link")).unwrap();
    let snap = save(&s);
    let dest = repo.temp.path().join("recovery");
    s.restore(&snap.id, Some(&dest)).unwrap();
    assert!(!dest.join("tracked.txt").exists());
    assert_eq!(
        fs::read_link(dest.join("link")).unwrap(),
        std::path::Path::new("absent")
    );
    assert_eq!(
        fs::metadata(dest.join(name)).unwrap().permissions().mode() & 0o111,
        0o111
    );
}

#[cfg(target_os = "macos")]
#[test]
fn apfs_rejects_non_utf8_filenames_before_changing_workspace_contents() {
    let repo = Repo::new();
    let invalid = repo.path.join(std::ffi::OsStr::from_bytes(b"odd\xff"));
    let error = fs::write(invalid, b"unrepresentable").unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EILSEQ));
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    assert!(repo.git(&["status", "--porcelain"]).is_empty());
}

#[test]
fn snapshots_preserve_actual_line_endings_under_every_autocrlf_setting() {
    for setting in ["true", "yes", "on", "1", "input", "false"] {
        let repo = Repo::new();
        repo.git(&["config", "core.autocrlf", setting]);
        let staged = b"staged\r\n";
        fs::write(repo.path.join("tracked.txt"), staged).unwrap();
        // The real index intentionally differs from the working bytes.
        repo.git(&["-c", "core.autocrlf=false", "add", "tracked.txt"]);
        let working = b"working\nsecond line\r\n";
        fs::write(repo.path.join("tracked.txt"), working).unwrap();
        fs::write(repo.path.join("untracked.txt"), b"untracked\n").unwrap();
        let index = fs::read(repo.path.join(".git/index")).unwrap();
        let s = service(&repo);
        let snapshot = save(&s);
        assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
        // A change in preferences after capture must not change recovered bytes.
        repo.git(&["config", "core.autocrlf", "true"]);
        let destination = repo.temp.path().join("recovered");
        s.restore(&snapshot.id, Some(&destination)).unwrap();
        assert_eq!(
            fs::read(destination.join("tracked.txt")).unwrap(),
            working,
            "{setting}"
        );
        assert_eq!(
            fs::read(destination.join("untracked.txt")).unwrap(),
            b"untracked\n",
            "{setting}"
        );
        let blob = support::command(&destination, "git")
            .args(["show", ":tracked.txt"])
            .output()
            .unwrap();
        assert!(blob.status.success());
        assert_eq!(blob.stdout, staged, "{setting}");
        fs::write(repo.path.join("tracked.txt"), "later\n").unwrap();
        s.restore(&snapshot.id, None).unwrap();
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            working,
            "{setting}"
        );
    }
}

#[test]
fn snapshots_keep_symlinks_and_chmod_even_when_git_ignores_them() {
    let repo = Repo::new();
    repo.git(&["config", "core.filemode", "false"]);
    repo.git(&["config", "core.symlinks", "false"]);
    fs::set_permissions(
        repo.path.join("tracked.txt"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    symlink("tracked.txt", repo.path.join("link")).unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let s = service(&repo);
    let snapshot = save(&s);
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    let destination = repo.temp.path().join("recovered");
    s.restore(&snapshot.id, Some(&destination)).unwrap();
    assert_eq!(
        fs::read_link(destination.join("link")).unwrap(),
        std::path::Path::new("tracked.txt")
    );
    assert_eq!(
        fs::metadata(destination.join("tracked.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o111,
        0o111
    );
    assert_eq!(
        support::git(&destination, &["ls-files", "--stage", "tracked.txt"])
            .split_whitespace()
            .next(),
        Some("100644")
    );
}

#[test]
fn unused_filter_definitions_do_not_prevent_checkpoints() {
    let repo = Repo::new();
    repo.git(&["config", "filter.lfs.clean", "false"]);
    repo.git(&["config", "filter.lfs.smudge", "false"]);
    repo.git(&["config", "filter.lfs.required", "true"]);
    fs::write(repo.path.join("tracked.txt"), "saved\n").unwrap();
    let s = service(&repo);
    let snapshot = save(&s);
    let destination = repo.temp.path().join("recovered");
    s.restore(&snapshot.id, Some(&destination)).unwrap();
    assert_eq!(
        fs::read(destination.join("tracked.txt")).unwrap(),
        b"saved\n"
    );
}

#[test]
fn restore_rejects_new_filter_attributes_before_modifying_files() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("new.txt"), "saved\n").unwrap();
    let snapshot = save(&s);
    fs::remove_file(repo.path.join("new.txt")).unwrap();
    fs::write(repo.path.join("tracked.txt"), "precious later work\n").unwrap();
    repo.git(&["config", "filter.audit.smudge", "false"]);
    fs::write(
        repo.path.join(".git/info/attributes"),
        "new.txt filter=audit\n",
    )
    .unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let destination = repo.temp.path().join("recovered");
    for target in [Some(destination.as_path()), None] {
        let error = s.restore(&snapshot.id, target).unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
        assert!(!destination.exists());
        assert!(!repo.path.join("new.txt").exists());
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            b"precious later work\n"
        );
        assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
        assert_eq!(s.show(&snapshot.id).unwrap().id, snapshot.id);
    }
}

fn assert_conversion_rejected(rules: &[&str]) {
    for rule in rules {
        let repo = Repo::new();
        let snapshots = service(&repo);
        fs::write(repo.path.join("tracked.txt"), b"saved\r\n").unwrap();
        let saved = save(&snapshots);
        fs::write(repo.path.join(".git/info/attributes"), rule).unwrap();
        fs::write(repo.path.join("tracked.txt"), b"later\r\n").unwrap();
        let index = fs::read(repo.path.join(".git/index")).unwrap();
        let error = snapshots.save(Default::default()).unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedConfiguration, "{rule}");
        let destination = repo.temp.path().join("recovered");
        for target in [None, Some(destination.as_path())] {
            let error = snapshots.restore(&saved.id, target).unwrap_err();
            assert_eq!(error.code, ErrorCode::UnsupportedConfiguration, "{rule}");
            assert!(!destination.exists());
            assert_eq!(
                fs::read(repo.path.join("tracked.txt")).unwrap(),
                b"later\r\n"
            );
            assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
        }
        assert_eq!(snapshots.list().unwrap().len(), 1);
    }
}

#[test]
fn legacy_crlf_attributes_reject_capture_and_restore_before_changing_files() {
    assert_conversion_rejected(&["*.txt crlf", "*.txt crlf=input"]);
}

#[test]
fn empty_attribute_values_cannot_hide_content_conversion() {
    assert_conversion_rejected(&["*.txt diff= text=auto"]);
}

#[test]
fn empty_nonconverting_attributes_and_unset_crlf_preserve_bytes() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), "*.txt diff= -crlf\n").unwrap();
    fs::write(repo.path.join("tracked.txt"), b"saved\r\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    let destination = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    assert_eq!(
        fs::read(destination.join("tracked.txt")).unwrap(),
        b"saved\r\n"
    );
}
#[test]
fn rejects_large_files_filters_conflicts_and_changed_head() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("new"), "large").unwrap();
    assert!(s
        .save(SaveOptions {
            max_file_bytes: 1,
            ..Default::default()
        })
        .is_err());
    assert!(s.list().unwrap().is_empty());
    let snap = save(&s);
    repo.git(&["commit", "--allow-empty", "-qm", "changed"]);
    assert!(s.restore(&snap.id, None).is_err());
    repo.git(&["config", "filter.lfs.clean", "cat"]);
    fs::write(repo.path.join(".gitattributes"), "*.txt filter=lfs\n").unwrap();
    assert!(s.save(Default::default()).is_err());
}
#[test]
fn no_change_reuses_file_trees_and_diff_includes_untracked() {
    let repo = Repo::new();
    let s = service(&repo);
    let a = save(&s);
    let b = save(&s);
    assert_eq!(a.work_tree, b.work_tree);
    assert_eq!(a.index_tree, b.index_tree);
    fs::write(repo.path.join("new"), "hello\n").unwrap();
    assert!(s.diff(&a.id, None).unwrap().contains("+hello"));
    assert_eq!(s.list().unwrap().len(), 2);
}
#[test]
fn ignored_collision_is_never_overwritten() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("collision"), "saved").unwrap();
    let snap = save(&s);
    fs::write(repo.path.join(".git/info/exclude"), "collision\n").unwrap();
    fs::write(repo.path.join("collision"), "precious ignored").unwrap();
    assert!(s.restore(&snap.id, None).is_err());
    assert_eq!(
        fs::read(repo.path.join("collision")).unwrap(),
        b"precious ignored"
    );
}
#[test]
fn linked_worktree_snapshots_are_shared() {
    let repo = Repo::new();
    let s = service(&repo);
    let other = repo.temp.path().join("linked");
    repo.git(&["worktree", "add", "--detach", other.to_str().unwrap()]);
    let other = Snapshots::open(other, Duration::from_secs(30)).unwrap();
    let a = save(&s);
    let b = save(&other);
    assert_ne!(a.workspace, b.workspace);
    assert_eq!(s.list().unwrap().len(), 2);
}
#[test]
fn sparse_checkout_preserves_outside_entries() {
    let repo = Repo::new();
    fs::create_dir(repo.path.join("inside")).unwrap();
    fs::create_dir(repo.path.join("outside")).unwrap();
    fs::write(repo.path.join("inside/a"), "a").unwrap();
    fs::write(repo.path.join("outside/b"), "b").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "directories"]);
    repo.git(&["sparse-checkout", "set", "inside"]);
    let s = service(&repo);
    fs::write(repo.path.join("inside/a"), "changed").unwrap();
    let snap = save(&s);
    assert!(!repo.path.join("outside/b").exists());
    assert_eq!(
        repo.git(&["show", &format!("{}:outside/b", snap.work_tree)]),
        "b"
    );
    let dest = repo.temp.path().join("sparse-recovery");
    s.restore(&snap.id, Some(&dest)).unwrap();
    assert_eq!(fs::read(dest.join("inside/a")).unwrap(), b"changed");
}
#[test]
fn malformed_manifest_and_conversion_attributes_fail_closed() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
    assert!(s.save(Default::default()).is_err());
    fs::remove_file(repo.path.join(".gitattributes")).unwrap();
    let snap = save(&s);
    repo.git(&[
        "update-ref",
        &format!("refs/groove/snapshots/{}", snap.id),
        "HEAD",
    ]);
    assert!(s.show(&snap.id).is_err());
}
#[test]
fn index_flags_and_gitlinks_fail_without_publication() {
    let repo = Repo::new();
    let s = service(&repo);
    repo.git(&["update-index", "--assume-unchanged", "tracked.txt"]);
    assert!(s.save(Default::default()).is_err());
    repo.git(&["update-index", "--no-assume-unchanged", "tracked.txt"]);
    let head = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},module"),
    ]);
    assert!(s.save(Default::default()).is_err());
    assert!(s.list().unwrap().is_empty());
}
#[test]
fn publication_failure_does_not_mutate_workspace() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("tracked.txt"), "unsaved\n").unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let head = repo.git(&["rev-parse", "HEAD"]);
    fs::create_dir_all(repo.path.join(".git/refs/groove")).unwrap();
    fs::write(
        repo.path.join(".git/refs/groove/snapshots"),
        "blocking file",
    )
    .unwrap();
    assert!(s.save(Default::default()).is_err());
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"unsaved\n"
    );
}
#[test]
fn unresolved_merge_is_rejected() {
    let repo = Repo::new();
    let original = repo.git(&["branch", "--show-current"]);
    repo.git(&["checkout", "-qb", "other"]);
    fs::write(repo.path.join("tracked.txt"), "other\n").unwrap();
    repo.git(&["commit", "-qam", "other"]);
    repo.git(&["checkout", &original]);
    fs::write(repo.path.join("tracked.txt"), "main\n").unwrap();
    repo.git(&["commit", "-qam", "main"]);
    assert!(!support::command(&repo.path, "git")
        .args(["merge", "other"])
        .output()
        .unwrap()
        .status
        .success());
    let s = service(&repo);
    assert!(s.save(Default::default()).is_err());
    assert!(s.list().unwrap().is_empty());
}
#[test]
fn tracked_files_under_ignored_directories_are_captured() {
    let repo = Repo::new();
    fs::create_dir(repo.path.join("ignored")).unwrap();
    fs::write(repo.path.join("ignored/tracked"), "base").unwrap();
    repo.git(&["add", "-f", "ignored/tracked"]);
    repo.git(&["commit", "-qm", "tracked ignored"]);
    fs::write(repo.path.join("ignored/tracked"), "changed").unwrap();
    fs::write(repo.path.join("ignored/untracked"), "excluded").unwrap();
    let s = service(&repo);
    let snap = save(&s);
    assert_eq!(
        repo.git(&["show", &format!("{}:ignored/tracked", snap.work_tree)]),
        "changed"
    );
    assert!(!repo
        .git(&["ls-tree", "-r", "--name-only", &snap.work_tree])
        .contains("untracked"));
}
#[test]
fn file_directory_transitions_preserve_only_selected_files() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::remove_file(repo.path.join("tracked.txt")).unwrap();
    fs::create_dir(repo.path.join("tracked.txt")).unwrap();
    fs::write(repo.path.join("tracked.txt/child"), "child").unwrap();
    fs::write(repo.path.join(".git/info/exclude"), "hidden\n").unwrap();
    fs::write(repo.path.join("tracked.txt/hidden"), "ignored").unwrap();
    let a = save(&s);
    assert_eq!(
        repo.git(&["show", &format!("{}:tracked.txt/child", a.work_tree)]),
        "child"
    );
    assert!(!repo
        .git(&["ls-tree", "-r", "--name-only", &a.work_tree])
        .contains("hidden"));
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-qm", "directory"]);
    fs::remove_dir_all(repo.path.join("tracked.txt")).unwrap();
    fs::write(repo.path.join("tracked.txt"), "file again").unwrap();
    let b = save(&s);
    assert_eq!(
        repo.git(&["show", &format!("{}:tracked.txt", b.work_tree)]),
        "file again"
    );
}
#[test]
fn deletion_releases_unique_objects_without_retaining_snapshot_history() {
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("tracked.txt"), "first unique content\n").unwrap();
    let a = save(&s);
    fs::write(repo.path.join("tracked.txt"), "second unique content\n").unwrap();
    let b = save(&s);
    s.delete(&a.id, false).unwrap();
    // Aggressive expiry is used only in this disposable fixture, never by the product.
    repo.git(&["gc", "--prune=now"]);
    assert!(!support::command(&repo.path, "git")
        .args(["cat-file", "-e", &a.work_tree])
        .output()
        .unwrap()
        .status
        .success());
    assert!(s.show(&b.id).is_ok());
}
#[test]
fn split_index_and_intent_to_add_are_handled_explicitly() {
    let repo = Repo::new();
    repo.git(&["update-index", "--split-index"]);
    let s = service(&repo);
    fs::write(repo.path.join("tracked.txt"), "split\n").unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let a = save(&s);
    assert_eq!(
        repo.git(&["show", &format!("{}:tracked.txt", a.work_tree)]),
        "split"
    );
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    fs::write(repo.path.join("intent"), "intent\n").unwrap();
    repo.git(&["add", "-N", "intent"]);
    assert!(s.save(Default::default()).is_err());
}
#[test]
fn unborn_sha256_recovery_preserves_object_format() {
    let temp = tempfile::tempdir().unwrap();
    support::git(temp.path(), &["init", "-q", "--object-format=sha256"]);
    fs::write(temp.path().join("first"), "first").unwrap();
    let s = Snapshots::open(temp.path(), Duration::from_secs(30)).unwrap();
    let snapshot = save(&s);
    let other = tempfile::tempdir().unwrap();
    let dest = other.path().join("recovery");
    s.restore(&snapshot.id, Some(&dest)).unwrap();
    assert_eq!(
        support::git(&dest, &["rev-parse", "--show-object-format"]),
        "sha256"
    );
    assert_eq!(fs::read(dest.join("first")).unwrap(), b"first");
}
#[test]
fn snapshot_survives_pool_release_and_slot_reuse() {
    use groove_core::{Pool, PoolOptions};
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
    let mut lease = pool.acquire(Default::default()).unwrap();
    let path = lease.info().worktree_path.clone();
    fs::write(path.join("tracked.txt"), "task result\n").unwrap();
    fs::write(path.join("new-result"), "new result\n").unwrap();
    let snapshots = Snapshots::open(&path, Duration::from_secs(30)).unwrap();
    let snapshot = save(&snapshots);
    lease.release().unwrap();
    let mut next = pool.acquire(Default::default()).unwrap();
    assert_eq!(
        fs::read(next.info().worktree_path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    let recovered = repo.temp.path().join("saved-task");
    service(&repo)
        .restore(&snapshot.id, Some(&recovered))
        .unwrap();
    assert_eq!(
        fs::read(recovered.join("tracked.txt")).unwrap(),
        b"task result\n"
    );
    assert_eq!(
        fs::read(recovered.join("new-result")).unwrap(),
        b"new result\n"
    );
    next.release().unwrap();
}
