use crate::{Error, Result};
use std::{
    ffi::OsStr,
    io::Read,
    os::{fd::RawFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Diagnostic capture bound for stderr and for ordinary maintenance stdout.
const OUTPUT_LIMIT: usize = 1024 * 1024;
/// Stdout bound for snapshot plumbing, which streams trees and patches.
const SNAPSHOT_OUTPUT_LIMIT: usize = 128 * 1024 * 1024;

/// Which Git configuration files a subprocess may read.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum ConfigScope {
    /// Ignore system and global configuration (pool maintenance).
    #[default]
    Isolated,
    /// Honor the caller's system/global configuration, e.g. ignore and attribute
    /// rules that decide what a snapshot captures.
    User,
}

/// How stdout is consumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum OutputMode {
    /// Keep stdout, failing if it exceeds the bound.
    #[default]
    Bounded,
    /// Stream NUL-delimited `ls-tree` records, retaining only whether a gitlink exists.
    ScanGitlinks,
}

/// Optional knobs for one Git subprocess. Defaults describe isolated maintenance.
#[derive(Debug, Default)]
pub(crate) struct Invocation<'a> {
    /// Descriptors the child (and its children) must inherit, e.g. ownership locks.
    pub inherit: &'a [RawFd],
    /// Additional environment, applied after the controlled baseline.
    pub env: &'a [(&'a str, &'a OsStr)],
    /// Standard input; defaults to `/dev/null`.
    pub stdin: Option<std::fs::File>,
    /// Configuration files Git may read.
    pub config: ConfigScope,
    /// Stdout capture bound in bytes; defaults to [`OUTPUT_LIMIT`].
    pub stdout_limit: Option<usize>,
    /// How stdout is consumed.
    pub output: OutputMode,
}
impl Invocation<'_> {
    /// Snapshot plumbing: user configuration and a larger stdout bound.
    pub fn snapshot() -> Self {
        Self {
            config: ConfigScope::User,
            stdout_limit: Some(SNAPSHOT_OUTPUT_LIMIT),
            ..Self::default()
        }
    }
}

pub(crate) struct Git {
    timeout: Duration,
}
impl Git {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
    /// Run isolated maintenance Git, letting `fds` be inherited by the child.
    pub fn run(&self, cwd: &Path, args: &[&str], fds: &[RawFd]) -> Result<Vec<u8>> {
        self.run_os(cwd, args.iter().map(OsStr::new), fds)
    }
    pub fn run_os<'a>(
        &self,
        cwd: &Path,
        args: impl IntoIterator<Item = &'a OsStr>,
        fds: &[RawFd],
    ) -> Result<Vec<u8>> {
        let invocation = Invocation {
            inherit: fds,
            ..Invocation::default()
        };
        Ok(self.spawn(cwd, args, invocation)?.bytes)
    }
    /// Run Git with explicit [`Invocation`] options.
    pub fn run_with(
        &self,
        cwd: &Path,
        args: &[&str],
        invocation: Invocation<'_>,
    ) -> Result<Vec<u8>> {
        Ok(self
            .spawn(cwd, args.iter().map(OsStr::new), invocation)?
            .bytes)
    }
    /// Scan NUL-delimited ls-tree records while retaining only bounded diagnostics.
    pub fn has_gitlinks(&self, cwd: &Path, oid: &str) -> Result<bool> {
        let invocation = Invocation {
            output: OutputMode::ScanGitlinks,
            ..Invocation::default()
        };
        let args = ["ls-tree", "-r", "-z", oid].into_iter().map(OsStr::new);
        Ok(self.spawn(cwd, args, invocation)?.gitlink)
    }
    fn spawn<'a>(
        &self,
        cwd: &Path,
        args: impl IntoIterator<Item = &'a OsStr>,
        invocation: Invocation<'_>,
    ) -> Result<Capture> {
        tracing::trace!(event = "git.started", "Starting Git operation");
        let scan_tree = invocation.output == OutputMode::ScanGitlinks;
        let mut cmd = Command::new("git");
        cmd.current_dir(cwd)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "core.quotePath=false",
            ])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        for (key, _) in std::env::vars_os() {
            if key.as_bytes().starts_with(b"GIT_") {
                cmd.env_remove(key);
            }
        }
        cmd.env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("LC_ALL", "C");
        match invocation.config {
            ConfigScope::Isolated => {
                cmd.env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null");
            }
            ConfigScope::User => {
                for key in [
                    "GIT_CONFIG_GLOBAL",
                    "GIT_CONFIG_SYSTEM",
                    "GIT_CONFIG_NOSYSTEM",
                ] {
                    if let Some(value) = std::env::var_os(key) {
                        cmd.env(key, value);
                    }
                }
            }
        }
        for (key, value) in invocation.env {
            cmd.env(key, value);
        }
        if let Some(input) = invocation.stdin {
            cmd.stdin(input);
        }
        let inherit = invocation.inherit.to_vec();
        // SAFETY: the child closure performs only async-signal-safe fcntl syscalls;
        // all allocations are performed before fork and descriptors stay owned by the caller.
        unsafe {
            cmd.pre_exec(move || {
                for fd in &inherit {
                    let flags = libc::fcntl(*fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(*fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped above");
        let stderr = child.stderr.take().expect("stderr is piped above");
        let limit = invocation.stdout_limit.unwrap_or(OUTPUT_LIMIT);
        let out_thread = thread::spawn(move || capture(stdout, scan_tree, limit));
        let err_thread = thread::spawn(move || capture(stderr, false, OUTPUT_LIMIT));
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if start.elapsed() >= self.timeout {
                // SAFETY: the child was spawned into its own process group; negative pid targets it.
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                child.wait()?;
                break None;
            }
            thread::sleep(Duration::from_millis(2));
        };
        // Close pipe-owning descendants too; background processes are unsupported in maintenance.
        // SAFETY: this is the dedicated process group established above.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let out = out_thread
            .join()
            .map_err(|_| Error::new(crate::ErrorCode::IoError, "stdout reader panicked"))??;
        let err = err_thread
            .join()
            .map_err(|_| Error::new(crate::ErrorCode::IoError, "stderr reader panicked"))??;
        tracing::debug!(
            event = "git.completed",
            elapsed_ms = start.elapsed().as_millis() as u64,
            exit_code = status.and_then(|s| s.code()),
            timed_out = status.is_none(),
            "Git operation finished"
        );
        if !status.is_some_and(|s| s.success()) || (out.large && !scan_tree) || err.large {
            return Err(Error::new(
                crate::ErrorCode::GitError,
                if status.is_none() {
                    "Git operation timed out"
                } else {
                    "Git operation failed or exceeded its output limit"
                },
            )
            .with_details(|d| {
                d.git = Some(crate::GitFailure {
                    exit_code: status.and_then(|s| s.code()),
                    stdout: String::from_utf8_lossy(&out.bytes).into_owned(),
                    stderr: String::from_utf8_lossy(&err.bytes).into_owned(),
                    output_truncated: out.large || err.large,
                })
            }));
        }
        Ok(out)
    }
}
use std::os::unix::ffi::OsStrExt;
struct Capture {
    bytes: Vec<u8>,
    large: bool,
    gitlink: bool,
}
fn capture(mut reader: impl Read, scan_tree: bool, limit: usize) -> std::io::Result<Capture> {
    let mut out = Vec::new();
    let mut large = false;
    let mut buffer = [0; 8192];
    let mut prefix = [0; 7];
    let mut prefix_len = 0;
    let mut gitlink = false;
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let keep = n.min(limit.saturating_sub(out.len()));
        out.extend_from_slice(&buffer[..keep]);
        large |= keep != n;
        if scan_tree {
            for &byte in &buffer[..n] {
                if byte == 0 {
                    prefix_len = 0;
                } else if prefix_len < prefix.len() {
                    prefix[prefix_len] = byte;
                    prefix_len += 1;
                    gitlink |= prefix_len == prefix.len() && &prefix == b"160000 ";
                }
            }
        }
    }
    Ok(Capture {
        bytes: out,
        large,
        gitlink,
    })
}
