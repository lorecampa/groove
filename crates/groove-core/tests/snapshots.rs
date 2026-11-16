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
fn recovery_rejects_destinations_inside_the_source_including_parent_aliases() {
    let repo = Repo::new();
    let snapshots = service(&repo);
    fs::write(repo.path.join("saved.txt"), b"saved bytes\n").unwrap();
    let saved = save(&snapshots);
    let alias = repo.temp.path().join("source-alias");
    symlink(&repo.path, &alias).unwrap();
    let registrations = repo.git(&["worktree", "list", "--porcelain"]);
    for destination in [repo.path.join("recovered"), alias.join("recovered")] {
        let error = snapshots
            .restore(&saved.id, Some(&destination))
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("source working tree"));
        assert!(!destination.exists());
        assert_eq!(
            repo.git(&["worktree", "list", "--porcelain"]),
            registrations
        );
        assert_eq!(
            fs::read(repo.path.join("saved.txt")).unwrap(),
            b"saved bytes\n"
        );
    }
    // Rejected recovery must not introduce a nested repository that blocks saves.
    save(&snapshots);
    let external = repo.temp.path().join("external-recovery");
    snapshots.restore(&saved.id, Some(&external)).unwrap();
    assert_eq!(
        fs::read(external.join("saved.txt")).unwrap(),
        b"saved bytes\n"
    );
}

#[test]
fn unfinished_git_operations_reject_capture_even_with_a_resolved_index() {
    let repo = Repo::new();
    let snapshots = service(&repo);
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let head = repo.git(&["rev-parse", "HEAD"]);
    fs::write(repo.path.join("pending.txt"), b"preserve pending work").unwrap();
    // These durable Git markers are the operation state omitted by snapshots.
    // Keep a conflict-free index to ensure the operation check itself refuses.
    for marker in [
        "rebase-merge",
        "rebase-apply",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "BISECT_LOG",
        "BISECT_START",
    ] {
        let path = repo.path.join(".git").join(marker);
        let directory = matches!(marker, "rebase-merge" | "rebase-apply" | "sequencer");
        if directory {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, &head).unwrap();
        }
        let error = snapshots.save(Default::default()).unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedConfiguration, "{marker}");
        assert!(error.message.contains("Complete or abort"), "{error:?}");
        assert!(path.exists());
        assert!(snapshots.list().unwrap().is_empty());
        assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
        assert_eq!(
            fs::read(repo.path.join("pending.txt")).unwrap(),
            b"preserve pending work"
        );
        if directory {
            fs::remove_dir(path).unwrap();
        } else {
            fs::remove_file(path).unwrap();
        }
    }
    save(&snapshots);
}

#[test]
fn external_recovery_uses_saved_rules_without_the_sources_later_working_attributes() {
    let repo = Repo::new();
    fs::write(repo.path.join("saved.x"), b"saved CRLF\r\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    fs::write(repo.path.join(".gitattributes"), b"*.x text eol=crlf\n").unwrap();
    let destination = repo.temp.path().join("external");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    assert_eq!(
        fs::read(destination.join("saved.x")).unwrap(),
        b"saved CRLF\r\n"
    );
    assert!(!destination.join(".gitattributes").exists());
    assert_eq!(
        fs::read(repo.path.join(".gitattributes")).unwrap(),
        b"*.x text eol=crlf\n"
    );
}

#[test]
fn tracked_crlf_edits_are_not_hidden_by_attribute_normalization() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.x text eol=lf\n").unwrap();
    fs::write(repo.path.join("physical.x"), b"same text\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "LF baseline"]);
    fs::write(repo.path.join("physical.x"), b"same text\r\n").unwrap();
    let error = service(&repo).save(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(error.message.contains("physical.x"));
}

#[test]
fn restore_rejects_saved_carriage_returns_under_new_text_rules() {
    let repo = Repo::new();
    fs::write(repo.path.join("saved.x"), b"saved CRLF\r\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    fs::write(repo.path.join(".git/info/attributes"), b"*.x text eol=lf\n").unwrap();
    fs::write(repo.path.join("saved.x"), b"later LF work\n").unwrap();
    let destination = repo.temp.path().join("recovery");
    for target in [Some(destination.as_path()), None] {
        let error = snapshots.restore(&saved.id, target).unwrap_err();
        assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
        assert!(error.message.contains("carriage returns"));
        assert!(!destination.exists());
        assert_eq!(
            fs::read(repo.path.join("saved.x")).unwrap(),
            b"later LF work\n"
        );
    }
}

#[test]
fn lf_text_attributes_roundtrip_special_filenames_in_place_and_to_a_new_directory() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.x text eol=lf\n").unwrap();
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "-qm", "LF rules"]);
    let names = ["space name.x", "line\nbreak.x", "2:name.x"];
    for name in names {
        fs::write(repo.path.join(name), b"exact LF bytes\nsecond line\n").unwrap();
    }
    repo.git(&["add", "--", "2:name.x", "line\nbreak.x"]);
    let original_index = fs::read(repo.path.join(".git/index")).unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    assert_eq!(
        fs::read(repo.path.join(".git/index")).unwrap(),
        original_index
    );
    let recovery = repo.temp.path().join("LF recovery");
    snapshots.restore(&saved.id, Some(&recovery)).unwrap();
    for name in names {
        assert_eq!(
            fs::read(recovery.join(name)).unwrap(),
            b"exact LF bytes\nsecond line\n"
        );
        fs::write(repo.path.join(name), b"later LF edit\n").unwrap();
    }
    snapshots.restore(&saved.id, None).unwrap();
    for name in names {
        assert_eq!(
            fs::read(repo.path.join(name)).unwrap(),
            b"exact LF bytes\nsecond line\n"
        );
    }
}

#[test]
fn crlf_under_lf_attributes_is_rejected_with_the_saved_filename() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.x text eol=lf\n").unwrap();
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "-qm", "LF rules"]);
    let path = repo.path.join("normalization.x");
    fs::write(&path, b"CRLF must not silently change\r\n").unwrap();
    let snapshots = service(&repo);
    let error = snapshots.save(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(error.message.contains("normalization.x"), "{error:?}");
    assert!(snapshots.list().unwrap().is_empty());
    assert_eq!(
        fs::read(path).unwrap(),
        b"CRLF must not silently change\r\n"
    );
}

#[test]
fn saving_attributes_uses_strict_whole_tree_validation() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.x text eol=lf\n").unwrap();
    fs::write(repo.path.join("unchanged.x"), b"unchanged\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "LF tree"]);
    fs::write(
        repo.path.join(".gitattributes"),
        b"*.x text eol=lf\n# saved attribute change\n",
    )
    .unwrap();
    let error = service(&repo).save(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(error.message.contains("unchanged.x"), "{error:?}");
    assert!(error.message.contains("attribute"));
    assert!(error.message.contains(".gitattributes"));
}

#[test]
fn in_place_restore_keeps_index_stats_so_later_saves_ignore_unchanged_crlf_rules() {
    // Real repositories (for example CPython's `*.bat text eol=crlf`) commit
    // converting rules for paths a task never edits. Restoring in place must not
    // leave every index entry stat-dirty: a later capture would then treat the
    // unchanged .gitattributes as saved and reject those rules in strict mode.
    let repo = Repo::new();
    fs::write(
        repo.path.join(".gitattributes"),
        b"*.bat text eol=crlf\n*.x text eol=lf\n",
    )
    .unwrap();
    fs::write(repo.path.join("run.bat"), b"echo unchanged\n").unwrap();
    fs::write(repo.path.join("edited.x"), b"base\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "converting rules"]);
    // Materialize the committed rule's CRLF working bytes, as a clone would.
    fs::remove_file(repo.path.join("run.bat")).unwrap();
    repo.git(&["checkout", "--", "run.bat"]);
    assert_eq!(
        fs::read(repo.path.join("run.bat")).unwrap(),
        b"echo unchanged\r\n"
    );
    let snapshots = service(&repo);
    let clean = save(&snapshots);
    fs::write(repo.path.join("edited.x"), b"task edit\n").unwrap();
    let dirty = save(&snapshots);
    snapshots.restore(&clean.id, None).unwrap();
    // Without a refresh, only paths whose stat data the restore genuinely
    // invalidated may appear; unchanged files keep their cached index stats.
    let stale = repo.git(&["diff-files", "--name-only"]);
    assert!(
        !stale
            .lines()
            .any(|path| path == ".gitattributes" || path == "run.bat"),
        "unchanged paths lost cached stats: {stale}"
    );
    assert_eq!(fs::read(repo.path.join("edited.x")).unwrap(), b"base\n");
    // The safety capture and later restores succeed under the committed rules.
    snapshots.restore(&dirty.id, None).unwrap();
    assert_eq!(
        fs::read(repo.path.join("edited.x")).unwrap(),
        b"task edit\n"
    );
    assert_eq!(
        fs::read(repo.path.join("run.bat")).unwrap(),
        b"echo unchanged\r\n"
    );
    save(&snapshots);
}

#[test]
fn unused_converting_paths_do_not_block_saving_other_paths() {
    let repo = Repo::new();
    fs::write(
        repo.path.join(".gitattributes"),
        b"*.blocked ident\n*.x text eol=lf\n",
    )
    .unwrap();
    fs::write(repo.path.join("unchanged.blocked"), b"no ident expansion\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "unrelated conversion"]);
    fs::write(repo.path.join("saved.x"), b"allowed\n").unwrap();
    save(&service(&repo));
}

#[test]
fn unreadable_untracked_directories_fail_capture_without_publishing() {
    if !support::in_subprocess("unreadable_untracked_directories_fail_capture_without_publishing") {
        return;
    }
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
    if !support::in_subprocess(
        "unresolved_merge_metadata_cannot_be_silently_omitted_from_a_snapshot",
    ) {
        return;
    }
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
    if !support::in_subprocess("roundtrip_preserves_index_files_and_head") {
        return;
    }
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
    if !support::in_subprocess("external_recovery_and_gc_keep_local_history") {
        return;
    }
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
    assert_eq!(
        s.restore(&snap.id, Some(&dest)).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}
#[test]
fn unborn_roundtrip_and_external_recovery() {
    if !support::in_subprocess("unborn_roundtrip_and_external_recovery") {
        return;
    }
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
    if !support::in_subprocess("delete_removes_only_the_selected_snapshot") {
        return;
    }
    let repo = Repo::new();
    let s = service(&repo);
    let a = save(&s);
    let b = save(&s);
    assert_eq!(s.list().unwrap().len(), 2);
    s.delete(&a.id, false).unwrap();
    assert_eq!(s.show(&a.id).unwrap_err().code, ErrorCode::InvalidArgument);
    assert!(s.show(&b.id).is_ok());
    assert_eq!(
        s.delete(&a.id, false).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
}
#[test]
fn file_modes_deletions_symlinks_and_platform_filenames() {
    if !support::in_subprocess("file_modes_deletions_symlinks_and_platform_filenames") {
        return;
    }
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
    if !support::in_subprocess("apfs_rejects_non_utf8_filenames_before_changing_workspace_contents")
    {
        return;
    }
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
    if !support::in_subprocess(
        "snapshots_preserve_actual_line_endings_under_every_autocrlf_setting",
    ) {
        return;
    }
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
    if !support::in_subprocess("snapshots_keep_symlinks_and_chmod_even_when_git_ignores_them") {
        return;
    }
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
    if !support::in_subprocess("unused_filter_definitions_do_not_prevent_checkpoints") {
        return;
    }
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
    if !support::in_subprocess("restore_rejects_new_filter_attributes_before_modifying_files") {
        return;
    }
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
    if !support::in_subprocess(
        "legacy_crlf_attributes_reject_capture_and_restore_before_changing_files",
    ) {
        return;
    }
    assert_conversion_rejected(&["*.txt crlf", "*.txt crlf=input"]);
}

#[test]
fn empty_attribute_values_cannot_hide_content_conversion() {
    if !support::in_subprocess("empty_attribute_values_cannot_hide_content_conversion") {
        return;
    }
    assert_conversion_rejected(&["*.txt diff= text"]);
}

#[test]
fn empty_nonconverting_attributes_and_unset_crlf_preserve_bytes() {
    if !support::in_subprocess("empty_nonconverting_attributes_and_unset_crlf_preserve_bytes") {
        return;
    }
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
    if !support::in_subprocess("rejects_large_files_filters_conflicts_and_changed_head") {
        return;
    }
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("new"), "large").unwrap();
    assert_eq!(
        s.save(SaveOptions {
            max_file_bytes: 1,
            ..Default::default()
        })
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    assert!(s.list().unwrap().is_empty());
    let snap = save(&s);
    repo.git(&["commit", "--allow-empty", "-qm", "changed"]);
    assert_eq!(
        s.restore(&snap.id, None).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    repo.git(&["config", "filter.lfs.clean", "cat"]);
    fs::write(repo.path.join(".gitattributes"), "*.txt filter=lfs\n").unwrap();
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
}
#[test]
fn no_change_reuses_file_trees_and_diff_includes_untracked() {
    if !support::in_subprocess("no_change_reuses_file_trees_and_diff_includes_untracked") {
        return;
    }
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
    if !support::in_subprocess("ignored_collision_is_never_overwritten") {
        return;
    }
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join("collision"), "saved").unwrap();
    let snap = save(&s);
    fs::write(repo.path.join(".git/info/exclude"), "collision\n").unwrap();
    fs::write(repo.path.join("collision"), "precious ignored").unwrap();
    assert_eq!(
        s.restore(&snap.id, None).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        fs::read(repo.path.join("collision")).unwrap(),
        b"precious ignored"
    );
}
#[test]
fn linked_worktree_snapshots_are_shared() {
    if !support::in_subprocess("linked_worktree_snapshots_are_shared") {
        return;
    }
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
    if !support::in_subprocess("sparse_checkout_preserves_outside_entries") {
        return;
    }
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
    if !support::in_subprocess("malformed_manifest_and_conversion_attributes_fail_closed") {
        return;
    }
    let repo = Repo::new();
    let s = service(&repo);
    fs::write(repo.path.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    fs::remove_file(repo.path.join(".gitattributes")).unwrap();
    let snap = save(&s);
    repo.git(&[
        "update-ref",
        &format!("refs/groove/snapshots/{}", snap.id),
        "HEAD",
    ]);
    assert_eq!(s.show(&snap.id).unwrap_err().code, ErrorCode::ProtocolError);
}
#[test]
fn index_flags_and_gitlinks_fail_without_publication() {
    if !support::in_subprocess("index_flags_and_gitlinks_fail_without_publication") {
        return;
    }
    let repo = Repo::new();
    let s = service(&repo);
    repo.git(&["update-index", "--assume-unchanged", "tracked.txt"]);
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    repo.git(&["update-index", "--no-assume-unchanged", "tracked.txt"]);
    let head = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&[
        "update-index",
        "--add",
        "--cacheinfo",
        &format!("160000,{head},module"),
    ]);
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert!(s.list().unwrap().is_empty());
}
#[test]
fn publication_failure_does_not_mutate_workspace() {
    if !support::in_subprocess("publication_failure_does_not_mutate_workspace") {
        return;
    }
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
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::GitError
    );
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"unsaved\n"
    );
}
#[test]
fn unresolved_merge_is_rejected() {
    if !support::in_subprocess("unresolved_merge_is_rejected") {
        return;
    }
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
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
    assert!(s.list().unwrap().is_empty());
}
#[test]
fn tracked_files_under_ignored_directories_are_captured() {
    if !support::in_subprocess("tracked_files_under_ignored_directories_are_captured") {
        return;
    }
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
    if !support::in_subprocess("file_directory_transitions_preserve_only_selected_files") {
        return;
    }
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
    if !support::in_subprocess(
        "deletion_releases_unique_objects_without_retaining_snapshot_history",
    ) {
        return;
    }
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
    if !support::in_subprocess("split_index_and_intent_to_add_are_handled_explicitly") {
        return;
    }
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
    assert_eq!(
        s.save(Default::default()).unwrap_err().code,
        ErrorCode::UnsupportedConfiguration
    );
}
#[test]
fn unborn_sha256_recovery_preserves_object_format() {
    if !support::in_subprocess("unborn_sha256_recovery_preserves_object_format") {
        return;
    }
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
    if !support::in_subprocess("snapshot_survives_pool_release_and_slot_reuse") {
        return;
    }
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

#[test]
fn unchanged_crlf_base_files_do_not_block_snapshot_recovery() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.bat text eol=crlf\n").unwrap();
    fs::write(repo.path.join("install.bat"), b"base\r\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "CRLF base"]);
    fs::write(repo.path.join("saved.txt"), b"task work\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    let destination = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    fs::write(repo.path.join("saved.txt"), b"later work\n").unwrap();
    snapshots.restore(&saved.id, None).unwrap();
    for root in [&destination, &repo.path] {
        assert_eq!(fs::read(root.join("saved.txt")).unwrap(), b"task work\n");
        assert_eq!(fs::read(root.join("install.bat")).unwrap(), b"base\r\n");
    }
}

#[test]
fn core_eol_cannot_change_saved_lf_bytes() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"* text=auto\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "automatic text"]);
    repo.git(&["config", "core.eol", "crlf"]);
    fs::write(repo.path.join("saved.txt"), b"saved LF\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    let destination = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    assert_eq!(
        fs::read(destination.join("saved.txt")).unwrap(),
        b"saved LF\n"
    );
}

#[test]
fn lone_carriage_returns_fail_capture_before_publication() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.txt text eol=lf\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "text rules"]);
    fs::write(repo.path.join("saved.txt"), b"lone\rcarriage return\n").unwrap();
    let snapshots = service(&repo);
    let error = snapshots.save(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(snapshots.list().unwrap().is_empty());
    assert_eq!(
        fs::read(repo.path.join("saved.txt")).unwrap(),
        b"lone\rcarriage return\n"
    );
}

#[test]
fn rejected_restore_preflights_do_not_accumulate_safety_snapshots() {
    let repo = Repo::new();
    let snapshots = service(&repo);
    fs::write(repo.path.join("ignored"), b"saved before it was ignored").unwrap();
    let saved = save(&snapshots);
    fs::remove_file(repo.path.join("ignored")).unwrap();
    fs::write(repo.path.join(".gitignore"), b"ignored\n").unwrap();
    fs::write(repo.path.join("ignored"), b"preserve ignored file").unwrap();
    let count = snapshots.list().unwrap().len();
    for _ in 0..3 {
        let error = snapshots.restore(&saved.id, None).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.details.safety_snapshot_id.is_none());
    }
    assert_eq!(snapshots.list().unwrap().len(), count);
    assert_eq!(
        fs::read(repo.path.join("ignored")).unwrap(),
        b"preserve ignored file"
    );
}

#[test]
fn foreign_refs_without_snapshot_ids_do_not_break_listing() {
    let repo = Repo::new();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    repo.git(&["update-ref", "refs/groove/snapshots/foreign", "HEAD"]);
    let listed = snapshots.list().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, saved.id);
    assert_eq!(
        repo.git(&["rev-parse", "refs/groove/snapshots/foreign"]),
        repo.git(&["rev-parse", "HEAD"])
    );
}

#[test]
fn clean_crlf_working_bytes_cannot_be_silently_saved_as_lf() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"* text=auto\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "automatic text"]);
    repo.git(&["config", "core.eol", "crlf"]);
    fs::remove_file(repo.path.join("tracked.txt")).unwrap();
    repo.git(&["checkout", "--", "tracked.txt"]);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\r\n"
    );
    assert!(repo.git(&["status", "--porcelain"]).is_empty());
    let snapshots = service(&repo);
    let error = snapshots.save(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(snapshots.list().unwrap().is_empty());
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\r\n"
    );
}

#[test]
fn auto_text_preserves_binary_lone_cr_and_legacy_crlf_bytes() {
    let repo = Repo::new();
    fs::write(repo.path.join("legacy.txt"), b"legacy\r\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "legacy CRLF blob"]);
    fs::write(repo.path.join(".gitattributes"), b"* text=auto\n").unwrap();
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "-qm", "automatic text"]);
    let files: &[(&str, &[u8])] = &[
        ("image.png", b"\x89PNG\r\n\x1a\n\0\0data"),
        ("lone.txt", b"one\rtwo\n"),
        ("legacy.txt", b"legacy\r\nextra\r\n"),
        ("tracked.txt", b"edited\n"),
    ];
    for (name, bytes) in files {
        fs::write(repo.path.join(name), bytes).unwrap();
    }
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    let destination = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    fs::write(repo.path.join("tracked.txt"), b"later\n").unwrap();
    snapshots.restore(&saved.id, None).unwrap();
    for root in [&repo.path, &destination] {
        for (name, bytes) in files {
            assert_eq!(fs::read(root.join(name)).unwrap(), *bytes, "{name}");
        }
    }
    fs::write(repo.path.join("tracked.txt"), b"baseline\r\n").unwrap();
    let error = snapshots.save(Default::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert!(error.message.contains("tracked.txt"), "{error}");
}

#[test]
fn unchanged_legacy_crlf_under_auto_text_does_not_block_other_edits() {
    let repo = Repo::new();
    fs::write(repo.path.join("legacy.txt"), b"legacy\r\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "legacy CRLF blob"]);
    fs::write(repo.path.join(".gitattributes"), b"* text=auto\n").unwrap();
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "-qm", "automatic text"]);
    fs::write(repo.path.join("tracked.txt"), b"edited\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    let destination = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    assert_eq!(
        fs::read(destination.join("legacy.txt")).unwrap(),
        b"legacy\r\n"
    );
    assert_eq!(
        fs::read(destination.join("tracked.txt")).unwrap(),
        b"edited\n"
    );
}

#[test]
fn automatic_text_keeps_symlinks_without_an_eol_classification() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"* text=auto\n").unwrap();
    symlink("tracked.txt", repo.path.join("link.txt")).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "automatic text with symlink"]);
    fs::write(repo.path.join("tracked.txt"), b"changed\n").unwrap();
    let snapshots = service(&repo);
    let saved = save(&snapshots);
    let destination = repo.temp.path().join("recovered");
    snapshots.restore(&saved.id, Some(&destination)).unwrap();
    snapshots.restore(&saved.id, None).unwrap();
    for root in [&repo.path, &destination] {
        assert_eq!(
            fs::read_link(root.join("link.txt")).unwrap(),
            std::path::Path::new("tracked.txt")
        );
        assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"changed\n");
    }
}
