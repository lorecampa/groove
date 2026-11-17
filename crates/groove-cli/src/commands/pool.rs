//! Pool-wide commands: init, list, show and path.

use super::{lease::lease_output, Context};
use crate::{
    args::{Cleanup, OptionalSelector, PageArgs, PoolSort, SlotSort},
    output::{fields, table, Output},
};
use groove_core::{LeaseInfo, Pool, PoolInspection, PoolOptions, Result, SlotId, SlotInspection};
use serde_json::json;
use std::path::PathBuf;

pub fn get(context: &Context<'_>, key: &str) -> Result<Output> {
    let normalized = key.replace('_', "-");
    let field = match normalized.as_str() {
        "capacity" => "capacity",
        "cleanup" => "cleanup_policy",
        "ready-target" => "ready_target",
        "idle-timeout" => "idle_timeout",
        "ready-base" => "ready_base",
        "ready-git-timeout" => "ready_git_timeout",
        "workspace-root" | "root" => "worktree_root",
        _ => {
            return Err(groove_core::Error::new(
                groove_core::ErrorCode::InvalidArgument,
                format!("Unknown setting: {key}"),
            ))
        }
    };
    let settings = serde_json::to_value(context.pool()?.settings()?)?;
    super::config::setting_output(key, settings[field].clone())
}

pub fn pools(
    context: &Context<'_>,
    all: bool,
    disk_usage: bool,
    page: &PageArgs,
    sort: PoolSort,
) -> Result<Output> {
    let mut pools = Pool::list_pools(context.repo, context.git_timeout)?;
    pools.retain(|pool| all || pool.phase != groove_core::PoolPhase::Deleted);
    pools.sort_by(|a, b| match sort {
        PoolSort::Name => a.name.cmp(&b.name),
        PoolSort::State => phase_name(a.phase)
            .cmp(phase_name(b.phase))
            .then_with(|| a.name.cmp(&b.name)),
    });
    let (pools, pagination) = page.apply(pools);
    let mut values = Vec::new();
    let mut rows = Vec::new();
    for p in &pools {
        let usage = if disk_usage
            && matches!(
                p.phase,
                groove_core::PoolPhase::Ready | groove_core::PoolPhase::Deleted
            ) {
            Some(
                Pool::open_retained_named(context.repo, &p.name, context.git_timeout)?
                    .storage_usage()?,
            )
        } else {
            None
        };
        let mut value = serde_json::to_value(p)?;
        if disk_usage {
            value["storage"] = serde_json::to_value(&usage)?;
        }
        values.push(value);
        let mut row = vec![
            p.name.to_string(),
            phase_name(p.phase).into(),
            p.capacity.map_or_else(|| "-".into(), |n| n.to_string()),
            p.worktree_root
                .as_ref()
                .map_or_else(|| "-".into(), |r| r.display().to_string()),
        ];
        if disk_usage {
            for bytes in usage
                .as_ref()
                .map(|u| {
                    [
                        u.workspace_bytes,
                        u.artifact_bytes,
                        u.log_bytes
                            .saturating_add(u.untracked_log_bytes)
                            .saturating_add(u.history_bytes),
                    ]
                })
                .map_or([None; 3], |sizes| sizes.map(Some))
            {
                row.push(bytes.map_or_else(|| "-".into(), human_bytes));
            }
        }
        rows.push(row);
    }
    let mut columns = vec!["POOL", "STATE", "CAPACITY", "WORKSPACE ROOT"];
    if disk_usage {
        columns.extend(["WORKSPACES", "CACHES", "LOGS/HISTORY"]);
    }
    let text = if rows.is_empty() {
        "No pools on this page.\n".into()
    } else {
        table(&columns, &rows)
    };
    Output::text(json!({"pools": values}), text)?.paginated(pagination)
}

fn phase_name(phase: groove_core::PoolPhase) -> &'static str {
    match phase {
        groove_core::PoolPhase::Initializing => "initializing",
        groove_core::PoolPhase::Ready => "ready",
        groove_core::PoolPhase::Deleting => "deleting",
        groove_core::PoolPhase::Deleted => "deleted",
    }
}

fn human_bytes(bytes: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < units.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", units[unit])
    }
}

fn storage_text(usage: &groove_core::StorageUsage) -> String {
    format!(
        "\nStorage (logical sizes):\n{}",
        table(
            &["WORKSPACES", "CACHES", "LOGS", "HISTORY", "UNTRACKED LOGS"],
            &[vec![
                human_bytes(usage.workspace_bytes),
                human_bytes(usage.artifact_bytes),
                human_bytes(usage.log_bytes),
                human_bytes(usage.history_bytes),
                human_bytes(usage.untracked_log_bytes)
            ]]
        )
    )
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
            "{} {} workspace {} and {} artifact {}.\n",
            if dry_run { "Would remove" } else { "Removed" },
            result.slots.len(),
            if result.slots.len() == 1 {
                "slot"
            } else {
                "slots"
            },
            result.artifacts.len(),
            if result.artifacts.len() == 1 {
                "directory"
            } else {
                "directories"
            }
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
    ready: groove_core::ReadySettings,
) -> Result<Output> {
    let result = Pool::init_named(
        context.repo,
        &context.pool_name(),
        PoolOptions {
            ready: Some(ready),
            resume,
            capacity,
            cleanup_policy: cleanup.into(),
            worktree_root,
            worktree_base: crate::preferences::load()?.worktrees.root,
            git_timeout: context.git_timeout,
        },
    )?;
    let slots = if result.capacity == 1 {
        "slot"
    } else {
        "slots"
    };
    let heading = if result.pool_name == groove_core::PoolName::default() {
        format!("Initialized a pool of {} {slots}.", result.capacity)
    } else {
        format!(
            "Initialized pool '{}' with {} {slots}.",
            result.pool_name, result.capacity
        )
    };
    let text = format!(
        "{heading}\n  workspaces: {}\n  state:      {}\n",
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
                task_label(slot.lease.as_ref().filter(|_| slot.lease_active)),
                git,
                slot.reason.clone(),
                slot.failure.clone().unwrap_or_else(|| "-".into()),
                slot.next_action.clone(),
            ]
        })
        .collect();
    table(
        &[
            "SLOT",
            "STATUS",
            "TASK",
            "CHANGES",
            "REASON",
            "FAILURE",
            "NEXT ACTION",
        ],
        &rows,
    )
}

fn sort_slots(slots: &mut [SlotInspection], sort: SlotSort) {
    slots.sort_by(|a, b| match sort {
        SlotSort::Slot => a.slot_id.cmp(&b.slot_id),
        SlotSort::Task => a
            .lease
            .as_ref()
            .filter(|_| a.lease_active)
            .and_then(|lease| lease.task.as_deref())
            .cmp(
                &b.lease
                    .as_ref()
                    .filter(|_| b.lease_active)
                    .and_then(|lease| lease.task.as_deref()),
            )
            .then_with(|| a.slot_id.cmp(&b.slot_id)),
    });
}

pub fn list(
    context: &Context<'_>,
    slots: bool,
    details: bool,
    page: &PageArgs,
    sort: SlotSort,
) -> Result<Output> {
    if slots || details {
        let pool = context.pool()?;
        let mut inspection = if details {
            pool.inspect_detailed(context.git_timeout)?
        } else {
            pool.inspect()?
        };
        sort_slots(&mut inspection.slots, sort);
        let (slots, pagination) = page.apply(inspection.slots);
        inspection.slots = slots;
        let text = slot_table(&inspection);
        return Output::text(json!({ "inspection": inspection }), text)?.paginated(pagination);
    }
    let names = if let Some(name) = context.selected_pool {
        vec![name.clone()]
    } else {
        Pool::list_pools(context.repo, context.git_timeout)?
            .into_iter()
            .filter(|pool| pool.phase == groove_core::PoolPhase::Ready)
            .map(|pool| pool.name)
            .collect()
    };
    let mut leases = Vec::new();
    for name in names {
        let pool = Pool::open_named_with_timeout(context.repo, &name, context.git_timeout)?;
        leases.extend(
            pool.protected_leases()?
                .into_iter()
                .map(|lease| (name.clone(), lease)),
        );
    }
    leases.sort_by(|a, b| match sort {
        SlotSort::Slot => a.1.slot_id.cmp(&b.1.slot_id).then_with(|| a.0.cmp(&b.0)),
        SlotSort::Task => {
            a.1.task
                .cmp(&b.1.task)
                .then_with(|| a.0.cmp(&b.0))
                .then_with(|| a.1.slot_id.cmp(&b.1.slot_id))
        }
    });
    let (leases, pagination) = page.apply(leases);
    let text = if leases.is_empty() {
        "No active reservations on this page.".to_owned()
    } else {
        let rows: Vec<Vec<String>> = leases
            .iter()
            .map(|(pool, lease)| {
                vec![
                    pool.to_string(),
                    lease.slot_id.to_string(),
                    task_label(Some(lease)),
                    lease.lease_id.to_string(),
                    lease.worktree_path.display().to_string(),
                ]
            })
            .collect();
        table(&["POOL", "SLOT", "TASK", "RESERVATION", "PATH"], &rows)
    };
    let leases: Result<Vec<_>> = leases
        .into_iter()
        .map(|(pool, lease)| {
            let mut value = serde_json::to_value(lease)?;
            value["pool"] = json!(pool);
            Ok(value)
        })
        .collect();
    let leases = leases?;
    Output::text(json!({ "leases": leases }), text)?.paginated(pagination)
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

pub fn show(
    context: &Context<'_>,
    selector: &OptionalSelector,
    details: bool,
    disk_usage: bool,
    page: &PageArgs,
    sort: SlotSort,
) -> Result<Output> {
    let pool = context.pool()?;
    let lease = match (&selector.lease_id, &selector.task) {
        (Some(id), _) => Some(pool.lease_info(*id)?),
        (None, Some(task)) => Some(pool.lease_by_task(task)?),
        (None, None) => None,
    };
    if let Some(lease) = lease {
        let slot = inspect_slot(&pool, &lease.slot_id, details, context.git_timeout)?;
        let extra = json!({
            "active": slot.lease_active && slot.lease.as_ref().is_some_and(|current| current.lease_id == lease.lease_id),
            "slot": {
                "status": slot.status,
                "reason": slot.reason,
                "next_action": slot.next_action,
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
    let mut inspection = inspect(&pool, details, context.git_timeout)?;
    sort_slots(&mut inspection.slots, sort);
    let (slots, pagination) = page.apply(inspection.slots);
    inspection.slots = slots;
    let mut text = fields(&serde_json::to_value(&settings)?, 0) + "\n" + &slot_table(&inspection);
    let mut value = json!({ "pool": settings, "inspection": inspection });
    let ready = pool.ready_report()?;
    text.push_str(&format!(
        "\nBackground workspaces: {} ready at last pass; {}.\n",
        ready.ready,
        if ready.running {
            "preparing or cleaning"
        } else {
            "idle"
        }
    ));
    if let Some(error) = &ready.error {
        text.push_str(&format!("  {error}\n"));
    }
    for (slot, error) in &ready.failures {
        text.push_str(&format!("  {slot}: {error}\n"));
    }
    value["ready"] = serde_json::to_value(ready)?;
    if disk_usage {
        let usage = pool.storage_usage()?;
        text.push_str(&storage_text(&usage));
        value["storage"] = serde_json::to_value(usage)?;
    }
    Output::text(value, text)?.paginated(pagination)
}

pub fn set(
    context: &Context<'_>,
    values: &[crate::args::PoolSettingArgument],
    mut update: groove_core::PoolUpdate,
) -> Result<Output> {
    use crate::args::PoolSettingArgument;
    let invalid =
        |message| groove_core::Error::new(groove_core::ErrorCode::InvalidArgument, message);
    let mut seen = std::collections::BTreeSet::new();
    for (index, argument) in values.iter().enumerate() {
        let (key, value) = match argument {
            PoolSettingArgument::Name(_) if index == 0 => continue,
            PoolSettingArgument::Name(_) => {
                return Err(invalid("Pool name must precede settings".into()))
            }
            PoolSettingArgument::Setting(key, value) => (key.replace('_', "-"), value),
        };
        if !seen.insert(key.clone()) {
            return Err(invalid(format!("Setting supplied more than once: {key}")));
        }
        macro_rules! assign {
            ($field:ident, $parsed:expr) => {{
                if update.$field.is_some() {
                    return Err(invalid(format!(
                        "Setting supplied by both flag and assignment: {key}"
                    )));
                }
                update.$field = Some($parsed);
            }};
        }
        match key.as_str() {
            "capacity" => assign!(
                capacity,
                value
                    .parse()
                    .map_err(|_| invalid(format!("Invalid capacity: {value}")))?
            ),
            "cleanup" => assign!(
                cleanup_policy,
                match value.as_str() {
                    "warm" => groove_core::CleanupPolicy::Warm,
                    "pristine" => groove_core::CleanupPolicy::Pristine,
                    _ => return Err(invalid(format!("Invalid cleanup: {value}"))),
                }
            ),
            "ready-target" => assign!(
                ready_target,
                value
                    .parse()
                    .map_err(|_| invalid(format!("Invalid ready-target: {value}")))?
            ),
            "ready-base" => assign!(ready_base, value.clone()),
            "idle-timeout" => assign!(idle_timeout, value.parse()?),
            "ready-git-timeout" => assign!(ready_git_timeout, value.parse()?),
            "root" | "workspace-root" => return Err(invalid("Pool root is read-only".into())),
            _ => return Err(invalid(format!("Unknown setting: {key}"))),
        }
    }
    if update.capacity.is_none()
        && update.cleanup_policy.is_none()
        && update.ready_target.is_none()
        && update.ready_base.is_none()
        && update.idle_timeout.is_none()
        && update.ready_git_timeout.is_none()
    {
        return Err(invalid("Supply at least one KEY=VALUE setting".into()));
    }
    let result = context.pool()?.configure(update)?;
    let headline = if result.updated {
        "Pool settings updated."
    } else {
        "Pool settings unchanged."
    };
    let text = format!(
        "{headline}\n{}",
        fields(&serde_json::to_value(&result.pool)?, 1)
    );
    Output::text(result, text)
}
