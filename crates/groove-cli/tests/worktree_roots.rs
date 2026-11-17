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
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response["result"].clone()
}

#[test]
fn a_symlinked_home_resolves_the_base_before_creating_managed_paths() {
    let repo = Repo::new();
    let home = repo.temp.path().join("real-home");
    std::fs::create_dir(&home).unwrap();
    let alias = repo.temp.path().join("home-alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let initialized = run(
        &repo,
        &alias,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    );
    let root = Path::new(initialized["workspace_root"].as_str().unwrap());
    assert!(root.starts_with(home.join(if cfg!(target_os = "macos") {
        ".worktrees.noindex"
    } else {
        ".worktrees"
    })));
    assert!(!root.starts_with(&alias));
    let lease = run(&repo, &alias, &["acquire"]);
    assert!(Path::new(lease["workspace_path"].as_str().unwrap()).starts_with(root));
    assert_eq!(
        run(&repo, &home, &["pool", "show", "default"])["pool"]["workspace_root"],
        initialized["workspace_root"]
    );
}

#[test]
fn root_precedence_and_global_changes_apply_only_to_new_pools() {
    let repo = Repo::new();
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let original = run(&repo, h, &["pool", "init", "--capacity", "1"]);
    let original_root = Path::new(original["workspace_root"].as_str().unwrap());
    assert!(
        original_root.starts_with(h.join(if cfg!(target_os = "macos") {
            ".worktrees.noindex"
        } else {
            ".worktrees"
        }))
    );
    assert_eq!(original_root.file_name().unwrap(), "default");

    let base = h.join("custom worktrees");
    run(
        &repo,
        h,
        &[
            "config",
            "set",
            &format!("worktrees.root={}", base.display()),
        ],
    );
    assert_eq!(
        run(&repo, h, &["config", "show"])["config"]["worktrees"]["root"],
        base.to_str().unwrap()
    );
    let front = run(
        &repo,
        h,
        &["--pool", "frontend", "pool", "init", "--capacity", "1"],
    );
    let front_root = Path::new(front["workspace_root"].as_str().unwrap());
    assert!(front_root.starts_with(&base));
    assert_eq!(front_root.file_name().unwrap(), "frontend");
    assert_eq!(
        original_root.parent().unwrap().file_name(),
        front_root.parent().unwrap().file_name()
    );
    assert_eq!(
        run(&repo, h, &["pool", "show", "default"])["pool"]["workspace_root"],
        original["workspace_root"]
    );

    let exact = h.join("exact pool root");
    let back = run(
        &repo,
        h,
        &[
            "--pool",
            "backend",
            "pool",
            "init",
            "--capacity",
            "1",
            "--workspace-root",
            exact.to_str().unwrap(),
        ],
    );
    assert_eq!(back["workspace_root"], exact.to_str().unwrap());
    run(&repo, h, &["config", "unset", "worktrees.root"]);
    assert!(run(&repo, h, &["config", "show"])["config"]["worktrees"]["root"].is_null());
    let reset = run(
        &repo,
        h,
        &["--pool", "reset", "pool", "init", "--capacity", "1"],
    );
    assert_eq!(
        Path::new(reset["workspace_root"].as_str().unwrap()).parent(),
        original_root.parent()
    );

    let lease = run(&repo, h, &["acquire", "--pool", "frontend"]);
    assert!(Path::new(lease["workspace_path"].as_str().unwrap()).starts_with(front_root));
    run(
        &repo,
        h,
        &[
            "release",
            "--pool",
            "frontend",
            "--reservation-id",
            lease["reservation_id"].as_str().unwrap(),
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
        .args(["pool", "init", "--capacity", "1"])
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
                "pool",
                "init",
                "--capacity",
                "1",
                "--workspace-root",
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
    assert!(Path::new(lease["result"]["workspace_path"].as_str().unwrap()).starts_with(&root));
    run(
        &repo,
        h,
        &[
            "release",
            "--reservation-id",
            lease["result"]["reservation_id"].as_str().unwrap(),
        ],
    );
    let base = h.join("configured-root");
    run(
        &repo,
        h,
        &[
            "config",
            "set",
            &format!("worktrees.root={}", base.display()),
        ],
    );
    let configured = support::assert_success(
        &command(&repo, h)
            .env_remove("HOME")
            .args(["--pool", "configured", "pool", "init", "--capacity", "1"])
            .output()
            .unwrap(),
    );
    assert!(Path::new(configured["result"]["workspace_root"].as_str().unwrap()).starts_with(base));
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
            "set",
            &format!("worktrees.root={}", base.display()),
        ],
    );
    for value in ["relative", "~/worktrees", "42", "true", "\"\""] {
        let output = command(&repo, h)
            .args(["config", "set", &format!("worktrees.root={value}")])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(
            run(&repo, h, &["config", "show"])["config"]["worktrees"]["root"],
            base.to_str().unwrap()
        );
    }
}
