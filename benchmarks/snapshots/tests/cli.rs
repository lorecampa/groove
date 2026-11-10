//! Runner failures leave readable reports and remove scratch fixtures.
use serde_json::Value;
use std::{fs, process::Command};
#[test]
fn help_and_invalid_profiles_and_failed_reports() {
    let binary = env!("CARGO_BIN_EXE_groove-bench");
    assert!(Command::new(binary)
        .arg("--help")
        .output()
        .unwrap()
        .status
        .success());
    let temp = tempfile::tempdir().unwrap();
    let scratch = temp.path().join("scratch");
    let report = temp.path().join("report.json");
    let output = Command::new(binary)
        .args([
            "run",
            "--mode",
            "benchmark",
            "--profile",
            "smoke",
            "--runs",
            "1",
            "--binary",
            "/usr/bin/false",
            "--temp-root",
        ])
        .arg(&scratch)
        .arg("--output")
        .arg(&report)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: Value = serde_json::from_slice(&fs::read(&report).unwrap()).unwrap();
    assert_eq!(value["status"], "failed");
    assert!(value["error"].as_str().unwrap().contains("exited"));
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    assert!(!Command::new(binary)
        .args(["run", "--mode", "benchmark", "--profile", "unknown"])
        .output()
        .unwrap()
        .status
        .success());
    assert!(!Command::new(binary)
        .args([
            "benchmark",
            "--profile",
            "smoke",
            "--runs",
            "0",
            "--temp-root"
        ])
        .arg(&scratch)
        .arg("--output")
        .arg(&report)
        .output()
        .unwrap()
        .status
        .success());
}

#[test]
#[ignore = "opt-in local roundtrip; build groove-cli --release first; see benchmarks/snapshots/README.md"]
fn real_cli_roundtrip_preserves_dirty_source_and_cleans_scratch() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cli = root
        .join("target/release/groove")
        .canonicalize()
        .expect("cargo build -p groove-cli --release --locked");
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| {
        let mut c = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                c.env_remove(key);
            }
        }
        let out = c
            .current_dir(&source)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    };
    git(&["init", "-q"]);
    fs::write(source.join("file"), "baseline\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "baseline"]);
    fs::write(source.join("file"), "staged\n").unwrap();
    git(&["add", "."]);
    fs::write(source.join("file"), "unstaged\n").unwrap();
    fs::write(source.join("untracked"), "keep\n").unwrap();
    let before = git(&["status", "--porcelain=v1", "-z"]);
    let index = fs::read(source.join(".git/index")).unwrap();
    let refs = git(&["show-ref"]);
    let scratch = temp.path().join("scratch");
    let report = temp.path().join("report.json");
    let out = Command::new(env!("CARGO_BIN_EXE_groove-bench"))
        .args([
            "run",
            "--mode",
            "benchmark",
            "--profile",
            "smoke",
            "--runs",
            "2",
            "--binary",
        ])
        .arg(cli)
        .arg("--temp-root")
        .arg(&scratch)
        .arg("--output")
        .arg(&report)
        .arg("--real-repo")
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let value: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(value["status"], "passed");
    assert_eq!(value["measurement_version"], 2);
    assert_eq!(value["source_unchanged"], true);
    for profile in ["smoke", "real_repo"] {
        assert_eq!(value["results"][profile]["recovery_verified"], true);
        assert_eq!(value["results"][profile]["dirty_recovery_verified"], true);
        assert_eq!(
            value["results"][profile]["warm_changed_saves"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            value["results"][profile]["first_save"]["peak_rss_kib"]
                .as_u64()
                .unwrap()
                > 0
        );
        for sample in [
            "first_save",
            "unchanged_save",
            "external_restore",
            "inplace_restore",
        ] {
            assert!(!value["results"][profile][sample]["git_steps_ms"]
                .as_array()
                .unwrap()
                .is_empty());
        }
        for sample in value["results"][profile]["warm_changed_saves"]
            .as_array()
            .unwrap()
        {
            assert!(!sample["git_steps_ms"].as_array().unwrap().is_empty());
        }
    }
    assert_eq!(value["environment"]["canonical"], false);
    assert_eq!(git(&["status", "--porcelain=v1", "-z"]), before);
    assert_eq!(fs::read(source.join(".git/index")).unwrap(), index);
    assert_eq!(git(&["show-ref"]), refs);
    assert_eq!(
        fs::read_to_string(source.join("file")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(source.join("untracked")).unwrap(),
        "keep\n"
    );
    assert_eq!(fs::read_dir(scratch).unwrap().count(), 0);
}
