//! Explicit CLI upgrades and best-effort update discovery, independent of pools.
//!
//! Only Cargo's crates.io installations are updated automatically. Other sources
//! are reported with manual guidance rather than silently changing provenance.
//! Native skill plugins are updated only when explicitly requested.
//!
//! ```no_run
//! use groove_core::upgrade::{self, Options};
//! # fn main() -> groove_core::Result<()> {
//! let report = upgrade::run(&Options {
//!     check: true,
//!     ..Options::default()
//! })?;
//! assert!(report.components.iter().all(|c| c.status != upgrade::Status::Upgraded));
//! # Ok(()) }
//! ```
mod cargo;
mod notice;
mod plugins;
mod process;

use crate::{Error, ErrorCode, Result};
pub use notice::{NoticeCheck, UpdateNotice};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

/// Inputs to an explicit upgrade; no pool or repository is opened.
#[derive(Debug, Clone)]
pub struct Options {
    /// Discover updates without installing or refreshing plugin catalogs.
    pub check: bool,
    /// Return the commands that would run without executing installers.
    pub dry_run: bool,
    /// Also update existing supported native skill plugins.
    pub with_plugins: bool,
    /// Exact stable version; omitted means latest non-yanked stable release.
    pub target_version: Option<String>,
    /// Deadline for each installer process, including its descendants.
    pub timeout: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            check: false,
            dry_run: false,
            with_plugins: false,
            target_version: None,
            timeout: Duration::from_secs(1800),
        }
    }
}

/// Observable state of an upgrade component.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Already running the selected version, or newer than latest stable.
    UpToDate,
    /// A newer CLI version exists; check mode made no changes.
    Available,
    /// Installer commands were prepared but not executed.
    Planned,
    /// Installer completed and the resulting version was verified.
    Upgraded,
    /// Installation intentionally left alone, with manual guidance.
    Skipped,
    /// Discovery or installation failed; other completed components remain updated.
    Failed,
    /// Plugin discovery succeeded, but the native manager has no remote read-only check.
    Unchecked,
}

/// An installer invocation, represented as arguments rather than shell text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeCommand {
    /// Executable resolved from PATH.
    pub program: String,
    /// Literal arguments; never interpreted by a shell.
    pub args: Vec<String>,
}

/// Result for the CLI or one existing harness plugin installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Component {
    /// `cli`, `codex`, or `claude`.
    pub name: String,
    /// Original installer/source description.
    pub source: String,
    /// Executable or plugin path, when supplied by its installer.
    pub path: Option<PathBuf>,
    /// Installed version before the operation.
    pub current_version: Option<String>,
    /// Selected CLI version or verified plugin version after updating.
    pub target_version: Option<String>,
    /// Null when the native manager cannot check without refreshing its catalog.
    pub update_available: Option<bool>,
    /// Whether this operation installed, planned, skipped or failed.
    pub status: Status,
    /// Recovery, restart or source-specific guidance.
    pub message: String,
    /// Installer commands; empty when none are appropriate.
    pub commands: Vec<UpgradeCommand>,
}

/// Complete or partial upgrade outcome, also retained in failure details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    /// True for a check-only invocation.
    pub check: bool,
    /// True for a preview invocation.
    pub dry_run: bool,
    /// True when the CLI target crosses a SemVer compatibility boundary.
    pub breaking_change: bool,
    /// Release notes for the selected stable CLI version.
    pub release_notes: Option<String>,
    /// Independent per-component results, including partial successes on failure.
    pub components: Vec<Component>,
}

/// Whether two versions cross a compatibility boundary (including pre-1.0).
///
/// A boundary signals possible breaking changes, not proof of a storage migration.
#[must_use]
pub fn compatibility_boundary(current: &Version, target: &Version) -> bool {
    current.major != target.major
        || (current.major == 0
            && (current.minor != target.minor
                || (current.minor == 0 && current.patch != target.patch)))
}

/// Upgrade the current executable and, optionally, existing native skill plugins.
///
/// Returns typed discovery/installer errors with `details.upgrade` retaining any
/// completed components. Checks and previews never run installers. Installs are
/// serialized by a permanent user-state descriptor lock inherited by installers;
/// failure never releases a repository reservation or migrates its metadata.
/// Cargo owns its native binary replacement and installation records. A killed
/// installer may require rerunning this command or the printed Cargo command.
pub fn run(options: &Options) -> Result<Report> {
    if options.timeout.is_zero() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Installer timeout must be positive",
        ));
    }
    if options.check && options.dry_run {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "--check and --dry-run are mutually exclusive",
        ));
    }
    let explicit = options
        .target_version
        .as_deref()
        .map(stable_version)
        .transpose()?;
    let current = stable_version(env!("CARGO_PKG_VERSION"))?;
    if explicit.as_ref().is_some_and(|target| target < &current) {
        return Err(Error::new(ErrorCode::InvalidArgument, "Downgrades are not supported; install the requested version manually after reviewing compatibility"));
    }
    let mut report = Report {
        check: options.check,
        dry_run: options.dry_run,
        breaking_change: false,
        release_notes: None,
        components: Vec::new(),
    };
    let installation = cargo::Installation::detect()?;
    // Lock before discovery and retain it through every installer. Check/preview
    // never create locks or change installer records.
    let guard = if !options.check && !options.dry_run {
        Some(notice::upgrade_lock()?)
    } else {
        None
    };
    let cli = cargo::plan(installation.as_ref(), &current, explicit.as_ref(), options);
    let mut first_error = None;
    match cli {
        Ok(mut component) => {
            if let Some(target) = component.target_version.as_deref() {
                let target = stable_version(target)?;
                report.breaking_change =
                    target > current && compatibility_boundary(&current, &target);
                report.release_notes = Some(release_notes(&target));
            }
            if !options.check && !options.dry_run {
                let applied = if let Some(installation) = installation.as_ref() {
                    if component.status == Status::Planned {
                        cargo::apply(
                            installation,
                            &mut component,
                            options,
                            guard.as_ref().map(|g| &g.file),
                        )
                    } else {
                        Ok(())
                    }
                } else {
                    Err(Error::new(
                        ErrorCode::UnsupportedConfiguration,
                        &component.message,
                    ))
                };
                if let Err(error) = applied {
                    component.status = Status::Failed;
                    component.message = error.to_string();
                    first_error = Some(error);
                }
            }
            report.components.push(component);
        }
        Err(error) => {
            report.components.push(Component {
                name: "cli".into(),
                source: "cargo".into(),
                path: std::env::current_exe().ok(),
                current_version: Some(current.to_string()),
                target_version: explicit.as_ref().map(ToString::to_string),
                update_available: None,
                status: Status::Failed,
                message: error.to_string(),
                commands: Vec::new(),
            });
            first_error = Some(error);
        }
    }
    if options.with_plugins {
        plugins::upgrade(
            options,
            &mut report,
            &mut first_error,
            guard.as_ref().map(|g| &g.file),
        );
    }
    if let Some(error) = first_error {
        return Err(error.with_details(|details| details.upgrade = Some(report)));
    }
    Ok(report)
}

fn stable_version(text: &str) -> Result<Version> {
    let version = Version::parse(text).map_err(|error| {
        Error::new(
            ErrorCode::InvalidArgument,
            "Expected an exact stable semantic version, for example 0.4.0",
        )
        .with_source(error)
    })?;
    if !version.pre.is_empty() || !version.build.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Prerelease and build-metadata versions are not supported by upgrade",
        ));
    }
    Ok(version)
}
fn release_notes(version: &Version) -> String {
    format!("https://github.com/lorecampa/groove/releases/tag/v{version}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_include_pre_one_minor_changes_but_not_compatible_patches() {
        for (current, target, expected) in [
            ("0.3.0", "0.4.0", true),
            ("0.3.0", "0.3.9", false),
            ("0.0.1", "0.0.2", true),
            ("0.9.0", "1.0.0", true),
            ("1.2.0", "1.3.0", false),
            ("1.9.0", "2.0.0", true),
        ] {
            assert_eq!(
                compatibility_boundary(
                    &Version::parse(current).unwrap(),
                    &Version::parse(target).unwrap()
                ),
                expected,
                "{current} -> {target}"
            );
        }
    }
}
