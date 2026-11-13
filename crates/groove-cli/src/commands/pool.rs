//! Pool-wide commands: init, list, show and path.

use super::{lease::lease_output, Context};
use crate::{
    args::{Cleanup, OptionalSelector},
    output::{fields, table, Output},
};
use groove_core::{LeaseInfo, Pool, PoolInspection, PoolOptions, Result, SlotId, SlotInspection};
use serde_json::json;
use std::path::PathBuf;

pub fn pools(context: &Context<'_>) -> Result<Output> {
    let pools = Pool::list_pools(context.repo, context.git_timeout)?;
    let rows: Vec<Vec<String>> = pools
        .iter()
        .map(|p| {
            vec![
                p.name.to_string(),
                match p.phase {
                    groove_core::PoolPhase::Initializing => "initializing",
                    groove_core::PoolPhase::Ready => "ready",
                    groove_core::PoolPhase::Deleting => "deleting",
                    groove_core::PoolPhase::Deleted => "deleted",
                }
                .into(),
                p.capacity.map_or_else(|| "-".into(), |n| n.to_string()),
                p.worktree_root
                    .as_ref()
                    .map_or_else(|| "-".into(), |r| r.display().to_string()),
            ]
        })
        .collect();
    let text = if rows.is_empty() {
        "No pools initialized.\n".into()
    } else {
        table(&["POOL", "STATE", "CAPACITY", "CHECKOUT ROOT"], &rows)
    };
    Output::text(json!({"pools": pools}), text)
}

pub fn destroy(
    context: &Context<'_>,
    dry_run: bool,
    discard_changes: bool,
    pool_id: Option<groove_core::PoolId>,
) -> Result<Output> {
    let options = groove_core::DestroyOptions {
        dry_run,
        discard_changes,
        pool_id,
        git_timeout: context.git_timeout,
    };
    let result = match context.selected_pool {
        Some(name) => Pool::destroy_named(context.repo, name, options)?,
        None => Pool::destroy(context.repo, options)?,
    };
    let mut text = if result.already_deleted {
        "Pool is already deleted.\n".to_owned()
    } else {
        format!(
            "{} {} workspace slots and {} artifact directories.\n",
            if dry_run { "Would remove" } else { "Removed" },
            result.slots.len(),
            result.artifacts.len()
        )
    };
    text.push_str(&format!("Pool: {}\n", result.pool_name));
    text.push_str(&format!(
        "Retained locks, history, receipts and snapshots; state: {}\n",
        result.state_dir.display()
    ));
    if let Some(id) = result.pool_id {
        text.push_str(&format!("Pool ID: {id}\n"));
    }
    Output::text(json!({"destruction": result}), text)
}

pub fn init(
    context: &Context<'_>,
    capacity: usize,
    cleanup: Cleanup,
    worktree_root: Option<PathBuf>,
    resume: bool,
) -> Result<Output> {
    let result = Pool::init_named(
        context.repo,
        &context.pool_name(),
        PoolOptions {
            resume,
            capacity,
            cleanup_policy: cleanup.into(),
            worktree_root,
            worktree_base: crate::preferences::load()?.worktrees.root,
            git_timeout: context.git_timeout,
        },
    )?;
    let heading = if result.pool_name == groove_core::PoolName::default() {
        format!("Initialized a pool of {} slots.", result.capacity)
    } else {
        format!(
            "Initialized pool '{}' with {} slots.",
            result.pool_name, result.capacity
        )
    };
    let text = format!(
        "{heading}\n  checkouts: {}\n  state:     {}\n",
        result.worktree_root.display(),
        result.state_dir.display()
    );
    Output::text(result, text)
}

fn task_label(lease: Option<&LeaseInfo>) -> String {
    lease
        .and_then(|lease| lease.task.clone())
        .unwrap_or_else(|| "-".into())
}

fn slot_table(inspection: &PoolInspection) -> String {
    let rows: Vec<Vec<String>> = inspection
        .slots
        .iter()
        .map(|slot| {
            let status = serde_json::to_value(&slot.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let git = slot
                .git
                .as_ref()
                .map_or(String::new(), |git| match &git.error {
                    Some(_) => "git: error".into(),
                    None => [
                        (git.staged, "staged"),
                        (git.modified, "modified"),
                        (git.untracked, "untracked"),
                    ]
                    .iter()
                    .filter(|(set, _)| *set)
                    .map(|(_, name)| *name)
                    .collect::<Vec<_>>()
                    .join(","),
                });
            vec![
                slot.slot_id.to_string(),
                status,
                task_label(slot.lease.as_ref()),
                git,
                slot.reason.clone(),
            ]
        })
        .collect();
    table(&["SLOT", "STATUS", "TASK", "CHANGES", "REASON"], &rows)
}

pub fn list(context: &Context<'_>, slots: bool, details: bool) -> Result<Output> {
    let pool = context.pool()?;
    if slots || details {
        let inspection = if details {
            pool.inspect_detailed(context.git_timeout)?
        } else {
            pool.inspect()?
        };
        let text = slot_table(&inspection);
        return Output::text(json!({ "inspection": inspection }), text);
    }
    let leases = pool.protected_leases()?;
    let text = if leases.is_empty() {
        "No active reservations.".to_owned()
    } else {
        let rows: Vec<Vec<String>> = leases
            .iter()
            .map(|lease| {
                vec![
                    lease.slot_id.to_string(),
                    task_label(Some(lease)),
                    lease.lease_id.to_string(),
                    lease.worktree_path.display().to_string(),
                ]
            })
            .collect();
        table(&["SLOT", "TASK", "LEASE", "PATH"], &rows)
    };
    Output::text(json!({ "leases": leases }), text)
}

fn inspect(pool: &Pool, details: bool, timeout: std::time::Duration) -> Result<PoolInspection> {
    if details {
        pool.inspect_detailed(timeout)
    } else {
        pool.inspect()
    }
}

fn inspect_slot(
    pool: &Pool,
    slot: &SlotId,
    details: bool,
    timeout: std::time::Duration,
) -> Result<SlotInspection> {
    if details {
        pool.inspect_slot_detailed(slot, timeout)
    } else {
        pool.inspect_slot(slot)
    }
}

pub fn show(context: &Context<'_>, selector: &OptionalSelector, details: bool) -> Result<Output> {
    let pool = context.pool()?;
    let lease = match (&selector.lease_id, &selector.task) {
        (Some(id), _) => Some(pool.lease_info(*id)?),
        (None, Some(task)) => Some(pool.lease_by_task(task)?),
        (None, None) => None,
    };
    if let Some(lease) = lease {
        let slot = inspect_slot(&pool, &lease.slot_id, details, context.git_timeout)?;
        let extra = json!({
            "slot": {
                "status": slot.status,
                "reason": slot.reason,
                "next_action": slot.next_action,
                "execution": slot.execution,
                "preparation": slot.preparation,
                "git": slot.git,
            }
        });
        return lease_output(lease, extra);
    }
    if let Some(slot) = &selector.slot {
        let slot = inspect_slot(&pool, slot, details, context.git_timeout)?;
        return Output::fields(json!({ "slot": slot }));
    }
    let settings = pool.settings()?;
    let inspection = inspect(&pool, details, context.git_timeout)?;
    let text = fields(&serde_json::to_value(&settings)?, 0) + "\n" + &slot_table(&inspection);
    Output::text(json!({ "pool": settings, "inspection": inspection }), text)
}
