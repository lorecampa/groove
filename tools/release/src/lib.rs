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
mod sbom;

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
    ///
    /// # Errors
    /// Reports filesystem failures while canonicalizing the checkout path.
    pub fn new(root: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            root: root.as_ref().canonicalize()?,
            runner: Runner::new(),
        })
    }

    /// Check shared versions and, optionally, the release tag and main ancestry.
    ///
    /// # Errors
    /// Reports unreadable or invalid manifests, inconsistent versions, invalid
    /// tag or branch ancestry, and failed Git commands.
    pub fn validate(&self, tag: Option<&str>, branch: Option<&str>) -> Result<String> {
        self.validate_manifest(tag, branch)
    }

    /// Update the version, lockfile and changelog without committing, tagging or uploading.
    ///
    /// # Errors
    /// Rejects invalid versions and checkouts that fail release preconditions.
    /// Reports manifest, lockfile, changelog and command failures. On failure,
    /// attempts to restore the original files; a restoration I/O failure can
    /// leave partial edits for inspection.
    pub fn prepare(&self, version: &str) -> Result<()> {
        self.prepare_manifest(version)
    }

    /// Return the current version's dated changelog section.
    ///
    /// # Errors
    /// Reports version validation failures, an unreadable changelog, or a missing
    /// or invalid dated section for the current version.
    pub fn notes(&self) -> Result<String> {
        manifest::notes(&self.root, &self.validate(None, None)?)
    }

    /// Compile extracted packages and exercise production binaries in a disposable repository.
    /// With `auditable`, requires cargo-auditable and embeds dependency metadata.
    ///
    /// # Errors
    /// Reports invalid release inputs, a dirty checkout unless allowed, missing
    /// build tools, and failures while packaging, building or smoke-testing the
    /// extracted packages.
    pub fn packages(
        &self,
        allow_dirty: bool,
        target: Option<&str>,
        release: bool,
        auditable: bool,
    ) -> Result<()> {
        self.check_packages(allow_dirty, target, release, auditable)
    }

    /// Publish in dependency order. An explicit execute flag and a clean tagged main commit are required.
    ///
    /// # Errors
    /// Reports failed release preconditions, registry or authentication failures,
    /// checksum mismatches, and failed publish or installation commands. A later
    /// failure does not undo packages already published to the registry.
    pub fn publish(&self, tag: &str, execute: bool) -> Result<()> {
        self.publish_registry(tag, execute)
    }

    /// Archive production binaries, documentation, checksums and build metadata.
    ///
    /// # Errors
    /// Reports invalid release inputs, missing binaries, and filesystem or command
    /// failures while assembling the bundle. Partial output may remain on failure.
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
        // The default pool root lives under the isolated HOME. Keep it outside
        // the source checkout, just as a real user's home is outside a project.
        let repository = scratch.path().join("source");
        fs::create_dir(&repository)?;
        let groove = |arguments: Vec<std::ffi::OsString>| -> Result<Value> {
            let arguments = [vec!["--json".into()], arguments].concat();
            let bytes = runner.run(&repository, binaries.join("groove"), arguments)?;
            let value: Value = serde_json::from_slice(&bytes)?;
            require(value["ok"] == true, "Release CLI smoke command failed")?;
            Ok(value["result"].clone())
        };
        let text = groove(args!["--version"])?;
        require(
            text["text"]
                .as_str()
                .is_some_and(|text| version_matches(text, version)),
            "CLI version mismatch",
        )?;
        runner.run(&repository, "git", args!["init", "-q"])?;
        runner.run(
            &repository,
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
        // No background replenishment may outlive this disposable fixture.
        groove(args!["init", "--capacity", "1", "--ready-target", "0"])?;
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
        runner.run(
            Path::new(field("worktree_path")?),
            "git",
            args!["rev-parse", "--is-inside-work-tree"],
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

fn version_matches(text: &str, version: &str) -> bool {
    let text = text.trim();
    let expected = format!("groove {version}");
    if text == expected {
        return true;
    }
    text.strip_prefix(&expected)
        .and_then(|suffix| suffix.strip_prefix(" (revision "))
        .and_then(|suffix| suffix.strip_suffix(')'))
        .is_some_and(|revision| {
            revision == "unknown"
                || (matches!(revision.len(), 40 | 64)
                    && revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
        })
}

#[cfg(test)]
mod version_tests {
    use super::version_matches;

    #[test]
    fn smoke_version_accepts_provenance_without_masking_version_mismatches() {
        for text in [
            "groove 0.1.0\n".to_owned(),
            format!("groove 0.1.0 (revision {})\n", "a".repeat(40)),
            format!("groove 0.1.0 (revision {})\n", "b".repeat(64)),
            "groove 0.1.0 (revision unknown)\n".to_owned(),
        ] {
            assert!(version_matches(&text, "0.1.0"), "{text}");
        }
        for text in [
            "groove 0.1.1",
            "groove 0.1.00 (revision unknown)",
            "groove 0.1.0-alpha.1 (revision unknown)",
            "groove 0.1.0 (revision bad)",
            "groove 0.1.0 (revision unknown) trailing",
            "another 0.1.0",
        ] {
            assert!(!version_matches(text, "0.1.0"), "{text}");
        }
    }
}
