mod support;

use serde_json::Value;
use support::{assert_success, Repo, TestChild};

#[test]
fn long_waits_report_json_liveness_preserve_errors_and_respect_quiet() {
    let repo = Repo::new();
    assert_success(
        &support::groove_in(&repo.path)
            .args(["init", "--capacity", "1"])
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
        serde_json::from_str(include_str!("../schemas/progress-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let response_schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let response_validator = jsonschema::validator_for(&response_schema).unwrap();
    for json in [false, true] {
        for quiet in [false, true] {
            let mut command = support::groove_human_in(&repo.path);
            command
                .args([
                    "acquire",
                    "--task",
                    "waiting",
                    "--wait-ms",
                    "1800",
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
        current["result"]["lease"]["lease_id"],
        lease["result"]["lease_id"]
    );
    assert_success(
        &support::groove_in(&repo.path)
            .args(["release", "--task", "occupied"])
            .output()
            .unwrap(),
    );
}
