mod support;

use serde_json::Value;
use std::{path::Path, process::Command};
use support::Repo;

fn command(repo: &Repo, home: &Path) -> Command {
    let mut command = support::groove_in(&repo.path);
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"));
    command
}

fn run(repo: &Repo, home: &Path, args: &[&str]) -> Value {
    let output = command(repo, home).args(args).output().unwrap();
    let response = support::assert_success(&output);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response["result"].clone()
}

#[test]
fn root_precedence_and_global_changes_apply_only_to_new_pools() {
    let repo = Repo::new();
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let original = run(&repo, h, &["init", "--capacity", "1"]);
    let original_root = Path::new(original["worktree_root"].as_str().unwrap());
    assert!(original_root.starts_with(h.join(".worktrees")));
    assert_eq!(original_root.file_name().unwrap(), "default");

    let base = h.join("custom worktrees");
    run(
        &repo,
        h,
        &[
            "config",
            "global",
            "set",
            &format!("worktrees.root={}", base.display()),
        ],
    );
    assert_eq!(
        run(&repo, h, &["config", "global", "show"])["config"]["worktrees"]["root"],
        base.to_str().unwrap()
    );
    let front = run(&repo, h, &["--pool", "frontend", "init", "--capacity", "1"]);
    let front_root = Path::new(front["worktree_root"].as_str().unwrap());
    assert!(front_root.starts_with(&base));
    assert_eq!(front_root.file_name().unwrap(), "frontend");
    assert_eq!(
        original_root.parent().unwrap().file_name(),
        front_root.parent().unwrap().file_name()
    );
    assert_eq!(
        run(&repo, h, &["config", "show"])["pool"]["worktree_root"],
        original["worktree_root"]
    );

    let exact = h.join("exact pool root");
    let back = run(
        &repo,
        h,
        &[
            "--pool",
            "backend",
            "init",
            "--capacity",
            "1",
            "--worktree-root",
            exact.to_str().unwrap(),
        ],
    );
    assert_eq!(back["worktree_root"], exact.to_str().unwrap());
    run(&repo, h, &["config", "global", "unset", "worktrees.root"]);
    assert!(run(&repo, h, &["config", "global", "show"])["config"]["worktrees"]["root"].is_null());
    let reset = run(&repo, h, &["--pool", "reset", "init", "--capacity", "1"]);
    assert_eq!(
        Path::new(reset["worktree_root"].as_str().unwrap()).parent(),
        original_root.parent()
    );

    let lease = run(&repo, h, &["acquire", "--pool", "frontend"]);
    assert!(Path::new(lease["worktree_path"].as_str().unwrap()).starts_with(front_root));
    run(
        &repo,
        h,
        &[
            "release",
            "--pool",
            "frontend",
            "--lease-id",
            lease["lease_id"].as_str().unwrap(),
        ],
    );
}

#[test]
fn missing_home_requires_a_root_only_when_initializing_and_legacy_pools_still_open() {
    let repo = Repo::new();
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let output = command(&repo, h)
        .env_remove("HOME")
        .args(["init", "--capacity", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    let root = repo.state().join("worktrees");
    support::assert_success(
        &command(&repo, h)
            .env_remove("HOME")
            .args([
                "init",
                "--capacity",
                "1",
                "--worktree-root",
                root.to_str().unwrap(),
            ])
            .output()
            .unwrap(),
    );
    let lease = support::assert_success(
        &command(&repo, h)
            .env_remove("HOME")
            .arg("acquire")
            .output()
            .unwrap(),
    );
    assert!(Path::new(lease["result"]["worktree_path"].as_str().unwrap()).starts_with(&root));
    run(
        &repo,
        h,
        &[
            "release",
            "--lease-id",
            lease["result"]["lease_id"].as_str().unwrap(),
        ],
    );
    let base = h.join("configured-root");
    run(
        &repo,
        h,
        &[
            "config",
            "global",
            "set",
            &format!("worktrees.root={}", base.display()),
        ],
    );
    let configured = support::assert_success(
        &command(&repo, h)
            .env_remove("HOME")
            .args(["--pool", "configured", "init", "--capacity", "1"])
            .output()
            .unwrap(),
    );
    assert!(Path::new(configured["result"]["worktree_root"].as_str().unwrap()).starts_with(base));
}

#[test]
fn global_root_validation_preserves_configuration_on_invalid_updates() {
    let repo = Repo::new();
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let base = h.join("valid-root");
    run(
        &repo,
        h,
        &[
            "config",
            "global",
            "set",
            &format!("worktrees.root={}", base.display()),
        ],
    );
    for value in ["relative", "~/worktrees", "42", "true", "\"\""] {
        let output = command(&repo, h)
            .args([
                "config",
                "global",
                "set",
                &format!("worktrees.root={value}"),
            ])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            run(&repo, h, &["config", "global", "show"])["config"]["worktrees"]["root"],
            base.to_str().unwrap()
        );
    }
}
