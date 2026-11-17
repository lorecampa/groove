mod support;
use support::{assert_success, Repo, TestChild};

#[test]
fn destroyed_root_can_be_claimed_by_another_repository_without_stale_retry_damage() {
    released_root_claim(false);
}

#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_root_claim_cleanup_resumes_after_durable_deletion() {
    released_root_claim(true);
}

fn released_root_claim(interrupt: bool) {
    let first = Repo::new();
    let second = Repo::new();
    let root = first.temp.path().join("reusable-root");
    let init = |repo: &Repo| {
        let output = support::groove_in(&repo.path)
            .args([
                "pool",
                "init",
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
                "--workspace-root",
            ])
            .arg(&root)
            .output()
            .unwrap();
        assert_success(&output)
    };
    init(&first);
    let marker = root.join(".groove-owner");
    assert!(marker.is_symlink());
    let preview = support::groove_in(&first.path)
        .args(["pool", "delete", "default", "--dry-run"])
        .output()
        .unwrap();
    assert_success(&preview);
    assert!(marker.is_symlink());
    if interrupt {
        use std::os::unix::net::UnixListener;
        let socket = first.temp.path().join("deleted.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut child = TestChild::spawn(
            support::groove_in(&first.path)
                .args(["pool", "delete", "default", "--yes"])
                .env("GROOVE_TEST_CHECKPOINT", "pool-deleted")
                .env("GROOVE_TEST_SOCKET", &socket),
        );
        let stream = child
            .checkpoint(&listener, "pool-deleted", support::PROCESS_TIMEOUT)
            .unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
        drop(stream);
        assert!(marker.is_symlink());
    }
    assert_success(
        &support::groove_in(&first.path)
            .args(["pool", "delete", "default", "--yes"])
            .output()
            .unwrap(),
    );
    assert!(!marker.is_symlink());
    init(&second);
    let owner = std::fs::read_link(&marker).unwrap();
    let lease = assert_success(
        &support::groove_in(&second.path)
            .arg("acquire")
            .output()
            .unwrap(),
    );
    let file = root.join("wt-0001/valuable.txt");
    std::fs::write(&file, b"new repository's work").unwrap();
    assert_success(
        &support::groove_in(&first.path)
            .args(["pool", "delete", "default", "--yes"])
            .output()
            .unwrap(),
    );
    assert_eq!(std::fs::read_link(&marker).unwrap(), owner);
    assert_eq!(std::fs::read(file).unwrap(), b"new repository's work");
    let active = assert_success(
        &support::groove_in(&second.path)
            .arg("list")
            .output()
            .unwrap(),
    );
    assert_eq!(
        active["result"]["reservations"][0]["reservation_id"],
        lease["result"]["reservation_id"]
    );
}

#[cfg(feature = "test-hooks")]
#[test]
fn initializer_losing_the_root_race_can_choose_another_root() {
    use std::{io::Write, os::unix::net::UnixListener};
    let first = Repo::new();
    let second = Repo::new();
    let root = first.temp.path().join("shared");
    let spawn = |repo: &Repo| {
        let socket = repo.temp.path().join("claim.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut child = TestChild::spawn(
            support::groove_in(&repo.path)
                .args([
                    "pool",
                    "init",
                    "--capacity",
                    "1",
                    "--ready-target",
                    "0",
                    "--idle-timeout",
                    "never",
                    "--workspace-root",
                ])
                .arg(&root)
                .env("GROOVE_TEST_CHECKPOINT", "init-before-root-claim")
                .env("GROOVE_TEST_SOCKET", socket),
        );
        let stream = child
            .checkpoint(
                &listener,
                "init-before-root-claim",
                support::PROCESS_TIMEOUT,
            )
            .unwrap();
        (child, stream)
    };
    let (first_child, mut first_ready) = spawn(&first);
    let (second_child, mut second_ready) = spawn(&second);
    first_ready.write_all(b"go").unwrap();
    assert_success(&first_child.wait_with_output().unwrap());
    second_ready.write_all(b"go").unwrap();
    let failure = second_child.wait_with_output().unwrap();
    assert_eq!(failure.status.code(), Some(4));
    assert!(!second.state().join("initializing.json").exists());
    assert_success(
        &support::groove_in(&second.path)
            .args([
                "pool",
                "init",
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
                "--workspace-root",
            ])
            .arg(second.temp.path().join("independent"))
            .output()
            .unwrap(),
    );
}

#[test]
fn independent_repositories_cannot_claim_the_same_empty_checkout_root() {
    let first = Repo::new();
    let second = Repo::new();
    let root = first.temp.path().join("shared");
    let spawn = |repo: &Repo| {
        TestChild::spawn(
            support::groove_in(&repo.path)
                .args([
                    "pool",
                    "init",
                    "--capacity",
                    "1",
                    "--ready-target",
                    "0",
                    "--idle-timeout",
                    "never",
                    "--workspace-root",
                ])
                .arg(&root),
        )
    };
    let a = spawn(&first);
    let b = spawn(&second);
    let a = a.wait_with_output().unwrap();
    let b = b.wait_with_output().unwrap();
    assert_ne!(
        a.status.success(),
        b.status.success(),
        "first={a:?}, second={b:?}"
    );
    let (owner, loser, failed) = if a.status.success() {
        (&first, &second, b)
    } else {
        (&second, &first, a)
    };
    let error: serde_json::Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(error["error"]["code"], "CONFIG_CONFLICT");
    let nested = support::groove_in(&loser.path)
        .args([
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
            "--workspace-root",
        ])
        .arg(root.join("nested"))
        .output()
        .unwrap();
    assert_eq!(nested.status.code(), Some(4), "{nested:?}");
    assert!(!root.join("nested").exists());
    let acquired = support::groove_in(&owner.path)
        .arg("acquire")
        .output()
        .unwrap();
    let lease = assert_success(&acquired);
    assert_eq!(
        std::fs::read(root.join("wt-0001/tracked.txt")).unwrap(),
        b"baseline\n"
    );
    assert_success(
        &support::groove_in(&owner.path)
            .args([
                "release",
                "--reservation-id",
                lease["result"]["reservation_id"].as_str().unwrap(),
            ])
            .output()
            .unwrap(),
    );
    assert_success(
        &support::groove_in(&owner.path)
            .args(["pool", "delete", "default", "--yes"])
            .output()
            .unwrap(),
    );
    assert_success(&spawn(owner).wait_with_output().unwrap());
}
