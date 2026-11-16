use super::{process, Component, Options, Status, UpgradeCommand};
use crate::{Error, ErrorCode, Result};
use semver::Version;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::{Path, PathBuf},
    time::Duration,
};

const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
const MANUAL: &str = "Automatic upgrades require a matching Cargo crates.io receipt for this executable. For Git/path builds, update and rebuild from the original source. For manual archives or other package managers, use the original installer. To deliberately switch to crates.io: cargo install --locked groove-cli (then check PATH).";
#[derive(Debug, Deserialize)]
struct Receipts {
    installs: BTreeMap<String, Receipt>,
}
#[derive(Debug, Deserialize)]
struct Receipt {
    bins: Vec<String>,
    features: Vec<String>,
    all_features: bool,
    no_default_features: bool,
    profile: String,
    target: String,
}
#[derive(Debug)]
pub(super) struct Installation {
    root: PathBuf,
    executable: PathBuf,
    receipt: Receipt,
}
impl Installation {
    pub fn detect() -> Result<Option<Self>> {
        let executable = fs::canonicalize(std::env::current_exe()?)?;
        let Some(bin) = executable
            .parent()
            .filter(|bin| bin.file_name().is_some_and(|name| name == "bin"))
        else {
            return Ok(None);
        };
        let Some(root) = bin.parent() else {
            return Ok(None);
        };
        let receipt_path = root.join(".crates2.json");
        match fs::symlink_metadata(&receipt_path) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let receipts: Receipts = crate::state::read_bounded(&receipt_path, process::LIMIT as u64)?;
        let key = format!("groove-cli {} ({REGISTRY})", env!("CARGO_PKG_VERSION"));
        let Some(receipt) = receipts
            .installs
            .into_iter()
            .find_map(|(name, receipt)| (name == key).then_some(receipt))
        else {
            return Ok(None);
        };
        if receipt.bins != ["groove"] || executable != root.join("bin/groove") {
            return Ok(None);
        }
        if root.to_str().is_none() {
            return Err(Error::new(ErrorCode::UnsupportedConfiguration,
                "Cargo installation paths must be UTF-8 for upgrade; use the original installer for this path"));
        }
        Ok(Some(Self {
            root: root.to_path_buf(),
            executable,
            receipt,
        }))
    }
}

#[derive(Deserialize)]
struct Release {
    vers: String,
    #[serde(default)]
    yanked: bool,
}
pub(super) fn select(bytes: &[u8], target: Option<&Version>) -> Result<Version> {
    let text = std::str::from_utf8(bytes).map_err(|error| {
        Error::new(ErrorCode::ProtocolError, "Release index is not UTF-8").with_source(error)
    })?;
    let mut selected: Option<Version> = None;
    for line in text.lines().filter(|line| !line.is_empty()) {
        let release: Release = serde_json::from_str(line)?;
        let version = Version::parse(&release.vers).map_err(|error| {
            Error::new(
                ErrorCode::ProtocolError,
                "Release index contains an invalid version",
            )
            .with_source(error)
        })?;
        if release.yanked
            || !version.pre.is_empty()
            || !version.build.is_empty()
            || target.is_some_and(|target| *target != version)
        {
            continue;
        }
        if selected.as_ref().is_none_or(|old| &version > old) {
            selected = Some(version);
        }
    }
    selected.ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidArgument,
            "No matching non-yanked stable groove-cli release exists on crates.io",
        )
    })
}
fn command(installation: &Installation, target: &Version) -> UpgradeCommand {
    let mut args = process::strings(&[
        "install",
        "--locked",
        "--registry",
        "crates-io",
        "--version",
        &format!("={target}"),
        "--root",
        &installation.root.to_string_lossy(),
        "--profile",
        &installation.receipt.profile,
        "--target",
        &installation.receipt.target,
        "--bin",
        "groove",
    ]);
    if installation.receipt.all_features {
        args.push("--all-features".into());
    }
    if installation.receipt.no_default_features {
        args.push("--no-default-features".into());
    }
    if !installation.receipt.features.is_empty() {
        args.push("--features".into());
        args.push(installation.receipt.features.join(","));
    }
    args.push("groove-cli".into());
    UpgradeCommand {
        program: "cargo".into(),
        args,
    }
}
pub(super) fn plan(
    installation: Option<&Installation>,
    current: &Version,
    explicit: Option<&Version>,
    options: &Options,
) -> Result<Component> {
    if installation.is_none() && !options.check && !options.dry_run {
        return Ok(Component {
            name: "cli".into(),
            source: "unmanaged".into(),
            path: Some(std::env::current_exe()?),
            current_version: Some(current.to_string()),
            target_version: None,
            update_available: None,
            status: Status::Skipped,
            message: MANUAL.into(),
            commands: Vec::new(),
        });
    }
    let target = select(
        &process::index_request(Duration::from_secs(10))?.wait()?,
        explicit,
    )?;
    let available = target > *current;
    let supported = installation.is_some();
    let mut component = Component {
        name: "cli".into(),
        source: if supported {
            "cargo:crates-io"
        } else {
            "unmanaged"
        }
        .into(),
        path: Some(std::env::current_exe()?),
        current_version: Some(current.to_string()),
        target_version: Some(std::cmp::max(&target, current).to_string()),
        update_available: Some(available),
        status: Status::UpToDate,
        message: String::new(),
        commands: Vec::new(),
    };
    let Some(installation) = installation else {
        component.status = Status::Skipped;
        component.message = MANUAL.into();
        return Ok(component);
    };
    component.path = Some(installation.executable.clone());
    if !available {
        component.message =
            "Already at the selected version or newer than latest stable; no downgrade performed."
                .into();
        return Ok(component);
    }
    component.commands.push(command(installation, &target));
    if options.check {
        component.status = Status::Available;
        return Ok(component);
    }
    component.status = Status::Planned;
    Ok(component)
}
pub(super) fn apply(
    installation: &Installation,
    component: &mut Component,
    options: &Options,
    lock: Option<&File>,
) -> Result<()> {
    let target = super::stable_version(
        component
            .target_version
            .as_deref()
            .expect("planned CLI has a target"),
    )?;
    let command = &component.commands[0];
    process::run(&command.program, &command.args, options.timeout, lock)?;
    verify(&installation.executable, &target)?;
    component.status = Status::Upgraded;
    component.message = "Subsequent invocations use the new CLI. Existing processes continue on their original executable. Pools and reservations were not changed; regenerate shell completions if installed.".into();
    Ok(())
}
fn verify(executable: &Path, target: &Version) -> Result<()> {
    let output = process::run(
        &executable.to_string_lossy(),
        &process::strings(&["--version"]),
        Duration::from_secs(10),
        None,
    )?;
    let text = String::from_utf8_lossy(&output);
    if text.split_whitespace().take(2).collect::<Vec<_>>() != ["groove", &target.to_string()] {
        return Err(Error::new(ErrorCode::ProtocolError, "Installer completed but the destination does not report the selected Groove version; inspect PATH and Cargo installation records"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_selection_uses_semantic_order_and_never_selects_yanked_or_prereleases() {
        let index = b"{\"vers\":\"0.9.0\"}\n{\"vers\":\"0.10.0\"}\n{\"vers\":\"0.11.0\",\"yanked\":true}\n{\"vers\":\"1.0.0-rc.1\"}\n";
        assert_eq!(select(index, None).unwrap().to_string(), "0.10.0");
        assert_eq!(
            select(index, Some(&Version::parse("0.9.0").unwrap()))
                .unwrap()
                .to_string(),
            "0.9.0"
        );
        for unavailable in ["0.8.0", "0.11.0", "1.0.0-rc.1"] {
            assert_eq!(
                select(index, Some(&Version::parse(unavailable).unwrap()))
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
        }
        assert_eq!(
            select(b"not a release index", None).unwrap_err().code,
            ErrorCode::ProtocolError
        );
    }
}
