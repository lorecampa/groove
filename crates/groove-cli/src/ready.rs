//! Internal short-lived worker transport. No public maintenance command or daemon.
use crate::args::{Args, Command, PoolCommand};
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
        Err(error) => {
            // Keep successful background passes log-free; retain failures even
            // when opening the pool failed before run_ready could report them.
            let terminal = crate::terminal::Terminal::new(crate::terminal::Verbosity {
                verbose: 0,
                quiet: true,
            });
            let settings = crate::preferences::load().map_or_else(
                |_| crate::preferences::Logging::disabled(),
                |settings| settings.logging,
            );
            let _logging = crate::logging::init(terminal, settings, None, "ready-worker");
            tracing::error!(event = "ready.failed", file_only = true, code = %error.code, %error, "Background checkout preparation failed; a later pool operation will retry");
            1
        }
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

pub fn trigger(args: &Args, selected_pool: Option<&groove_core::PoolName>) {
    if !matches!(
        args.command,
        Command::Pool(PoolCommand::Init { .. })
            | Command::Acquire { .. }
            | Command::Release { .. }
            | Command::Pool(PoolCommand::Set { .. })
    ) {
        return;
    }
    restart(args, selected_pool);
}

/// Resume preparation after a refused destroy cancelled the previous worker.
pub fn restart(args: &Args, selected_pool: Option<&groove_core::PoolName>) {
    if let Err(error) = spawn(args, selected_pool) {
        tracing::warn!(%error, "Background checkout preparation could not start; a later pool operation will retry");
    }
}
fn spawn(args: &Args, selected_pool: Option<&groove_core::PoolName>) -> Result<()> {
    let name = selected_pool.cloned().unwrap_or_default();
    let pool = Pool::open_named_with_timeout(
        &args.repo,
        &name,
        Duration::from_millis(args.git_timeout_ms),
    )?;
    let Some(generation) = pool.ready_generation()? else {
        return Ok(());
    };
    // Snapshot inherited descriptors before spawning. Rust's own descriptors,
    // including any subsequently opened logging/spawn pipes, are CLOEXEC.
    // Enumerate instead of trusting RLIMIT_NOFILE: a caller may have lowered
    // its limit after opening a high-numbered descriptor.
    let directory = if cfg!(target_os = "linux") {
        "/proc/self/fd"
    } else {
        "/dev/fd"
    };
    let mut inherited = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        if let Some(fd) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
            .filter(|fd| *fd > 2)
        {
            inherited.push(fd);
        }
    }
    let settings = pool.settings()?;
    let foreground = if args.git_timeout_explicit {
        args.git_timeout_ms
    } else {
        settings.git_timeout.duration().as_millis() as u64
    };
    let worker_timeout_ms = settings
        .ready
        .and_then(|r| r.ready_git_timeout)
        .map_or(foreground, |t| t.duration().as_millis() as u64);
    let mut command = Process::new(std::env::current_exe()?);
    command
        .arg("--internal-ready-worker")
        .arg(pool.common_git_dir())
        .arg(name.as_str())
        .arg(generation.to_string())
        .arg(worker_timeout_ms.to_string())
        .env("GROOVE_INTERNAL_READY_WORKER", "1")
        // Foreground fault checkpoints do not also pause a detached successor.
        // Worker crash tests launch the internal entry point with their own hooks.
        .env_remove("GROOVE_TEST_CHECKPOINT")
        .env_remove("GROOVE_TEST_SOCKET")
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid/fcntl are async-signal-safe; the descriptor list was allocated
    // before fork. Marking CLOEXEC preserves Rust's spawn-error pipe until exec.
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for &fd in &inherited {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() == Some(libc::EBADF) {
                        continue;
                    }
                    return Err(error);
                }
                if libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
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
