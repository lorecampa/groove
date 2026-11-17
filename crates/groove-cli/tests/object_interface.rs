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
fn snapshot_prefixes_and_bulk_deletion_preserve_unconfirmed_checkpoints() {
    let repo = Repo::new();
    std::fs::write(repo.path.join("tracked.txt"), "saved bytes\n").unwrap();
    let first = invoke(&repo, &["snapshot", "save", "-m", "first"], 0);
    let first = first["result"]["snapshot"]["id"].as_str().unwrap();
    let second = invoke(&repo, &["snapshot", "save", "-m", "second"], 0);
    let second = second["result"]["snapshot"]["id"].as_str().unwrap();
    let prefix = &first[..8];
    assert_eq!(
        invoke(&repo, &["snapshot", "show", prefix], 0)["result"]["snapshot"]["id"],
        first
    );
    invoke(&repo, &["snapshot", "show", &first[..7]], 2);
    invoke(&repo, &["snapshot", "diff", prefix, &second[..8]], 0);
    let destination = repo.temp.path().join("restored");
    invoke(
        &repo,
        &[
            "snapshot",
            "restore",
            prefix,
            "--to",
            destination.to_str().unwrap(),
        ],
        0,
    );
    assert_eq!(
        std::fs::read(destination.join("tracked.txt")).unwrap(),
        b"saved bytes\n"
    );
    invoke(&repo, &["snapshot", "delete", prefix, "00000000", "-y"], 2);
    invoke(&repo, &["snapshot", "show", first], 0);
    invoke(&repo, &["snapshot", "delete", prefix, &second[..8]], 2);
    let preview = invoke(
        &repo,
        &["snapshot", "delete", prefix, &second[..8], "-n"],
        0,
    );
    assert_eq!(preview["result"]["snapshots"].as_array().unwrap().len(), 2);
    invoke(&repo, &["snapshot", "show", first], 0);
    invoke(&repo, &["snapshot", "show", second], 0);
    let deleted = invoke(
        &repo,
        &["snapshot", "delete", prefix, &second[..8], "-y"],
        0,
    );
    assert_eq!(
        deleted["result"]["deleted_snapshots"],
        serde_json::json!([first, second])
    );
    invoke(&repo, &["snapshot", "show", first], 2);
    invoke(&repo, &["snapshot", "show", second], 2);
}

#[test]
fn reservation_sorting_and_pool_slot_inspection_keep_their_object_scope() {
    let repo = Repo::new();
    for pool in ["alpha", "zeta"] {
        invoke(
            &repo,
            &[
                "pool",
                "create",
                pool,
                "--capacity",
                "1",
                "--idle-timeout",
                "never",
            ],
            0,
        );
    }
    let older = invoke(&repo, &["acquire", "z-task", "-p", "alpha"], 0);
    invoke(&repo, &["acquire", "a-task", "-p", "zeta"], 0);
    for (sort, expected) in [
        ("task", "a-task"),
        ("pool", "z-task"),
        ("acquired", "a-task"),
    ] {
        let page = invoke(&repo, &["list", "--sort", sort, "--limit", "1"], 0);
        assert_eq!(
            page["result"]["reservations"][0]["task"], expected,
            "{page}"
        );
    }
    let reversed = invoke(
        &repo,
        &[
            "list", "--sort", "acquired", "--order", "asc", "--limit", "1",
        ],
        0,
    );
    assert_eq!(reversed["result"]["reservations"][0]["task"], "z-task");
    let slot = older["result"]["slot_id"].as_str().unwrap();
    let inspected = invoke(&repo, &["pool", "show", "alpha", "--slot", slot], 0);
    assert_eq!(inspected["result"]["slot"]["slot_id"], slot, "{inspected}");
    invoke(&repo, &["release", "z-task"], 0);
    invoke(&repo, &["release", "a-task"], 0);
}

#[test]
fn concise_help_keeps_examples_and_reference_topics_remain_installed() {
    let repo = Repo::new();
    for command in ["acquire", "release", "show", "list", "upgrade"] {
        let help = support::groove_human_in(&repo.path)
            .args([command, "--help"])
            .output()
            .unwrap();
        assert!(help.status.success());
        let text = String::from_utf8(help.stdout).unwrap();
        assert!(text.contains("EXAMPLE"), "{text}");
        assert!(
            text.lines().count() < 120,
            "{command}: {} lines",
            text.lines().count()
        );
        assert!(!text.contains("OUTPUT TYPES"), "{text}");
    }
    for topic in ["exit-codes", "durations", "recovery", "environment"] {
        let help = invoke(&repo, &["help", topic], 0);
        assert!(!help["result"]["text"].as_str().unwrap().is_empty());
    }
    let contract = invoke(&repo, &["acquire", "--help"], 0);
    assert!(contract["result"]["text"]
        .as_str()
        .unwrap()
        .contains("OUTPUT TYPES"));
    invoke(&repo, &["help", "json"], 2);
    assert!(contract["result"]["text"]
        .as_str()
        .unwrap()
        .contains("reservation_id"));
    let command = invoke(&repo, &["help", "pool", "create"], 0);
    let text = command["result"]["text"].as_str().unwrap();
    assert!(
        text.contains("--capacity") && text.contains("--color"),
        "{text}"
    );
    invoke(&repo, &["help", "does-not-exist"], 2);
}

#[test]
fn pool_selection_is_explicit_owned_or_unique_and_git_budgets_round_trip() {
    let repo = Repo::new();
    invoke(&repo, &["config", "set", "pool-defaults.git-timeout=7m"], 0);
    invoke(
        &repo,
        &[
            "pool",
            "create",
            "frontend",
            "--capacity",
            "1",
            "--idle-timeout",
            "never",
        ],
        0,
    );
    assert_eq!(
        invoke(&repo, &["pool", "get", "git-timeout"], 0)["result"]["value"],
        "7m"
    );
    invoke(&repo, &["pool", "set", "git-timeout=90s"], 0);
    assert_eq!(
        invoke(&repo, &["pool", "get", "frontend", "git_timeout"], 0)["result"]["value"],
        "90s"
    );
    let lease = invoke(&repo, &["acquire", "owned"], 0);
    invoke(
        &repo,
        &[
            "pool",
            "create",
            "backend",
            "--capacity",
            "1",
            "--idle-timeout",
            "never",
            "--git-timeout",
            "3m",
        ],
        0,
    );
    for args in [
        vec!["acquire", "ambiguous"],
        vec!["pool", "show"],
        vec!["pool", "get", "capacity"],
        vec!["pool", "set", "capacity=2"],
    ] {
        let error = invoke(&repo, &args, 2);
        let message = error["error"]["message"].as_str().unwrap();
        assert!(
            message.contains("frontend") && message.contains("backend"),
            "{message}"
        );
    }
    let workspace = std::path::Path::new(lease["result"]["workspace_path"].as_str().unwrap());
    std::fs::create_dir(workspace.join("subdir")).unwrap();
    let owned = support::groove_in(&workspace.join("subdir"))
        .args(["pool", "get", "git-timeout"])
        .output()
        .unwrap();
    assert!(owned.status.success(), "{owned:?}");
    let value: Value = serde_json::from_slice(&owned.stdout).unwrap();
    assert_eq!(value["result"]["value"], "90s");
    invoke(&repo, &["release", "owned"], 0);
}

#[test]
fn deletion_requires_explicit_confirmation_and_messages_save_snapshots() {
    let repo = Repo::new();
    invoke(
        &repo,
        &[
            "pool",
            "create",
            "frontend",
            "--capacity",
            "1",
            "--idle-timeout",
            "never",
        ],
        0,
    );
    invoke(&repo, &["--pool", "frontend", "acquire", "fix"], 0);
    let saved = invoke(&repo, &["release", "fix", "-m", "nav fix"], 0);
    assert_eq!(saved["result"]["snapshot"]["description"], "nav fix");
    let snapshot = saved["result"]["snapshot"]["id"].as_str().unwrap();
    invoke(&repo, &["snapshot", "delete", snapshot, "--pinned"], 0);
    let saved = invoke(&repo, &["snapshot", "save", "-m", "ordinary"], 0);
    let snapshot = saved["result"]["snapshot"]["id"].as_str().unwrap();
    invoke(
        &repo,
        &[
            "snapshot",
            "prune",
            "--older-than",
            "0s",
            "--keep-last",
            "0",
        ],
        2,
    );
    invoke(&repo, &["snapshot", "show", snapshot], 0);
    invoke(
        &repo,
        &[
            "snapshot",
            "prune",
            "--older-than",
            "0s",
            "--keep-last",
            "0",
            "-n",
        ],
        0,
    );
    invoke(&repo, &["snapshot", "show", snapshot], 0);
    invoke(
        &repo,
        &[
            "snapshot",
            "prune",
            "--older-than",
            "0s",
            "--keep-last",
            "0",
            "-y",
        ],
        0,
    );
    invoke(&repo, &["pool", "delete", "frontend"], 2);
    invoke(&repo, &["pool", "show", "frontend"], 0);
    invoke(&repo, &["pool", "delete", "frontend", "-n"], 0);
    invoke(&repo, &["pool", "delete", "frontend", "-y"], 0);
    invoke(&repo, &["pool", "delete", "-y"], 2);
}

#[test]
fn concurrent_acquirers_cannot_publish_the_same_label_in_different_pools() {
    use std::os::unix::fs::MetadataExt;
    let repo = Repo::new();
    for pool in ["frontend", "backend"] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                pool,
                "--capacity",
                "1",
                "--idle-timeout",
                "never",
            ],
            0,
        );
    }
    let children: Vec<_> = ["frontend", "backend"]
        .into_iter()
        .map(|pool| {
            support::TestChild::spawn(
                support::groove_in(&repo.path).args(["--pool", pool, "acquire", "fix"]),
            )
        })
        .collect();
    let results: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|result| result.status.success())
            .count(),
        1,
        "{results:?}"
    );
    let rejected = results
        .iter()
        .find(|result| !result.status.success())
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    let registry = repo.state().join("task-labels");
    let identities = || {
        std::fs::read_dir(&registry)
            .unwrap()
            .filter_map(|entry| {
                let path = entry.unwrap().path();
                (path
                    .extension()
                    .is_some_and(|extension| extension == "lock"))
                .then(|| {
                    let metadata = std::fs::metadata(&path).unwrap();
                    (path, metadata.dev(), metadata.ino())
                })
            })
            .collect::<Vec<_>>()
    };
    let before = identities();
    assert_eq!(before.len(), 1);
    invoke(&repo, &["release", "fix"], 0);
    invoke(&repo, &["--pool", "backend", "acquire", "fix"], 0);
    assert_eq!(identities(), before);
    invoke(&repo, &["release", "fix"], 0);
}

#[test]
fn legacy_duplicate_labels_require_uuid_migration_without_changing_reservations() {
    let repo = Repo::new();
    let mut ids = Vec::new();
    for pool in ["frontend", "backend"] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                pool,
                "--capacity",
                "2",
                "--idle-timeout",
                "never",
            ],
            0,
        );
        let lease = invoke(&repo, &["--pool", pool, "acquire", pool], 0);
        ids.push(
            lease["result"]["reservation_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        // Model pre-upgrade durable metadata. Protection and slot records agree;
        // permanent lock identities and the reservation UUID stay untouched.
        let slots = repo.state().join("pools").join(pool).join("slots");
        for entry in std::fs::read_dir(slots).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let mut record: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                if record["lease"]["reservation_id"].is_string()
                    || record["lease"]["lease_id"].is_string()
                {
                    record["lease"]["task"] = "legacy".into();
                    std::fs::write(path, serde_json::to_vec(&record).unwrap()).unwrap();
                }
            }
        }
    }
    let rejected = invoke(&repo, &["show", "legacy"], 2);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("duplicate"));
    let rejected = invoke(&repo, &["--pool", "frontend", "acquire", "legacy"], 2);
    assert!(rejected["error"]["message"]
        .as_str()
        .unwrap()
        .contains("legacy duplicate"));
    for id in ids {
        invoke(&repo, &["show", &id], 0);
        invoke(&repo, &["release", &id], 0);
    }
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
    assert!(
        !text
            .lines()
            .any(|line| line.trim_start().starts_with("--git-timeout ")),
        "{text}"
    );
    let acquire = invoke(&repo, &["acquire", "--help"], 0);
    let text = acquire["result"]["text"].as_str().unwrap();
    assert!(text.contains("--pool"), "{text}");
    assert!(text.contains("--git-timeout"), "{text}");
}

#[test]
fn true_global_flags_work_on_every_command_and_color_never_leaks_into_json() {
    let repo = Repo::new();
    for command in [
        "acquire",
        "release",
        "path",
        "show",
        "list",
        "pool",
        "snapshot",
        "config",
        "completions",
        "upgrade",
    ] {
        let help = invoke(
            &repo,
            &[
                command,
                "-C",
                repo.path.to_str().unwrap(),
                "--color",
                "never",
                "-q",
                "--request-id",
                "global-test",
                "--help",
            ],
            0,
        );
        assert_eq!(help["request_id"], "global-test");
        assert!(!help["result"]["text"].as_str().unwrap().contains('\u{1b}'));
    }
    let colored = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"))
        .args(["--color", "always", "--help"])
        .output()
        .unwrap();
    assert!(colored.status.success());
    assert!(colored.stdout.contains(&0x1b));
    let plain = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"))
        .env("NO_COLOR", "1")
        .args(["--color", "auto", "--help"])
        .output()
        .unwrap();
    assert!(plain.status.success());
    assert!(!plain.stdout.contains(&0x1b));
    invoke(
        &repo,
        &[
            "pool",
            "create",
            "frontend",
            "--capacity",
            "1",
            "--idle-timeout",
            "never",
        ],
        0,
    );
    invoke(
        &repo,
        &[
            "acquire",
            "short-flags",
            "-p",
            "frontend",
            "-b",
            "HEAD",
            "-w",
            "0s",
            "--timeout",
            "10s",
        ],
        0,
    );
    invoke(
        &repo,
        &[
            "release",
            "short-flags",
            "-s",
            "-m",
            "saved",
            "--timeout",
            "10s",
        ],
        0,
    );
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
        invoke(&repo, &["pool", "get", "default", "capacity"], 0)["result"]["value"],
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
    invoke(&repo, &["pool", "destroy", "frontend", "--yes"], 0);
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
