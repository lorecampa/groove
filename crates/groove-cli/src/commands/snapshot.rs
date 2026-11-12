//! Local checkpoint commands.

use super::Context;
use crate::{
    args::SnapshotCommand,
    output::{table, Output},
};
use groove_core::{
    snapshot::{SaveOptions, Snapshot, Snapshots},
    Result,
};
use serde_json::json;

fn created(snapshot: &Snapshot) -> String {
    i64::try_from(snapshot.created_at)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map_or_else(
            || snapshot.created_at.to_string(),
            |time| time.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        )
}

fn describe(snapshot: &Snapshot) -> String {
    format!(
        "{}{}\n  created: {}\n  branch:  {}\n  {}\n",
        snapshot.id,
        if snapshot.pinned { " (pinned)" } else { "" },
        created(snapshot),
        snapshot.branch.as_deref().unwrap_or("detached"),
        if snapshot.description.is_empty() {
            "(no description)"
        } else {
            &snapshot.description
        },
    )
}

fn snapshot_output(snapshot: Snapshot) -> Result<Output> {
    let text = describe(&snapshot);
    Output::text(json!({ "snapshot": snapshot }), text)
}

pub fn run(context: &Context<'_>, command: &SnapshotCommand) -> Result<Output> {
    let service = Snapshots::open(context.repo, context.git_timeout)?;
    match command {
        SnapshotCommand::Save {
            description,
            session,
            max_file_bytes,
        } => snapshot_output(service.save(SaveOptions {
            description: description.clone(),
            session: session.clone(),
            max_file_bytes: *max_file_bytes,
        })?),
        SnapshotCommand::List { page } => {
            let snapshots = page.apply(service.list()?);
            let rows: Vec<Vec<String>> = snapshots
                .iter()
                .map(|s| {
                    vec![
                        s.id.clone(),
                        created(s),
                        if s.pinned { "yes" } else { "" }.into(),
                        s.description.clone(),
                    ]
                })
                .collect();
            let text = if rows.is_empty() {
                "No snapshots.".into()
            } else {
                table(&["ID", "CREATED", "PINNED", "DESCRIPTION"], &rows)
            };
            Output::text(json!({ "snapshots": snapshots }), text)
        }
        SnapshotCommand::Show { id } => snapshot_output(service.show(id)?),
        SnapshotCommand::Diff { id, other } => {
            let patch = service.diff(id, other.as_deref())?;
            Output::raw(json!({ "patch": patch }), patch)
        }
        SnapshotCommand::Restore { id, to } => {
            let restored = service.restore(id, to.as_deref())?;
            let text = format!(
                "Restored {} into {}.",
                restored.snapshot_id,
                restored.path.display()
            ) + &restored
                .safety_snapshot_id
                .as_ref()
                .map(|safety| format!("\n  safety snapshot: {safety}"))
                .unwrap_or_default();
            Output::text(restored, text)
        }
        SnapshotCommand::Delete { id, force } => {
            service.delete(id, *force)?;
            Output::text(json!({ "deleted": id }), format!("Deleted snapshot {id}."))
        }
    }
}
