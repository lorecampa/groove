#[path = "support/json_progress.rs"]
mod json_progress;
mod support;
use serde_json::Value;
use support::{assert_success, Repo};

fn cli(r: &Repo, args: &[&str]) -> std::process::Output {
    support::groove_in(&r.path)
        .env("HOME", r.temp.path())
        .env("GROOVE_LOG_DIR", r.path.join("test-logs"))
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env_remove("GROOVE_LOG_ENABLED")
        .arg("--repo")
        .arg(&r.path)
        .args(args)
        .output()
        .unwrap()
}
fn check(output: &std::process::Output) -> Value {
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}: {v}");
    v
}

#[test]
#[cfg(feature = "test-hooks")]
fn unwinding_panics_return_an_error_envelope_and_keep_protection() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo,
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
    ));
    // The test-only checkpoint panics if its notification socket is missing,
    // after durable protection was published but before the command returned.
    let output = support::groove_in(&repo.path)
        .args(["--request-id", "panic-request", "acquire"])
        .env("GROOVE_TEST_CHECKPOINT", "reuse-protected")
        .env("GROOVE_TEST_SOCKET", repo.temp.path().join("missing.sock"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let response = check(&output);
    assert_eq!(response["request_id"], "panic-request");
    assert_eq!(response["error"]["code"], "PROTOCOL_ERROR");
    assert_eq!(response["error"]["retryable"], false);
    assert_eq!(
        groove_core::Pool::open(&repo.path)
            .unwrap()
            .protected_leases()
            .unwrap()
            .len(),
        1
    );
    let exhausted = cli(&repo, &["acquire"]);
    assert_eq!(check(&exhausted)["error"]["code"], "POOL_EXHAUSTED");
}

#[test]
fn preference_defaults_in_help_can_be_used_as_a_real_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let config_home = temp.path().join("config");
    let run = |args: &[&str]| {
        support::groove_in(temp.path())
            .env("XDG_CONFIG_HOME", &config_home)
            .env_remove("GROOVE_LOG_DIR")
            .env_remove("GROOVE_LOG_ENABLED")
            .args(args)
            .output()
            .unwrap()
    };
    let original = run(&["config", "show"]);
    assert_success(&original);
    let expected = check(&original)["result"]["config"].clone();
    let output = run(&["config", "--help"]);
    assert_success(&output);
    let response = check(&output);
    let help = response["result"]["text"].as_str().unwrap();
    let defaults = help
        .split_once("PREFERENCE DEFAULTS (TOML, before environment overrides):\n")
        .unwrap()
        .1
        .split_once("\nOUTPUT CONTRACT:")
        .unwrap()
        .0;
    let directory = config_home.join("groove");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("config.toml"), defaults).unwrap();
    let configured = run(&["config", "show"]);
    assert_success(&configured);
    assert_eq!(check(&configured)["result"]["config"], expected);
    let leaf = run(&["config", "set", "--help"]);
    let leaf = check(&leaf);
    let leaf = leaf["result"]["text"].as_str().unwrap();
    assert!(leaf.contains("groove config --help"));
    assert!(!leaf.contains("PREFERENCE DEFAULTS"));
}

#[test]
fn command_help_exposes_its_output_fields_without_repository_or_preferences() {
    let temp = tempfile::tempdir().unwrap();
    let preferences = temp.path().join("config/groove");
    std::fs::create_dir_all(&preferences).unwrap();
    std::fs::write(preferences.join("config.toml"), "invalid = [").unwrap();
    let run = |args: &[&str], json: bool| {
        support::groove_human_in(temp.path())
            .env("XDG_CONFIG_HOME", temp.path().join("config"))
            .args(if json { vec!["--json"] } else { vec![] })
            .args(args)
            .output()
            .unwrap()
    };
    for (command, fields) in [
        (
            vec!["acquire"],
            vec![
                "acquire -> result: reservation",
                "reservation_id: uuid",
                "workspace_path: string",
                "idempotency_key (optional): string",
            ],
        ),
        (
            vec!["release"],
            vec!["release -> result: release_result", "release:", "snapshot:"],
        ),
        (
            vec!["snapshot", "restore"],
            vec![
                "snapshot restore -> result: snapshot_restore_result",
                "snapshot_id: uuid",
                "safety_snapshot_id:",
            ],
        ),
    ] {
        let mut args = command;
        args.push("--help");
        let human = run(&args, false);
        assert!(human.status.success());
        let human = String::from_utf8(human.stdout).unwrap();
        for field in fields {
            assert!(human.contains(field), "{args:?}: missing {field}");
        }
        for rule in ["groove --help", "error: object", "retryable: boolean"] {
            assert!(human.contains(rule), "{args:?}: missing {rule}");
        }
        assert!(!human.contains("JSON OUTPUT CONTRACT"));
        assert!(!human.contains("ERROR EXIT CODES:"));
        let json = check(&run(&args, true));
        assert_eq!(json["result"]["text"], human);
    }
    let root = run(&["--help"], false);
    assert!(root.status.success());
    let root = String::from_utf8(root.stdout).unwrap();
    for rule in [
        "JSON OUTPUT CONTRACT",
        "request_id",
        "exit status",
        "api_version",
    ] {
        assert!(root.contains(rule), "root help: missing {rule}");
    }
    for mapping in ["2: INVALID_ARGUMENT", "3: POOL_EXHAUSTED", "7: IO_ERROR"] {
        assert!(root.contains(mapping));
    }
    assert!(!temp.path().join(".git").exists());
}

#[test]
fn command_output_types_used_by_help_validate_real_responses() {
    let repo = Repo::new();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    let verify = |name: &str, args: &[&str]| {
        let result = assert_success(&cli(&repo, args));
        let variants: Vec<_> = schema["x-groove-command-results"][name]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| serde_json::json!({"$ref": format!("#/$defs/{}", name.as_str().unwrap())}))
            .collect();
        let contract = serde_json::json!({"$schema": schema["$schema"], "$defs": schema["$defs"], "anyOf": variants});
        let validator = jsonschema::validator_for(&contract).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&result["result"])
            .map(|error| error.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
        result
    };
    verify("pool init", &["pool", "init", "--capacity", "1"]);
    let lease = verify("acquire", &["acquire", "--task", "contract"]);
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    verify("list", &["list"]);
    verify("show", &["show", "--reservation-id", id]);
    let finish = verify("release", &["release", "--save", "--reservation-id", id]);
    let snapshot = finish["result"]["snapshot"]["id"].as_str().unwrap();
    verify("snapshot show", &["snapshot", "show", snapshot]);
    verify("snapshot list", &["snapshot", "list"]);
    verify(
        "snapshot prune",
        &["snapshot", "prune", "--older-than", "30d"],
    );
    verify(
        "snapshot storage",
        &["snapshot", "storage", "--largest", "10"],
    );
    let destination = repo.temp.path().join("restored");
    verify(
        "snapshot restore",
        &[
            "snapshot",
            "restore",
            snapshot,
            "--to",
            destination.to_str().unwrap(),
        ],
    );
}

#[test]
fn every_output_path_is_one_json_response() {
    let r = Repo::new();
    for (args, exit, text) in [
        (vec!["--help"], 0, Some("Usage:")),
        (vec!["--version"], 0, Some(env!("CARGO_PKG_VERSION"))),
        (vec![], 2, None),
        (vec!["unknown"], 2, None),
        (vec!["pool", "init", "--capacity", "0"], 2, None),
        (vec!["acquire", "--lease-channel-fd", "3"], 2, None),
        (vec!["release", "--reservation-id", "no"], 2, None),
        (vec!["release"], 2, None),
    ] {
        let output = cli(&r, &args);
        let response = check(&output);
        assert_eq!(output.status.code(), Some(exit), "{args:?}: {response}");
        assert_eq!(response["ok"], exit == 0, "{args:?}: {response}");
        if let Some(text) = text {
            assert!(response["result"]["text"].as_str().unwrap().contains(text));
        } else {
            assert_eq!(response["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        }
        if exit == 0 {
            assert!(output.stderr.is_empty());
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("INVALID_ARGUMENT"));
        }
        assert!(
            !r.state().exists(),
            "{args:?} unexpectedly initialized a pool"
        );
        assert_eq!(
            std::fs::read(r.path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
    }
    let output = cli(&r, &["pool", "init", "--capacity", "2"]);
    assert_success(&output);
    let v = check(&output);
    assert_eq!(v["request_id"], Value::Null);
    assert_eq!(v["result"]["created"], true);
    let output = cli(
        &r,
        &["--request-id", "again", "pool", "init", "--capacity", "2"],
    );
    assert_eq!(check(&output)["error"]["code"], "CONFIG_CONFLICT");
    assert_eq!(output.status.code(), Some(4));
    let output = cli(&r, &["-v", "pool", "init", "--capacity", "3"]);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(check(&output)["error"]["code"], "CONFIG_CONFLICT");
    assert!(String::from_utf8_lossy(&output.stderr).contains("CONFIG_CONFLICT"));
}
#[test]
fn concurrent_init_creates_exactly_one_pool() {
    let r = Repo::new();
    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(
            support::groove_in(&r.path)
                .arg("--repo")
                .arg(&r.path)
                .args(["pool", "init", "--capacity", "2"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut created = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        let response = check(&out);
        if response["ok"] == true {
            assert_success(&out);
            assert_eq!(response["result"]["created"], true);
            created += 1;
        } else {
            assert_eq!(out.status.code(), Some(4));
            assert_eq!(response["error"]["code"], "CONFIG_CONFLICT");
        }
    }
    assert_eq!(created, 1);
}

#[test]
fn persisted_metadata_matches_published_schemas() {
    let r = Repo::new();
    assert_success(&cli(&r, &["pool", "init", "--capacity", "1"]));
    let pool = groove_core::Pool::open(&r.path).unwrap();
    let mut lease = pool.acquire(Default::default()).unwrap();
    for (file, text) in [
        (
            "config.json",
            include_str!("../schemas/config-v1.schema.json"),
        ),
        (
            "slots/wt-0001.json",
            include_str!("../schemas/slot-v1.schema.json"),
        ),
    ] {
        let schema: Value = serde_json::from_str(text).unwrap();
        let record: Value =
            serde_json::from_slice(&std::fs::read(r.state().join(file)).unwrap()).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&record)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{file}: {errors:?}");
    }
    lease.release().unwrap();
}

#[test]
fn settings_and_read_commands_match_the_protocol() {
    let r = Repo::new();
    assert_success(&cli(&r, &["pool", "init", "--capacity", "1"]));
    let lease = check(&cli(&r, &["acquire", "--task", "live"]));
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    let configured = cli(&r, &["pool", "set", "--capacity", "3", "--cleanup", "warm"]);
    assert_success(&configured);
    let response = check(&configured);
    assert_eq!(response["result"]["pool"]["capacity"], 3);
    assert_eq!(response["result"]["updated"], true);
    assert_eq!(
        check(&cli(&r, &["pool", "show"]))["result"]["pool"]["cleanup_policy"],
        "warm"
    );
    assert_eq!(
        check(&cli(&r, &["list", "--slots"]))["result"]["inspection"]["slots"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        check(&cli(&r, &["show", "--reservation-id", id]))["result"]["reservation"]["task"],
        "live"
    );
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/config-v1.schema.json")).unwrap();
    let record: Value =
        serde_json::from_slice(&std::fs::read(r.state().join("config.json")).unwrap()).unwrap();
    assert!(jsonschema::validator_for(&schema)
        .unwrap()
        .is_valid(&record));
    // Removed spellings fail as argument errors rather than doing something else.
    for old in [
        vec!["save"],
        vec!["config", "--global"],
        vec!["show", "--snapshot", id],
        vec!["list", "--snapshots"],
        vec!["pool", "init", "--capacity", "1", "--backend", "git"],
        vec![
            "exec",
            "--reservation-id",
            id,
            "--cgroup-parent",
            "/",
            "--",
            "true",
        ],
        vec!["inspect"],
        vec!["reservations"],
    ] {
        let out = cli(&r, &old);
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(check(&out)["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_success(&cli(&r, &["release", "--reservation-id", id]));
}

#[test]
fn malformed_identifiers_are_argument_errors_that_keep_the_request_id() {
    let r = Repo::new();
    for args in [
        ["show", "--slot", "../outside"],
        ["show", "--slot", "wt-0000"],
        ["show", "--slot", "wt-4097"],
        ["show", "--slot", "wt-1"],
        ["release", "--reservation-id", "not-a-uuid"],
    ] {
        let output = cli(
            &r,
            &[&["--request-id", "invalid-id"][..], &args[..]].concat(),
        );
        assert_eq!(output.status.code(), Some(2));
        let response = check(&output);
        assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(response["request_id"], "invalid-id");
    }
}

#[test]
fn published_slot_schema_agrees_with_validated_rust_ids() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema["$defs"]["slot"]).unwrap();
    for number in 0..=9999 {
        let text = format!("wt-{number:04}");
        assert_eq!(
            validator.is_valid(&serde_json::json!(text)),
            text.parse::<groove_core::SlotId>().is_ok(),
            "{text}"
        );
    }
}

#[test]
fn logging_flags_preserve_protocol_and_failures_are_best_effort() {
    let r = Repo::new();
    let logs = r.path.join("test-logs");
    let output = cli(&r, &["pool", "init", "--capacity", "1"]);
    assert_success(&output);
    json_progress::assert_only(&output, "pool init");
    let entries: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .collect();
    assert_eq!(entries.len(), 1);
    let contents = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(contents.contains("Command completed"));
    for line in contents.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
    let output = support::groove_in(&r.path)
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env("GROOVE_LOG_ENABLED", "false")
        .arg("--repo")
        .arg(&r.path)
        .args(["-vvv", "acquire"])
        .output()
        .unwrap();
    let lease = assert_success(&output)["result"].clone();
    assert!(String::from_utf8_lossy(&output.stderr).contains("TRACE"));
    // A regular file cannot be a log directory. Release still succeeds.
    let blocked = r.path.join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    let output = support::groove_in(&r.path)
        .arg("--repo")
        .arg(&r.path)
        .env("GROOVE_LOG_DIR", blocked)
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env_remove("GROOVE_LOG_ENABLED")
        .args([
            "release",
            "--reservation-id",
            lease["reservation_id"].as_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("WARN File logging unavailable"));
    assert_eq!(
        std::fs::read_dir(&logs)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .count(),
        1
    );
    let removed = cli(&r, &["--diagnostics", "list"]);
    assert_eq!(check(&removed)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn logging_controls_apply_to_help_and_invalid_arguments() {
    let r = Repo::new();
    let logs = r.path.join("test-logs");
    for args in [vec!["--help"], vec!["unknown"]] {
        let out = support::groove_in(&r.path)
            .env("XDG_CONFIG_HOME", r.path.join("config-home"))
            .env("GROOVE_LOG_DIR", &logs)
            .env("GROOVE_LOG_ENABLED", "false")
            .args(args)
            .output()
            .unwrap();
        check(&out);
        assert!(!logs.exists());
    }
    let out = cli(&r, &["-v", "unknown"]);
    assert_eq!(check(&out)["error"]["code"], "INVALID_ARGUMENT");
    assert!(String::from_utf8_lossy(&out.stderr).contains("INVALID_ARGUMENT"));
    assert!(std::fs::read_dir(logs)
        .unwrap()
        .flatten()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl")));
}

#[test]
fn command_help_is_available_without_a_repository_or_valid_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_home = temp.path().join("config");
    std::fs::create_dir_all(config_home.join("groove")).unwrap();
    std::fs::write(config_home.join("groove/config.toml"), "invalid toml [").unwrap();
    let commands: &[&[&str]] = &[
        &[],
        &["pool"],
        &["pool", "list"],
        &["pool", "destroy"],
        &["pool", "init"],
        &["acquire"],
        &["release"],
        &["show"],
        &["list"],
        &["completions"],
        &["release", "--save"],
        &["config"],
        &["pool", "show"],
        &["pool", "set"],
        &["pool", "get"],
        &["config"],
        &["config", "path"],
        &["config", "edit"],
        &["config", "set"],
        &["config", "unset"],
        &["config", "show"],
        &["config", "get"],
        &["snapshot"],
        &["snapshot", "save"],
        &["snapshot", "list"],
        &["snapshot", "show"],
        &["snapshot", "diff"],
        &["snapshot", "restore"],
        &["snapshot", "delete"],
    ];
    for command in commands {
        for flag in ["-h", "--help"] {
            let output = support::groove_in(temp.path())
                .current_dir(temp.path())
                .env("XDG_CONFIG_HOME", &config_home)
                .env("GROOVE_LOG_ENABLED", "false")
                .args(*command)
                .arg(flag)
                .output()
                .unwrap();
            assert!(output.status.success(), "{command:?}: {output:?}");
            assert!(output.stderr.is_empty());
            let response = check(&output);
            assert_eq!(response["ok"], true);
            let help = response["result"]["text"].as_str().unwrap();
            assert!(help.contains("Usage:"), "{command:?}: {help}");
            if flag == "-h" {
                assert!(
                    help.contains("--help for detailed help"),
                    "{command:?}: {help}"
                );
                assert!(!help.contains("OUTPUT TYPES"), "{command:?}: {help}");
                assert!(!help.contains("PREFERENCE DEFAULTS"), "{command:?}: {help}");
            } else {
                assert!(help.contains("-h for concise help"), "{command:?}: {help}");
            }
            if *command == ["release"] {
                assert!(help.contains("groove release --reservation-id UUID"));
                assert!(help.contains("groove release --save"));
                assert!(!help.contains("--writers-stopped"));
                assert!(help.contains("next acquisition"));
            }
        }
    }
    let output = support::groove_in(temp.path())
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROOVE_LOG_ENABLED", "false")
        .args(["snapshot", "help", "restore"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let response = check(&output);
    let help = response["result"]["text"].as_str().unwrap();
    assert!(help.contains("safety snapshot"));
    assert!(help.contains("must not exist"));
    assert!(help.contains("HEAD must equal the"));
}

#[test]
fn concise_and_detailed_help_preserve_safety_and_json_output() {
    let temp = tempfile::tempdir().unwrap();
    let cases: &[(&[&str], &[&str])] = &[
        (&[], &["Reservations persist", "Stop all writers"]),
        (&["acquire"], &["detached HEAD", "lost response"]),
        (
            &["release"],
            &["Stop ALL writers", "ignored", "--force", "work can be lost"],
        ),
        (
            &["pool", "destroy"],
            &["stop ALL writers", "Deletes pool checkouts", "--force"],
        ),
        (
            &["snapshot", "save"],
            &["Stop all writers", "Ignored files", "do not back"],
        ),
        (
            &["snapshot", "restore"],
            &[
                "Stop all writers",
                "overwrites",
                "removes files",
                "safety snapshot",
            ],
        ),
        (
            &["snapshot", "delete"],
            &["Deletes", "--force", "pinned snapshot"],
        ),
        (&["config"], &["every repository"]),
    ];
    for (command, safety) in cases {
        let mut texts = Vec::new();
        for flag in ["-h", "--help"] {
            let human = support::groove_human_in(temp.path())
                .args(*command)
                .arg(flag)
                .output()
                .unwrap();
            assert!(human.status.success(), "{command:?}: {human:?}");
            assert!(human.stderr.is_empty());
            let text = String::from_utf8(human.stdout).unwrap();
            assert!(text.contains("Global options:"), "{command:?}: {text}");
            let json = support::groove_in(temp.path())
                .args(*command)
                .arg(flag)
                .output()
                .unwrap();
            assert!(json.status.success());
            assert!(json.stderr.is_empty());
            let response = check(&json);
            assert_eq!(response["ok"], true);
            assert_eq!(response["result"]["text"], text);
            texts.push(text);
        }
        let short = &texts[0];
        let long = &texts[1];
        assert!(
            short.len() < long.len(),
            "{command:?}: short help must be shorter"
        );
        assert!(
            short.lines().count() < 80,
            "{command:?}: concise help should be scannable"
        );
        for rule in *safety {
            assert!(short.contains(rule), "{command:?}: missing {rule}: {short}");
        }
        assert!(long.contains(if command.is_empty() {
            "JSON OUTPUT CONTRACT"
        } else {
            "OUTPUT TYPES"
        }));

        // The help subcommand remains an alternate route to the detailed reference.
        let output = support::groove_human_in(temp.path())
            .arg("help")
            .args(*command)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap(), *long);
    }
}

#[test]
fn release_save_options_require_save_and_rejections_keep_the_reservation() {
    let repo = Repo::new();
    assert_success(&cli(&repo, &["pool", "init", "--capacity", "1"]));
    let lease = assert_success(&cli(&repo, &["acquire", "--task", "unfinished"]));
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    let workspace = std::path::Path::new(lease["result"]["workspace_path"].as_str().unwrap());
    std::fs::write(workspace.join("tracked.txt"), "work in progress").unwrap();
    for args in [
        vec!["finish", "--reservation-id", id],
        vec![
            "release",
            "--reservation-id",
            id,
            "--description",
            "unfinished",
        ],
        vec!["release", "--reservation-id", id, "--max-file-size", "0"],
    ] {
        let output = cli(&repo, &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(check(&output)["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(
            assert_success(&cli(&repo, &["show", "--reservation-id", id]))["result"]["reservation"]
                ["reservation_id"],
            id
        );
        assert_eq!(
            std::fs::read(workspace.join("tracked.txt")).unwrap(),
            b"work in progress"
        );
    }
    let output = cli(
        &repo,
        &["release", "--save", "--force", "--reservation-id", id],
    );
    assert!(output.status.success(), "{output:?}");
    let saved = check(&output);
    assert_eq!(saved["result"]["release"]["released"], true);
    assert_eq!(saved["result"]["snapshot"]["pinned"], true);
    assert_eq!(
        saved["result"]["snapshot"]["description"],
        "workspace saved"
    );
    let snapshot = saved["result"]["snapshot"]["id"].as_str().unwrap();
    let recovered = repo.temp.path().join("recovered");
    assert_success(&cli(
        &repo,
        &[
            "snapshot",
            "restore",
            snapshot,
            "--to",
            recovered.to_str().unwrap(),
        ],
    ));
    assert_eq!(
        std::fs::read(recovered.join("tracked.txt")).unwrap(),
        b"work in progress"
    );
}

#[test]
fn human_output_is_the_default_and_json_is_opt_in() {
    let r = Repo::new();
    let human = |args: &[&str]| {
        support::groove_human_in(&r.path)
            .env("HOME", r.temp.path())
            .env("XDG_CONFIG_HOME", r.temp.path().join("config-home"))
            .env("GROOVE_LOG_ENABLED", "false")
            .arg("--repo")
            .arg(&r.path)
            .args(args)
            .output()
            .unwrap()
    };
    let init = human(&["pool", "init", "--capacity", "1"]);
    assert!(init.status.success());
    let text = String::from_utf8(init.stdout).unwrap();
    assert!(text.starts_with("Initialized a pool of 1 slot."), "{text}");
    assert!(serde_json::from_str::<Value>(&text).is_err());
    let acquired = human(&["acquire", "--task", "human"]);
    assert!(String::from_utf8_lossy(&acquired.stdout).contains("reserved for 'human'"));
    let shown = String::from_utf8(human(&["show", "--task", "human"]).stdout).unwrap();
    let path = shown
        .lines()
        .find_map(|line| line.trim().strip_prefix("path:"))
        .unwrap()
        .trim();
    assert!(std::path::Path::new(path).is_dir(), "{shown}");
    // Errors go to stderr with the exit category; stdout stays empty.
    let missing = human(&["release", "--task", "absent"]);
    assert_eq!(missing.status.code(), Some(5));
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("RESERVATION_MISMATCH"));
    // --json overrides the human default.
    let listed = human(&["--json", "list"]);
    assert_eq!(check(&listed)["result"]["reservations"][0]["task"], "human");
    // Help is plain text unless JSON is requested.
    let help = human(&["--help"]);
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("Reusable Git worktree"));
    assert_eq!(check(&human(&["--json", "--version"]))["ok"], true);
}

#[test]
fn legacy_output_environment_cannot_select_json_for_help_version_or_errors() {
    let temp = tempfile::tempdir().unwrap();
    for args in [vec!["--help"], vec!["--version"], vec!["--unknown-option"]] {
        for json in [false, true] {
            let mut command = support::groove_human_in(temp.path());
            command.env("GROOVE_OUTPUT", "json").args(&args);
            if json {
                command.arg("--json");
            }
            let output = command.output().unwrap();
            let is_error = args == ["--unknown-option"];
            assert_eq!(output.status.code(), Some(if is_error { 2 } else { 0 }));
            if json {
                let response = check(&output);
                assert_eq!(response["ok"], !is_error);
                if is_error {
                    assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
                } else {
                    assert!(response["result"]["text"].is_string());
                }
            } else if is_error {
                assert!(output.stdout.is_empty());
                assert!(String::from_utf8_lossy(&output.stderr).contains("--unknown-option"));
            } else {
                let text = String::from_utf8(output.stdout).unwrap();
                assert!(text.contains(if args == ["--help"] {
                    "Usage:"
                } else {
                    "groove"
                }));
                assert!(serde_json::from_str::<Value>(&text).is_err());
            }
        }
    }
}

#[test]
fn completions_preserve_output_modes_without_a_repository_or_valid_config() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    std::fs::create_dir_all(config.join("groove")).unwrap();
    std::fs::write(config.join("groove/config.toml"), "invalid toml [").unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let run = |json_flag: bool| {
            let mut command = support::groove_human_in(temp.path());
            command
                .current_dir(temp.path())
                .env("XDG_CONFIG_HOME", &config)
                .env("GROOVE_OUTPUT", "json") // Legacy settings cannot change output.
                .args(["completions", shell]);
            if json_flag {
                command.arg("--json");
            }
            command.output().unwrap()
        };
        let human = run(false);
        assert!(human.status.success(), "{human:?}");
        assert!(human.stderr.is_empty());
        let script = String::from_utf8(human.stdout).unwrap();
        assert!(script.contains("details"), "{shell}");
        assert!(script.contains("completions"));
        {
            let json = run(true);
            assert_success(&json);
            json_progress::assert_only(&json, "completions");
            assert_eq!(check(&json)["result"]["text"], script);
        }
        if shell == "bash" {
            let path = temp.path().join("groove.bash");
            std::fs::write(&path, script).unwrap();
            let output = std::process::Command::new("bash")
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "source \"$1\"; COMP_WORDS=(groove sh); COMP_CWORD=1; _groove groove sh groove; printf '%s\\n' \"${COMPREPLY[@]}\"",
                    "completion-test",
                ])
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "show");
        }
    }
    let invalid = support::groove_in(temp.path())
        .current_dir(temp.path())
        .args(["completions", "unknown-shell"])
        .env("GROOVE_LOG_ENABLED", "false")
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn common_failures_have_the_same_advice_in_human_and_json_output() {
    let r = Repo::new();
    let expect_hint = |args: &[&str], code: &str, status: i32, action: &str| {
        let json = cli(&r, args);
        let response = check(&json);
        assert_eq!(json.status.code(), Some(status));
        assert!(String::from_utf8_lossy(&json.stderr).contains(code));
        assert_eq!(response["error"]["code"], code);
        let hint = response["error"]["hint"].as_str().unwrap();
        assert!(hint.contains(action), "{hint}");
        assert!(!hint.contains("--force"));
        let human = support::groove_human_in(&r.path)
            .env("GROOVE_LOG_ENABLED", "false")
            .env("XDG_CONFIG_HOME", r.path.join("config-home"))
            .arg("--repo")
            .arg(&r.path)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(human.status.code(), Some(status));
        assert!(human.stdout.is_empty());
        assert!(String::from_utf8_lossy(&human.stderr).contains(&format!("Hint: {hint}")));
    };
    expect_hint(&["list"], "NOT_INITIALIZED", 4, "groove pool init");
    assert_success(&cli(&r, &["pool", "init", "--capacity", "1"]));
    assert_success(&cli(&r, &["acquire", "--task", "busy"]));
    expect_hint(&["acquire"], "POOL_EXHAUSTED", 3, "--wait");
    expect_hint(
        &["acquire", "--base", "missing-revision"],
        "INVALID_REVISION",
        4,
        "--base",
    );
    expect_hint(
        &["show", "--task", "absent"],
        "RESERVATION_MISMATCH",
        5,
        "groove list",
    );
    std::fs::write(r.slot(1).join("tracked.txt"), "unsaved\n").unwrap();
    expect_hint(
        &["release", "--task", "busy"],
        "UNSAVED_CHANGES",
        5,
        "groove release --save",
    );
    assert_eq!(
        check(&cli(&r, &["list"]))["result"]["reservations"][0]["task"],
        "busy"
    );
}

#[test]
fn selected_show_runs_git_status_only_in_the_selected_workspace() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let r = Repo::new();
    assert_success(&cli(&r, &["pool", "init", "--capacity", "2"]));
    let first = check(&cli(&r, &["acquire", "--task", "first"]));
    let second = assert_success(&cli(&r, &["acquire", "--task", "second"]));
    let first_path = first["result"]["workspace_path"].as_str().unwrap();
    let second_path = second["result"]["workspace_path"].as_str().unwrap();
    fs::write(
        std::path::Path::new(first_path).join("tracked.txt"),
        "edited\n",
    )
    .unwrap();
    let real_git = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let bin = r.temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let log = r.temp.path().join("git-status-calls");
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        "#!/bin/bash\nfor arg in \"$@\"; do\n  if [[ $arg == status ]]; then printf '%s\\n' \"$PWD\" >> \"$GROOVE_STATUS_CALLS\"; fi\ndone\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n",
    ).unwrap();
    fs::set_permissions(wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(paths).unwrap();
    let run = |args: &[&str]| {
        fs::write(&log, "").unwrap();
        let output = support::groove_in(&r.path)
            .env("PATH", &path)
            .env("GROOVE_REAL_GIT", real_git.trim())
            .env("GROOVE_STATUS_CALLS", &log)
            .env("GROOVE_LOG_ENABLED", "false")
            .env("XDG_CONFIG_HOME", r.temp.path().join("config"))
            .args(args)
            .output()
            .unwrap();
        (output, fs::read_to_string(&log).unwrap())
    };
    for selector in [
        ["--task", "first"],
        [
            "--reservation-id",
            first["result"]["reservation_id"].as_str().unwrap(),
        ],
        ["--slot", first["result"]["slot_id"].as_str().unwrap()],
    ] {
        let (output, calls) = run(&["show", selector[0], selector[1], "--details"]);
        assert_success(&output);
        let response = check(&output);
        assert_eq!(
            response["result"]["slot"]["git"]["modified"], true,
            "{response}"
        );
        assert_eq!(calls.lines().collect::<Vec<_>>(), [first_path]);
        let (output, calls) = run(&["show", selector[0], selector[1]]);
        assert_success(&output);
        assert!(check(&output)["result"]["slot"]["git"].is_null());
        assert!(calls.is_empty());
    }
    for command in [vec!["pool", "show", "--details"], vec!["list", "--details"]] {
        let (output, calls) = run(&command);
        assert_success(&output);
        check(&output);
        let mut expected = [first_path, second_path];
        expected.sort();
        // Slots are inspected concurrently, so invocation order is not a contract.
        let mut observed = calls.lines().collect::<Vec<_>>();
        observed.sort();
        assert_eq!(observed, expected);
    }
    let (output, calls) = run(&["show", "--slot", "wt-0003", "--details"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(check(&output)["error"]["code"], "INVALID_ARGUMENT");
    assert!(calls.is_empty());
}

#[test]
fn terminal_verbosity_and_file_logging_are_independent() {
    let r = Repo::new();
    assert_success(&cli(&r, &["pool", "init", "--capacity", "1"]));
    for (flag, levels) in [
        (None, vec![]),
        (Some("-q"), vec![]),
        (Some("-v"), vec!["INFO"]),
        (Some("-vv"), vec!["INFO", "DEBUG"]),
        (Some("-vvv"), vec!["INFO", "DEBUG", "TRACE"]),
    ] {
        let logs = r.temp.path().join(flag.unwrap_or("default"));
        let mut cmd = support::groove_in(&r.path);
        cmd.env("XDG_CONFIG_HOME", r.path.join("config-home"))
            .env("GROOVE_LOG_DIR", &logs)
            .env("GROOVE_LOG_ENABLED", "true");
        if let Some(flag) = flag {
            cmd.arg(flag);
        }
        let out = cmd.arg("list").output().unwrap();
        assert_success(&out);
        check(&out);
        let text = String::from_utf8(out.stderr).unwrap();
        for level in ["INFO", "DEBUG", "TRACE"] {
            assert_eq!(
                text.contains(&format!(" {level} ")),
                levels.contains(&level),
                "{flag:?}: {text}"
            );
        }
        let log = std::fs::read_dir(logs)
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .unwrap();
        let contents = std::fs::read_to_string(log.path()).unwrap();
        let events: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events.iter().any(|v| v["level"] == "DEBUG"));
        assert!(!events.iter().any(|v| v["level"] == "TRACE"));
    }
    let blocked = r.temp.path().join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    for quiet in [false, true] {
        for json in [false, true] {
            let mut cmd = support::groove_human_in(&r.path);
            cmd.env("XDG_CONFIG_HOME", r.path.join("config-home"))
                .env("GROOVE_LOG_DIR", &blocked)
                .env("GROOVE_LOG_ENABLED", "true");
            if quiet {
                cmd.arg("--quiet");
            }
            if json {
                cmd.arg("--json");
            }
            let out = cmd.args(["show", "--task", "absent"]).output().unwrap();
            assert_eq!(out.status.code(), Some(5));
            if json {
                assert_eq!(check(&out)["error"]["code"], "RESERVATION_MISMATCH");
            } else {
                assert!(out.stdout.is_empty());
            }
            let stderr = String::from_utf8(out.stderr).unwrap();
            assert_eq!(stderr.contains("WARN File logging unavailable"), !quiet);
            assert_eq!(stderr.matches("(RESERVATION_MISMATCH)").count(), 1);
            assert!(!stderr.contains("Command failed"));
        }
    }
    let invalid = cli(&r, &["--quiet", "-v", "list"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
    let invalid = cli(&r, &["--quiet", "unknown"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("INVALID_ARGUMENT"));
}

#[test]
fn persistent_trace_can_be_enabled_while_terminal_is_quiet() {
    let r = Repo::new();
    assert_success(&cli(&r, &["pool", "init", "--capacity", "1"]));
    assert_success(&cli(&r, &["config", "set", "logging.level=trace"]));
    assert_eq!(
        check(&cli(&r, &["config", "show"]))["result"]["config"]["logging"]["level"],
        "trace"
    );
    let out = cli(&r, &["--quiet", "list"]);
    assert_success(&out);
    assert!(out.stderr.is_empty());
    let events: Vec<Value> = std::fs::read_dir(r.path.join("test-logs"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect();
    assert!(events
        .iter()
        .any(|v| v["command"] == "list" && v["level"] == "TRACE"));
    let invalid = cli(&r, &["config", "set", "logging.level=invalid"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        check(&cli(&r, &["config", "show"]))["result"]["config"]["logging"]["level"],
        "trace"
    );
}

#[test]
fn copied_advice_keeps_pool_selection_and_destroy_does_not_suggest_saving_released_tasks() {
    let repo = Repo::new();
    let missing = cli(&repo, &["--pool", "review", "list"]);
    assert_eq!(missing.status.code(), Some(4));
    let response = check(&missing);
    let command = response["error"]["hint"]
        .as_str()
        .unwrap()
        .split('`')
        .find(|part| part.starts_with("groove pool init"))
        .unwrap();
    let argv = shell_words::split(command).unwrap();
    let initialized = support::groove_in(&repo.path)
        .args(&argv[1..])
        // This advice regression deliberately retains released ignored files.
        .args(["--ready-target", "0", "--idle-timeout", "never"])
        .output()
        .unwrap();
    let initialized = assert_success(&initialized);
    assert_eq!(initialized["result"]["pool_name"], "review");
    assert_eq!(
        check(&cli(&repo, &["list"]))["error"]["code"],
        "NOT_INITIALIZED"
    );
    let lease = assert_success(&cli(&repo, &["--pool", "review", "acquire"]));
    let workspace = std::path::Path::new(lease["result"]["workspace_path"].as_str().unwrap());
    std::fs::create_dir(workspace.join("ignored")).unwrap();
    std::fs::write(workspace.join("ignored/cache"), "keep me").unwrap();
    assert_success(&cli(
        &repo,
        &[
            "--pool",
            "review",
            "release",
            "--save",
            "--reservation-id",
            lease["result"]["reservation_id"].as_str().unwrap(),
        ],
    ));
    let output = cli(&repo, &["--pool", "review", "pool", "destroy", "--dry-run"]);
    assert_eq!(output.status.code(), Some(5));
    let result = check(&output);
    let hint = result["error"]["hint"].as_str().unwrap();
    assert!(hint.contains("ignored"));
    assert!(hint.contains("--force"));
    assert!(!hint.contains("`groove release --save"));
    assert_eq!(
        std::fs::read(workspace.join("ignored/cache")).unwrap(),
        b"keep me"
    );
}

#[test]
fn listing_windows_preserve_order_and_reject_zero_limits() {
    let repo = Repo::new();
    assert_success(&cli(&repo, &["pool", "init", "--capacity", "1"]));
    let lease = assert_success(&cli(&repo, &["acquire"]));
    for n in 0..3 {
        assert_success(
            &support::groove_in(std::path::Path::new(
                lease["result"]["workspace_path"].as_str().unwrap(),
            ))
            .env("GROOVE_LOG_ENABLED", "false")
            .args(["snapshot", "save", &n.to_string()])
            .output()
            .unwrap(),
        );
    }
    {
        let (args, field) = (vec!["snapshot", "list"], "snapshots");
        let all = check(&cli(&repo, &args));
        assert_eq!(all["result"][field].as_array().unwrap().len(), 3);
        let mut page = args.clone();
        page.extend(["--limit", "1", "--offset", "1"]);
        let window = check(&cli(&repo, &page));
        assert_eq!(
            window["result"][field],
            serde_json::json!([all["result"][field][1]])
        );
        assert_eq!(
            window["result"]["pagination"],
            serde_json::json!({
                "total":3, "offset":1, "limit":1, "next_offset":2
            })
        );
        let oldest = check(&cli(
            &repo,
            &[
                "snapshot", "list", "--sort", "created", "--order", "asc", "--limit", "1",
            ],
        ));
        assert_eq!(oldest["result"][field][0], all["result"][field][2]);
        let mut by_id = all["result"][field].as_array().unwrap().clone();
        by_id.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        let sorted = check(&cli(
            &repo,
            &["snapshot", "list", "--sort", "id", "--unlimited"],
        ));
        assert_eq!(sorted["result"][field], serde_json::json!(by_id));
        for sort in ["created", "id"] {
            let mut expected = all["result"][field].as_array().unwrap().clone();
            expected.sort_by(|a, b| {
                let time = if sort == "created" {
                    a["created_at_ns"]
                        .as_u64()
                        .cmp(&b["created_at_ns"].as_u64())
                } else {
                    std::cmp::Ordering::Equal
                };
                time.then_with(|| a["id"].as_str().cmp(&b["id"].as_str()))
            });
            for order in ["asc", "desc"] {
                if order == "desc" {
                    expected.reverse();
                }
                let page = check(&cli(
                    &repo,
                    &[
                        "snapshot", "list", "--sort", sort, "--order", order, "--offset", "1",
                        "--limit", "2",
                    ],
                ));
                assert_eq!(page["result"][field], serde_json::json!(expected[1..]));
            }
        }
        for invalid in [
            vec!["snapshot", "list", "--limit", "1", "--unlimited"],
            vec!["snapshot", "list", "--sort", "unknown"],
            vec!["snapshot", "list", "--order", "unknown"],
        ] {
            let rejected = cli(&repo, &invalid);
            assert_eq!(rejected.status.code(), Some(2));
            assert_eq!(check(&rejected)["error"]["code"], "INVALID_ARGUMENT");
        }
        let mut invalid = args;
        invalid.extend(["--limit", "0"]);
        assert_eq!(cli(&repo, &invalid).status.code(), Some(2));
    }
}

#[test]
fn removed_execution_commands_reject_without_creating_pool_state() {
    let repo = Repo::new();
    for args in [
        vec!["exec", "--task", "task", "--", "true"],
        vec!["stop", "--task", "task"],
        vec!["recover", "--task", "task"],
        vec!["history"],
        vec!["storage", "show"],
        vec!["config", "project"],
        vec!["acquire", "--setup", "dev"],
        vec!["acquire", "--retry"],
        vec!["acquire", "--cache-source", "/tmp"],
    ] {
        let output = cli(&repo, &args);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let result = check(&output);
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"]["code"], "INVALID_ARGUMENT");
        assert!(!repo.state().exists(), "{args:?} created pool state");
    }
}

#[test]
fn snapshot_defaults_bound_real_output_and_unlimited_restores_complete_listing() {
    let repo = Repo::new();
    for n in 0..51 {
        // Diagnostic logs inside the fixture would be writers during capture.
        assert_success(
            &support::groove_in(&repo.path)
                .env("GROOVE_LOG_ENABLED", "false")
                .args(["snapshot", "save", &format!("snapshot-{n}")])
                .output()
                .unwrap(),
        );
    }
    let output = cli(&repo, &["snapshot", "list"]);
    assert_success(&output);
    let default = check(&output);
    assert_eq!(default["result"]["snapshots"].as_array().unwrap().len(), 50);
    assert_eq!(default["result"]["pagination"]["next_offset"], 50);
    let tail = check(&cli(&repo, &["snapshot", "list", "--offset", "50"]));
    assert_eq!(tail["result"]["snapshots"].as_array().unwrap().len(), 1);
    assert!(tail["result"]["pagination"]["next_offset"].is_null());
    let all = check(&cli(&repo, &["snapshot", "list", "--unlimited"]));
    assert_eq!(all["result"]["snapshots"].as_array().unwrap().len(), 51);
    assert!(all["result"]["pagination"]["limit"].is_null());
    let human = support::groove_human_in(&repo.path)
        .args(["snapshot", "list", "--limit", "1"])
        .output()
        .unwrap();
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("--offset 1"));
    assert!(text.contains("--unlimited"));
    assert_eq!(
        text.lines()
            .filter(|line| line.contains("snapshot-"))
            .count(),
        1
    );
}

#[test]
fn pool_and_slot_views_sort_before_paging_and_preserve_capacity() {
    let repo = Repo::new();
    assert_success(&cli(&repo, &["pool", "init", "--capacity", "51"]));
    let mut acquired = Vec::new();
    for label in ["z-last", "a-first"] {
        let lease = assert_success(&cli(&repo, &["acquire", "--task", label]));
        acquired.push((
            lease["result"]["slot_id"].as_str().unwrap().to_owned(),
            label,
        ));
    }
    for args in [vec!["list", "--slots"], vec!["pool", "show"]] {
        let default = check(&cli(&repo, &args));
        assert_eq!(
            default["result"]["inspection"]["slots"]
                .as_array()
                .unwrap()
                .len(),
            50
        );
        assert_eq!(default["result"]["inspection"]["capacity"], 51);
        let mut sorted = args.clone();
        sorted.extend(["--sort", "task", "--offset", "49", "--limit", "1"]);
        let page = check(&cli(&repo, &sorted));
        assert_eq!(
            page["result"]["inspection"]["slots"][0]["reservation"]["task"],
            "a-first"
        );
        let mut descending = args.clone();
        descending.extend(["--sort", "task", "--order", "desc", "--limit", "1"]);
        let page = check(&cli(&repo, &descending));
        assert_eq!(
            page["result"]["inspection"]["slots"][0]["reservation"]["task"],
            "z-last"
        );
    }
    let leases = check(&cli(&repo, &["list", "--sort", "task", "--limit", "1"]));
    assert_eq!(leases["result"]["reservations"][0]["task"], "a-first");
    for sort in ["task", "slot"] {
        let leases = check(&cli(
            &repo,
            &["list", "--sort", sort, "--order", "desc", "--limit", "1"],
        ));
        assert_eq!(
            leases["result"]["reservations"][0]["task"],
            if sort == "task" {
                "z-last"
            } else {
                acquired.iter().max_by_key(|(slot, _)| slot).unwrap().1
            }
        );
    }
    let invalid = cli(&repo, &["show", "--task", "a-first", "--limit", "1"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
    let invalid = cli(&repo, &["show", "--task", "a-first", "--order", "desc"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
    // This catalog fixture needs immediate deletion, not background preparation.
    assert_success(&cli(
        &repo,
        &[
            "--pool",
            "zzz",
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    assert_success(&cli(&repo, &["--pool", "zzz", "pool", "destroy"]));
    for (sort, first) in [("name", "default"), ("state", "zzz")] {
        let pools = check(&cli(
            &repo,
            &["pool", "list", "--all", "--sort", sort, "--limit", "1"],
        ));
        assert_eq!(pools["result"]["pools"][0]["name"], first);
        assert_eq!(pools["result"]["pagination"]["total"], 2);
        let reversed = check(&cli(
            &repo,
            &[
                "pool", "list", "--all", "--sort", sort, "--order", "desc", "--limit", "1",
            ],
        ));
        assert_eq!(
            reversed["result"]["pools"][0]["name"],
            if sort == "name" { "zzz" } else { "default" }
        );
    }
}

#[test]
fn removed_spellings_and_unused_pool_selection_are_v2_usage_errors() {
    let repo = Repo::new();
    for arguments in [
        vec!["init", "--capacity", "1"],
        vec!["pools"],
        vec!["destroy"],
        vec!["show"],
        vec!["config", "global", "show"],
        vec!["acquire", "--request-key", "old"],
        vec!["acquire", "--wait-ms", "0"],
        vec![
            "release",
            "--lease-id",
            "00000000-0000-4000-8000-000000000000",
        ],
        vec![
            "pool",
            "init",
            "--capacity",
            "1",
            "--worktree-root",
            "/tmp/unused",
        ],
        vec!["--git-timeout-ms", "1000", "list"],
        vec!["snapshot", "save", "--max-file-bytes", "100"],
        vec!["pool", "destroy", "--discard-changes"],
        vec!["--pool", "unused", "snapshot", "list"],
        vec!["--pool", "unused", "config", "path"],
        vec!["--pool", "unused", "pool", "list"],
        vec!["--pool", "unused", "completions", "bash"],
    ] {
        let output = cli(&repo, &arguments);
        assert_eq!(output.status.code(), Some(2), "{arguments:?}");
        let response = check(&output);
        assert_eq!(response["api_version"], 2);
        assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    }
}

#[test]
fn path_only_and_plain_release_use_the_v2_presentation_contract() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo,
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
    ));
    let output = support::groove_human_in(&repo.path)
        .args(["acquire", "--task", "path", "--path-only"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        format!("{}\n", repo.slot(1).display()).as_bytes()
    );
    let shown = check(&cli(&repo, &["show", "--task", "path"]));
    let reservation = &shown["result"]["reservation"];
    assert!(reservation["reservation_id"].is_string());
    assert!(reservation["workspace_path"].is_string());
    assert!(reservation.get("lease_id").is_none());
    let released = check(&cli(&repo, &["release", "--task", "path"]));
    assert!(released["result"]["release"]["reservation_id"].is_string());
    assert_eq!(released["result"]["snapshot"], Value::Null);
    assert!(released["result"].get("snapshot").is_some());
    let disk: Value =
        serde_json::from_slice(&std::fs::read(repo.state().join("slots/wt-0001.json")).unwrap())
            .unwrap();
    assert!(disk["lease"].get("lease_id").is_some(), "{disk}");
    assert!(disk["lease"].get("reservation_id").is_none());
}
