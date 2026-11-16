//! Presentation gating for optional, nonblocking update discovery.
use crate::{
    args::{Args, Command},
    preferences,
    terminal::Terminal,
};
use groove_core::upgrade::NoticeCheck;
use std::io::{self, IsTerminal};

pub fn start(args: &Args) -> Option<NoticeCheck> {
    let interactive = io::stdout().is_terminal() && io::stderr().is_terminal();
    let disabled = std::env::var_os("GROOVE_NO_UPDATE_NOTIFIER").is_some()
        || [
            "CI",
            "CONTINUOUS_INTEGRATION",
            "GITHUB_ACTIONS",
            "BUILD_NUMBER",
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some());
    if !interactive
        || disabled
        || args.json
        || args.quiet
        || matches!(
            args.command,
            Command::Upgrade(_) | Command::Config(_) | Command::Completions { .. }
        )
        || !preferences::load().ok()?.updates.check
    {
        return None;
    }
    // Discovery errors are intentionally advisory. Explicit --check reports them.
    match NoticeCheck::start() {
        Ok(check) => check,
        Err(error) => {
            tracing::debug!(%error, "Automatic update discovery unavailable");
            None
        }
    }
}
pub fn finish(check: Option<NoticeCheck>, terminal: &Terminal) {
    if let Some(notice) = check.and_then(|check| check.finish().ok().flatten()) {
        let message = format!("Groove {} is available; you have {}.\nRelease notes: {}\nRun `groove upgrade` to update (add --with-plugins for installed agent plugins).\n",
            notice.latest_version, notice.current_version,
            notice.release_notes);
        tracing::info!(event = "update.available", file_only = true, version = %notice.latest_version, "Groove update available");
        terminal.write(message.as_bytes());
    }
}
