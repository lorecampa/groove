//! Native managers retain ownership of plugin configuration and caches.
use super::{process, Component, Options, Report, Status, UpgradeCommand};
use crate::{Error, ErrorCode, Result};
use serde_json::Value;
use std::{fs::File, path::PathBuf, time::Duration};

fn json(program: &str, args: &[&str]) -> Result<Value> {
    let bytes = process::run(
        program,
        &process::strings(args),
        Duration::from_secs(20),
        None,
    )?;
    serde_json::from_slice(&bytes).map_err(Into::into)
}
fn present(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(dir.join(program))
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
    })
}
fn official(source: &Value, codex: bool) -> bool {
    let (kind, remote) = if codex {
        (source["sourceType"].as_str(), source["source"].as_str())
    } else {
        (
            source["source"].as_str(),
            source["repo"].as_str().or(source["url"].as_str()),
        )
    };
    matches!(kind, Some("git" | "github"))
        && remote.is_some_and(|remote| {
            [
                "lorecampa/groove",
                "https://github.com/lorecampa/groove",
                "https://github.com/lorecampa/groove.git",
                "git@github.com:lorecampa/groove.git",
            ]
            .contains(&remote)
        })
}
fn discover(program: &str) -> Result<Vec<Component>> {
    if !present(program) {
        return Ok(Vec::new());
    }
    let codex = program == "codex";
    let installed = json(program, &["plugin", "list", "--json"])?;
    let entries = if codex {
        installed["installed"].as_array()
    } else {
        installed.as_array()
    }
    .ok_or_else(|| {
        Error::new(
            ErrorCode::ProtocolError,
            format!("Unrecognized {program} plugin list response"),
        )
    })?;
    let matching: Vec<_> = entries
        .iter()
        .filter(|entry| entry[if codex { "pluginId" } else { "id" }] == "groove@groove")
        .collect();
    if matching.is_empty() {
        return Ok(Vec::new());
    }
    let catalogs = json(program, &["plugin", "marketplace", "list", "--json"])?;
    let catalogs = if codex {
        catalogs["marketplaces"].as_array()
    } else {
        catalogs.as_array()
    }
    .ok_or_else(|| {
        Error::new(
            ErrorCode::ProtocolError,
            format!("Unrecognized {program} marketplace response"),
        )
    })?;
    let catalog = catalogs.iter().find(|catalog| catalog["name"] == "groove");
    let source = catalog.map(|catalog| {
        if codex {
            &catalog["marketplaceSource"]
        } else {
            catalog
        }
    });
    Ok(matching.into_iter().map(|entry| {
        let mutable = source.is_some_and(|source| official(source, codex))
            && entry["enabled"] == true
            && (codex || entry["scope"] == "user");
        let mut component = Component { name: program.into(), source: "groove@groove".into(),
            path: entry["installPath"].as_str().map(PathBuf::from),
            current_version: entry["version"].as_str().map(str::to_owned), target_version: None,
            update_available: None, status: if mutable { Status::Unchecked } else { Status::Skipped },
            message: if mutable { "Native managers select the plugin version when refreshing their catalog; check/preview does not refresh it." }
                else { "Left unchanged: only enabled, user-scope groove@groove plugins from the official Git marketplace are updated. Local/development copies, other scopes and sources require their original installer." }.into(), commands: Vec::new() };
        if mutable {
            component.commands = vec![
                UpgradeCommand { program: program.into(), args: process::strings(&["plugin", "marketplace", if codex { "upgrade" } else { "update" }, "groove"]) },
                UpgradeCommand { program: program.into(), args: if codex { process::strings(&["plugin", "add", "groove@groove", "--json"]) }
                    else { process::strings(&["plugin", "update", "groove@groove", "--scope", "user", "--json"]) } },
            ];
        }
        component
    }).collect())
}
fn verify_catalog(program: &str, target: &str) -> Result<()> {
    let codex = program == "codex";
    let catalogs = json(program, &["plugin", "marketplace", "list", "--json"])?;
    let catalogs = if codex {
        catalogs["marketplaces"].as_array()
    } else {
        catalogs.as_array()
    };
    let root = catalogs
        .into_iter()
        .flatten()
        .find(|catalog| catalog["name"] == "groove")
        .and_then(|catalog| catalog[if codex { "root" } else { "installLocation" }].as_str())
        .ok_or_else(|| {
            Error::new(
                ErrorCode::ProtocolError,
                "Native manager did not report its refreshed Groove marketplace path",
            )
        })?;
    let path = PathBuf::from(root).join(format!(
        "plugins/groove/.{}-plugin/plugin.json",
        if codex { "codex" } else { "claude" }
    ));
    let metadata: Value = crate::state::read(&path)?;
    if metadata["version"] != target {
        return Err(Error::new(ErrorCode::UnsupportedConfiguration,
            format!("Refreshed {program} plugin catalog does not match CLI {target}; plugin installation was left unchanged. Use a compatible catalog/release through the native manager.")));
    }
    Ok(())
}
pub(super) fn upgrade(
    options: &Options,
    report: &mut Report,
    first_error: &mut Option<Error>,
    lock: Option<&File>,
) {
    let cli_failed = first_error.is_some();
    let target = report
        .components
        .first()
        .and_then(|c| c.target_version.clone());
    for program in ["codex", "claude"] {
        let components = match discover(program) {
            Ok(components) => components,
            Err(error) => {
                report.components.push(Component {
                    name: program.into(),
                    source: "native-manager".into(),
                    path: None,
                    current_version: None,
                    target_version: None,
                    update_available: None,
                    status: Status::Failed,
                    message: error.to_string(),
                    commands: Vec::new(),
                });
                if first_error.is_none() {
                    *first_error = Some(error);
                }
                continue;
            }
        };
        for mut component in components {
            if component.status == Status::Unchecked {
                if options.check { /* discovery only: no catalog writes */
                } else if options.dry_run {
                    component.status = Status::Planned;
                } else if cli_failed {
                    component.status = Status::Skipped;
                    component.message = "CLI upgrade failed; plugin installation left unchanged to avoid a version mismatch.".into();
                } else {
                    let apply = || -> Result<Component> {
                        let target = target.as_deref().ok_or_else(|| {
                            Error::new(ErrorCode::ProtocolError, "CLI target version is missing")
                        })?;
                        let selected = super::stable_version(target)?;
                        if component
                            .current_version
                            .as_deref()
                            .and_then(|v| semver::Version::parse(v).ok())
                            .is_some_and(|installed| installed > selected)
                        {
                            return Err(Error::new(ErrorCode::UnsupportedConfiguration,
                                "Installed plugin is newer than the selected CLI; use a compatible release through the native manager. Plugin downgrades are not automatic."));
                        }
                        let refresh = &component.commands[0];
                        process::run(&refresh.program, &refresh.args, options.timeout, lock)?;
                        verify_catalog(program, target)?;
                        // Recheck existence/enabled/source after refresh; never add an
                        // installation that disappeared while the CLI was building.
                        let existing = discover(program)?;
                        if !existing.iter().any(|c| c.status == Status::Unchecked) {
                            return Err(Error::new(
                                ErrorCode::ConfigConflict,
                                "Plugin installation changed during upgrade; rerun discovery",
                            ));
                        }
                        let install = &component.commands[1];
                        process::run(&install.program, &install.args, options.timeout, lock)?;
                        let updated = discover(program)?;
                        let version = updated
                            .iter()
                            .filter(|c| c.status == Status::Unchecked)
                            .find_map(|c| c.current_version.as_deref());
                        if version != Some(target) {
                            return Err(Error::new(ErrorCode::ProtocolError, "Native plugin manager completed but did not report the compatible target version; inspect its plugin list"));
                        }
                        let mut result = component.clone();
                        result.status = Status::Upgraded;
                        result.target_version = version.map(str::to_owned);
                        result.update_available =
                            Some(result.current_version != result.target_version);
                        result.message =
                            "Plugin updated. Start a fresh agent session to load the new skill."
                                .into();
                        Ok(result)
                    };
                    match apply() {
                        Ok(result) => component = result,
                        Err(error) => {
                            component.status = Status::Failed;
                            component.message = error.to_string();
                            if first_error.is_none() {
                                *first_error = Some(error);
                            }
                        }
                    }
                }
            }
            report.components.push(component);
        }
    }
}
