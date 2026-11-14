mod support;
use support::{assert_success, Repo, TestChild};

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
                    "init",
                    "--capacity",
                    "1",
                    "--ready-target",
                    "0",
                    "--idle-timeout",
                    "never",
                    "--worktree-root",
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
                "init",
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
                "--worktree-root",
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
                    "init",
                    "--capacity",
                    "1",
                    "--ready-target",
                    "0",
                    "--idle-timeout",
                    "never",
                    "--worktree-root",
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
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
            "--worktree-root",
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
                "--lease-id",
                lease["result"]["lease_id"].as_str().unwrap(),
            ])
            .output()
            .unwrap(),
    );
    assert_success(
        &support::groove_in(&owner.path)
            .arg("destroy")
            .output()
            .unwrap(),
    );
    assert_success(&spawn(owner).wait_with_output().unwrap());
}
