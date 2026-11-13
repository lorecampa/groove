//! One handler per command. Handlers return typed results wrapped in [`Output`].

mod completions;
mod config;
mod lease;
mod pool;
mod snapshot;

use crate::{
    args::{Command, LeaseSelector},
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

/// Resolve `--lease-id` or `--task` to one reservation.
fn select(pool: &Pool, selector: &LeaseSelector) -> Result<LeaseInfo> {
    match (&selector.lease_id, &selector.task) {
        (Some(id), _) => pool.lease_info(*id),
        (None, Some(task)) => pool.lease_by_task(task),
        (None, None) => unreachable!("clap requires --lease-id or --task"),
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
        Command::Pools { all, disk_usage } => pool::pools(context, *all, *disk_usage),
        Command::Destroy {
            dry_run,
            discard_changes,
            pool_id,
        } => pool::destroy(context, *dry_run, *discard_changes, *pool_id),
        Command::Init {
            capacity,
            cleanup,
            worktree_root,
            resume,
        } => pool::init(context, *capacity, *cleanup, worktree_root.clone(), *resume),
        Command::Acquire {
            base,
            task,
            request_key,
            wait_ms,
        } => lease::acquire(
            context,
            lease::AcquireRequest {
                base: base.clone(),
                task: task.clone(),
                request_key: request_key.clone(),
                wait: Duration::from_millis(*wait_ms),
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
        Command::List { slots, details } => pool::list(context, *slots, *details),
        Command::Show {
            selector,
            details,
            disk_usage,
        } => pool::show(context, selector, *details, *disk_usage),
        Command::Completions { shell } => completions::run(*shell),
        Command::Snapshot(command) => snapshot::run(context, command),
        Command::Config(command) => config::run(context, command),
    }
}
