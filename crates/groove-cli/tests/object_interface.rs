mod support;

use serde_json::Value;
use support::Repo;

fn invoke(repo: &Repo, args: &[&str], code: i32) -> Value {
    let output = support::groove_in(&repo.path).args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response
}

#[test]
fn positional_reservations_generated_labels_and_path_preserve_retry_identity() {
    let repo = Repo::new();
    invoke(
        &repo,
        &["pool", "init", "--capacity", "2", "--idle-timeout", "never"],
        0,
    );
    let acquired = invoke(
        &repo,
        &[
            "acquire", "fix-nav", "--key", "nav-1", "-b", "HEAD", "-w", "0s",
        ],
        0,
    );
    let id = acquired["result"]["reservation_id"].as_str().unwrap();
    assert_eq!(
        invoke(&repo, &["show", "fix-nav"], 0)["result"]["reservation"]["reservation_id"],
        id
    );
    assert_eq!(
        invoke(&repo, &["path", id], 0)["result"]["workspace_path"],
        acquired["result"]["workspace_path"]
    );
    let path = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"))
        .args(["path", "fix-nav"])
        .output()
        .unwrap();
    assert!(path.status.success(), "{path:?}");
    assert_eq!(
        String::from_utf8(path.stdout).unwrap(),
        format!(
            "{}\n",
            acquired["result"]["workspace_path"].as_str().unwrap()
        )
    );
    let alias = support::groove_in(&repo.path)
        .args(["show", "--task", "fix-nav"])
        .output()
        .unwrap();
    assert!(alias.status.success());
    assert!(String::from_utf8_lossy(&alias.stderr).contains("deprecated"));
    invoke(&repo, &["release", "fix-nav"], 0);
    let generated = invoke(&repo, &["acquire", "--key", "generated-1"], 0);
    let task = generated["result"]["task"].as_str().unwrap();
    assert!(task.starts_with("task-"));
    assert_eq!(
        invoke(&repo, &["acquire", "--key", "generated-1"], 0)["result"]["reservation_id"],
        generated["result"]["reservation_id"]
    );
    invoke(&repo, &["release", task], 0);
    for bad in [
        "contains spaces",
        "a/b",
        "00000000-0000-0000-0000-000000000000",
    ] {
        invoke(&repo, &["acquire", bad], 2);
    }
}

#[test]
fn settings_assignments_round_trip_and_reject_ambiguous_updates() {
    let repo = Repo::new();
    invoke(
        &repo,
        &["pool", "init", "--capacity", "1", "--idle-timeout", "never"],
        0,
    );
    invoke(
        &repo,
        &[
            "pool",
            "set",
            "capacity=2",
            "ready_target=0",
            "cleanup=warm",
        ],
        0,
    );
    invoke(
        &repo,
        &["pool", "set", "default", "capacity=3", "idle-timeout=never"],
        0,
    );
    for key in ["ready-target", "ready_target"] {
        assert_eq!(
            invoke(&repo, &["pool", "get", key], 0)["result"]["value"],
            0
        );
    }
    for arguments in [
        vec!["pool", "set", "capacity=4", "capacity=5"],
        vec!["pool", "set", "capacity=4", "--capacity", "5"],
        vec!["pool", "set", "capacity=4", "unknown=5"],
        vec!["pool", "set", "root=/tmp/elsewhere"],
        vec!["pool", "set", "capacity=4", "default"],
    ] {
        invoke(&repo, &arguments, 2);
        assert_eq!(
            invoke(&repo, &["pool", "get", "capacity"], 0)["result"]["value"],
            3
        );
    }
    invoke(
        &repo,
        &[
            "config",
            "set",
            "pool-defaults.ready-target=0",
            "logging.max-files=7",
        ],
        0,
    );
    for key in ["logging.max-files", "logging.max_files"] {
        assert_eq!(
            invoke(&repo, &["config", "get", key], 0)["result"]["value"],
            7
        );
    }
    invoke(&repo, &["config", "unset", "logging.max-files"], 0);
}

#[test]
fn command_help_only_advertises_applicable_context_flags() {
    let repo = Repo::new();
    for command in ["config", "upgrade", "completions"] {
        let help = invoke(&repo, &[command, "--help"], 0);
        let help = help["result"]["text"].as_str().unwrap();
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with("--pool ")),
            "{help}"
        );
        assert!(
            !help
                .lines()
                .any(|line| line.trim_start().starts_with("--git-timeout ")),
            "{help}"
        );
    }
    let snapshot = invoke(&repo, &["snapshot", "--help"], 0);
    let text = snapshot["result"]["text"].as_str().unwrap();
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("--pool ")),
        "{text}"
    );
    assert!(text.contains("--git-timeout"), "{text}");
    let acquire = invoke(&repo, &["acquire", "--help"], 0);
    let text = acquire["result"]["text"].as_str().unwrap();
    assert!(text.contains("--pool"), "{text}");
    assert!(text.contains("--git-timeout"), "{text}");
}

#[test]
fn pool_objects_accept_positional_names_and_legacy_flags_without_ambiguity() {
    let repo = Repo::new();
    for name in ["default", "frontend"] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                name,
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
            ],
            0,
        );
    }
    assert_eq!(
        invoke(&repo, &["pool", "show", "frontend"], 0)["result"]["pool"]["pool_name"],
        "frontend"
    );
    assert_eq!(
        invoke(&repo, &["pool", "get", "frontend", "capacity"], 0)["result"]["value"],
        1
    );
    assert_eq!(
        invoke(&repo, &["pool", "get", "capacity"], 0)["result"]["value"],
        1
    );
    invoke(&repo, &["pool", "set", "frontend", "--capacity", "2"], 0);
    assert_eq!(
        invoke(&repo, &["--pool", "frontend", "pool", "get", "capacity"], 0)["result"]["value"],
        2
    );
    assert_eq!(
        invoke(&repo, &["--pool", "default", "pool", "show", "frontend"], 2)["error"]["code"],
        "INVALID_ARGUMENT"
    );
    invoke(&repo, &["pool", "destroy", "frontend", "--dry-run"], 0);
    invoke(&repo, &["pool", "destroy", "frontend"], 0);
    assert_eq!(
        invoke(&repo, &["pool", "get", "default", "capacity"], 0)["result"]["value"],
        1
    );
}

#[test]
fn reservation_selectors_search_pools_and_explicit_misses_explain_the_location() {
    let repo = Repo::new();
    for name in ["default", "frontend"] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                name,
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
            ],
            0,
        );
    }
    for selector in ["--task", "--reservation-id"] {
        let lease = invoke(
            &repo,
            &["--pool", "frontend", "acquire", "--task", "fix"],
            0,
        );
        let id = lease["result"]["reservation_id"].as_str().unwrap();
        let value = if selector == "--task" { "fix" } else { id };
        assert_eq!(
            invoke(&repo, &["show", selector, value], 0)["result"]["reservation"]["reservation_id"],
            id
        );
        let wrong = invoke(&repo, &["--pool", "default", "release", selector, value], 5);
        assert_eq!(wrong["error"]["code"], "RESERVATION_MISMATCH");
        assert!(wrong["error"]["message"]
            .as_str()
            .unwrap()
            .contains("frontend"));
        assert!(!wrong["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("--pool default"));
        assert_eq!(
            invoke(&repo, &["show", selector, value], 0)["result"]["active"],
            true
        );
        invoke(&repo, &["release", selector, value], 0);
    }
    let missing = invoke(&repo, &["show", "--task", "missing"], 5);
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("any repository pool"));
    assert!(!missing["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("--pool default"));
}

#[test]
fn saved_release_retries_find_the_original_pool_after_slot_reuse() {
    let repo = Repo::new();
    invoke(
        &repo,
        &[
            "pool",
            "init",
            "frontend",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
        0,
    );
    let lease = invoke(
        &repo,
        &["--pool", "frontend", "acquire", "--task", "first"],
        0,
    );
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    let saved = invoke(&repo, &["release", "--reservation-id", id, "--save"], 0);
    let next = invoke(
        &repo,
        &["--pool", "frontend", "acquire", "--task", "next"],
        0,
    );
    assert_eq!(
        invoke(&repo, &["release", "--reservation-id", id, "--save"], 0)["result"]["snapshot"]
            ["id"],
        saved["result"]["snapshot"]["id"]
    );
    assert_eq!(
        invoke(&repo, &["show", "--task", "next"], 0)["result"]["reservation"]["reservation_id"],
        next["result"]["reservation_id"]
    );
    invoke(&repo, &["release", "--task", "next"], 0);
}

#[test]
fn listing_includes_every_pool_before_global_pagination() {
    let repo = Repo::new();
    for (name, task) in [("frontend", "nav"), ("backend", "api")] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                name,
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
            ],
            0,
        );
        invoke(&repo, &["--pool", name, "acquire", "--task", task], 0);
    }
    let all = invoke(&repo, &["list", "--unlimited"], 0);
    assert_eq!(all["result"]["reservations"].as_array().unwrap().len(), 2);
    assert_eq!(all["result"]["reservations"][0]["pool"], "backend");
    assert_eq!(all["result"]["reservations"][1]["pool"], "frontend");
    let page = invoke(&repo, &["list", "--limit", "1", "--offset", "1"], 0);
    assert_eq!(page["result"]["pagination"]["total"], 2);
    assert_eq!(page["result"]["reservations"][0]["pool"], "frontend");
    let filtered = invoke(&repo, &["list", "--pool", "frontend"], 0);
    assert_eq!(
        filtered["result"]["reservations"].as_array().unwrap().len(),
        1
    );
    let human = support::groove_human_in(&repo.path)
        .arg("list")
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8(human.stdout).unwrap().contains("POOL"));
    invoke(&repo, &["release", "--task", "nav"], 0);
    invoke(&repo, &["release", "--task", "api"], 0);
}

#[test]
fn show_infers_the_reservation_from_a_workspace_and_its_subdirectories() {
    let repo = Repo::new();
    invoke(
        &repo,
        &[
            "pool",
            "init",
            "frontend",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
        0,
    );
    let lease = invoke(
        &repo,
        &["--pool", "frontend", "acquire", "--task", "nav"],
        0,
    );
    let path = std::path::Path::new(lease["result"]["workspace_path"].as_str().unwrap());
    std::fs::create_dir(path.join("nested")).unwrap();
    for directory in [path.to_owned(), path.join("nested")] {
        let output = support::groove_in(&directory).arg("show").output().unwrap();
        let shown = support::assert_success(&output);
        assert_eq!(
            shown["result"]["reservation"]["reservation_id"],
            lease["result"]["reservation_id"]
        );
        assert_eq!(shown["result"]["active"], true);
    }
    invoke(&repo, &["release", "--task", "nav"], 0);
}
