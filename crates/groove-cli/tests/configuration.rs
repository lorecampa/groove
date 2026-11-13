//! Historical inputs and independent expectations protect the configuration API.
mod support;

use serde_json::Value;
use std::{fs, path::Path, process::Command};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/config/v1");

fn fixture(name: &str) -> String {
    fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap()
}

fn expected(name: &str) -> Value {
    serde_json::from_str(&fixture(name)).unwrap()
}

fn command(home: &Path) -> Command {
    let mut command = support::groove();
    command
        .current_dir(home)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env_remove("GROOVE_LOG_DIR")
        .env_remove("GROOVE_LOG_ENABLED");
    command
}

fn run(home: &Path, args: &[&str]) -> Value {
    let output = command(home).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    assert_eq!(response["ok"], true);
    assert!(output.stderr.is_empty());
    response["result"].clone()
}

fn install(home: &Path, text: &str) -> std::path::PathBuf {
    let path = home.join("groove/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

#[test]
fn historical_user_files_keep_their_effective_values_without_rewriting() {
    for name in ["minimal", "partial", "full"] {
        let home = tempfile::tempdir().unwrap();
        let input = fixture(&format!("user-{name}.toml"));
        let path = install(home.path(), &input);
        let result = run(home.path(), &["config", "global", "show"]);
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
        run(home.path(), &["config", "global", "show"])["config"],
        expected("user-minimal.json")
    );
    assert!(!home.path().join("groove/config.toml").exists());
}

#[test]
fn historical_project_files_keep_their_effective_values_and_match_the_schema() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/project-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for name in ["minimal", "partial", "full"] {
        let input = fixture(&format!("project-{name}.toml"));
        // Validate the user's sparse input too, not just the default-filled output.
        let raw: toml::Value = toml::from_str(&input).unwrap();
        validator
            .validate(&serde_json::to_value(raw).unwrap())
            .unwrap();
        let project = groove_core::Project::parse(&input).unwrap();
        let actual = serde_json::to_value(&project).unwrap();
        assert_eq!(actual, expected(&format!("project-{name}.json")), "{name}");
        validator.validate(&actual).unwrap();
        let repo = support::Repo::new();
        let path = repo.path.join(".groove.toml");
        fs::write(&path, &input).unwrap();
        assert_eq!(run(&repo.path, &["config", "project"])["project"], actual);
        assert_eq!(fs::read_to_string(path).unwrap(), input);
        assert!(!repo.path.join("MUST_NOT_RUN").exists());
        assert!(!repo.state().exists());
    }
}

#[test]
fn preference_updates_preserve_the_reviewed_toml_document() {
    let home = tempfile::tempdir().unwrap();
    let path = install(home.path(), &fixture("edit-before.toml"));
    run(
        home.path(),
        &["config", "global", "set", "logging.max_files=23"],
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), fixture("edit-set.toml"));
    run(
        home.path(),
        &["config", "global", "unset", "logging.enabled"],
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        fixture("edit-unset.toml")
    );
    assert_eq!(
        run(home.path(), &["config", "global", "show"])["config"]["logging"]["enabled"],
        true
    );
    let before = fs::read(&path).unwrap();
    let output = command(home.path())
        .args(["config", "global", "set", "logging.max_files=0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn published_project_example_is_exact_and_valid_without_execution() {
    let home = tempfile::tempdir().unwrap();
    let example = include_str!("../templates/project.toml");
    assert_eq!(
        run(home.path(), &["config", "project", "--example"])["text"],
        example
    );
    // Use a fresh process without the harness's --json for the human contract.
    let output = support::command(home.path(), env!("CARGO_BIN_EXE_groove"))
        .env("XDG_CONFIG_HOME", home.path())
        .args(["config", "project", "--example"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, example.as_bytes());
    groove_core::Project::parse(example).unwrap();
}
