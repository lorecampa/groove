mod support;
use serde_json::Value;
use std::process::Command;
use support::{assert_success, Repo};

fn cli(r: &Repo, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_groove"))
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
    let output = cli(&r, &["--diagnostics", "init", "--capacity", "3"]);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(check(&output)["error"]["code"], "CONFIG_CONFLICT");
    for line in output
        .stderr
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
    {
        serde_json::from_slice::<Value>(line).unwrap();
    }
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
