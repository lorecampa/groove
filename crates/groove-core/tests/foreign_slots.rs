mod support;

use groove_core::{DestroyOptions, ErrorCode, Pool, PoolName, PoolOptions, RecoveryOutcome};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use support::{git, Repo};

fn pool(repo: &Repo, name: &str, capacity: usize) -> Pool {
    let name: PoolName = name.parse().unwrap();
    Pool::init_named(
        &repo.path,
        &name,
        PoolOptions {
            capacity,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open_named(&repo.path, &name).unwrap()
}

fn destroy(repo: &Repo, dry_run: bool) -> groove_core::Result<groove_core::DestroyResult> {
    Pool::destroy_named(
        &repo.path,
        &PoolName::default(),
        DestroyOptions {
            dry_run,
            discard_changes: true,
            ..Default::default()
        },
    )
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, Vec<u8>>) {
        if !path.exists() {
            return;
        }
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                read(root, &entry.path(), result);
            } else {
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_owned(),
                    fs::read(entry.path()).unwrap(),
                );
            }
        }
    }
    let mut result = BTreeMap::new();
    read(root, root, &mut result);
    result
}

fn locks(repo: &Repo) -> BTreeMap<PathBuf, (u64, u64)> {
    fs::read_dir(repo.state().join("locks"))
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            (entry.path(), (metadata.dev(), metadata.ino()))
        })
        .collect()
}

fn precious_commit(path: &Path) {
    git(path, &["config", "user.name", "Foreign Test"]);
    git(path, &["config", "user.email", "foreign@example.invalid"]);
    git(path, &["checkout", "-qb", "occupant-result"]);
    fs::write(path.join("precious-tracked"), "unique unpushed history\n").unwrap();
    git(path, &["add", "precious-tracked"]);
    git(path, &["commit", "-qm", "foreign result"]);
}

fn assert_precious(path: &Path, head: &str, refs: &str) {
    assert_eq!(fs::read(path.join("precious.txt")).unwrap(), b"keep me\n");
    assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(path, &["show-ref"]), refs);
    assert_eq!(
        git(path, &["show", "occupant-result:precious-tracked"]),
        "unique unpushed history"
    );
}

// All cases use real repositories and isolated processes: an unrelated fixture's
// fork must not keep a descriptor alive after explicit release in this fixture.
#[test]
fn foreign_repositories_survive_acquire_and_discarding_destroy() {
    if !support::in_subprocess("foreign_repositories_survive_acquire_and_discarding_destroy") {
        return;
    }
    for kind in ["clone", "bare", "linked", "separate"] {
        let repo = Repo::new();
        let p = pool(&repo, "default", 1);
        let original = p.reserve(Default::default()).unwrap();
        p.release_lease(original.lease_id).unwrap();
        let sibling = pool(&repo, "sibling", 1);
        let active = sibling.reserve(Default::default()).unwrap();
        fs::write(active.worktree_path.join("sibling.txt"), "sibling result").unwrap();
        let source = Repo::new();
        let slot = repo.slot(1);
        fs::remove_dir_all(&slot).unwrap();
        match kind {
            "clone" | "separate" => {
                git(
                    &repo.path,
                    &[
                        "clone",
                        "--no-local",
                        source.path.to_str().unwrap(),
                        slot.to_str().unwrap(),
                    ],
                );
                precious_commit(&slot);
                if kind == "separate" {
                    let store = repo.temp.path().join("foreign-store");
                    git(
                        &slot,
                        &["init", &format!("--separate-git-dir={}", store.display())],
                    );
                }
            }
            "bare" => {
                precious_commit(&source.path);
                git(
                    &repo.path,
                    &[
                        "clone",
                        "--bare",
                        "--no-local",
                        source.path.to_str().unwrap(),
                        slot.to_str().unwrap(),
                    ],
                );
            }
            "linked" => {
                precious_commit(&source.path);
                source.git(&[
                    "worktree",
                    "add",
                    "--detach",
                    slot.to_str().unwrap(),
                    "HEAD",
                ]);
            }
            _ => unreachable!(),
        }
        fs::write(slot.join("precious.txt"), "keep me\n").unwrap();
        let head = git(&slot, &["rev-parse", "HEAD"]);
        let refs = git(&slot, &["show-ref"]);
        let registrations = files(&repo.path.join(".git/worktrees"));
        let foreign_registrations = files(&source.path.join(".git/worktrees"));
        let lock_ids = locks(&repo);
        let lifecycle = fs::read(repo.state().join("lifecycle.json")).unwrap();
        for dry_run in [true, false] {
            let error = destroy(&repo, dry_run).unwrap_err();
            assert_eq!(error.code, ErrorCode::RecoveryFailed, "{kind}: {error}");
            assert_eq!(error.details.path.as_ref(), Some(&slot));
            assert_precious(&slot, &head, &refs);
            assert_eq!(files(&repo.path.join(".git/worktrees")), registrations);
            assert_eq!(
                fs::read(repo.state().join("lifecycle.json")).unwrap(),
                lifecycle
            );
        }
        let error = p.acquire(Default::default()).unwrap_err();
        assert_eq!(error.code, ErrorCode::PoolUnhealthy, "{kind}: {error}");
        assert_eq!(
            error.details.capacity.as_ref().unwrap().failed_slots.len(),
            1
        );
        let record: serde_json::Value =
            serde_json::from_slice(&fs::read(repo.state().join("slots/wt-0001.json")).unwrap())
                .unwrap();
        assert_eq!(record["phase"], "quarantined");
        assert!(!repo.state().join("slots/wt-0001.protected.json").exists());
        assert_eq!(locks(&repo), lock_ids);
        assert_precious(&slot, &head, &refs);
        assert_eq!(files(&repo.path.join(".git/worktrees")), registrations);
        assert_eq!(
            files(&source.path.join(".git/worktrees")),
            foreign_registrations
        );
        assert_eq!(
            fs::read(repo.state().join("lifecycle.json")).unwrap(),
            lifecycle
        );
        assert_eq!(
            fs::read(active.worktree_path.join("sibling.txt")).unwrap(),
            b"sibling result"
        );
        sibling.release_lease(active.lease_id).unwrap();
        assert_eq!(
            sibling.acquire(Default::default()).unwrap().info().recovery,
            RecoveryOutcome::Sanitized
        );
    }
}

#[test]
fn a_moved_sibling_is_preserved_until_restored_to_its_own_path() {
    if !support::in_subprocess("a_moved_sibling_is_preserved_until_restored_to_its_own_path") {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, "default", 1);
    drop(p.acquire(Default::default()).unwrap());
    let sibling = pool(&repo, "sibling", 1);
    let lease = sibling.acquire(Default::default()).unwrap();
    let sibling_path = lease.info().worktree_path.clone();
    precious_commit(&sibling_path);
    fs::write(sibling_path.join("precious.txt"), "keep me\n").unwrap();
    drop(lease);
    fs::remove_dir_all(repo.slot(1)).unwrap();
    fs::rename(&sibling_path, repo.slot(1)).unwrap();
    let registrations = files(&repo.path.join(".git/worktrees"));
    let head = git(&repo.slot(1), &["rev-parse", "HEAD"]);
    let refs = repo.git(&["show-ref"]);
    let lifecycle = fs::read(repo.state().join("lifecycle.json")).unwrap();
    let lock_ids = locks(&repo);
    assert_eq!(
        p.acquire(Default::default()).unwrap_err().code,
        ErrorCode::PoolUnhealthy
    );
    for dry_run in [true, false] {
        assert_eq!(
            destroy(&repo, dry_run).unwrap_err().code,
            ErrorCode::RecoveryFailed
        );
        assert_precious(&repo.slot(1), &head, &refs);
        assert_eq!(files(&repo.path.join(".git/worktrees")), registrations);
    }
    assert_eq!(
        fs::read(repo.state().join("lifecycle.json")).unwrap(),
        lifecycle
    );
    assert_eq!(locks(&repo), lock_ids);
    fs::rename(repo.slot(1), &sibling_path).unwrap();
    assert_precious(&sibling_path, &head, &refs);
    drop(sibling.acquire(Default::default()).unwrap());
    assert_eq!(
        p.acquire(Default::default()).unwrap().info().recovery,
        RecoveryOutcome::Created
    );
    assert_eq!(repo.git(&["rev-parse", "occupant-result"]), head);
}

#[test]
fn duplicate_claims_refuse_both_healthy_normalization_and_missing_slot_repair() {
    if !support::in_subprocess(
        "duplicate_claims_refuse_both_healthy_normalization_and_missing_slot_repair",
    ) {
        return;
    }
    for missing in [false, true] {
        let repo = Repo::new();
        let p = pool(&repo, "default", 1);
        let lease = p.acquire(Default::default()).unwrap();
        let admin = PathBuf::from(git(&repo.slot(1), &["rev-parse", "--absolute-git-dir"]));
        precious_commit(&repo.slot(1));
        fs::write(repo.slot(1).join("precious.txt"), "keep me\n").unwrap();
        drop(lease);
        let head = repo.git(&["rev-parse", "occupant-result"]);
        let refs = repo.git(&["show-ref"]);
        let duplicate = repo.path.join(".git/worktrees/duplicate-claim");
        fs::create_dir(&duplicate).unwrap();
        for (name, bytes) in files(&admin) {
            let path = duplicate.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        // The second claim is relative, as newer Git may write it. The slot's
        // original pointer remains valid: this must fail before in-place cleanup.
        fs::write(
            duplicate.join("gitdir"),
            "../../groove/worktrees/wt-0001/.git\n",
        )
        .unwrap();
        if missing {
            fs::remove_dir_all(repo.slot(1)).unwrap();
        }
        let registrations = files(&repo.path.join(".git/worktrees"));
        let lock_ids = locks(&repo);
        let lifecycle = fs::read(repo.state().join("lifecycle.json")).unwrap();
        assert_eq!(
            p.acquire(Default::default()).unwrap_err().code,
            ErrorCode::PoolUnhealthy
        );
        for dry_run in [true, false] {
            assert_eq!(
                destroy(&repo, dry_run).unwrap_err().code,
                ErrorCode::RecoveryFailed
            );
            if !missing {
                assert_precious(&repo.slot(1), &head, &refs);
            }
            assert_eq!(files(&repo.path.join(".git/worktrees")), registrations);
            assert_eq!(
                fs::read(repo.state().join("lifecycle.json")).unwrap(),
                lifecycle
            );
        }
        assert_eq!(locks(&repo), lock_ids);
        fs::remove_dir_all(duplicate).unwrap();
        assert!(p.acquire(Default::default()).is_ok());
        assert_eq!(repo.git(&["rev-parse", "occupant-result"]), head);
    }
}

#[test]
fn destroy_preflights_later_slots_before_discarding_any_owned_data() {
    if !support::in_subprocess("destroy_preflights_later_slots_before_discarding_any_owned_data") {
        return;
    }
    let repo = Repo::new();
    let p = pool(&repo, "default", 2);
    let first = p.acquire(Default::default()).unwrap();
    let second = p.acquire(Default::default()).unwrap();
    fs::write(repo.slot(1).join("precious.txt"), "owned residue").unwrap();
    drop((first, second));
    fs::remove_dir_all(repo.slot(2)).unwrap();
    fs::create_dir(repo.slot(2)).unwrap();
    git(&repo.slot(2), &["init", "-q"]);
    let registrations = files(&repo.path.join(".git/worktrees"));
    let lifecycle = fs::read(repo.state().join("lifecycle.json")).unwrap();
    assert_eq!(
        destroy(&repo, false).unwrap_err().code,
        ErrorCode::RecoveryFailed
    );
    assert_eq!(
        fs::read(repo.slot(1).join("precious.txt")).unwrap(),
        b"owned residue"
    );
    assert_eq!(files(&repo.path.join(".git/worktrees")), registrations);
    assert_eq!(
        fs::read(repo.state().join("lifecycle.json")).unwrap(),
        lifecycle
    );
    let healthy = p.acquire(Default::default()).unwrap();
    assert_eq!(healthy.info().slot_id.as_str(), "wt-0001");
    assert_eq!(healthy.info().recovery, RecoveryOutcome::Sanitized);
}

#[test]
fn damaged_own_checkouts_still_rebuild_and_nested_repositories_are_disposable() {
    if !support::in_subprocess(
        "damaged_own_checkouts_still_rebuild_and_nested_repositories_are_disposable",
    ) {
        return;
    }
    for damage in [
        "missing-pointer",
        "malformed-pointer",
        "index",
        "primary-pointer",
        "nested",
    ] {
        let repo = Repo::new();
        let p = pool(&repo, "default", 1);
        let lease = p.acquire(Default::default()).unwrap();
        let admin = PathBuf::from(git(&repo.slot(1), &["rev-parse", "--absolute-git-dir"]));
        repo.git(&["branch", "kept-result"]);
        fs::write(repo.slot(1).join("tracked.txt"), "discard own edits").unwrap();
        drop(lease);
        match damage {
            "missing-pointer" => fs::remove_file(repo.slot(1).join(".git")).unwrap(),
            "malformed-pointer" => fs::write(repo.slot(1).join(".git"), "broken").unwrap(),
            "primary-pointer" => fs::write(
                repo.slot(1).join(".git"),
                format!("gitdir: {}\n", repo.path.join(".git").display()),
            )
            .unwrap(),
            "index" => {
                fs::write(admin.join("index"), "broken index").unwrap();
                fs::write(admin.join("index.lock"), "interrupted").unwrap();
            }
            "nested" => {
                fs::create_dir(repo.slot(1).join("nested")).unwrap();
                git(&repo.slot(1).join("nested"), &["init", "-q"]);
            }
            _ => unreachable!(),
        }
        let lock_ids = locks(&repo);
        let next = p.acquire(Default::default()).unwrap();
        assert_eq!(
            next.info().recovery,
            if damage == "nested" {
                RecoveryOutcome::Sanitized
            } else {
                RecoveryOutcome::Rebuilt
            }
        );
        assert_eq!(
            fs::read(repo.slot(1).join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        assert!(!repo.slot(1).join("nested").exists());
        assert_eq!(
            repo.git(&["rev-parse", "kept-result"]),
            next.info().base_oid.as_str()
        );
        assert!(repo.git(&["status", "--porcelain"]).is_empty());
        assert_eq!(locks(&repo), lock_ids);
    }
}

#[cfg(feature = "test-hooks")]
#[test]
fn disposal_checkpoint_child() {
    let Some(path) = std::env::var_os("GROOVE_FOREIGN_SLOT_REPO") else {
        return;
    };
    let error = Pool::destroy_named(
        PathBuf::from(path),
        &PoolName::default(),
        DestroyOptions {
            discard_changes: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::RecoveryFailed);
}

#[cfg(feature = "test-hooks")]
#[test]
fn destroy_rechecks_the_occupant_after_publishing_deleting_and_on_retry() {
    if !support::in_subprocess(
        "destroy_rechecks_the_occupant_after_publishing_deleting_and_on_retry",
    ) {
        return;
    }
    use std::{os::unix::net::UnixListener, process::Command};
    let repo = Repo::new();
    let p = pool(&repo, "default", 1);
    drop(p.acquire(Default::default()).unwrap());
    let foreign = Repo::new();
    precious_commit(&foreign.path);
    fs::write(foreign.path.join("precious.txt"), "keep me\n").unwrap();
    let head = foreign.git(&["rev-parse", "HEAD"]);
    let refs = foreign.git(&["show-ref"]);
    let registrations = files(&repo.path.join(".git/worktrees"));
    let lock_ids = locks(&repo);
    let socket = repo.temp.path().join("checkpoint.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = support::TestChild::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "disposal_checkpoint_child", "--nocapture"])
            .env("GROOVE_FOREIGN_SLOT_REPO", &repo.path)
            .env("GROOVE_TEST_CHECKPOINT", "pool-deleting")
            .env("GROOVE_TEST_SOCKET", &socket),
    );
    let stream = child
        .checkpoint(&listener, "pool-deleting", support::PROCESS_TIMEOUT)
        .unwrap();
    let own = repo.temp.path().join("saved-own-slot");
    fs::rename(repo.slot(1), &own).unwrap();
    fs::rename(&foreign.path, repo.slot(1)).unwrap();
    drop(stream);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for _ in 0..2 {
        assert_eq!(
            destroy(&repo, false).unwrap_err().code,
            ErrorCode::RecoveryFailed
        );
        assert_precious(&repo.slot(1), &head, &refs);
        assert_eq!(files(&repo.path.join(".git/worktrees")), registrations);
        assert_eq!(locks(&repo), lock_ids);
        let lifecycle: serde_json::Value =
            serde_json::from_slice(&fs::read(repo.state().join("lifecycle.json")).unwrap())
                .unwrap();
        assert_eq!(lifecycle["phase"], "deleting");
    }
    fs::rename(repo.slot(1), &foreign.path).unwrap();
    fs::rename(own, repo.slot(1)).unwrap();
    assert!(destroy(&repo, false).unwrap().deleted);
    assert_precious(&foreign.path, &head, &refs);
    assert_eq!(locks(&repo), lock_ids);
}
