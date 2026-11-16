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
        "workspace-build",
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
        "repository-small",
        "repository-large",
    ] {
        assert!(listing.contains(name));
    }
    // Pinned real repositories are discoverable with their exact upstream pins.
    assert!(listing.contains("https://github.com/torvalds/linux.git"));
    assert!(listing.contains("pinned cpython v3.13.0 (60403a5409ff)"));
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
#[ignore = "requires release CLI, cc, GNU make and ccache; complete offline build profile"]
fn build_profile_verifies_outputs_and_rejects_forged_raw_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let report = temp.path().join("report.json");
    let output = bench()
        .args(["check", "workspace-build", "--host", "--binary"])
        .arg(release_cli())
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
    let original: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(original["status"], "passed");
    for strategy in ["pristine", "external-cache", "warm"] {
        let observation =
            &original["cases"][format!("workspace-build/{strategy}")]["blocks"][0]["candidate"];
        assert_eq!(observation["verified"], true);
        assert!(
            observation["metrics"]["repeat_ready_seconds"]
                .as_f64()
                .unwrap()
                > 0.0
        );
        assert!(
            observation["metrics"]["cache_removed_total_seconds"]
                .as_f64()
                .unwrap()
                > 0.0
        );
    }
    assert!(bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
    let mut forged = original.clone();
    forged["cases"]["workspace-build/external-cache"]["blocks"][0]["candidate"]["detail"]
        ["build"]["phases"][4]["output"] = "stale output\n".into();
    fs::write(&report, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
    let mut forged = original.clone();
    forged["cases"]["workspace-build/external-cache"]["blocks"][0]["candidate"]["detail"]
        ["build"]["phases"][1]["hits"] = 0.into();
    fs::write(&report, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
    let mut forged = original;
    forged["case_schedule"]["workspace-build"][0] = serde_json::json!(["warm", "warm", "warm"]);
    fs::write(&report, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
fn pinned_repository_runs_never_download_and_name_the_fetch_step() {
    let temp = tempfile::tempdir().unwrap();
    let report = temp.path().join("report.json");
    let empty = temp.path().join("empty-cache");
    std::fs::create_dir(&empty).unwrap();
    // A missing cache fails before any fixture work; runs never fall back to the network.
    let output = bench()
        .args([
            "check",
            "repository-small",
            "--host",
            "--binary",
            "/usr/bin/false",
        ])
        .arg("--fixture-cache")
        .arg(&empty)
        .arg("--temp-root")
        .arg(temp.path().join("scratch"))
        .arg("--output")
        .arg(&report)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(value["status"], "failed");
    let error = value["error"].as_str().unwrap();
    assert!(
        error.contains("missing or stale") && error.contains("fetch"),
        "{error}"
    );
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
    // Generated profiles need no download, so fetch succeeds without network access.
    let output = bench()
        .args(["fetch", "workspace-reuse", "--fixture-cache"])
        .arg(&empty)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("nothing to fetch"));
    assert_eq!(fs::read_dir(&empty).unwrap().count(), 0);
    let help = bench().args(["fetch", "--help"]).output().unwrap();
    assert!(String::from_utf8_lossy(&help.stdout).contains("only networked command"));
}

#[test]
#[ignore = "opt-in release CLI and pinned cache: run `fetch repository-small` first; checks real repositories once"]
fn pinned_small_repositories_verify_real_trees_and_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let report = temp.path().join("report.json");
    let scratch = temp.path().join("scratch");
    let output = bench()
        .args(["check", "repository-small", "--host", "--binary"])
        .arg(release_cli())
        .arg("--temp-root")
        .arg(&scratch)
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
    assert_eq!(value["status"], "passed");
    assert_eq!(value["cases"].as_object().unwrap().len(), 6);
    assert_eq!(
        value["cases"]["repository-small/cpython-restore"]["identity"],
        "60403a5409ff2c3f3b07dd2ca91a7a3e096839c7"
    );
    for id in ["ripgrep", "cpython"] {
        let packs = value["repositories"][id]["manifest"]["packs"]
            .as_array()
            .unwrap();
        assert!(!packs.is_empty() && packs.iter().all(|p| p["sha256"].is_string()));
    }
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    let inspected = bench().arg("inspect").arg(&report).output().unwrap();
    assert!(inspected.status.success());
    // A report claiming another measured commit is rejected against raw evidence.
    let mut forged = value.clone();
    forged["cases"]["repository-small/ripgrep-warm"]["identity"] =
        "e50df40a1967708b9781486b1c017e48040bceb0".into();
    let path = temp.path().join("forged.json");
    fs::write(&path, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&path)
        .output()
        .unwrap()
        .status
        .success());
    let mut forged = value;
    forged["repositories"]["cpython"]["definition"]["reachable_commits"] = 1.into();
    fs::write(&path, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(!bench()
        .arg("inspect")
        .arg(&path)
        .output()
        .unwrap()
        .status
        .success());
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
    assert_eq!(value["schema_version"], 5);
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
