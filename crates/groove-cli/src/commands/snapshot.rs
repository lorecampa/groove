//! Local checkpoint commands.

use super::Context;
use crate::{
    args::{SnapshotCommand, SortOrder, TimeSort},
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
        .and_then(|seconds| time::OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|time| {
            time.format(time::macros::format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second]Z"
            ))
            .ok()
        })
        .unwrap_or_else(|| snapshot.created_at.to_string())
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

fn resolve_id(service: &Snapshots, selector: &str) -> Result<String> {
    if let Ok(id) = uuid::Uuid::parse_str(selector) {
        return Ok(id.to_string());
    }
    let prefix = selector.to_ascii_lowercase();
    if prefix.len() < 8
        || prefix.len() > 36
        || !prefix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return Err(groove_core::Error::new(
            groove_core::ErrorCode::InvalidArgument,
            "SNAPSHOT must be a UUID or unique prefix of at least eight characters",
        ));
    }
    let matches: Vec<_> = service
        .list()?
        .into_iter()
        .filter(|snapshot| snapshot.id.starts_with(&prefix))
        .collect();
    match matches.as_slice() {
        [snapshot] => Ok(snapshot.id.clone()),
        [] => Err(groove_core::Error::new(
            groove_core::ErrorCode::InvalidArgument,
            format!("No snapshot matches '{selector}'"),
        )),
        _ => Err(groove_core::Error::new(
            groove_core::ErrorCode::InvalidArgument,
            format!("Snapshot prefix '{selector}' is ambiguous; use more characters"),
        )),
    }
}

pub fn run(context: &Context<'_>, options: &crate::args::SnapshotArgs) -> Result<Output> {
    let service = Snapshots::open(context.repo, context.git_timeout)?
        .with_lock_timeout(std::time::Duration::from_millis(options.lock_timeout_ms));
    let command = &options.command;
    match command {
        SnapshotCommand::Save {
            description,
            session,
            max_file_bytes,
            ..
        } => snapshot_output(service.save(SaveOptions {
            description: description.clone(),
            session: session.clone(),
            max_file_bytes: *max_file_bytes,
        })?),
        SnapshotCommand::List { page, sort, pinned } => {
            let mut snapshots = service.list()?;
            snapshots.retain(|snapshot| !pinned || snapshot.pinned);
            let default_order = match sort {
                TimeSort::Created => SortOrder::Desc,
                TimeSort::Id => {
                    snapshots.sort_by(|a, b| a.id.cmp(&b.id));
                    SortOrder::Asc
                }
            };
            let (snapshots, pagination) = page.apply_ordered(snapshots, default_order);
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
                "No snapshots on this page.".into()
            } else {
                table(&["ID", "CREATED", "PINNED", "DESCRIPTION"], &rows)
            };
            Output::text(json!({ "snapshots": snapshots }), text)?.paginated(pagination)
        }
        SnapshotCommand::Storage { largest } => {
            let report = service.storage(usize::from(*largest))?;
            let mut text = format!(
                "Snapshots: {} (pinned: {})\nOldest/newest creation (Unix seconds): {} / {}\nWhole local repository Git object store (includes ordinary history; excludes alternates):\n  loose: {} objects, {} bytes\n  packed: {} objects, {} bytes in {} packs\nStorage uses Git's rounded KiB measurements converted to bytes; not exact disk, snapshot-attributable or reclaimable bytes.",
                report.snapshots, report.pinned,
                report.oldest_created_at.map_or_else(|| "none".into(), |t| t.to_string()),
                report.newest_created_at.map_or_else(|| "none".into(), |t| t.to_string()),
                report.repository_objects.loose_objects, report.repository_objects.loose_bytes,
                report.repository_objects.packed_objects, report.repository_objects.packed_bytes,
                report.repository_objects.packs,
            );
            if *largest > 0 {
                text.push_str("\nLargest unique saved blobs (logical bytes; representative paths use lossy UTF-8):");
                for blob in &report.largest_blobs {
                    text.push_str(&format!(
                        "\n  {} {} {} {} {:?}",
                        blob.logical_bytes, blob.oid, blob.snapshot_id, blob.tree, blob.path
                    ));
                }
            }
            Output::text(json!({ "storage": report }), text)
        }
        SnapshotCommand::Prune {
            older_than_ms,
            keep_last,
            apply,
            yes,
            dry_run,
        } => {
            if !dry_run {
                super::confirmation::confirm(
                    *yes || *apply,
                    "Delete eligible unpinned snapshots?",
                )?;
            }
            super::snapshot_prune::run(&service, *older_than_ms, *keep_last, !dry_run)
        }
        SnapshotCommand::Show { id } => snapshot_output(service.show(&resolve_id(&service, id)?)?),
        SnapshotCommand::Diff {
            id,
            other,
            max_file_bytes,
        } => {
            use base64::Engine;
            let id = resolve_id(&service, id)?;
            let other = other
                .as_ref()
                .map(|id| resolve_id(&service, id))
                .transpose()?;
            let bytes = service.diff_bytes_with_limit(&id, other.as_deref(), *max_file_bytes)?;
            let result = match std::str::from_utf8(&bytes) {
                Ok(patch) => json!({ "patch": patch }),
                Err(_) => json!({
                    "patch": base64::engine::general_purpose::STANDARD.encode(&bytes),
                    "patch_encoding": "base64"
                }),
            };
            Output::raw_bytes(result, bytes)
        }
        SnapshotCommand::Restore {
            id,
            to,
            max_file_bytes,
        } => {
            let restored = service.restore_with_limit(
                &resolve_id(&service, id)?,
                to.as_deref(),
                *max_file_bytes,
            )?;
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
        SnapshotCommand::Delete {
            ids,
            force,
            dry_run,
            yes,
        } => {
            let mut snapshots = Vec::new();
            let mut seen = std::collections::BTreeSet::new();
            for selector in ids {
                let id = resolve_id(&service, selector)?;
                if seen.insert(id.clone()) {
                    let snapshot = service.show(&id)?;
                    if snapshot.pinned && !force && !dry_run {
                        return Err(groove_core::Error::new(
                            groove_core::ErrorCode::InvalidArgument,
                            format!("Snapshot {id} is pinned; use --pinned to delete it"),
                        ));
                    }
                    snapshots.push(snapshot);
                }
            }
            if *dry_run {
                let text = snapshots
                    .iter()
                    .map(describe)
                    .collect::<Vec<_>>()
                    .join("\n");
                let page = crate::args::PageArgs {
                    unlimited: true,
                    ..Default::default()
                };
                let (snapshots, pagination) = page.apply(snapshots);
                return Output::text(
                    json!({"snapshots": snapshots}),
                    format!("Would delete:\n{text}"),
                )?
                .paginated(pagination);
            }
            if snapshots.len() > 1 {
                super::confirmation::confirm(*yes, "Delete these snapshots?")?;
            }
            let mut deleted = Vec::new();
            for snapshot in snapshots {
                service.delete(&snapshot.id, *force).map_err(|mut error| {
                    if !deleted.is_empty() {
                        error.message = format!(
                            "Deleted {}; failed next deletion: {}",
                            deleted.join(", "),
                            error.message
                        );
                    }
                    error
                })?;
                deleted.push(snapshot.id);
            }
            let mut result = json!({"deleted": deleted[0]});
            if deleted.len() > 1 {
                result["deleted_snapshots"] = json!(deleted);
            }
            Output::text(
                result,
                format!("Deleted snapshots: {}.", deleted.join(", ")),
            )
        }
    }
}
