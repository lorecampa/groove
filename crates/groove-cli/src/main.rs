mod args;
mod protocol;
mod transport;
use args::{Action, Args, Cleanup};
use clap::{error::ErrorKind, Parser};
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
            let help = matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            );
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
    match execute(&args) {
        Ok((response, handed_off)) => {
            if protocol::write_response(&response).is_err() && !handed_off {
                ExitCode::from(7)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            if args.diagnostics {
                eprintln!(
                    "{}",
                    serde_json::json!({"level":"error","code":e.code,"message":e.message,"request_id":args.request_id})
                );
            }
            let _ = protocol::write_response(&protocol::failure(args.request_id.as_deref(), &e));
            ExitCode::from(e.exit_code())
        }
    }
}
fn execute(args: &Args) -> Result<(serde_json::Value, bool)> {
    let timeout = Duration::from_millis(args.git_timeout_ms);
    let result = match &args.command {
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
            let channel = transport::channel(*lease_channel_fd)?;
            let pool = Pool::open(&args.repo)?;
            let lease = pool.acquire(AcquireOptions {
                base: base.clone(),
                wait: Duration::from_millis(*wait_ms),
                git_timeout: timeout,
            })?;
            let (mut info, fd) = lease.into_descriptor()?;
            info.lease_transport = groove_core::LeaseTransport::ScmRights;
            let response = protocol::success(args.request_id.as_deref(), info);
            groove_core::transport::send_record(channel.as_fd(), &response, fd.as_fd())?;
            return Ok((response, true));
        }
        Action::Release {
            slot,
            lease_id,
            lease_fd,
        } => {
            let fd = transport::duplicate(*lease_fd)?;
            let pool = Pool::open(&args.repo)?;
            serde_json::to_value(pool.release_fd(slot, *lease_id, fd.as_fd())?)?
        }
    };
    Ok((protocol::success(args.request_id.as_deref(), result), false))
}
