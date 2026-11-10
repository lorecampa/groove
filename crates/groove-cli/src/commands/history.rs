//! Durable history and explicit log retention handlers.
use super::Context;
use crate::{
    args::{HistoryCommand, StorageCommand},
    output::Output,
};
use groove_core::{CleanupStatus, HistoryFilter, LeaseId, Result};
use serde_json::json;

pub fn history(
    context: &Context<'_>,
    task: Option<String>,
    lease_id: Option<LeaseId>,
    command: Option<&HistoryCommand>,
) -> Result<Output> {
    let pool = context.pool()?;
    match command {
        None => {
            let runs = pool.history(&HistoryFilter { task, lease_id })?;
            if runs.is_empty() {
                Output::text(json!({"runs": runs}), "No execution history matches.\n")
            } else {
                Output::fields(json!({"runs": runs}))
            }
        }
        Some(HistoryCommand::Show { run_id }) => {
            Output::fields(json!({"run": pool.history_show(*run_id)?}))
        }
        Some(HistoryCommand::Delete { run_id }) => {
            pool.history_delete(*run_id)?;
            Output::fields(json!({"deleted_run_id": run_id}))
        }
    }
}
pub fn storage(context: &Context<'_>, command: &StorageCommand) -> Result<Output> {
    let pool = context.pool()?;
    match command {
        StorageCommand::Show => Output::fields(json!({"storage": pool.storage_usage()?})),
        StorageCommand::Clean {
            older_than,
            dry_run,
            ..
        } => {
            let report = pool.clean_logs(*older_than, *dry_run)?;
            let status = if report
                .entries
                .iter()
                .any(|entry| entry.status == CleanupStatus::Failed)
            {
                7
            } else {
                0
            };
            Ok(Output::fields(json!({"cleanup": report}))?.with_status(status))
        }
    }
}
