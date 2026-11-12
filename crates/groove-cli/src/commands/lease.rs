//! Reservation lifecycle: acquire, exec, stop, recover, finish and release.

use super::{select_id, Context};
use crate::{
    args::{ExecLimits, LeaseSelector, SetupArgs},
    output::{fields, preparation_status, Output},
};
use groove_core::{
    snapshot::SaveOptions, ExecOptions, ExecResult, FinishResult, LeaseInfo, NamedCommand,
    Preparation, PreparationOptions, ReleaseResult, ReservationOptions, Result, StopRequest,
    Termination,
};
use serde_json::{json, Value};
use std::{ffi::OsString, time::Duration};

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

fn setup_options(
    context: &Context<'_>,
    setup: &SetupArgs,
    mut execution: ExecOptions,
) -> PreparationOptions {
    execution.observer = (!context.terminal.verbosity.quiet).then(|| {
        std::sync::Arc::new(context.terminal.clone())
            as std::sync::Arc<dyn groove_core::ExecutionObserver>
    });
    PreparationOptions {
        retry: setup.retry,
        cache_source: setup.cache_source.clone(),
        timeout: context.git_timeout,
        execution,
    }
}

fn describe_preparation(preparation: &Preparation) -> String {
    let mut text = format!(
        "setup '{}': {:?}\n",
        preparation.profile, preparation.status
    )
    .to_lowercase();
    for step in &preparation.steps {
        let outcome = match &step.execution {
            None => "skipped (cache hit)".to_owned(),
            Some(run) => describe_status(run),
        };
        text.push_str(&format!("  {}: {outcome}\n", step.name));
    }
    if let Some(error) = &preparation.error {
        text.push_str(&format!("  error: {error}\n"));
    }
    text
}

fn describe_status(run: &ExecResult) -> String {
    match run.termination {
        Termination::TimedOut => "timed out".into(),
        Termination::Cancelled => "cancelled".into(),
        Termination::OutputLimited => "stopped: output limit exceeded".into(),
        _ => match (run.exit_code, run.signal) {
            (Some(code), _) => format!("exited with status {code}"),
            (None, Some(signal)) => format!("killed by signal {signal}"),
            (None, None) => "ended".into(),
        },
    }
}

pub fn acquire(
    context: &Context<'_>,
    request: AcquireRequest,
    setup: &SetupArgs,
) -> Result<Output> {
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
    let mut result = serde_json::to_value(&lease)?;
    let mut text = describe_lease(&lease);
    let mut status = 0;
    if let Some(profile) = &setup.profile {
        let options = setup_options(context, setup, ExecOptions::default());
        let preparation = pool
            .prepare(lease.lease_id, profile, options)
            .map_err(|error| {
                error.with_details(|d| {
                    d.lease_id = Some(lease.lease_id);
                    d.worktree_path = Some(lease.worktree_path.clone());
                })
            })?;
        status = preparation_status(&preparation);
        text.push_str(&describe_preparation(&preparation));
        result["preparation"] = serde_json::to_value(preparation)?;
    }
    Ok(Output::text(result, text)?.with_status(status))
}

pub fn exec(
    context: &Context<'_>,
    selector: &LeaseSelector,
    limits: &ExecLimits,
    setup: &SetupArgs,
    recipe: Option<&str>,
    agent: Option<&str>,
    command: &[OsString],
) -> Result<Output> {
    let pool = context.pool()?;
    let id = select_id(&pool, selector)?;
    let options = ExecOptions {
        command: command.to_vec(),
        timeout: Duration::from_millis(limits.timeout_ms),
        grace: Duration::from_millis(limits.grace_ms),
        max_log_bytes: limits.max_log_bytes,
        observer: (!context.terminal.verbosity.quiet).then(|| {
            std::sync::Arc::new(context.terminal.clone())
                as std::sync::Arc<dyn groove_core::ExecutionObserver>
        }),
        ..ExecOptions::default()
    };
    if let Some(profile) = &setup.profile {
        let preparation = pool.prepare(id, profile, setup_options(context, setup, options))?;
        let status = preparation_status(&preparation);
        let text = describe_preparation(&preparation);
        return Ok(Output::text(json!({ "preparation": preparation }), text)?.with_status(status));
    }
    let result = match (recipe, agent) {
        (Some(name), _) => pool.exec_named(id, NamedCommand::Recipe(name), command, options)?,
        (None, Some(name)) => pool.exec_named(id, NamedCommand::Agent(name), command, options)?,
        (None, None) => pool.exec(id, options)?,
    };
    let text = format!(
        "{} {}\n  stdout: {}\n  stderr: {}\n",
        result.run.program,
        describe_status(&result),
        result.run.stdout_path.display(),
        result.run.stderr_path.display(),
    );
    let status = result.shell_status();
    Ok(Output::text(json!({ "execution": result }), text)?.with_status(status))
}

pub fn stop(context: &Context<'_>, selector: &LeaseSelector) -> Result<Output> {
    let pool = context.pool()?;
    let request = pool.request_stop(select_id(&pool, selector)?)?;
    let text = match &request {
        StopRequest::Preparation { .. } => "Stop requested for running setup.",
        StopRequest::Execution { .. } => "Stop requested for running command.",
    };
    Output::text(
        request,
        format!("{text} Run `groove show` until it clears."),
    )
}

pub fn recover(context: &Context<'_>, selector: &LeaseSelector) -> Result<Output> {
    let pool = context.pool()?;
    let info = pool.recover_execution(select_id(&pool, selector)?)?;
    let text = format!(
        "Cleared orphaned execution of {}; the reservation is kept.",
        info.program
    );
    Output::text(json!({ "execution_recovered": info }), text)
}

pub fn finish(
    context: &Context<'_>,
    selector: &LeaseSelector,
    save: SaveOptions,
    force: bool,
) -> Result<Output> {
    let pool = context.retained_pool()?;
    let id = select_id(&pool, selector)?;
    if !force {
        // Saving changes is finish's job; only other writers block it.
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

pub fn release(context: &Context<'_>, selector: &LeaseSelector, force: bool) -> Result<Output> {
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
