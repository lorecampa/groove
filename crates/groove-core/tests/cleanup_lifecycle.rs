mod support;
use groove_core::{AcquireOptions, CleanupPolicy, Pool, PoolOptions, ReservationOptions};
use std::fs;
use support::Repo;

#[test]
fn pristine_commit_switches_replace_ignored_directory_collisions() {
    if support::in_subprocess("pristine_commit_switches_replace_ignored_directory_collisions") {
        assert_commit_switches(CleanupPolicy::Pristine);
    }
}

#[test]
fn warm_commit_switches_replace_ignored_directory_collisions() {
    if support::in_subprocess("warm_commit_switches_replace_ignored_directory_collisions") {
        assert_commit_switches(CleanupPolicy::Warm);
    }
}

fn assert_commit_switches(cleanup_policy: CleanupPolicy) {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitignore"), "ignored/\ncollision/\n").unwrap();
    repo.git(&["add", ".gitignore"]);
    repo.git(&["commit", "-qm", "A ignores a generated directory"]);
    let a = repo.git(&["rev-parse", "HEAD"]);
    fs::write(repo.path.join(".gitignore"), "ignored/\n").unwrap();
    fs::write(repo.path.join("collision"), "tracked at B\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&[
        "commit",
        "-qm",
        "B tracks a file at the generated directory",
    ]);
    let b = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "-q", &a]);
    let source_index = fs::read(repo.path.join(".git/index")).unwrap();
    let source_refs = repo.git(&["show-ref"]);
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            cleanup_policy,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    let sibling = pool
        .reserve(ReservationOptions {
            acquire: AcquireOptions {
                base: a.clone(),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
    fs::write(
        sibling.worktree_path.join("tracked.txt"),
        "protected sibling\n",
    )
    .unwrap();
    let first = pool
        .acquire(AcquireOptions {
            base: a.clone(),
            ..Default::default()
        })
        .unwrap();
    let workspace = first.info().worktree_path.clone();
    fs::create_dir(workspace.join("collision")).unwrap();
    fs::write(
        workspace.join("collision/cache"),
        "unverified generated output\n",
    )
    .unwrap();
    fs::create_dir(workspace.join("ignored")).unwrap();
    fs::write(workspace.join("ignored/cache"), "unrelated cache\n").unwrap();
    fs::write(workspace.join("tracked.txt"), "old task\n").unwrap();
    drop(first);
    for base in [&b, &a] {
        let next = pool
            .acquire(AcquireOptions {
                base: base.clone(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(next.info().worktree_path, workspace);
        // Compare the complete tracked manifest and index against an independent
        // checkout. Git status alone can miss ignored collision residue.
        let oracle = repo.temp.path().join(format!("oracle-{base}"));
        repo.git(&[
            "worktree",
            "add",
            "--detach",
            oracle.to_str().unwrap(),
            base,
        ]);
        assert_eq!(support::git(&workspace, &["rev-parse", "HEAD"]), *base);
        assert_eq!(
            support::git(&workspace, &["ls-files", "--stage"]),
            support::git(&oracle, &["ls-files", "--stage"])
        );
        for file in support::git(&oracle, &["ls-files"]).lines() {
            assert_eq!(
                fs::read(workspace.join(file)).unwrap(),
                fs::read(oracle.join(file)).unwrap(),
                "{file}"
            );
        }
        assert_eq!(support::git(&workspace, &["status", "--porcelain"]), "");
        if base == &b {
            assert_eq!(
                fs::read(workspace.join("collision")).unwrap(),
                b"tracked at B\n"
            );
        } else {
            assert!(
                !workspace.join("collision").exists(),
                "B's tracked file must not become ignored residue at A"
            );
        }
        assert_eq!(
            workspace.join("ignored/cache").exists(),
            cleanup_policy == CleanupPolicy::Warm
        );
        if cleanup_policy == CleanupPolicy::Warm {
            assert_eq!(
                fs::read(workspace.join("ignored/cache")).unwrap(),
                b"unrelated cache\n"
            );
        }
        assert_eq!(
            fs::read(sibling.worktree_path.join("tracked.txt")).unwrap(),
            b"protected sibling\n"
        );
        assert_eq!(
            pool.protected_leases().unwrap()[0].lease_id,
            sibling.lease_id
        );
        drop(next);
    }
    assert_eq!(repo.git(&["rev-parse", "HEAD"]), a);
    assert_eq!(repo.git(&["show-ref"]), source_refs);
    assert_eq!(
        fs::read(repo.path.join(".git/index")).unwrap(),
        source_index
    );
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    assert!(!repo.path.join("collision").exists());
    pool.release_lease(sibling.lease_id).unwrap();
}
