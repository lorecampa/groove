use crate::args::SnapshotAction;
use groove_core::{
    snapshot::{Retention, SaveOptions, Snapshots},
    Result,
};
use serde_json::{json, Value};
use std::{
    path::Path,
    time::{Duration, Instant},
};
pub fn execute(repo: &Path, timeout: Duration, action: &SnapshotAction) -> Result<Value> {
    let started = Instant::now();
    let service = Snapshots::open(repo, timeout)?;
    let result = match action {
        SnapshotAction::Save {
            description,
            session,
            max_file_bytes,
        } => {
            json!({"snapshot":service.save(SaveOptions {description:description.clone(),session:session.clone(),max_file_bytes:*max_file_bytes})?})
        }
        SnapshotAction::List => json!({"snapshots":service.list()?}),
        SnapshotAction::Show { id } => json!({"snapshot":service.show(id)?}),
        SnapshotAction::Diff { id, other } => json!({"patch":service.diff(id,other.as_deref())?}),
        SnapshotAction::Restore { id, to } => service.restore(id, to.as_deref())?,
        SnapshotAction::Delete { id, force } => {
            service.delete(id, *force)?;
            json!({"deleted":[id]})
        }
        SnapshotAction::Pin { id } => {
            service.pin(id, true)?;
            json!({"snapshot":service.show(id)?})
        }
        SnapshotAction::Unpin { id } => {
            service.pin(id, false)?;
            json!({"snapshot":service.show(id)?})
        }
        SnapshotAction::Prune {
            older_than,
            keep_last,
            dry_run,
        } => {
            let policy = if older_than.is_none() && keep_last.is_none() {
                service.retention()?
            } else {
                Retention {
                    older_than_seconds: *older_than,
                    keep_last: *keep_last,
                }
            };
            json!({"selected":service.prune(policy,*dry_run)?,"dry_run":dry_run})
        }
        SnapshotAction::Retention {
            older_than,
            keep_last,
            disable,
        } => {
            if *disable || older_than.is_some() || keep_last.is_some() {
                service.set_retention(Retention {
                    older_than_seconds: *older_than,
                    keep_last: *keep_last,
                })?;
            }
            json!({"retention":service.retention()?})
        }
        SnapshotAction::Compact => {
            service.compact()?;
            json!({"compacted":true})
        }
    };
    Ok(json!({"snapshot_result":result,"elapsed_ms":started.elapsed().as_millis() as u64}))
}
