//! Successful JSON commands can emit liveness on stderr, but no diagnostics.
use std::process::Output;

pub fn assert_only(output: &Output, command: &str) {
    if output.stderr.is_empty() {
        return;
    }
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../../schemas/progress-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for line in std::str::from_utf8(&output.stderr).unwrap().lines() {
        let event: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("unexpected stderr diagnostic {line:?}: {error}"));
        validator.validate(&event).unwrap();
        assert_eq!(event["command"], command);
        assert_eq!(event["request_id"], response["request_id"]);
        assert!(event["elapsed_ms"].as_u64().unwrap() >= 1000);
    }
}
