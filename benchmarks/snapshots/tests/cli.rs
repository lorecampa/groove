//! Public development CLI contracts and real release-CLI profile regressions.
use serde_json::Value;
use std::{fs, path::Path, process::Command};
fn bench() -> Command {
    Command::new(env!("CARGO_BIN_EXE_groove-bench"))
}
fn release_cli() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/release/groove")
        .canonicalize()
        .expect("cargo build -p groove-cli --release --locked")
}
#[test]
fn profiles_are_discoverable_and_workload_overrides_are_rejected() {
    let help = bench().arg("--help").output().unwrap();
    assert!(help.status.success());
    let help = String::from_utf8(help.stdout).unwrap();
    assert!(
        help.contains("profiles")
            && help.contains("benchmark")
            && help.contains("check")
            && help.contains("inspect")
    );
    assert!(!help.contains("__run") && !help.contains("verify-environment"));
    let listing = bench().arg("profiles").output().unwrap();
    assert!(listing.status.success());
    let listing = String::from_utf8(listing.stdout).unwrap();
    for name in [
        "monorepo-reuse",
        "workspace-conversions",
        "monorepo-snapshots",
        "snapshot-incremental",
        "snapshot-restore",
        "snapshot-scale",
        "snapshot-binary",
        "workspace-reuse",
        "workspace-concurrency",
        "workspace-multi-pool",
        "workspace-deletion",
    ] {
        assert!(listing.contains(name));
    }
    for flag in [
        "--workload",
        "--lifecycle",
        "--concurrency",
        "--runs",
        "--warmup",
        "--history-depth",
        "--fixture-bundle",
        "--profile",
    ] {
        assert!(
            !bench()
                .args(["benchmark", "snapshot-incremental", flag, "1"])
                .output()
                .unwrap()
                .status
                .success(),
            "Accepted workload-shaping override {flag}"
        );
    }
    assert!(!bench()
        .args(["benchmark", "million"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!bench()
        .args(["benchmark", "all", "snapshot-scale"])
        .output()
        .unwrap()
        .status
        .success());
}
#[test]
fn operational_failures_leave_one_artifact_and_no_running_fixture() {
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("scratch");
    let report = temp.path().join("report.json");
    let output = bench()
        .current_dir(temp.path())
        .args([
            "check",
            "workspace-reuse",
            "--host",
            "--binary",
            "/usr/bin/false",
            "--temp-root",
        ])
        .arg("scratch")
        .arg("--output")
        .arg(&report)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(report["status"], "failed");
    assert!(report["error"].as_str().unwrap().contains("exited"));
    assert_eq!(fs::read_dir(scratch).unwrap().count(), 0);
}
#[test]
#[ignore = "opt-in release CLI: checks every routine scenario once, including binary and concurrent workers"]
fn routine_profiles_verify_raw_evidence_and_preserve_host_configuration() {
    let temp = tempfile::tempdir().unwrap();
    let report = temp.path().join("report.json");
    let scratch = temp.path().join("scratch");
    let hostile = temp.path().join("hostile.gitconfig");
    fs::write(
        &hostile,
        "[core]\n autocrlf = true\n[init]\n defaultBranch = unexpected\n",
    )
    .unwrap();
    let output = bench()
        .args([
            "check",
            "snapshot-incremental",
            "snapshot-restore",
            "snapshot-binary",
            "workspace-reuse",
            "workspace-conversions",
            "workspace-concurrency",
            "workspace-multi-pool",
            "workspace-deletion",
            "--host",
            "--binary",
        ])
        .arg(release_cli())
        .arg("--temp-root")
        .arg(&scratch)
        .arg("--output")
        .arg(&report)
        .env("GIT_CONFIG_GLOBAL", &hostile)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.autocrlf")
        .env("GIT_CONFIG_VALUE_0", "true")
        .env("BASH_ENV", "/nonexistent")
        .env("RUSTFLAGS", "hostile-parent")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(value["schema_version"], 4);
    assert_eq!(value["status"], "passed");
    assert_eq!(value["environment"]["canonical"], false);
    assert_eq!(value["cases"].as_object().unwrap().len(), 21);
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    let inspected = bench().arg("inspect").arg(&report).output().unwrap();
    assert!(inspected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&inspected.stdout).unwrap(),
        value["summary"]
    );
    let mut changed = value.clone();
    changed["summary"] = serde_json::json!({"fabricated":true});
    fs::write(&report, serde_json::to_vec(&changed).unwrap()).unwrap();
    let inspected = bench().arg("inspect").arg(&report).output().unwrap();
    assert!(inspected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&inspected.stdout).unwrap(),
        value["summary"]
    );
    let mut changed = value.clone();
    changed["cases"]["snapshot-incremental/text-edits"]["blocks"][0]["candidate"]["metrics"]
        ["history_changed_seconds"] = serde_json::json!(999);
    fs::write(&report, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
    let mut changed = value.clone();
    changed["profiles"]["workspace-reuse"]["definition"]["sampling"]["blocks"] =
        serde_json::json!(99);
    fs::write(&report, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
    let mut changed = value;
    changed["cases"]
        .as_object_mut()
        .unwrap()
        .remove("workspace-reuse/pristine");
    fs::write(&report, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
    assert_eq!(
        fs::read_to_string(hostile).unwrap(),
        "[core]\n autocrlf = true\n[init]\n defaultBranch = unexpected\n"
    );
}
#[test]
#[ignore = "opt-in release CLI: complete paired profile with fixed sampling"]
fn paired_profile_uses_ab_ba_and_the_profile_sampling_policy() {
    let temp = tempfile::tempdir().unwrap();
    let report = temp.path().join("report.json");
    let cli = release_cli();
    let output = bench()
        .args(["benchmark", "workspace-reuse", "--host", "--binary"])
        .arg(&cli)
        .arg("--baseline-binary")
        .arg(&cli)
        .arg("--temp-root")
        .arg(temp.path().join("scratch"))
        .arg("--output")
        .arg(&report)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    let blocks = value["cases"]["workspace-reuse/pristine"]["blocks"]
        .as_array()
        .unwrap();
    assert_eq!(blocks.len(), 10);
    assert_eq!(
        blocks[0]["order"],
        serde_json::json!(["baseline", "candidate"])
    );
    assert_eq!(
        blocks[1]["order"],
        serde_json::json!(["candidate", "baseline"])
    );
    for block in blocks {
        for side in ["baseline", "candidate"] {
            assert_eq!(block[side]["verified"], true);
        }
    }
    let inspected = bench().arg("inspect").arg(&report).output().unwrap();
    assert!(inspected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&inspected.stdout).unwrap(),
        value["summary"]
    );
}
