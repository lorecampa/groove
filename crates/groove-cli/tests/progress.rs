#[path = "support/json_progress.rs"]
mod json_progress;
mod support;

use serde_json::Value;
use support::{assert_success, Repo, TestChild};

#[test]
fn json_contract_guard_accepts_liveness_but_rejects_unrelated_or_malformed_stderr() {
    use std::{os::unix::process::ExitStatusExt, process::Output};
    let output = |stderr: Vec<u8>| Output {
        status: std::process::ExitStatus::from_raw(0),
        stdout: br#"{"api_version":2,"ok":true,"request_id":"contract","result":{}}"#.to_vec(),
        stderr,
    };
    json_progress::assert_only(&output(Vec::new()), "config");
    let valid = serde_json::json!({
        "api_version": 2, "event": "progress", "command": "config",
        "request_id": "contract", "elapsed_ms": 1500
    });
    json_progress::assert_only(&output(serde_json::to_vec(&valid).unwrap()), "config");
    let mut rejected = vec![b"unexpected warning\n".to_vec(), b"{malformed}\n".to_vec()];
    for (key, value) in [
        ("api_version", serde_json::json!(3)),
        ("command", serde_json::json!("show")),
        ("request_id", serde_json::json!("another request")),
        ("elapsed_ms", serde_json::json!(500)),
    ] {
        let mut event = valid.clone();
        event[key] = value;
        rejected.push(serde_json::to_vec(&event).unwrap());
    }
    for stderr in rejected {
        let out = output(stderr);
        assert!(std::panic::catch_unwind(|| json_progress::assert_only(&out, "config")).is_err());
    }
}

#[test]
fn long_waits_report_json_liveness_preserve_errors_and_respect_quiet() {
    let repo = Repo::new();
    assert_success(
        &support::groove_in(&repo.path)
            .args(["pool", "init", "--capacity", "1"])
            .output()
            .unwrap(),
    );
    let lease = assert_success(
        &support::groove_in(&repo.path)
            .args(["acquire", "--task", "occupied"])
            .output()
            .unwrap(),
    );
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/progress-v2.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let response_schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    let response_validator = jsonschema::validator_for(&response_schema).unwrap();
    for json in [false, true] {
        for quiet in [false, true] {
            let mut command = support::groove_human_in(&repo.path);
            command
                .args([
                    "acquire",
                    "--task",
                    "waiting",
                    "--wait",
                    "1800ms",
                    "--request-id",
                    "slow-acquire",
                ])
                .env("GROOVE_LOG_ENABLED", "false");
            if json {
                command.arg("--json");
            }
            if quiet {
                command.arg("--quiet");
            }
            // A real capacity wait supplies a bounded silent interval and a
            // typed failure without changing ownership of the occupied slot.
            let out = TestChild::spawn(&mut command).wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(3), "{out:?}");
            if json {
                let response: Value = serde_json::from_slice(&out.stdout).unwrap();
                assert!(response_validator.is_valid(&response), "{response}");
                assert_eq!(response["ok"], false);
                assert_eq!(response["error"]["code"], "ACQUIRE_TIMEOUT");
                assert_eq!(out.stdout.iter().filter(|byte| **byte == b'\n').count(), 1);
            }
            let stderr = std::str::from_utf8(&out.stderr).unwrap();
            assert!(stderr.contains("ACQUIRE_TIMEOUT"), "{out:?}");
            let events: Vec<Value> = stderr
                .lines()
                .filter(|line| line.starts_with('{'))
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            if json && !quiet {
                assert!(!events.is_empty(), "missing liveness: {out:?}");
                for event in events {
                    assert!(validator.is_valid(&event), "{event}");
                    assert_eq!(event["command"], "acquire");
                    assert_eq!(event["request_id"], "slow-acquire");
                    assert!(event["elapsed_ms"].as_u64().unwrap() >= 1000);
                }
            } else {
                assert!(events.is_empty(), "{out:?}");
            }
            assert!(!out.stderr.contains(&b'\r'));
            assert!(!out.stderr.contains(&0x1b));
        }
    }
    let current = assert_success(
        &support::groove_in(&repo.path)
            .args(["show", "--task", "occupied"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        current["result"]["reservation"]["reservation_id"],
        lease["result"]["reservation_id"]
    );
    assert_success(
        &support::groove_in(&repo.path)
            .args(["release", "--task", "occupied"])
            .output()
            .unwrap(),
    );
}
