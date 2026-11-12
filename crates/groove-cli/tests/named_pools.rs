mod support;
use serde_json::Value;
use support::{assert_success, Repo};

fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    support::groove_in(&repo.path)
        .env("GROOVE_POOL", "untrusted-parent")
        .arg("--repo")
        .arg(&repo.path)
        .args(args)
        .output()
        .unwrap()
}
fn checked(output: &std::process::Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&response)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {response}");
    response
}

#[test]
fn pool_selection_is_explicit_scoped_and_schema_conformant() {
    let repo = Repo::new();
    let empty = cli(&repo, &["pools"]);
    assert!(empty.status.success());
    assert_eq!(checked(&empty)["result"]["pools"], serde_json::json!([]));
    assert!(!repo.state().exists());
    for pool in ["frontend", "backend"] {
        let init = cli(&repo, &["--pool", pool, "init", "--capacity", "1"]);
        assert!(init.status.success());
        let value = checked(&init);
        assert_eq!(value["result"]["pool_name"], pool);
        assert!(value["result"]["state_dir"]
            .as_str()
            .unwrap()
            .ends_with(&format!("/groove/pools/{pool}")));
    }
    let missing = cli(&repo, &["acquire"]);
    assert_eq!(missing.status.code(), Some(4));
    assert_eq!(checked(&missing)["error"]["code"], "NOT_INITIALIZED");
    let f = assert_success(&cli(
        &repo,
        &[
            "acquire",
            "--pool",
            "frontend",
            "--task",
            "same",
            "--request-key",
            "same",
        ],
    ));
    let b = assert_success(&cli(
        &repo,
        &[
            "--pool=backend",
            "acquire",
            "--task",
            "same",
            "--request-key",
            "same",
        ],
    ));
    assert_ne!(f["result"]["worktree_path"], b["result"]["worktree_path"]);
    let wrong = cli(
        &repo,
        &[
            "--pool",
            "frontend",
            "release",
            "--lease-id",
            b["result"]["lease_id"].as_str().unwrap(),
        ],
    );
    assert_eq!(wrong.status.code(), Some(5));
    assert_eq!(checked(&wrong)["error"]["code"], "LEASE_MISMATCH");
    let ambiguity = cli(&repo, &["destroy", "--discard-changes"]);
    assert_eq!(ambiguity.status.code(), Some(2));
    assert_eq!(checked(&ambiguity)["error"]["code"], "INVALID_ARGUMENT");
    assert_success(&cli(
        &repo,
        &[
            "--pool",
            "frontend",
            "exec",
            "--task",
            "same",
            "--",
            "sh",
            "-c",
            "printf %s \"$GROOVE_POOL\"",
        ],
    ));
    let history = cli(&repo, &["--pool", "frontend", "history"]);
    assert!(history.status.success());
    assert_eq!(
        checked(&history)["result"]["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let history = checked(&history);
    assert_eq!(
        std::fs::read_to_string(
            history["result"]["runs"][0]["stdout_path"]
                .as_str()
                .unwrap()
        )
        .unwrap(),
        "frontend"
    );
    let history = cli(&repo, &["--pool", "backend", "history"]);
    assert!(history.status.success());
    assert_eq!(checked(&history)["result"]["runs"], serde_json::json!([]));
    // Snapshot selection follows its source checkout and remains repository-wide.
    let backend_path = std::path::Path::new(b["result"]["worktree_path"].as_str().unwrap());
    let snapshot = support::groove_in(backend_path)
        .args(["--pool", "frontend", "snapshot", "save"])
        .output()
        .unwrap();
    assert!(snapshot.status.success());
    let snapshot = checked(&snapshot);
    assert_eq!(
        snapshot["result"]["snapshot"]["workspace"],
        support::git(backend_path, &["rev-parse", "--absolute-git-dir"])
    );
    let shared = cli(
        &repo,
        &[
            "--pool",
            "frontend",
            "snapshot",
            "show",
            snapshot["result"]["snapshot"]["id"].as_str().unwrap(),
        ],
    );
    assert!(shared.status.success());
    checked(&shared);
    assert_success(&cli(
        &repo,
        &["--pool", "frontend", "release", "--task", "same"],
    ));
    let preview = cli(&repo, &["--pool", "frontend", "destroy", "--dry-run"]);
    assert!(preview.status.success());
    let preview = checked(&preview);
    assert_eq!(preview["result"]["destruction"]["pool_name"], "frontend");
    let id = preview["result"]["destruction"]["pool_id"]
        .as_str()
        .unwrap();
    let wrong = cli(&repo, &["--pool", "backend", "destroy", "--pool-id", id]);
    assert_eq!(wrong.status.code(), Some(4));
    assert_eq!(checked(&wrong)["error"]["code"], "CONFIG_CONFLICT");
    let removed = cli(&repo, &["--pool", "frontend", "destroy", "--pool-id", id]);
    assert!(removed.status.success());
    assert_eq!(checked(&removed)["result"]["destruction"]["deleted"], true);
    let catalog = cli(&repo, &["pools"]);
    assert!(catalog.status.success());
    let catalog = checked(&catalog);
    let pools = catalog["result"]["pools"].as_array().unwrap();
    assert_eq!(pools.len(), 2);
    assert_eq!(pools[0]["name"], "backend");
    assert_eq!(pools[0]["phase"], "ready");
    assert_eq!(pools[1]["name"], "frontend");
    assert_eq!(pools[1]["phase"], "deleted");
    let shown = assert_success(&cli(
        &repo,
        &["--pool", "backend", "show", "--task", "same"],
    ));
    assert_eq!(
        shown["result"]["lease"]["lease_id"],
        b["result"]["lease_id"]
    );
    assert_success(&cli(
        &repo,
        &[
            "--pool",
            "frontend",
            "storage",
            "clean",
            "--logs",
            "--older-than",
            "0s",
        ],
    ));
}

#[test]
fn invalid_names_fail_before_filesystem_effects_and_help_teaches_selection() {
    let repo = Repo::new();
    for pool in ["../escape", "Frontend", "a/b", "bad name", ""] {
        let output = cli(&repo, &["--pool", pool, "init", "--capacity", "1"]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(checked(&output)["error"]["code"], "INVALID_ARGUMENT");
        assert!(!repo.state().exists());
    }
    let help = cli(&repo, &["pools", "--help"]);
    assert!(help.status.success());
    assert!(checked(&help)["result"]["text"]
        .as_str()
        .unwrap()
        .contains("--pool frontend"));
    let unknown = cli(&repo, &["--pool", "unknown", "show"]);
    assert_eq!(unknown.status.code(), Some(4));
    assert_eq!(checked(&unknown)["error"]["code"], "NOT_INITIALIZED");
    assert!(!repo.state().exists());
}

#[test]
fn an_explicit_default_preserves_existing_layout_and_human_output_names_named_pools() {
    let repo = Repo::new();
    let init = cli(&repo, &["--pool", "default", "init", "--capacity", "1"]);
    assert!(init.status.success());
    assert_eq!(
        checked(&init)["result"]["state_dir"],
        repo.state().to_str().unwrap()
    );
    let output = support::groove_in(&repo.path)
        .env_remove("GROOVE_OUTPUT")
        .args(["--pool", "frontend", "init", "--capacity", "1"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("pool 'frontend'"));
    let ambiguous = cli(&repo, &["destroy", "--dry-run"]);
    assert_eq!(ambiguous.status.code(), Some(2));
    checked(&ambiguous);
    assert_success(&cli(&repo, &["--pool", "default", "destroy"]));
    let output = support::groove_in(&repo.path)
        .env_remove("GROOVE_OUTPUT")
        .arg("pools")
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("default") && text.contains("deleted") && text.contains("frontend"));
    assert_success(&cli(&repo, &["--pool", "frontend", "acquire"]));
}
