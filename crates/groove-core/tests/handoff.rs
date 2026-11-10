mod support;
use groove_core::{
    snapshot::{SaveOptions, Snapshots},
    Pool, PoolOptions,
};
use std::{fs, time::Duration};
use support::{git, Repo};

const TIMEOUT: Duration = Duration::from_secs(10);

#[test]
fn finished_snapshots_restore_after_slot_reuse_without_changing_the_successor() {
    if !support::in_subprocess(
        "finished_snapshots_restore_after_slot_reuse_without_changing_the_successor",
    ) {
        return;
    }
    for format in ["sha1", "sha256"] {
        let repo = Repo::format(format);
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&repo.path).unwrap();
        let lease = pool.reserve(Default::default()).unwrap();
        let head = git(&lease.worktree_path, &["rev-parse", "HEAD"]);
        fs::write(lease.worktree_path.join("tracked.txt"), "task result\n").unwrap();
        fs::write(lease.worktree_path.join("new.bin"), [0, 1, 2, 0, 255]).unwrap();
        let index = git(&lease.worktree_path, &["write-tree"]);
        let finish = pool.finish(lease.lease_id, Default::default()).unwrap();
        assert!(finish.snapshot.pinned);
        let id = &finish.snapshot.id;
        let next = pool.reserve(Default::default()).unwrap();
        fs::write(next.worktree_path.join("tracked.txt"), "successor result\n").unwrap();
        let retried = pool.finish(lease.lease_id, Default::default()).unwrap();
        assert_eq!(retried.snapshot.id, *id);
        let recovered = repo.temp.path().join("recovered");
        Snapshots::open(&repo.path, TIMEOUT)
            .unwrap()
            .restore(id, Some(&recovered))
            .unwrap();
        // The successor's checkout is untouched.
        assert_eq!(git(&next.worktree_path, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&next.worktree_path, &["write-tree"]), index);
        assert_eq!(
            fs::read(next.worktree_path.join("tracked.txt")).unwrap(),
            b"successor result\n"
        );
        assert_eq!(
            fs::read(recovered.join("tracked.txt")).unwrap(),
            b"task result\n"
        );
        assert_eq!(
            fs::read(recovered.join("new.bin")).unwrap(),
            [0, 1, 2, 0, 255]
        );
    }
}

#[test]
fn unborn_snapshots_restore_into_a_new_repository() {
    if !support::in_subprocess("unborn_snapshots_restore_into_a_new_repository") {
        return;
    }
    let repo = Repo::new();
    repo.git(&["checkout", "--orphan", "empty"]);
    repo.git(&["rm", "-rf", "."]);
    fs::write(repo.path.join("new.txt"), "unborn result\n").unwrap();
    let snapshots = Snapshots::open(&repo.path, TIMEOUT).unwrap();
    let saved = snapshots.save(SaveOptions::default()).unwrap();
    let target = repo.temp.path().join("unborn");
    snapshots.restore(&saved.id, Some(&target)).unwrap();
    assert_eq!(
        fs::read(target.join("new.txt")).unwrap(),
        b"unborn result\n"
    );
}
