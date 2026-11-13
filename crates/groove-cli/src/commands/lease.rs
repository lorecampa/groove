//! Reservation lifecycle: acquire and release.

use super::{select_id, Context};
use crate::{
    args::LeaseSelector,
    output::{fields, Output},
};
use groove_core::{
    snapshot::SaveOptions, FinishResult, LeaseInfo, ReleaseResult, ReservationOptions, Result,
};
use serde_json::{json, Value};
use std::time::Duration;

pub struct AcquireRequest {
    pub base: String,
    pub task: Option<String>,
    pub request_key: Option<String>,
    pub wait: Duration,
}

/// Human summary of one reservation.
pub fn describe_lease(lease: &LeaseInfo) -> String {
    let label = lease.task.as_deref().unwrap_or("untitled task");
    format!(
        "{slot} reserved for '{label}'\n  lease:  {id}\n  path:   {path}\n  base:   {base}\n",
        base = short(lease.base_oid.as_str()),
        slot = lease.slot_id,
        id = lease.lease_id,
        path = lease.worktree_path.display(),
    )
}

fn short(oid: &str) -> &str {
    &oid[..oid.len().min(12)]
}

pub fn acquire(context: &Context<'_>, request: AcquireRequest) -> Result<Output> {
    let pool = context.pool()?;
    let lease = pool.reserve(ReservationOptions {
        acquire: groove_core::AcquireOptions {
            base: request.base,
            wait: request.wait,
            git_timeout: context.git_timeout,
        },
        task: request.task,
        request_key: request.request_key,
    })?;
    tracing::info!(event = "workspace.reserved", slot = %lease.slot_id, lease_id = %lease.lease_id, "Workspace reserved");
    Output::text(serde_json::to_value(&lease)?, describe_lease(&lease))
}

fn release_saved(
    context: &Context<'_>,
    selector: &LeaseSelector,
    save: SaveOptions,
    force: bool,
) -> Result<Output> {
    let pool = context.retained_pool()?;
    let id = select_id(&pool, selector)?;
    if !force {
        // --save preserves changes; only other writers block it.
        pool.ensure_quiescent(id, false, context.git_timeout)?;
    }
    let save = SaveOptions {
        session: Some(id.to_string()),
        ..save
    };
    let result = pool.finish(id, save)?;
    tracing::info!(event = "workspace.released", slot = %result.release.slot_id, lease_id = %id, "Workspace released");
    let text = describe_finish(&result);
    Output::text(result, text)
}

fn describe_finish(result: &FinishResult) -> String {
    format!(
        "Pinned snapshot {} and released {}.\n",
        result.snapshot.id, result.release.slot_id
    )
}

pub fn release(
    context: &Context<'_>,
    selector: &LeaseSelector,
    save: Option<SaveOptions>,
    force: bool,
) -> Result<Output> {
    if let Some(save) = save {
        return release_saved(context, selector, save, force);
    }
    let pool = context.pool()?;
    let id = select_id(&pool, selector)?;
    if !force {
        pool.ensure_quiescent(id, true, context.git_timeout)?;
    }
    let result: ReleaseResult = pool.release_lease(id)?;
    tracing::info!(event = "workspace.released", slot = %result.slot_id, lease_id = %id, "Workspace released");
    let text = format!(
        "Released {} (lease {}). Its contents are discarded on the next acquisition.",
        result.slot_id, result.lease_id
    );
    Output::text(result, text)
}

/// Human view of a lease plus any extra JSON context.
pub fn lease_output(lease: LeaseInfo, extra: Value) -> Result<Output> {
    let text = describe_lease(&lease) + &fields(&extra, 1);
    let mut result = json!({ "lease": lease });
    if let (Some(map), Value::Object(extra)) = (result.as_object_mut(), extra) {
        map.extend(extra);
    }
    Output::text(result, text)
}
