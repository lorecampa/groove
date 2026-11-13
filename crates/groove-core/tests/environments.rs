//! Repository layouts, Git preferences and out-of-band user actions that a pool
//! must survive. Each scenario uses real Git in a disposable repository and checks
//! the observable result: the checkout handed out, Git's own view of branches and
//! registrations, and typed errors that leave reservations intact.

mod support;
use groove_core::{
    snapshot::SaveOptions, ErrorCode, Pool, PoolOptions, RecoveryOutcome, ReservationOptions,
    SlotStatus,
};
use std::{fs, path::Path};
use support::{git, Repo};

fn pool(path: &Path, capacity: usize) -> Pool {
    Pool::init(
        path,
        PoolOptions {
            capacity,
            worktree_root: Some(support::worktree_root(path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(path).unwrap()
}

fn task(label: &str) -> ReservationOptions {
    ReservationOptions {
        task: Some(label.into()),
        ..Default::default()
    }
}

/// Whether the installed Git is at least `major.minor`. Features newer than the
/// supported minimum (2.40) are exercised only where Git provides them.
fn git_at_least(major: u32, minor: u32) -> bool {
    let version = git(Path::new("."), &["--version"]);
    let mut parts = version
        .trim_start_matches("git version ")
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0)) >= (major, minor)
}

/// A disposable repository with one commit, created with extra `git init` options.
fn repo_with(init: &[&str]) -> Repo {
    let repo = Repo::new();
    fs::remove_dir_all(&repo.path).unwrap();
    fs::create_dir(&repo.path).unwrap();
    let mut args = vec!["init", "-q"];
    args.extend_from_slice(init);
    repo.git(&args);
    repo.git(&["config", "user.name", "Groove Test"]);
    repo.git(&["config", "user.email", "test@example.invalid"]);
    fs::write(repo.path.join("tracked.txt"), "baseline\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "baseline"]);
    repo
}

/// Reserve, dirty, finish and reacquire: the full cycle every layout must support.
fn assert_full_cycle(repo: &Repo) {
    let pool = pool(&repo.path, 1);
    let lease = pool.reserve(task("cycle")).unwrap();
    fs::write(lease.worktree_path.join("tracked.txt"), "task work\n").unwrap();
    fs::write(lease.worktree_path.join("new.txt"), "untracked\n").unwrap();
    let finished = pool.finish(lease.lease_id, SaveOptions::default()).unwrap();
    assert!(finished.snapshot.pinned);
    let next = pool.reserve(task("next")).unwrap();
    assert_eq!(next.slot_id, lease.slot_id);
    assert_eq!(next.recovery, RecoveryOutcome::Sanitized);
    assert_eq!(
        fs::read_to_string(next.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert!(!next.worktree_path.join("new.txt").exists());
    // The finished work is still recoverable after the slot was reused.
    let recovered = repo.temp.path().join("recovered");
    groove_core::snapshot::Snapshots::open(&repo.path, groove_core::DEFAULT_GIT_TIMEOUT)
        .unwrap()
        .restore(&finished.snapshot.id, Some(&recovered))
        .unwrap();
    assert_eq!(
        fs::read_to_string(recovered.join("tracked.txt")).unwrap(),
        "task work\n"
    );
}

#[test]
fn reftable_repositories_support_the_full_cycle() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("reftable_repositories_support_the_full_cycle") {
        return;
    }
    if !git_at_least(2, 45) {
        eprintln!("skipped: reftable needs Git 2.45");
        return;
    }
    // Reftable keeps a placeholder in each worktree's HEAD file, so detachment
    // must be read through Git rather than from that file.
    assert_full_cycle(&repo_with(&["--ref-format=reftable"]));
}

#[test]
fn separate_git_dir_repositories_support_the_full_cycle() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("separate_git_dir_repositories_support_the_full_cycle") {
        return;
    }
    let repo = Repo::new();
    let checkout = repo.temp.path().join("separate checkout");
    let gitdir = repo.temp.path().join("separate.git");
    git(
        repo.temp.path(),
        &[
            "init",
            "-q",
            "--separate-git-dir",
            gitdir.to_str().unwrap(),
            checkout.to_str().unwrap(),
        ],
    );
    git(&checkout, &["config", "user.name", "Groove Test"]);
    git(&checkout, &["config", "user.email", "test@example.invalid"]);
    fs::write(checkout.join("tracked.txt"), "baseline\n").unwrap();
    git(&checkout, &["add", "."]);
    git(&checkout, &["commit", "-qm", "baseline"]);
    let pool = pool(&checkout, 1);
    let lease = pool.reserve(task("separate")).unwrap();
    assert!(lease.worktree_path.starts_with(&gitdir));
    pool.release_lease(lease.lease_id).unwrap();
    assert_eq!(
        pool.reserve(task("again")).unwrap().recovery,
        RecoveryOutcome::Sanitized
    );
}

#[test]
fn relative_worktree_preference_does_not_break_slots() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("relative_worktree_preference_does_not_break_slots") {
        return;
    }
    if !git_at_least(2, 48) {
        eprintln!("skipped: worktree.useRelativePaths needs Git 2.48");
        return;
    }
    let repo = Repo::new();
    repo.git(&["config", "worktree.useRelativePaths", "true"]);
    let pool = pool(&repo.path, 1);
    let lease = pool.reserve(task("relative")).unwrap();
    // Groove's own registrations stay absolute so they can be validated.
    let backlink = fs::read_to_string(repo.path.join(".git/worktrees/wt-0001/gitdir")).unwrap();
    assert_eq!(
        backlink.trim_end(),
        lease.worktree_path.join(".git").to_str().unwrap()
    );
    pool.release_lease(lease.lease_id).unwrap();
    // Unsetting the preference later (Git keeps its extension) changes nothing.
    repo.git(&["config", "--unset", "worktree.useRelativePaths"]);
    assert_eq!(
        pool.reserve(task("after")).unwrap().recovery,
        RecoveryOutcome::Sanitized
    );
}

#[test]
fn relative_registrations_written_by_git_are_validated_and_rebuilt() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("relative_registrations_written_by_git_are_validated_and_rebuilt") {
        return;
    }
    if !git_at_least(2, 48) {
        eprintln!("skipped: relative worktree links need Git 2.48");
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo.path, 1);
    let lease = pool.reserve(task("first")).unwrap();
    pool.release_lease(lease.lease_id).unwrap();
    // `git worktree repair` rewrites both links relative, as Git does for
    // repositories that opt into relative paths.
    repo.git(&[
        "-c",
        "worktree.useRelativePaths=true",
        "worktree",
        "repair",
        lease.worktree_path.to_str().unwrap(),
    ]);
    let pointer = fs::read_to_string(lease.worktree_path.join(".git")).unwrap();
    assert!(pointer.starts_with("gitdir: .."), "{pointer}");
    // A relative but valid registration is normalized in place.
    let second = pool.reserve(task("second")).unwrap();
    assert_eq!(second.recovery, RecoveryOutcome::Sanitized);
    pool.release_lease(second.lease_id).unwrap();
    repo.git(&[
        "-c",
        "worktree.useRelativePaths=true",
        "worktree",
        "repair",
        lease.worktree_path.to_str().unwrap(),
    ]);
    // A deleted slot with a relative registration is unregistered and rebuilt,
    // instead of leaving a stale registration that blocks every acquisition.
    fs::remove_dir_all(&lease.worktree_path).unwrap();
    let third = pool.reserve(task("third")).unwrap();
    assert_eq!(third.recovery, RecoveryOutcome::Created);
    assert_eq!(
        fs::read_to_string(third.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    let registrations: Vec<_> = fs::read_dir(repo.path.join(".git/worktrees"))
        .unwrap()
        .collect();
    assert_eq!(registrations.len(), 1, "stale registration left behind");
}

#[test]
fn default_base_is_the_callers_head_in_a_linked_worktree() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("default_base_is_the_callers_head_in_a_linked_worktree") {
        return;
    }
    let repo = Repo::new();
    let main_head = repo.git(&["rev-parse", "HEAD"]);
    pool(&repo.path, 3);
    let linked = repo.temp.path().join("feature checkout");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "feature",
        linked.to_str().unwrap(),
    ]);
    fs::write(linked.join("feature.txt"), "feature\n").unwrap();
    git(&linked, &["add", "."]);
    git(&linked, &["commit", "-qm", "feature"]);
    let feature_head = git(&linked, &["rev-parse", "HEAD"]);
    assert_ne!(feature_head, main_head);

    let from_linked = Pool::open(&linked)
        .unwrap()
        .reserve(task("linked"))
        .unwrap();
    assert_eq!(from_linked.base_oid.as_str(), feature_head);
    assert!(from_linked.worktree_path.join("feature.txt").exists());

    let from_main = Pool::open(&repo.path)
        .unwrap()
        .reserve(task("main"))
        .unwrap();
    assert_eq!(from_main.base_oid.as_str(), main_head);

    // From inside a slot, HEAD is that slot's commit.
    git(
        &from_main.worktree_path,
        &["commit", "-q", "--allow-empty", "-m", "slot work"],
    );
    let slot_head = git(&from_main.worktree_path, &["rev-parse", "HEAD"]);
    let from_slot = Pool::open(&from_main.worktree_path)
        .unwrap()
        .reserve(task("nested"))
        .unwrap();
    assert_eq!(from_slot.base_oid.as_str(), slot_head);
}

#[test]
fn an_unborn_head_reports_that_there_are_no_commits() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("an_unborn_head_reports_that_there_are_no_commits") {
        return;
    }
    let repo = Repo::new();
    let empty = repo.temp.path().join("empty");
    fs::create_dir(&empty).unwrap();
    git(&empty, &["init", "-q"]);
    let pool = pool(&empty, 1);
    let error = pool.reserve(task("first")).unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidRevision);
    assert!(error.message.contains("no commits"), "{}", error.message);
    assert!(pool.protected_leases().unwrap().is_empty());
}

#[test]
fn release_and_finish_free_branches_created_in_the_slot() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("release_and_finish_free_branches_created_in_the_slot") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo.path, 2);
    for (label, finish) in [("released", false), ("finished", true)] {
        let lease = pool.reserve(task(label)).unwrap();
        let branch = format!("work-{label}");
        git(&lease.worktree_path, &["switch", "-q", "-c", &branch]);
        fs::write(lease.worktree_path.join("work.txt"), label).unwrap();
        git(&lease.worktree_path, &["add", "work.txt"]);
        git(&lease.worktree_path, &["commit", "-qm", label]);
        let commit = git(&lease.worktree_path, &["rev-parse", "HEAD"]);
        // Uncommitted edits survive the release untouched until the next acquisition.
        fs::write(lease.worktree_path.join("tracked.txt"), "uncommitted\n").unwrap();
        if finish {
            pool.finish(lease.lease_id, SaveOptions::default()).unwrap();
        } else {
            pool.release_lease(lease.lease_id).unwrap();
        }
        // The branch keeps its commit but is no longer checked out in the slot:
        // the main checkout can switch to it, and it can be deleted.
        assert_eq!(repo.git(&["rev-parse", &branch]), commit);
        assert_eq!(git(&lease.worktree_path, &["rev-parse", "HEAD"]), commit);
        assert!(command_fails(
            &lease.worktree_path,
            &["symbolic-ref", "-q", "HEAD"]
        ));
        assert_eq!(
            fs::read_to_string(lease.worktree_path.join("tracked.txt")).unwrap(),
            "uncommitted\n"
        );
        repo.git(&["switch", "-q", &branch]);
        repo.git(&["switch", "-q", "-"]);
        repo.git(&["branch", "-q", "-D", &branch]);
    }
}

fn command_fails(path: &Path, args: &[&str]) -> bool {
    !support::command(path, "git")
        .args(args)
        .output()
        .unwrap()
        .status
        .success()
}

#[test]
fn a_worktree_root_inside_a_checkout_is_rejected() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("a_worktree_root_inside_a_checkout_is_rejected") {
        return;
    }
    let repo = Repo::new();
    let linked = repo.temp.path().join("linked");
    repo.git(&[
        "worktree",
        "add",
        "-q",
        "--detach",
        linked.to_str().unwrap(),
    ]);
    for root in [repo.path.join("pool"), linked.join("nested/pool")] {
        let error = Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(root.clone()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{root:?}");
        // Rejected before any external storage or initialization intent exists.
        assert!(!root.exists());
        assert!(!repo.state().join("initializing.json").exists());
        assert!(!repo.state().join("config.json").exists());
    }
    // A sibling directory outside every checkout is accepted.
    let outside = repo.temp.path().join("pool outside");
    let created = Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(outside.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(created.worktree_root, outside);
}

#[test]
fn a_reserved_checkout_removed_outside_groove_needs_explicit_release() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("a_reserved_checkout_removed_outside_groove_needs_explicit_release")
    {
        return;
    }
    for removal in ["rm -rf", "git worktree remove --force --force"] {
        let repo = Repo::new();
        let pool = pool(&repo.path, 1);
        let lease = pool.reserve(task("lost")).unwrap();
        if removal == "rm -rf" {
            fs::remove_dir_all(&lease.worktree_path).unwrap();
        } else {
            repo.git(&[
                "worktree",
                "remove",
                "--force",
                "--force",
                lease.worktree_path.to_str().unwrap(),
            ]);
        }
        // Neither finish nor a saved-work check can proceed, and both say why.
        let finish = pool
            .finish(lease.lease_id, SaveOptions::default())
            .unwrap_err();
        assert_eq!(
            finish.code,
            ErrorCode::RecoveryFailed,
            "{removal}: {finish}"
        );
        assert_eq!(
            finish.details.worktree_path.as_deref(),
            Some(lease.worktree_path.as_path())
        );
        let check = pool
            .ensure_quiescent(lease.lease_id, true, groove_core::DEFAULT_GIT_TIMEOUT)
            .unwrap_err();
        assert_eq!(check.code, ErrorCode::RecoveryFailed, "{removal}");
        // The reservation is retained and reported as needing attention.
        assert_eq!(pool.lease_by_task("lost").unwrap().lease_id, lease.lease_id);
        let slot = pool.inspect_slot(&lease.slot_id).unwrap();
        assert_eq!(slot.status, SlotStatus::Unhealthy, "{removal}");
        assert!(slot.failure.is_some());
        // Explicit release (the CLI's --force path) discards it; the slot is rebuilt.
        pool.release_lease(lease.lease_id).unwrap();
        let next = pool.reserve(task("next")).unwrap();
        assert_eq!(next.slot_id, lease.slot_id);
        assert_eq!(
            fs::read_to_string(next.worktree_path.join("tracked.txt")).unwrap(),
            "baseline\n"
        );
    }
}

#[test]
fn slot_failures_keep_gits_explanation() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("slot_failures_keep_gits_explanation") {
        return;
    }
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root ignores directory permissions");
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo.path, 1);
    let root = repo.state().join("worktrees");
    // Git cannot create the slot directory beneath a read-only root.
    let mut permissions = fs::metadata(&root).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o500);
    fs::set_permissions(&root, permissions.clone()).unwrap();
    let error = pool.reserve(task("blocked")).unwrap_err();
    std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
    fs::set_permissions(&root, permissions).unwrap();
    assert_eq!(error.code, ErrorCode::PoolUnhealthy);
    let reason = &error.details.capacity.as_ref().unwrap().failed_slots
        [&"wt-0001".parse::<groove_core::SlotId>().unwrap()];
    // The generic summary is followed by Git's own diagnostic line.
    assert!(reason.contains("fatal:"), "{reason}");
    assert!(pool.reserve(task("unblocked")).is_ok());
}

#[test]
fn a_moved_repository_reports_its_original_location() {
    // Another fixture's fork can briefly inherit this fixture's lock descriptors
    // until exec, making a release or finish see a phantom owner.
    if !support::in_subprocess("a_moved_repository_reports_its_original_location") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo.path, 1);
    pool.reserve(task("before move")).unwrap();
    let moved = repo.temp.path().join("moved");
    fs::rename(&repo.path, &moved).unwrap();
    let error = Pool::open(&moved).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigConflict);
    assert_eq!(
        error.details.path.as_deref(),
        Some(repo.path.join(".git/groove/worktrees").as_path())
    );
    // Moving it back restores the untouched pool and its reservation.
    fs::rename(&moved, &repo.path).unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    assert!(pool.lease_by_task("before move").is_ok());
}
