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
fn destroy_contract_includes_preview_rejection_history_and_generation_preconditions() {
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
    assert_success(&cli(&repo, &["init", "--capacity", "1"]));
    let lease = assert_success(&cli(&repo, &["acquire", "--task", "retained"]));
    let path = lease["result"]["worktree_path"].as_str().unwrap();
    let busy = cli(&repo, &["destroy", "--discard-changes"]);
    assert_eq!(busy.status.code(), Some(4));
    assert_eq!(check(&busy)["error"]["code"], "CONFIG_CONFLICT");
    assert!(std::path::Path::new(path).exists());
    assert_success(&cli(
        &repo,
        &[
            "exec",
            "--task",
            "retained",
            "--",
            "sh",
            "-c",
            "printf retained-output",
        ],
    ));
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
    let history = cli(&repo, &["history"]);
    assert!(history.status.success());
    assert_eq!(
        check(&history)["result"]["runs"].as_array().unwrap().len(),
        1
    );
    assert_success(&cli(
        &repo,
        &["storage", "clean", "--logs", "--older-than", "0s"],
    ));
    assert_success(&cli(&repo, &["init", "--capacity", "2"]));
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
    assert_success(&cli(&repo, &["init", "--capacity", "1"]));
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
