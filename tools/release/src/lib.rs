//! Development-only release tooling. Public packages and application behavior are unchanged.

macro_rules! args {
    ($($value:expr),* $(,)?) => {
        vec![$(::std::ffi::OsStr::new(&$value).to_os_string()),*]
    };
}

mod archive;
mod manifest;
mod process;
mod publish;

use serde_json::{json, Value};
use std::{fs, path::Path, path::PathBuf};

use process::{require, Runner};
pub use process::{Error, Result};

pub(crate) const PACKAGES: [&str; 2] = ["groove-core", "groove-cli"];
pub(crate) const BINARIES: [&str; 1] = ["groove"];

/// A repository and isolated command environment for release operations.
pub struct Release {
    root: PathBuf,
    runner: Runner,
}

impl Release {
    /// Select the source checkout. Commands never change the current process directory.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            root: root.as_ref().canonicalize()?,
            runner: Runner::new(),
        })
    }

    /// Check shared versions and, optionally, the release tag and main ancestry.
    pub fn validate(&self, tag: Option<&str>, branch: Option<&str>) -> Result<String> {
        self.validate_manifest(tag, branch)
    }

    /// Update the version, lockfile and changelog without committing, tagging or uploading.
    pub fn prepare(&self, version: &str) -> Result<()> {
        self.prepare_manifest(version)
    }

    /// Return the current version's dated changelog section.
    pub fn notes(&self) -> Result<String> {
        manifest::notes(&self.root, &self.validate(None, None)?)
    }

    /// Compile extracted packages and exercise production binaries in a disposable repository.
    pub fn packages(&self, allow_dirty: bool, target: Option<&str>, release: bool) -> Result<()> {
        self.check_packages(allow_dirty, target, release)
    }

    /// Publish in dependency order. An explicit execute flag and a clean tagged main commit are required.
    pub fn publish(&self, tag: &str, execute: bool) -> Result<()> {
        self.publish_registry(tag, execute)
    }

    /// Archive production binaries, documentation, checksums and build metadata.
    pub fn bundle(&self, binaries: &Path, target: &str, output: &Path) -> Result<()> {
        self.bundle_archive(binaries, target, output)
    }

    pub(crate) fn smoke(&self, binaries: &Path, version: &str) -> Result<()> {
        let binaries = binaries.canonicalize()?;
        let scratch = tempfile::Builder::new()
            .prefix("groove-release-smoke-")
            .tempdir()?;
        let mut runner = self.runner.clone();
        runner.isolate(scratch.path())?;
        let groove = |arguments: Vec<std::ffi::OsString>| -> Result<Value> {
            let arguments = [vec!["--json".into()], arguments].concat();
            let bytes = runner.run(scratch.path(), binaries.join("groove"), arguments)?;
            let value: Value = serde_json::from_slice(&bytes)?;
            require(value["ok"] == true, "Release CLI smoke command failed")?;
            Ok(value["result"].clone())
        };
        let text = groove(args!["--version"])?;
        require(
            text["text"].as_str().map(str::trim) == Some(&format!("groove {version}")),
            "CLI version mismatch",
        )?;
        runner.run(scratch.path(), "git", args!["init", "-q"])?;
        runner.run(
            scratch.path(),
            "git",
            args![
                "-c",
                "user.name=Release",
                "-c",
                "user.email=release@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "initial"
            ],
        )?;
        groove(args!["init", "--capacity", "1"])?;
        let lease = groove(args!["acquire", "--base", "HEAD"])?;
        let field = |key: &str| -> Result<&str> {
            lease[key]
                .as_str()
                .ok_or_else(|| Error::Invalid(format!("Missing lease field {key}")))
        };
        require(
            Path::new(field("worktree_path")?).is_dir(),
            "Acquire did not create a worktree",
        )?;
        let execution = groove(args![
            "exec",
            "--lease-id",
            field("lease_id")?,
            "--grace-ms",
            "0",
            "--",
            "git",
            "rev-parse",
            "--is-inside-work-tree"
        ])?;
        require(
            execution["execution"]["termination"] == "exited"
                && execution["execution"]["exit_code"] == 0,
            "Packaged managed execution failed",
        )?;
        let stdout = execution["execution"]["run"]["stdout_path"]
            .as_str()
            .ok_or_else(|| Error::Invalid("Missing captured stdout path".into()))?;
        require(
            fs::read(stdout)? == b"true\n",
            "Packaged worker did not run in its worktree",
        )?;
        groove(args!["release", "--lease-id", field("lease_id")?])?;
        require(
            groove(args!["list"])?["leases"]
                .as_array()
                .is_some_and(Vec::is_empty),
            "Smoke test left a reservation",
        )
    }

    pub(crate) fn git(&self, arguments: Vec<std::ffi::OsString>) -> Result<String> {
        self.runner.text(&self.root, "git", arguments)
    }

    pub(crate) fn clean_tree(&self) -> Result<()> {
        require(
            self.git(args!["status", "--porcelain"])?.trim().is_empty(),
            "Release operations require a clean working tree, including untracked files",
        )
    }

    pub(crate) fn build_metadata(&self, target: &str, archive: &Path) -> Result<Value> {
        #[cfg(target_os = "linux")]
        let libc = self
            .runner
            .text(&self.root, "getconf", args!["GNU_LIBC_VERSION"])?;
        #[cfg(target_os = "linux")]
        let (family, version) = libc
            .trim()
            .split_once(' ')
            .unwrap_or((libc.trim(), "unknown"));
        #[cfg(target_os = "macos")]
        let os_version = self
            .runner
            .text(&self.root, "sw_vers", args!["-productVersion"])?;
        #[cfg(target_os = "macos")]
        let (family, version) = ("libSystem", os_version.trim());
        Ok(json!({
            "version": self.validate(None, None)?,
            "commit": self.git(args!["rev-parse", "HEAD"])?.trim(),
            "dirty": !self.git(args!["status", "--porcelain"])?.trim().is_empty(),
            "target": target,
            "build_libc": {"family": family, "version": version},
            "build_os": std::env::consts::OS,
            "rustc": self.runner.text(&self.root, "rustc", args!["--version"])?.trim(),
            "features": "default (no test-hooks)",
            "artifacts": {archive.file_name().unwrap_or_default().to_string_lossy(): archive::digest(archive)?}
        }))
    }
}

fn read_toml(path: &Path) -> Result<toml::Value> {
    Ok(toml::from_str(&fs::read_to_string(path)?)?)
}
