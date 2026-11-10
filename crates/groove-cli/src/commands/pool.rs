//! Pool-wide commands: init, list, show and path.

use super::{lease::lease_output, Context};
use crate::{
    args::{Cleanup, OptionalSelector},
    output::{fields, table, Output},
};
use groove_core::{LeaseInfo, Pool, PoolInspection, PoolOptions, Result, SlotId, SlotInspection};
use serde_json::json;
use std::path::PathBuf;

pub fn init(
    context: &Context<'_>,
    capacity: usize,
    cleanup: Cleanup,
    worktree_root: Option<PathBuf>,
    resume: bool,
) -> Result<Output> {
    let result = Pool::init(
        context.repo,
        PoolOptions {
            resume,
            capacity,
            cleanup_policy: cleanup.into(),
            worktree_root,
            git_timeout: context.git_timeout,
        },
    )?;
    let text = format!(
        "Initialized a pool of {} slots.\n  checkouts: {}\n  state:     {}\n",
        result.capacity,
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
