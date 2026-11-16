mod support;
use serde_json::Value;
use support::{assert_success, Repo};

fn validate(response: &Value) {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(response)
        .unwrap();
}

fn storage(repo: &Repo, args: &[&str]) -> Value {
    let output = support::groove_in(&repo.path)
        .args(["snapshot", "storage"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.stderr.is_empty(), "{:?}", output);
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let response = assert_success(&output);
    validate(&response);
    response["result"]["storage"].clone()
}

#[test]
fn storage_json_contract_reports_pins_and_opt_in_logical_sizes() {
    let repo = Repo::new();
    let empty = storage(&repo, &[]);
    assert_eq!(empty["snapshots"], 0);
    assert_eq!(empty["oldest_created_at"], Value::Null);
    assert_eq!(empty["newest_created_at"], Value::Null);
    assert_eq!(empty["largest_limit"], 0);
    assert_eq!(empty["largest_blobs"], serde_json::json!([]));
    assert_success(
        &support::groove_in(&repo.path)
            .args(["pool", "init", "--capacity", "1", "--ready-target", "0"])
            .output()
            .unwrap(),
    );
    let acquired = assert_success(
        &support::groove_in(&repo.path)
            .args(["acquire", "--task", "storage-test"])
            .output()
            .unwrap(),
    );
    let path = acquired["result"]["workspace_path"].as_str().unwrap();
    std::fs::write(std::path::Path::new(path).join("large"), vec![0; 1025]).unwrap();
    assert_success(
        &support::groove_in(&repo.path)
            .args(["release", "--task", "storage-test", "--save"])
            .output()
            .unwrap(),
    );
    let cheap = storage(&repo, &[]);
    assert_eq!(cheap["snapshots"], 1);
    assert_eq!(cheap["pinned"], 1);
    assert!(cheap["oldest_created_at"].is_u64());
    assert_eq!(cheap["oldest_created_at"], cheap["newest_created_at"]);
    assert_eq!(cheap["largest_blobs"], serde_json::json!([]));
    let detailed = storage(&repo, &["--largest", "1"]);
    assert_eq!(detailed["largest_limit"], 1);
    assert_eq!(detailed["largest_blobs"].as_array().unwrap().len(), 1);
    assert_eq!(detailed["largest_blobs"][0]["logical_bytes"], 1025);
    assert_eq!(detailed["largest_blobs"][0]["path"], "large");
    for argument in ["1001", "18446744073709551615", "-1"] {
        let output = support::groove_in(&repo.path)
            .args(["snapshot", "storage", "--largest", argument])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        validate(&response);
        assert_eq!(response["ok"], false);
    }
    let help = assert_success(
        &support::groove_in(&repo.path)
            .args(["snapshot", "storage", "--help"])
            .output()
            .unwrap(),
    );
    assert!(help["result"]["text"]
        .as_str()
        .unwrap()
        .contains("snapshot_storage_result"));
}
