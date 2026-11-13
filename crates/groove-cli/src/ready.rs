//! Internal short-lived worker transport. No public maintenance command or daemon.
use crate::args::{Args, Command, ConfigCommand};
use groove_core::{Pool, Result};
use std::{
    os::unix::process::CommandExt,
    process::{Command as Process, Stdio},
    time::Duration,
};

pub fn internal(raw: &[std::ffi::OsString]) -> Option<u8> {
    if std::env::var_os("GROOVE_INTERNAL_READY_WORKER").as_deref()
        != Some(std::ffi::OsStr::new("1"))
    {
        return None;
    }
    if raw
        .get(1)
        .is_none_or(|arg| arg != "--internal-ready-worker")
    {
        return None;
    }
    Some(match worker(raw) {
        Ok(()) => 0,
        Err(_) => 1,
    })
}
fn worker(raw: &[std::ffi::OsString]) -> Result<()> {
    if raw.len() != 6 {
        return Err(groove_core::Error::new(
            groove_core::ErrorCode::InvalidArgument,
            "Invalid internal worker arguments",
        ));
    }
    let invalid = || {
        groove_core::Error::new(
            groove_core::ErrorCode::InvalidArgument,
            "Invalid internal worker arguments",
        )
    };
    let name = raw[3].to_str().ok_or_else(invalid)?.parse()?;
    let generation = raw[4].to_str().ok_or_else(invalid)?.parse()?;
    let timeout: u64 = raw[5]
        .to_str()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    let pool = Pool::open_named_with_timeout(&raw[2], &name, Duration::from_millis(timeout))?;
    if pool.ready_generation()? != Some(generation) {
        return Ok(());
    }
    pool.request_ready()?;
    pool.run_ready(generation)
}

pub fn trigger(args: &Args) {
    if !matches!(
        args.command,
        Command::Init { .. }
            | Command::Acquire { .. }
            | Command::Release { .. }
            | Command::Config(ConfigCommand::Set { .. })
    ) {
        return;
    }
    if let Err(error) = spawn(args) {
        tracing::warn!(%error, "Background checkout preparation could not start; a later pool operation will retry");
    }
}
fn spawn(args: &Args) -> Result<()> {
    let name = args.pool.clone().unwrap_or_default();
    let pool = Pool::open_named_with_timeout(
        &args.repo,
        &name,
        Duration::from_millis(args.git_timeout_ms),
    )?;
    let Some(generation) = pool.ready_generation()? else {
        return Ok(());
    };
    let mut command = Process::new(std::env::current_exe()?);
    command
        .arg("--internal-ready-worker")
        .arg(pool.common_git_dir())
        .arg(name.as_str())
        .arg(generation.to_string())
        .arg(args.git_timeout_ms.to_string())
        .env("GROOVE_INTERNAL_READY_WORKER", "1")
        // Foreground fault checkpoints do not also pause a detached successor.
        // Worker crash tests launch the internal entry point with their own hooks.
        .env_remove("GROOVE_TEST_CHECKPOINT")
        .env_remove("GROOVE_TEST_SOCKET")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and the child has not created threads.
    // No caller descriptors are deliberately inherited; Groove locks are CLOEXEC.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    // Reap while the CLI lives. If it exits first the detached child is adopted
    // by the OS reaper; no output pipe can keep the caller waiting.
    let _ = std::thread::Builder::new()
        .name("ready-reaper".into())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}
