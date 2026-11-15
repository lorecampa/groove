//! Historical inputs and independent expectations protect the configuration API.
#[path = "support/json_progress.rs"]
mod json_progress;
mod support;

use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/config/v1");

fn fixture(name: &str) -> String {
    fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

fn expected(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn command(home: &Path) -> Command {
    let mut command = support::groove_in(home);
    command
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env_remove("GROOVE_LOG_DIR")
        .env_remove("GROOVE_LOG_ENABLED");
    command
}

fn response(output: &Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response
}

fn run(home: &Path, args: &[&str]) -> Value {
    let output = command(home).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let response = response(&output);
    assert_eq!(response["ok"], true);
    json_progress::assert_only(&output, "config");
    response["result"].clone()
}

fn reject(home: &Path, args: &[&str], code: &str) {
    let output = command(home).args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
    let response = response(&output);
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], code, "{response}");
}

fn install(home: &Path, text: &str) -> std::path::PathBuf {
    let path = home.join("groove/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn historical_user_files_keep_their_effective_values_without_rewriting() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/user-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for name in ["minimal", "partial", "full"] {
        let home = tempfile::tempdir().unwrap();
        let input = fixture(&format!("user-{name}.toml"));
        let raw: toml::Value = toml::from_str(&input).unwrap();
        validator
            .validate(&serde_json::to_value(raw).unwrap())
            .unwrap();
        let path = install(home.path(), &input);
        let result = run(home.path(), &["config", "show"]);
        validator.validate(&result["config"]).unwrap();
        assert_eq!(
            result["config"],
            expected(&format!("user-{name}.json")),
            "{name}"
        );
        assert_eq!(
            result["environment_overrides"],
            serde_json::json!({
                "logging.enabled": false, "logging.directory": false
            })
        );
        assert_eq!(fs::read_to_string(path).unwrap(), input);
    }
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        run(home.path(), &["config", "show"])["config"],
        expected("user-minimal.json")
    );
    assert!(!home.path().join("groove/config.toml").exists());
}

#[test]
fn preference_updates_preserve_the_reviewed_toml_document() {
    let home = tempfile::tempdir().unwrap();
    let path = install(home.path(), &fixture("edit-before.toml"));
    run(home.path(), &["config", "set", "logging.max_files=23"]);
    assert_eq!(fs::read_to_string(&path).unwrap(), fixture("edit-set.toml"));
    run(home.path(), &["config", "unset", "logging.enabled"]);
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        fixture("edit-unset.toml")
    );
    assert_eq!(
        run(home.path(), &["config", "show"])["config"]["logging"]["enabled"],
        true
    );
    let before = fs::read(&path).unwrap();
    let output = command(home.path())
        .args(["config", "set", "logging.max_files=0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let response = response(&output);
    assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn configuration_rejections_keep_their_error_categories_and_original_bytes() {
    let cases = expected("../rejections.json");
    for case in cases.as_array().unwrap() {
        let home = tempfile::tempdir().unwrap();
        let input = case["input"].as_str().unwrap();
        let path = install(home.path(), input);
        let code = case["code"].as_str().unwrap();
        reject(home.path(), &["config", "show"], code);
        reject(home.path(), &["list"], code);
        assert_eq!(fs::read_to_string(&path).unwrap(), input);
        assert!(!home.path().join("state").exists());

        if let Some(valid) = case["schema_valid"].as_bool() {
            let raw: toml::Value = toml::from_str(input).unwrap();
            let raw = serde_json::to_value(raw).unwrap();
            let schema: Value =
                serde_json::from_str(include_str!("../schemas/user-v1.schema.json")).unwrap();
            assert_eq!(
                jsonschema::validator_for(&schema).unwrap().is_valid(&raw),
                valid,
                "{case}"
            );
        }
    }
}

#[test]
fn empty_experiments_preserve_legacy_output_fingerprints_and_editability() {
    let home = tempfile::tempdir().unwrap();
    let input = fixture("../experimental-empty.toml");
    let path = install(home.path(), &input);
    assert_eq!(
        run(home.path(), &["config", "show"])["config"],
        expected("user-minimal.json")
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), input);
    run(home.path(), &["config", "set", "experimental.features=[]"]);
    assert_eq!(fs::read_to_string(&path).unwrap(), input);
    reject(
        home.path(),
        &[
            "config",
            "set",
            "experimental.features=[\"future-feature\"]",
        ],
        "UNSUPPORTED_CONFIGURATION",
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), input);
    reject(
        home.path(),
        &["config", "set", "experimental.features=true"],
        "INVALID_ARGUMENT",
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), input);
    install(
        home.path(),
        "[experimental]\nfeatures = [\"future-feature\"]\n",
    );
    run(home.path(), &["config", "path"]);
    run(home.path(), &["--help"]);
    run(home.path(), &["config", "unset", "experimental.features"]);
    assert_eq!(
        run(home.path(), &["config", "show"])["config"],
        expected("user-minimal.json")
    );
}

#[test]
fn stable_configuration_types_versions_and_constraints_remain_strict() {
    let home = tempfile::tempdir().unwrap();
    for input in [
        "[logging]\nenabled = 1",
        "[logging]\nmax_files = 0",
        "[worktrees]\nroot = 'relative'",
        "[logging]\nmax_file_mib = 101\nmax_total_mib = 100",
    ] {
        let path = install(home.path(), input);
        reject(home.path(), &["config", "show"], "INVALID_ARGUMENT");
        assert_eq!(fs::read_to_string(path).unwrap(), input);
    }
}
