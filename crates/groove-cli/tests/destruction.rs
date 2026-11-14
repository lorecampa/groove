mod support;
use serde_json::Value;
use support::{assert_success, Repo};

fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    support::groove_in(&repo.path)
        .arg("--repo")
        .arg(&repo.path)
        .args(args)
        .output()
        .unwrap()
}
fn check(output: &std::process::Output) -> Value {
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response
}

#[test]
fn destruction_prunes_missing_snapshot_receipts_but_preserves_retry_and_lock_identity() {
    use std::os::unix::fs::MetadataExt;
    let repo = Repo::new();
    let init = [
        "init",
        "--capacity",
        "1",
        "--ready-target",
        "0",
        "--idle-timeout",
        "never",
    ];
    assert_success(&cli(&repo, &init));
    let mut receipts = Vec::new();
    for task in ["removed", "retained"] {
        let lease = assert_success(&cli(
            &repo,
            &["acquire", "--task", task, "--request-key", task],
        ));
        let id = lease["result"]["lease_id"].as_str().unwrap();
        let saved = assert_success(&cli(&repo, &["release", "--save", "--lease-id", id]));
        receipts.push((
            repo.state().join(format!("slots/finish-{id}.json")),
            saved["result"]["snapshot"]["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        ));
    }
    let coordination: Vec<_> = std::fs::read_dir(repo.state().join("requests"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let metadata = std::fs::metadata(&path).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            (path, metadata.ino(), bytes)
        })
        .collect();
    assert_success(&cli(
        &repo,
        &["snapshot", "delete", &receipts[0].1, "--force"],
    ));
    assert_success(&cli(&repo, &["destroy", "--dry-run"]));
    assert!(receipts[0].0.exists());
    assert_success(&cli(&repo, &["destroy"]));
    assert!(!receipts[0].0.exists());
    assert!(receipts[1].0.exists());
    for (path, inode, bytes) in coordination {
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    assert_success(&cli(&repo, &init));
    let retried = cli(
        &repo,
        &["acquire", "--task", "removed", "--request-key", "removed"],
    );
    assert!(!retried.status.success());
    assert_eq!(check(&retried)["error"]["code"], "INVALID_LEASE");
    let retained = assert_success(&cli(&repo, &["snapshot", "show", &receipts[1].1]));
    assert_eq!(retained["result"]["snapshot"]["pinned"], true);
}

#[test]
fn destroy_contract_includes_preview_rejection_and_generation_preconditions() {
    let repo = Repo::new();
    let output = cli(&repo, &["destroy", "--help"]);
    assert!(output.status.success());
    assert!(check(&output)["result"]["text"]
        .as_str()
        .unwrap()
        .contains("--discard-changes"));
    let missing = cli(&repo, &["destroy"]);
    assert_eq!(missing.status.code(), Some(4));
    assert_eq!(check(&missing)["error"]["code"], "NOT_INITIALIZED");
    assert!(!repo.state().exists());
    assert_success(&cli(
        &repo,
        &[
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let lease = assert_success(&cli(&repo, &["acquire", "--task", "retained"]));
    let path = lease["result"]["worktree_path"].as_str().unwrap();
    let busy = cli(&repo, &["destroy", "--discard-changes"]);
    assert_eq!(busy.status.code(), Some(4));
    assert_eq!(check(&busy)["error"]["code"], "CONFIG_CONFLICT");
    assert!(std::path::Path::new(path).exists());
    assert_success(&cli(&repo, &["release", "--task", "retained"]));
    let preview = cli(&repo, &["destroy", "--dry-run"]);
    assert!(preview.status.success());
    let preview = check(&preview);
    let id = preview["result"]["destruction"]["pool_id"]
        .as_str()
        .unwrap();
    assert_eq!(preview["result"]["destruction"]["deleted"], false);
    assert!(std::path::Path::new(path).exists());
    let deleted = cli(&repo, &["destroy", "--pool-id", id]);
    assert!(deleted.status.success());
    assert_eq!(check(&deleted)["result"]["destruction"]["deleted"], true);
    assert!(!std::path::Path::new(path).exists());
    let again = cli(&repo, &["destroy", "--pool-id", id]);
    assert!(again.status.success());
    assert_eq!(
        check(&again)["result"]["destruction"]["already_deleted"],
        true
    );
    assert_success(&cli(
        &repo,
        &[
            "init",
            "--capacity",
            "2",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let stale = cli(&repo, &["destroy", "--pool-id", id]);
    assert_eq!(stale.status.code(), Some(4));
    assert_eq!(check(&stale)["error"]["code"], "CONFIG_CONFLICT");
    assert_success(&cli(&repo, &["acquire", "--task", "new"]));
    let lifecycle: Value =
        serde_json::from_slice(&std::fs::read(repo.state().join("lifecycle.json")).unwrap())
            .unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/lifecycle-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&lifecycle)
        .unwrap();
}

#[test]
fn deletion_refuses_a_shell_in_a_released_workspace_even_with_discard() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo,
        &[
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let lease = assert_success(&cli(&repo, &["acquire"]));
    let id = lease["result"]["lease_id"].as_str().unwrap();
    let path = lease["result"]["worktree_path"].as_str().unwrap();
    assert_success(&cli(&repo, &["release", "--lease-id", id]));
    let output = support::groove_in(std::path::Path::new(path))
        .arg("--repo")
        .arg(&repo.path)
        .args(["destroy", "--discard-changes"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(5));
    assert_eq!(check(&output)["error"]["code"], "WORKSPACE_IN_USE");
    assert!(std::path::Path::new(path).exists());
    assert_success(&cli(&repo, &["destroy"]));
}
