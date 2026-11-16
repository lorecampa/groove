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
use groove_core::{LeaseInfo, Pool, Result};
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

/// Resolve `--reservation-id` or `--task` to one reservation.
fn select(pool: &Pool, selector: &LeaseSelector) -> Result<LeaseInfo> {
    match (&selector.lease_id, &selector.task) {
        (Some(id), _) => pool.lease_info(*id),
        (None, Some(task)) => pool.lease_by_task(task),
        (None, None) => unreachable!("clap requires --reservation-id or --task"),
    }
}

/// Resolve the selector to a lease ID without validating an explicit ID, so
/// core retry logic (e.g. saved-release receipts for superseded generations) still applies.
fn select_id(pool: &Pool, selector: &LeaseSelector) -> Result<groove_core::LeaseId> {
    match (&selector.lease_id, &selector.task) {
        (Some(id), _) => Ok(*id),
        _ => select(pool, selector).map(|lease| lease.lease_id),
    }
}

/// Run one parsed command.
pub fn run(context: &Context<'_>, command: &Command) -> Result<Output> {
    match command {
        Command::Upgrade(options) => upgrade::run(options),
        Command::Pool(command) => match command {
            PoolCommand::Get { key } => pool::get(context, key),
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
            } => pool::destroy(context, *dry_run, *discard_changes, *pool_id),
            PoolCommand::Init {
                ready_target,
                idle_timeout,
                ready_base,
                capacity,
                cleanup,
                worktree_root,
                resume,
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
                    },
                )
            }
            PoolCommand::Show {
                details,
                disk_usage,
                page,
                sort,
            } => pool::show(
                context,
                &Default::default(),
                *details,
                *disk_usage,
                page,
                *sort,
            ),
            PoolCommand::Set {
                capacity,
                cleanup,
                ready_target,
                idle_timeout,
                ready_base,
            } => pool::set(
                context,
                groove_core::PoolUpdate {
                    capacity: *capacity,
                    cleanup_policy: cleanup.map(Into::into),
                    ready_target: *ready_target,
                    idle_timeout: *idle_timeout,
                    ready_base: ready_base.clone(),
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
        Command::Show { selector, details } => pool::show(
            context,
            selector,
            *details,
            false,
            &Default::default(),
            crate::args::SlotSort::Slot,
        ),
        Command::Completions { shell } => completions::run(*shell),
        Command::Snapshot(command) => snapshot::run(context, command),
        Command::Config(command) => config::run(context, command),
    }
}
