//! One handler per command. Handlers return typed results wrapped in [`Output`].

mod completions;
mod config;
mod history;
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
    pub terminal: &'a crate::terminal::Terminal,
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
/// core retry logic (e.g. finish receipts for superseded generations) still applies.
fn select_id(pool: &Pool, selector: &LeaseSelector) -> Result<groove_core::LeaseId> {
    match (&selector.lease_id, &selector.task) {
        (Some(id), _) => Ok(*id),
        _ => select(pool, selector).map(|lease| lease.lease_id),
    }
}

/// Run one parsed command.
pub fn run(context: &Context<'_>, command: &Command) -> Result<Output> {
    match command {
        Command::Pools => pool::pools(context),
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
            setup,
        } => lease::acquire(
            context,
            lease::AcquireRequest {
                base: base.clone(),
                task: task.clone(),
                request_key: request_key.clone(),
                wait: Duration::from_millis(*wait_ms),
            },
            setup,
        ),
        Command::Exec {
            lease,
            limits,
            setup,
            recipe,
            agent,
            command,
        } => lease::exec(
            context,
            lease,
            limits,
            setup,
            recipe.as_deref(),
            agent.as_deref(),
            command,
        ),
        Command::Stop { lease } => lease::stop(context, lease),
        Command::Recover { lease } => lease::recover(context, lease),
        Command::Finish {
            lease,
            description,
            max_file_bytes,
            force,
        } => lease::finish(
            context,
            lease,
            groove_core::snapshot::SaveOptions {
                description: description.clone(),
                session: None,
                max_file_bytes: *max_file_bytes,
            },
            *force,
        ),
        Command::Release { lease, force } => lease::release(context, lease, *force),
        Command::List { slots, details } => pool::list(context, *slots, *details),
        Command::Show { selector, details } => pool::show(context, selector, *details),
        Command::Completions { shell } => completions::run(*shell),
        Command::Snapshot(command) => snapshot::run(context, command),
        Command::Config(command) => config::run(context, command),
        Command::History {
            task,
            lease_id,
            command,
        } => history::history(context, task.clone(), *lease_id, command.as_ref()),
        Command::Storage(command) => history::storage(context, command),
    }
}
