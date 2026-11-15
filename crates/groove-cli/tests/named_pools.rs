#[path = "support/json_progress.rs"]
mod json_progress;
mod support;
use serde_json::Value;
use support::{assert_success, Repo};

fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    let mut command = support::groove_in(&repo.path);
    command
        .env("GROOVE_POOL", "untrusted-parent")
        .arg("--repo")
        .arg(&repo.path)
        .args(args);
    // These catalog/storage fixtures exercise lazy pools with stable disk usage.
    // Automatic readiness and namespace independence have separate process tests.
    if args.contains(&"init") {
        command.args(["--ready-target", "0", "--idle-timeout", "never"]);
    }
    command.output().unwrap()
}

#[test]
fn disk_usage_is_opt_in_scoped_and_includes_ignored_files_without_following_symlinks() {
    let repo = Repo::new();
    for pool in ["default", "frontend"] {
        assert_success(&cli(
            &repo,
            &["--pool", pool, "pool", "init", "--capacity", "1"],
        ));
    }
    let acquired = assert_success(&cli(&repo, &["acquire", "--task", "sizing"]));
    let path = std::path::Path::new(acquired["result"]["workspace_path"].as_str().unwrap());
    let before = cli(&repo, &["pool", "show", "--disk-usage"]);
    assert!(before.status.success());
    let before = checked(&before)["result"]["storage"]["workspace_bytes"]
        .as_u64()
        .unwrap();
    std::fs::create_dir(path.join("ignored")).unwrap();
    std::fs::write(path.join("ignored/build.bin"), vec![0; 4096]).unwrap();
    let outside = repo.temp.path().join("outside.bin");
    std::fs::write(&outside, vec![0; 8192]).unwrap();
    std::os::unix::fs::symlink(&outside, path.join("ignored/link")).unwrap();

    let plain = cli(&repo, &["pool", "list"]);
    assert!(plain.status.success());
    assert!(checked(&plain)["result"]["pools"][0]
        .get("storage")
        .is_none());
    let plain = cli(&repo, &["pool", "show"]);
    assert!(plain.status.success());
    assert!(checked(&plain)["result"].get("storage").is_none());

    for args in [
        vec!["pool", "list", "--disk-usage"],
        vec!["pool", "list", "--disk-usage", "--quiet"],
    ] {
        let output = cli(&repo, &args);
        assert!(output.status.success());
        json_progress::assert_only(&output, "pool list");
        let value = checked(&output);
        let pools = value["result"]["pools"].as_array().unwrap();
        assert_eq!(pools[0]["name"], "default");
        assert_eq!(pools[0]["storage"]["workspace_bytes"], before + 4096);
        assert_eq!(pools[1]["name"], "frontend");
        assert_eq!(pools[1]["storage"]["workspace_bytes"], 0);
    }
    let output = cli(&repo, &["pool", "show", "--disk-usage", "--details"]);
    assert!(output.status.success());
    json_progress::assert_only(&output, "show");
    let value = checked(&output);
    assert_eq!(value["result"]["storage"]["workspace_bytes"], before + 4096);
    assert_eq!(value["result"]["inspection"]["capacity"], 1);
    for args in [
        vec!["pool", "list", "--disk-usage"],
        vec!["pool", "show", "--disk-usage"],
    ] {
        let output = support::groove_human_in(&repo.path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("WORKSPACES") && text.contains("KiB"),
            "{text}"
        );
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    for selector in [
        vec!["--task", "sizing"],
        vec!["--slot", "wt-0001"],
        vec![
            "--reservation-id",
            acquired["result"]["reservation_id"].as_str().unwrap(),
        ],
    ] {
        let mut args = vec!["pool", "show", "--disk-usage"];
        args.extend(selector);
        let output = cli(&repo, &args);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(checked(&output)["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_success(&cli(&repo, &["--pool", "frontend", "pool", "destroy"]));
    let output = cli(&repo, &["pool", "list", "--all", "--disk-usage"]);
    assert!(output.status.success());
    assert_eq!(
        checked(&output)["result"]["pools"][1]["storage"]["workspace_bytes"],
        0
    );
    assert_success(&cli(&repo, &["release", "--task", "sizing"]));
}
fn checked(output: &std::process::Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&response)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {response}");
    response
}

#[test]
fn ordinary_pool_views_do_not_scan_storage_and_scan_errors_preserve_external_files() {
    let repo = Repo::new();
    assert_success(&cli(&repo, &["pool", "init", "--capacity", "1"]));
    let artifacts = repo.state().join("artifacts");
    if artifacts.exists() {
        std::fs::rename(&artifacts, repo.temp.path().join("saved-artifacts")).unwrap();
    }
    let external = repo.temp.path().join("external");
    std::fs::create_dir(&external).unwrap();
    std::fs::write(external.join("keep"), "external result").unwrap();
    std::os::unix::fs::symlink(&external, artifacts).unwrap();
    for command in ["list", "show"] {
        let plain = cli(&repo, &["pool", command]);
        assert!(plain.status.success(), "{plain:?}");
        checked(&plain);
        let scan = cli(&repo, &["pool", command, "--disk-usage"]);
        assert_eq!(scan.status.code(), Some(2));
        assert_eq!(checked(&scan)["error"]["code"], "UNSUPPORTED_CONFIGURATION");
        assert_eq!(
            std::fs::read_to_string(external.join("keep")).unwrap(),
            "external result"
        );
    }
}

#[test]
fn pool_selection_is_explicit_scoped_and_schema_conformant() {
    let repo = Repo::new();
    let empty = cli(&repo, &["pool", "list"]);
    assert!(empty.status.success());
    assert_eq!(checked(&empty)["result"]["pools"], serde_json::json!([]));
    assert!(!repo.state().exists());
    for pool in ["frontend", "backend"] {
        let init = cli(&repo, &["--pool", pool, "pool", "init", "--capacity", "1"]);
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
            "--idempotency-key",
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
            "--idempotency-key",
            "same",
        ],
    ));
    assert_ne!(f["result"]["workspace_path"], b["result"]["workspace_path"]);
    let wrong = cli(
        &repo,
        &[
            "--pool",
            "frontend",
            "release",
            "--reservation-id",
            b["result"]["reservation_id"].as_str().unwrap(),
        ],
    );
    assert_eq!(wrong.status.code(), Some(5));
    assert_eq!(checked(&wrong)["error"]["code"], "RESERVATION_MISMATCH");
    let ambiguity = cli(&repo, &["pool", "destroy", "--force"]);
    assert_eq!(ambiguity.status.code(), Some(2));
    assert_eq!(checked(&ambiguity)["error"]["code"], "INVALID_ARGUMENT");
    // Snapshot selection follows its source checkout and remains repository-wide.
    let backend_path = std::path::Path::new(b["result"]["workspace_path"].as_str().unwrap());
    let snapshot = support::groove_in(backend_path)
        .args(["snapshot", "save"])
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
    let preview = cli(
        &repo,
        &["--pool", "frontend", "pool", "destroy", "--dry-run"],
    );
    assert!(preview.status.success());
    let preview = checked(&preview);
    assert_eq!(preview["result"]["destruction"]["pool_name"], "frontend");
    let id = preview["result"]["destruction"]["pool_id"]
        .as_str()
        .unwrap();
    let wrong = cli(
        &repo,
        &["--pool", "backend", "pool", "destroy", "--pool-id", id],
    );
    assert_eq!(wrong.status.code(), Some(4));
    assert_eq!(checked(&wrong)["error"]["code"], "CONFIG_CONFLICT");
    let removed = cli(
        &repo,
        &["--pool", "frontend", "pool", "destroy", "--pool-id", id],
    );
    assert!(removed.status.success());
    assert_eq!(checked(&removed)["result"]["destruction"]["deleted"], true);
    let catalog = cli(&repo, &["pool", "list"]);
    assert!(catalog.status.success());
    let catalog = checked(&catalog);
    let pools = catalog["result"]["pools"].as_array().unwrap();
    assert_eq!(pools.len(), 1);
    assert_eq!(pools[0]["name"], "backend");
    let catalog = cli(&repo, &["pool", "list", "--all"]);
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
        shown["result"]["reservation"]["reservation_id"],
        b["result"]["reservation_id"]
    );
}

#[test]
fn invalid_names_fail_before_filesystem_effects_and_help_teaches_selection() {
    let repo = Repo::new();
    for pool in ["../escape", "Frontend", "a/b", "bad name", ""] {
        let output = cli(&repo, &["--pool", pool, "pool", "init", "--capacity", "1"]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(checked(&output)["error"]["code"], "INVALID_ARGUMENT");
        assert!(!repo.state().exists());
    }
    let help = cli(&repo, &["pool", "list", "--help"]);
    assert!(help.status.success());
    assert!(checked(&help)["result"]["text"]
        .as_str()
        .unwrap()
        .contains("--pool frontend"));
    let unknown = cli(&repo, &["--pool", "unknown", "pool", "show"]);
    assert_eq!(unknown.status.code(), Some(4));
    assert_eq!(checked(&unknown)["error"]["code"], "NOT_INITIALIZED");
    assert!(!repo.state().exists());
}

#[test]
fn an_explicit_default_preserves_existing_layout_and_human_output_names_named_pools() {
    let repo = Repo::new();
    let init = cli(
        &repo,
        &["--pool", "default", "pool", "init", "--capacity", "1"],
    );
    assert!(init.status.success());
    assert_eq!(
        checked(&init)["result"]["state_dir"],
        repo.state().to_str().unwrap()
    );
    let output = support::groove_human_in(&repo.path)
        .args([
            "--pool",
            "frontend",
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("pool 'frontend'"));
    let ambiguous = cli(&repo, &["pool", "destroy", "--dry-run"]);
    assert_eq!(ambiguous.status.code(), Some(2));
    checked(&ambiguous);
    assert_success(&cli(&repo, &["--pool", "default", "pool", "destroy"]));
    let output = support::groove_human_in(&repo.path)
        .args(["pool", "list"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("frontend"));
    assert!(!text.contains("default"), "{text}");
    assert!(!text.contains("deleted"), "{text}");
    let output = support::groove_human_in(&repo.path)
        .args(["pool", "list", "--all"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("default") && text.contains("deleted") && text.contains("frontend"));
    assert_success(&cli(&repo, &["--pool", "frontend", "pool", "destroy"]));
    let empty = cli(&repo, &["pool", "list"]);
    assert!(empty.status.success());
    assert_eq!(checked(&empty)["result"]["pools"], serde_json::json!([]));
    let empty = support::groove_human_in(&repo.path)
        .args(["pool", "list"])
        .output()
        .unwrap();
    assert!(empty.status.success());
    assert_eq!(
        String::from_utf8_lossy(&empty.stdout),
        "No pools on this page.\nTotal: 0 entries; offset: 0.\n"
    );
    assert_success(&cli(
        &repo,
        &["--pool", "frontend", "pool", "init", "--capacity", "1"],
    ));
    assert_success(&cli(&repo, &["--pool", "frontend", "acquire"]));
}
