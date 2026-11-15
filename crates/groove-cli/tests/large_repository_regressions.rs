mod support;
use serde_json::Value;
use std::{
    fs,
    time::{Duration, Instant},
};
use support::Repo;

fn cli(repo: &Repo, args: &[&str]) -> Value {
    support::assert_success(&support::groove_in(&repo.path).args(args).output().unwrap())["result"]
        .clone()
}
fn init(repo: &Repo) {
    cli(
        repo,
        &[
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    );
}

#[test]
#[cfg(target_os = "linux")]
fn release_supports_transitive_pipeline_readers() {
    use std::os::unix::process::CommandExt;
    for stages in ["cat", "cat | cat", "cat | cat | cat"] {
        let repo = Repo::new();
        init(&repo);
        let lease = cli(&repo, &["acquire"]);
        let mut command = support::command(&repo.slot(1), "bash");
        command
            .process_group(0)
            .env("GROOVE", support::groove_binary())
            .env("ID", lease["reservation_id"].as_str().unwrap())
            .args([
                "-c",
                &format!(
                    "set -o pipefail; \"$GROOVE\" --json release --reservation-id \"$ID\" | {stages}"
                ),
            ]);
        let output = support::TestChild::spawn(&mut command)
            .wait_with_output()
            .unwrap();
        support::assert_success(&output);
    }
}

#[test]
#[cfg(target_os = "linux")]
fn transitive_pipeline_does_not_exempt_background_workspace_writers() {
    use std::os::unix::process::CommandExt;
    let repo = Repo::new();
    init(&repo);
    let lease = cli(&repo, &["acquire"]);
    let mut command = support::command(&repo.slot(1), "bash");
    command.process_group(0).env("GROOVE", support::groove_binary())
        .env("ID", lease["reservation_id"].as_str().unwrap()).env("REPO", &repo.path)
        .args(["-c", "set -o pipefail; sleep 30 & writer=$!; trap 'kill $writer; wait $writer 2>/dev/null' EXIT; \"$GROOVE\" --json --repo \"$REPO\" release --reservation-id \"$ID\" | cat | cat"]);
    let output = support::TestChild::spawn(&mut command)
        .wait_with_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], "WORKSPACE_IN_USE");
    assert!(response["error"]["details"]["processes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|process| process["command"] == "sleep"));
    assert!(repo.slot(1).join("tracked.txt").exists());
    cli(
        &repo,
        &[
            "release",
            "--reservation-id",
            lease["reservation_id"].as_str().unwrap(),
        ],
    );
}

#[test]
fn detailed_listing_bounds_busy_execution_wait() {
    use std::os::fd::AsRawFd;
    let repo = Repo::new();
    init(&repo);
    cli(&repo, &["acquire"]);
    let lock = fs::File::open(repo.state().join("locks/wt-0001.execution.lock")).unwrap();
    // SAFETY: lock owns its valid descriptor through the child observation.
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let start = Instant::now();
    let mut command = support::groove_in(&repo.path);
    command.args(["--git-timeout", "60000ms", "list", "--slots", "--details"]);
    let output = support::TestChild::spawn(&mut command)
        .wait_with_output()
        .unwrap();
    assert!(start.elapsed() < Duration::from_secs(20));
    let response = support::assert_success(&output);
    assert!(
        response["result"]["inspection"]["slots"][0]["git"]["error"]
            .as_str()
            .unwrap()
            .contains("busy"),
        "{response}"
    );
}

#[test]
fn deleted_pool_succeeds_despite_unreadable_snapshot_ref_cleanup() {
    let repo = Repo::new();
    init(&repo);
    let lease = cli(&repo, &["acquire"]);
    fs::write(repo.slot(1).join("tracked.txt"), b"saved work\n").unwrap();
    let saved = cli(
        &repo,
        &[
            "release",
            "--save",
            "--reservation-id",
            lease["reservation_id"].as_str().unwrap(),
        ],
    );
    let id = saved["snapshot"]["id"].as_str().unwrap();
    fs::write(
        repo.path.join(".git/refs/groove/snapshots/broken"),
        b"garbage\n",
    )
    .unwrap();
    for _ in 0..2 {
        assert_eq!(
            cli(&repo, &["pool", "destroy", "--force"])["destruction"]["deleted"],
            true
        );
        assert!(!repo.slot(1).exists());
        assert_eq!(
            repo.git(&[
                "cat-file",
                "-p",
                &format!("refs/groove/snapshots/{id}:work/tracked.txt")
            ]),
            "saved work"
        );
    }
}
