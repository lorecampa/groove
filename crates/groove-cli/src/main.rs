mod args;
mod config;
mod help;
mod logging;
mod protocol;
mod snapshot;
mod transport;
use args::{Action, Args, Cleanup};
use clap::{error::ErrorKind, CommandFactory, Parser};
use groove_core::{AcquireOptions, CleanupPolicy, Error, Pool, PoolOptions, Result};
use std::{os::fd::AsFd, process::ExitCode, time::Duration};

fn main() -> ExitCode {
    let raw: Vec<_> = std::env::args_os().collect();
    let request_id = raw
        .windows(2)
        .find(|v| v[0] == "--request-id")
        .and_then(|v| v[1].to_str())
        .map(str::to_owned)
        .or_else(|| {
            raw.iter()
                .filter_map(|v| v.to_str())
                .find_map(|v| v.strip_prefix("--request-id=").map(str::to_owned))
        });
    let args = match Args::try_parse_from(&raw) {
        Ok(args) => args,
        Err(error) => {
            // Reuse Clap's parser to honor logging controls even for help and
            // malformed commands, without recording raw arguments.
            let partial = Args::command()
                .ignore_errors(true)
                .try_get_matches_from(&raw)
                .ok();
            let verbose = partial.as_ref().map_or(0, |m| m.get_count("verbose"));
            let settings = config::load().unwrap_or_else(|_| config::Config {
                logging: config::Logging {
                    enabled: false,
                    ..Default::default()
                },
            });
            let _logging =
                logging::init(verbose, settings.logging, request_id.clone(), "arguments");

            let help = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
            if help {
                tracing::debug!("Help or version requested");
            } else {
                tracing::error!(request_id = ?request_id, "Invalid command arguments");
            }
            let response = if help {
                protocol::success(
                    request_id.as_deref(),
                    serde_json::json!({"text":error.to_string()}),
                )
            } else {
                protocol::failure(
                    request_id.as_deref(),
                    &Error::new(groove_core::ErrorCode::InvalidArgument, error.to_string()),
                )
            };
            return if protocol::write_response(&response).is_err() {
                ExitCode::from(7)
            } else {
                ExitCode::from(if help { 0 } else { 2 })
            };
        }
    };
    let settings = if matches!(args.command, Action::Config { .. }) {
        config::Config {
            logging: config::Logging {
                enabled: false,
                ..Default::default()
            },
        }
    } else {
        match config::load() {
            Ok(settings) => settings,
            Err(error) => {
                let _ = protocol::write_response(&protocol::failure(
                    args.request_id.as_deref(),
                    &error,
                ));
                return ExitCode::from(error.exit_code());
            }
        }
    };
    let _logging = logging::init(
        args.verbose,
        settings.logging,
        args.request_id.clone(),
        args.command.name(),
    );
    let started = std::time::Instant::now();
    tracing::debug!(event = "command.started", "Command started");
    match execute(&args) {
        Ok((response, handed_off)) => {
            tracing::info!(
                event = "command.completed",
                elapsed_ms = started.elapsed().as_millis() as u64,
                "Command completed"
            );
            if protocol::write_response(&response).is_err() && !handed_off {
                ExitCode::from(7)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            tracing::error!(event = "command.failed", code = %serde_json::to_value(e.code).unwrap_or_default().as_str().unwrap_or("UNKNOWN"), elapsed_ms = started.elapsed().as_millis() as u64, "Command failed");
            let _ = protocol::write_response(&protocol::failure(args.request_id.as_deref(), &e));
            ExitCode::from(e.exit_code())
        }
    }
}
fn execute(args: &Args) -> Result<(serde_json::Value, bool)> {
    let timeout = Duration::from_millis(args.git_timeout_ms);
    let result = match &args.command {
        Action::Config { command } => config::execute(command)?,
        Action::Snapshot { command } => snapshot::execute(&args.repo, timeout, command)?,
        Action::Init {
            capacity,
            cleanup,
            worktree_root,
        } => {
            let options = PoolOptions {
                capacity: *capacity as usize,
                cleanup_policy: match cleanup {
                    Cleanup::Pristine => CleanupPolicy::Pristine,
                    Cleanup::Warm => CleanupPolicy::Warm,
                },
                worktree_root: worktree_root.clone(),
                git_timeout: timeout,
            };
            serde_json::to_value(Pool::init(&args.repo, options)?)?
        }
        Action::Acquire {
            base,
            lease_channel_fd,
            wait_ms,
        } => {
            let channel = lease_channel_fd.map(transport::channel).transpose()?;
            let pool = Pool::open(&args.repo)?;
            let lease = pool.acquire(AcquireOptions {
                base: base.clone(),
                wait: Duration::from_millis(*wait_ms),
                git_timeout: timeout,
            })?;
            let Some(channel) = channel else {
                let info = lease.into_protected()?;
                tracing::info!(event = "workspace.reserved", slot = %info.slot_id, lease_id = %info.lease_id, "Workspace reserved");
                return Ok((protocol::success(args.request_id.as_deref(), info), false));
            };
            let (mut info, fd) = lease.into_descriptor()?;
            info.lease_transport = groove_core::LeaseTransport::ScmRights;
            let response = protocol::success(args.request_id.as_deref(), info);
            groove_core::transport::send_record(channel.as_fd(), &response, fd.as_fd())?;
            return Ok((response, true));
        }
        Action::Leases => {
            serde_json::json!({"leases": Pool::open(&args.repo)?.protected_leases()?})
        }
        Action::Release {
            slot,
            lease_id,
            lease_fd,
            writers_stopped,
        } => {
            let pool = Pool::open(&args.repo)?;
            let result = if let Some(fd) = lease_fd {
                let fd = transport::duplicate(*fd)?;
                pool.release_fd(slot, *lease_id, fd.as_fd())?
            } else {
                if !writers_stopped {
                    return Err(Error::new(
                        groove_core::ErrorCode::InvalidArgument,
                        "Stop all writers and save results before release",
                    ));
                }
                pool.release_protected(slot, *lease_id)?
            };
            tracing::info!(event = "workspace.released", slot = %slot, lease_id = %lease_id, "Workspace released");
            serde_json::to_value(result)?
        }
    };
    Ok((protocol::success(args.request_id.as_deref(), result), false))
}
