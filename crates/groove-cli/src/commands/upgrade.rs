//! The core owns discovery/installers; this handler only renders the result.
use crate::{args::UpgradeArgs, output::Output};
use groove_core::{
    upgrade::{self, Options},
    Result,
};
use std::time::Duration;

pub fn run(args: &UpgradeArgs) -> Result<Output> {
    let report = upgrade::run(&Options {
        check: args.check,
        dry_run: args.dry_run,
        with_plugins: args.with_plugins,
        target_version: args.target_version.clone(),
        timeout: Duration::from_millis(args.timeout),
    })?;
    let mut text = String::new();
    for component in &report.components {
        text.push_str(&format!("{}: {:?}", component.name, component.status));
        if let Some(current) = &component.current_version {
            text.push_str(&format!(" ({current}"));
            if let Some(target) = &component.target_version {
                text.push_str(&format!(" -> {target}"));
            }
            text.push(')');
        }
        text.push('\n');
        if let Some(path) = &component.path {
            text.push_str(&format!("  Location: {}\n", path.display()));
        }
        text.push_str(&format!("  Source: {}\n", component.source));
        if !component.message.is_empty() {
            text.push_str(&format!("  {}\n", component.message));
        }
        if args.dry_run {
            for command in &component.commands {
                let words = std::iter::once(command.program.as_str())
                    .chain(command.args.iter().map(String::as_str));
                text.push_str(&format!("  {}\n", shell_words::join(words)));
            }
        }
    }
    if let Some(notes) = &report.release_notes {
        text.push_str(&format!("Release notes: {notes}\n"));
    }
    Output::text(report, text)
}
