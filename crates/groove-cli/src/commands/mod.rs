//! One handler per command. Handlers return typed results wrapped in [`Output`].

mod completions;
mod config;
mod lease;
mod pool;
mod snapshot;
mod snapshot_prune;
mod upgrade;

use crate::{
    args::{Command, LeaseSelector, PoolCommand},
    output::Output,
};
use groove_core::{Pool, Result};
use std::{path::Path, time::Duration};

/// Inputs shared by every handler.
pub struct Context<'a> {
    pub repo: &'a Path,
    pub selected_pool: Option<&'a groove_core::PoolName>,
    pub git_timeout: Duration,
}
impl Context<'_> {
    fn pool_name(&self) -> groove_core::PoolName {
        self.selected_pool.cloned().unwrap_or_default()
    }
    fn pool(&self) -> Result<Pool> {
        Pool::open_named_with_timeout(self.repo, &self.pool_name(), self.git_timeout)
    }
    fn retained_pool(&self) -> Result<Pool> {
        Pool::open_retained_named(self.repo, &self.pool_name(), self.git_timeout)
    }
}

/// Locate an explicit reservation selector without silently choosing default.
pub fn reservation_pool(
    repo: &Path,
    selected: Option<&groove_core::PoolName>,
    selector: &LeaseSelector,
    saved: bool,
    timeout: Duration,
) -> Result<groove_core::PoolName> {
    use groove_core::{Error, ErrorCode, PoolPhase};
    let label = selector
        .task
        .as_ref()
        .map(|task| format!("task '{task}'"))
        .unwrap_or_else(|| {
            format!(
                "reservation '{}'",
                selector.lease_id.expect("explicit selector")
            )
        });
    let mut matches = Vec::new();
    for entry in Pool::list_pools(repo, timeout)? {
        if entry.phase != PoolPhase::Ready && !(saved && entry.phase == PoolPhase::Deleted) {
            continue;
        }
        let pool = Pool::open_retained_named(repo, &entry.name, timeout)?;
        let found = if let Some(id) = selector.lease_id {
            if saved && pool.has_saved_release(id)? {
                true
            } else {
                match pool.lease_info(id) {
                    Ok(_) => true,
                    Err(error) if error.code == ErrorCode::LeaseMismatch => false,
                    Err(error) => return Err(error),
                }
            }
        } else if entry.phase == PoolPhase::Ready {
            match pool.lease_by_task(selector.task.as_deref().expect("explicit task selector")) {
                Ok(_) => true,
                Err(error) if error.code == ErrorCode::LeaseMismatch => false,
                Err(error) => return Err(error),
            }
        } else {
            false
        };
        if found {
            matches.push(entry.name);
        }
    }
    if let Some(selected) = selected {
        if matches.contains(selected) {
            return Ok(selected.clone());
        }
        let elsewhere = if matches.is_empty() {
            String::new()
        } else {
            format!(
                "; it is present in {}",
                matches
                    .iter()
                    .map(|name| format!("'{name}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        return Err(Error::new(
            ErrorCode::LeaseMismatch,
            format!("No {label} in pool '{selected}'{elsewhere}"),
        ));
    }
    match matches.as_slice() {
        [name] => Ok(name.clone()),
        [] => Err(Error::new(ErrorCode::LeaseMismatch, format!("No {label} found in any repository pool"))),
        _ => Err(Error::new(ErrorCode::InvalidArgument, format!("Several pools contain {label}: {}. Select the reservation UUID; migrate duplicate task labels before reusing the label", matches.iter().map(ToString::to_string).collect::<Vec<_>>().join(", ")))),
    }
}

/// Resolve the selector to a lease ID without validating an explicit ID, so
/// core retry logic (e.g. saved-release receipts for superseded generations) still applies.
fn select_id(pool: &Pool, selector: &LeaseSelector, path: &Path) -> Result<groove_core::LeaseId> {
    match (&selector.lease_id, &selector.task) {
        (Some(id), _) => Ok(*id),
        (None, None) => pool.lease_for_workspace(path).map(|lease| lease.lease_id),
        (None, Some(task)) => pool.lease_by_task(task).map(|lease| lease.lease_id),
    }
}

/// Run one parsed command.
pub fn run(context: &Context<'_>, command: &Command) -> Result<Output> {
    match command {
        Command::Upgrade(options) => upgrade::run(options),
        Command::Pool(command) => match command {
            PoolCommand::Get { key, .. } => pool::get(context, key),
            PoolCommand::List {
                all,
                disk_usage,
                page,
                sort,
            } => pool::pools(context, *all, *disk_usage, page, *sort),
            PoolCommand::Destroy {
                dry_run,
                discard_changes,
                pool_id,
                ..
            } => pool::destroy(context, *dry_run, *discard_changes, *pool_id),
            PoolCommand::Init {
                ready_target,
                idle_timeout,
                ready_base,
                ready_git_timeout,
                capacity,
                cleanup,
                worktree_root,
                resume,
                ..
            } => {
                let defaults = crate::preferences::load()?.pool_defaults;
                pool::init(
                    context,
                    *capacity,
                    *cleanup,
                    worktree_root.clone(),
                    *resume,
                    groove_core::ReadySettings {
                        ready_target: ready_target.unwrap_or(defaults.ready_target),
                        idle_timeout: idle_timeout.unwrap_or(defaults.idle_timeout),
                        ready_base: ready_base.clone().unwrap_or_else(|| "HEAD".into()),
                        ready_git_timeout: *ready_git_timeout,
                    },
                )
            }
            PoolCommand::Show {
                details,
                disk_usage,
                page,
                sort,
                ..
            } => pool::show(
                context,
                &Default::default(),
                *details,
                *disk_usage,
                page,
                *sort,
            ),
            PoolCommand::Set {
                values,
                capacity,
                cleanup,
                ready_target,
                idle_timeout,
                ready_base,
                ready_git_timeout,
                ..
            } => pool::set(
                context,
                values,
                groove_core::PoolUpdate {
                    capacity: *capacity,
                    cleanup_policy: cleanup.map(Into::into),
                    ready_target: *ready_target,
                    idle_timeout: *idle_timeout,
                    ready_base: ready_base.clone(),
                    ready_git_timeout: *ready_git_timeout,
                    timeout: context.git_timeout,
                },
            ),
        },
        Command::Acquire {
            base,
            task,
            request_key,
            wait_ms,
            path_only,
            ..
        } => lease::acquire(
            context,
            lease::AcquireRequest {
                base: base.clone(),
                task: task.clone(),
                request_key: request_key.clone(),
                wait: Duration::from_millis(*wait_ms),
                path_only: *path_only,
            },
        ),
        Command::Release {
            lease,
            save,
            description,
            max_file_bytes,
            force,
        } => lease::release(
            context,
            lease,
            save.then(|| groove_core::snapshot::SaveOptions {
                description: description.clone(),
                session: None,
                max_file_bytes: *max_file_bytes,
            }),
            *force,
        ),
        Command::List {
            slots,
            details,
            page,
            sort,
        } => pool::list(context, *slots, *details, page, *sort),
        Command::Show { selector, details } => {
            let mut selector = selector.clone();
            if selector.lease_id.is_none() && selector.task.is_none() && selector.slot.is_none() {
                selector.lease_id =
                    Some(context.pool()?.lease_for_workspace(context.repo)?.lease_id);
            }
            pool::show(
                context,
                &selector,
                *details,
                false,
                &Default::default(),
                crate::args::SlotSort::Slot,
            )
        }
        Command::Path { selector } => {
            let pool = context.pool()?;
            let id = select_id(&pool, selector, context.repo)?;
            let lease = pool.lease_info(id)?;
            Output::raw(
                serde_json::to_value(&lease)?,
                format!("{}\n", lease.worktree_path.display()),
            )
        }
        Command::Completions { shell } => completions::run(*shell),
        Command::Snapshot(command) => snapshot::run(context, command),
        Command::Config(command) => config::run(context, command),
    }
}
