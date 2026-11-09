use crate::{Error, Result};
use serde_json::json;
use std::{
    ffi::OsStr,
    io::Read,
    os::{fd::RawFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
pub(crate) struct Git {
    timeout: Duration,
}
impl Git {
    pub fn new(timeout: Duration) -> Self {
        Self { timeout }
    }
    pub fn run(&self, cwd: &Path, args: &[&str], fds: &[RawFd]) -> Result<Vec<u8>> {
        self.run_os(cwd, args.iter().map(OsStr::new), fds)
    }
    pub fn run_os<'a>(
        &self,
        cwd: &Path,
        args: impl IntoIterator<Item = &'a OsStr>,
        fds: &[RawFd],
    ) -> Result<Vec<u8>> {
        Ok(self.run_output(cwd, args, fds, false, &[], None)?.bytes)
    }
    /// Scan NUL-delimited ls-tree records while retaining only bounded diagnostics.
    pub fn has_gitlinks(&self, cwd: &Path, oid: &str) -> Result<bool> {
        Ok(self
            .run_output(
                cwd,
                ["ls-tree", "-r", "-z", oid].into_iter().map(OsStr::new),
                &[],
                true,
                &[],
                None,
            )?
            .gitlink)
    }
    pub(crate) fn snapshot(
        &self,
        cwd: &Path,
        args: &[&str],
        env: &[(&str, &OsStr)],
        input: Option<std::fs::File>,
        fds: &[RawFd],
    ) -> Result<Vec<u8>> {
        Ok(self
            .run_output(cwd, args.iter().map(OsStr::new), fds, false, env, input)?
            .bytes)
    }
    fn run_output<'a>(
        &self,
        cwd: &Path,
        args: impl IntoIterator<Item = &'a OsStr>,
        fds: &[RawFd],
        scan_tree: bool,
        env: &[(&str, &OsStr)],
        input: Option<std::fs::File>,
    ) -> Result<Capture> {
        tracing::trace!(event = "git.started", "Starting Git operation");
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
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("LC_ALL", "C");
        if !env.is_empty() {
            // Snapshots must honor user/system ignore and attribute configuration.
            // Pool maintenance retains its existing isolated configuration policy.
            for key in [
                "GIT_CONFIG_GLOBAL",
                "GIT_CONFIG_SYSTEM",
                "GIT_CONFIG_NOSYSTEM",
            ] {
                match std::env::var_os(key) {
                    Some(value) => {
                        cmd.env(key, value);
                    }
                    None => {
                        cmd.env_remove(key);
                    }
                }
            }
        }
        for (key, value) in env {
            cmd.env(key, value);
        }
        if let Some(input) = input {
            cmd.stdin(input);
        }
        let inherit = fds.to_vec();
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
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let limit = if env.is_empty() {
            OUTPUT_LIMIT
        } else {
            128 * 1024 * 1024
        };
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
            return Err(Error::new(crate::ErrorCode::GitError, if status.is_none() { "Git operation timed out" } else { "Git operation failed or exceeded its output limit" })
                .details(json!({"exit_code": status.and_then(|s|s.code()), "stdout": String::from_utf8_lossy(&out.bytes), "stderr": String::from_utf8_lossy(&err.bytes), "output_truncated": out.large || err.large})));
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
