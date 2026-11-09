mod support;
use serde_json::Value;
use std::process::Command;
use support::{assert_success, Repo};

fn cli(r: &Repo, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_groove"))
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
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}: {v}");
    v
}
#[test]
fn every_output_path_is_one_json_response() {
    let r = Repo::new();
    for args in [
        vec!["--help"],
        vec!["--version"],
        vec![],
        vec!["unknown"],
        vec!["init", "--capacity", "0"],
        vec!["acquire", "--base", "HEAD", "--lease-channel-fd", "99999"],
        vec![
            "release",
            "--slot",
            "../bad",
            "--lease-id",
            "no",
            "--lease-fd",
            "99999",
        ],
    ] {
        let output = cli(&r, &args);
        check(&output);
        assert!(output.stderr.is_empty());
    }
    let output = cli(&r, &["init", "--capacity", "2"]);
    assert_success(&output);
    let v = check(&output);
    assert_eq!(v["request_id"], Value::Null);
    assert_eq!(v["result"]["created"], true);
    let output = cli(&r, &["--request-id", "again", "init", "--capacity", "2"]);
    assert_eq!(check(&output)["result"]["created"], false);
    let output = cli(&r, &["-v", "init", "--capacity", "3"]);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(check(&output)["error"]["code"], "CONFIG_CONFLICT");
    assert!(String::from_utf8_lossy(&output.stderr).contains("CONFIG_CONFLICT"));
}
#[test]
fn concurrent_init_is_idempotent() {
    let r = Repo::new();
    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(
            Command::new(env!("CARGO_BIN_EXE_groove"))
                .arg("--repo")
                .arg(&r.path)
                .args(["init", "--capacity", "2"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut created = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert_success(&out);
        if check(&out)["result"]["created"] == true {
            created += 1;
        }
    }
    assert_eq!(created, 1);
}

#[test]
fn persisted_metadata_matches_published_schemas() {
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
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
fn release_rejects_malformed_identifiers_before_descriptor_access() {
    let r = Repo::new();
    let valid_lease = "00000000-0000-4000-8000-000000000001";
    for (slot, lease) in [
        ("../outside", valid_lease),
        ("wt-0000", valid_lease),
        ("wt-4097", valid_lease),
        ("wt-1", valid_lease),
        ("wt-0001", "not-a-uuid"),
    ] {
        let output = cli(
            &r,
            &[
                "--request-id",
                "invalid-id",
                "release",
                "--slot",
                slot,
                "--lease-id",
                lease,
                "--lease-fd",
                "99999",
            ],
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
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
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
    let output = cli(&r, &["init", "--capacity", "1"]);
    assert_success(&output);
    assert!(output.stderr.is_empty());
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
    let output = Command::new(env!("CARGO_BIN_EXE_groove"))
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env("GROOVE_LOG_ENABLED", "false")
        .arg("--repo")
        .arg(&r.path)
        .args(["-vv", "acquire"])
        .output()
        .unwrap();
    let lease = assert_success(&output)["result"].clone();
    assert!(String::from_utf8_lossy(&output.stderr).contains("TRACE"));
    // A regular file cannot be a log directory. Release still succeeds.
    let blocked = r.path.join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_groove"))
        .arg("--repo")
        .arg(&r.path)
        .env("GROOVE_LOG_DIR", blocked)
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env_remove("GROOVE_LOG_ENABLED")
        .args([
            "release",
            "--slot",
            lease["slot_id"].as_str().unwrap(),
            "--lease-id",
            lease["lease_id"].as_str().unwrap(),
            "--writers-stopped",
        ])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(output.stderr.is_empty());
    assert_eq!(
        std::fs::read_dir(&logs)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .count(),
        1
    );
    let removed = cli(&r, &["--diagnostics", "leases"]);
    assert_eq!(check(&removed)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn logging_controls_apply_to_help_and_invalid_arguments() {
    let r = Repo::new();
    let logs = r.path.join("test-logs");
    for args in [vec!["--help"], vec!["unknown"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_groove"))
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
    assert!(String::from_utf8_lossy(&out.stderr).contains("Invalid command arguments"));
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
        &["init"],
        &["acquire"],
        &["release"],
        &["leases"],
        &["config"],
        &["config", "show"],
        &["config", "path"],
        &["config", "set"],
        &["config", "unset"],
        &["config", "edit"],
        &["snapshot"],
        &["snapshot", "save"],
        &["snapshot", "list"],
        &["snapshot", "show"],
        &["snapshot", "diff"],
        &["snapshot", "restore"],
        &["snapshot", "delete"],
        &["snapshot", "pin"],
        &["snapshot", "unpin"],
        &["snapshot", "prune"],
        &["snapshot", "retention"],
        &["snapshot", "compact"],
    ];
    for command in commands {
        for flag in ["-h", "--help"] {
            let output = Command::new(env!("CARGO_BIN_EXE_groove"))
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
        }
    }
    let output = Command::new(env!("CARGO_BIN_EXE_groove"))
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROOVE_LOG_ENABLED", "false")
        .args(["help", "snapshot", "restore"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let response = check(&output);
    let help = response["result"]["text"].as_str().unwrap();
    assert!(help.contains("safety snapshot"));
    assert!(help.contains("destination must not exist"));
    assert!(help.contains("HEAD must equal the saved base"));
}
