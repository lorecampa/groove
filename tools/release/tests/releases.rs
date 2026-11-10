//! Real release commands in disposable Git workspaces; these tests never upload.
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .prefix("groove release tests è-")
            .tempdir()
            .unwrap();
        let f = Self { temp };
        fs::write(
            f.path("Cargo.toml"),
            r#"[workspace]
members = ["crates/groove-core", "crates/groove-cli"]
resolver = "2"
[workspace.package]
version = "0.1.0"
edition = "2021"
[workspace.dependencies]
groove-core = { path = "crates/groove-core", version = "0.1.0" }
"#,
        )
        .unwrap();
        for name in ["groove-core", "groove-cli"] {
            let package = f.path(&format!("crates/{name}"));
            fs::create_dir_all(package.join("src")).unwrap();
            let mut text = format!(
                "[package]\nname = {name:?}\nversion.workspace = true\nedition.workspace = true\n"
            );
            if name != "groove-core" {
                text.push_str("[dependencies]\ngroove-core.workspace = true\n");
            }
            fs::write(package.join("Cargo.toml"), text).unwrap();
            fs::write(
                package.join(if name == "groove-core" {
                    "src/lib.rs"
                } else {
                    "src/main.rs"
                }),
                if name == "groove-core" {
                    ""
                } else {
                    "fn main() {}\n"
                },
            )
            .unwrap();
        }
        fs::write(
            f.path("Cargo.lock"),
            r#"version = 3
[[package]]
name = "groove-core"
version = "0.1.0"
[[package]]
name = "groove-cli"
version = "0.1.0"
dependencies = ["groove-core"]
"#,
        )
        .unwrap();
        fs::write(
            f.path("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n- First release.\n",
        )
        .unwrap();
        f.git(&["init", "-q", "-b", "main"]);
        f.git(&["config", "user.name", "Release test"]);
        f.git(&["config", "user.email", "release@example.invalid"]);
        f.git(&["config", "core.hooksPath", "/dev/null"]);
        f.commit();
        f
    }
    fn path(&self, path: &str) -> PathBuf {
        self.temp.path().join(path)
    }
    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command.current_dir(self.temp.path());
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
        {
            command.env_remove(key);
        }
        command
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1");
        command
    }
    fn git(&self, args: &[&str]) -> String {
        let out = self.command("git").args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().into()
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "fixture"]);
    }
    fn tool(&self, args: &[&str], success: bool) -> Output {
        let out = self
            .command(env!("CARGO_BIN_EXE_groove-release"))
            .arg("--repo-root")
            .arg(self.temp.path())
            .args(args)
            .output()
            .unwrap();
        assert_eq!(
            out.status.success(),
            success,
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    fn ready(&self) {
        fs::write(
            self.path("CHANGELOG.md"),
            "# Changelog\n\n## Unreleased\n\n## [0.1.0] - 2026-11-10\n\n- First release.\n",
        )
        .unwrap();
        self.commit();
        self.git(&["tag", "v0.1.0"]);
    }
    fn private_tool(&self, with_target: bool) {
        let manifest = self.path("Cargo.toml");
        fs::write(
            &manifest,
            fs::read_to_string(&manifest).unwrap().replace(
                "\"crates/groove-cli\"]",
                "\"crates/groove-cli\", \"tools/lab\"]",
            ),
        )
        .unwrap();
        fs::create_dir_all(self.path("tools/lab")).unwrap();
        fs::write(
            self.path("tools/lab/Cargo.toml"),
            "[package]\nname = \"lab\"\nversion = \"0.1.0\"\npublish = false\n",
        )
        .unwrap();
        if with_target {
            fs::create_dir(self.path("tools/lab/src")).unwrap();
            fs::write(self.path("tools/lab/src/main.rs"), "fn main() {}\n").unwrap();
        }
        self.commit();
    }
}

#[test]
fn prepare_changes_version_lock_and_notes_without_committing_or_tagging() {
    let f = Fixture::new();
    let head = f.git(&["rev-parse", "HEAD"]);
    f.tool(&["prepare", "0.2.0"], true);
    let out = f.tool(&["validate"], true);
    assert_eq!(out.stdout, b"0.2.0\n");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), head);
    assert_eq!(f.git(&["tag"]), "");
    assert_eq!(
        f.git(&["diff", "--name-only"]).lines().collect::<Vec<_>>(),
        ["CHANGELOG.md", "Cargo.lock", "Cargo.toml"]
    );
    let output = f.path("notes.md");
    f.tool(&["notes", "--output", output.to_str().unwrap()], true);
    assert_eq!(fs::read_to_string(output).unwrap(), "- First release.\n");
}

#[test]
fn first_release_can_use_the_current_version_and_preserves_manifest_comments() {
    let f = Fixture::new();
    let manifest = f.path("Cargo.toml");
    fs::write(
        &manifest,
        format!(
            "# Keep the release configuration comment\n{}",
            fs::read_to_string(&manifest).unwrap()
        ),
    )
    .unwrap();
    f.commit();
    f.tool(&["prepare", "0.1.0"], true);
    assert!(fs::read_to_string(manifest)
        .unwrap()
        .starts_with("# Keep the release configuration comment\n"));
    f.tool(&["validate"], true);
}

#[test]
fn cargo_failure_rolls_back_manifest_lockfile_and_changelog() {
    let f = Fixture::new();
    f.private_tool(false); // Valid release metadata, but Cargo rejects this tool's missing target.
    let paths = ["Cargo.toml", "Cargo.lock", "CHANGELOG.md"];
    let original: Vec<_> = paths.iter().map(|p| fs::read(f.path(p)).unwrap()).collect();
    f.tool(&["prepare", "0.2.0"], false);
    for (path, before) in paths.iter().zip(original) {
        assert_eq!(fs::read(f.path(path)).unwrap(), before);
    }
    assert_eq!(f.git(&["status", "--porcelain"]), "");
}

#[test]
fn dirty_trees_and_older_versions_are_rejected() {
    let f = Fixture::new();
    let out = f.tool(&["prepare", "0.0.9"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("older version"));
    fs::write(f.path("untracked"), "keep").unwrap();
    let out = f.tool(&["prepare", "0.2.0"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("clean working tree"));
}

#[test]
fn tag_must_match_version_head_and_main_ancestry() {
    let f = Fixture::new();
    f.ready();
    f.tool(&["validate", "--tag", "v0.1.0", "--branch", "main"], true);
    f.tool(&["validate", "--tag", "v0.2.0"], false);
    fs::write(f.path("next"), "next").unwrap();
    f.commit();
    let out = f.tool(&["validate", "--tag", "v0.1.0"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("exact tagged commit"));
}

#[test]
fn unmerged_side_branch_cannot_be_released() {
    let f = Fixture::new();
    f.git(&["checkout", "-qb", "candidate"]);
    f.ready();
    f.tool(&["validate", "--tag", "v0.1.0", "--branch", "main"], false);
}

#[test]
fn versions_notes_and_dates_are_checked() {
    let f = Fixture::new();
    let output = f.path("notes.md");
    f.tool(&["notes", "--output", output.to_str().unwrap()], false);
    f.ready();
    fs::write(
        f.path("CHANGELOG.md"),
        "## [0.1.0] - 2026-02-30\n\n- Invalid date.\n",
    )
    .unwrap();
    f.tool(&["notes", "--output", output.to_str().unwrap()], false);
    let manifest = f.path("crates/groove-cli/Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("groove-core.workspace = true", "groove-core = \"0.1.0\""),
    )
    .unwrap();
    f.tool(&["validate"], false);
}

#[test]
fn internal_workspace_tools_must_be_explicitly_private() {
    let f = Fixture::new();
    f.private_tool(true);
    f.tool(&["validate"], true);
    let manifest = f.path("tools/lab/Cargo.toml");
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)
            .unwrap()
            .replace("publish = false\n", ""),
    )
    .unwrap();
    let out = f.tool(&["validate"], false);
    assert!(String::from_utf8_lossy(&out.stderr).contains("publish = false"));
}

#[test]
fn uploads_require_an_explicit_execute_before_running_commands() {
    let f = Fixture::new();
    let out = f.tool(&["publish", "--tag", "v0.1.0"], false);
    let error = String::from_utf8_lossy(&out.stderr);
    assert!(error.contains("--execute"));
    assert!(!error.contains("+ "));
}

#[test]
fn prereleases_are_supported_and_invalid_versions_are_rejected() {
    let f = Fixture::new();
    for version in ["v0.1.0", "01.0.0", "0.1", "0.1.0-alpha.01", "0.1.0+build"] {
        f.tool(&["prepare", version], false);
    }
    f.tool(&["prepare", "0.2.0-rc.1"], true);
    assert_eq!(f.tool(&["validate"], true).stdout, b"0.2.0-rc.1\n");
}

#[test]
fn target_bundles_keep_independent_metadata_and_valid_checksums() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    f.ready();
    for name in [
        "LICENSE-MIT",
        "LICENSE-APACHE",
        "README.md",
        "CONTRIBUTING.md",
    ] {
        fs::write(f.path(name), "fixture\n").unwrap();
    }
    for name in ["assets", "docs", "skills/groove", "bin", "checkout"] {
        fs::create_dir_all(f.path(name)).unwrap();
    }
    let stdout = f.path("captured-stdout");
    fs::write(&stdout, "true\n").unwrap();
    // Exercise real archive/metadata commands without compiling a second Groove
    // inside the fixture. This peer supplies only the smoke protocol responses;
    // product execution semantics are covered by the native CLI/process suite.
    let result = |value: serde_json::Value| {
        let response = serde_json::json!({"api_version":1,"ok":true,"result":value}).to_string();
        format!("printf '%s\\n' '{}'", response.replace('\'', "'\\''"))
    };
    let script = format!(
        "#!/bin/sh\nset -eu\nshift\ncase \"$1\" in\n--version) {};;\nacquire) {};;\nexec) {};;\nlist) {};;\n*) {};;\nesac\n",
        result(serde_json::json!({"text":"groove 0.1.0"})),
        result(serde_json::json!({"worktree_path":f.path("checkout"),"lease_id":"fixture"})),
        result(serde_json::json!({"execution":{"termination":"exited","exit_code":0,"run":{"stdout_path":stdout}}})),
        result(serde_json::json!({"leases":[]})),
        result(serde_json::json!({})),
    );
    let binary = f.path("bin/groove");
    fs::write(&binary, script).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let output = f.path("dist");
    let checksum = |bytes: Vec<u8>| -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    };
    let mut previous = Vec::new();
    for target in ["fixture-a", "fixture-b"] {
        f.tool(
            &[
                "bundle",
                "--bin-dir",
                f.path("bin").to_str().unwrap(),
                "--target",
                target,
                "--output",
                output.to_str().unwrap(),
            ],
            true,
        );
        for (path, bytes) in &previous {
            assert_eq!(
                fs::read(path).unwrap(),
                *bytes,
                "a later target overwrote an asset"
            );
        }
        let prefix = format!("groove-0.1.0-{target}");
        let archive = output.join(format!("{prefix}.tar.gz"));
        let manifest = output.join(format!("{prefix}.manifest.json"));
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
        assert_eq!(value["target"], target);
        assert_eq!(value["build_os"], std::env::consts::OS);
        let sums = output.join(format!("SHA256SUMS-{target}"));
        let checksums = fs::read_to_string(&sums).unwrap();
        assert_eq!(checksums.lines().count(), 2);
        for line in checksums.lines() {
            let (expected, name) = line.split_once("  ").unwrap();
            let bytes = fs::read(output.join(name)).unwrap();
            assert_eq!(checksum(bytes), expected);
        }
        assert_eq!(
            value["artifacts"][archive.file_name().unwrap().to_str().unwrap()],
            checksum(fs::read(&archive).unwrap())
        );
        for path in [archive, manifest, sums] {
            previous.push((path.clone(), fs::read(path).unwrap()));
        }
    }
}
