use crate::{Error, Result};
use std::{
    ffi::OsStr,
    io::{Read, Write},
    os::{fd::RawFd, unix::process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Diagnostic capture bound for stderr and for ordinary maintenance stdout.
const OUTPUT_LIMIT: usize = 1024 * 1024;
/// Per-stream error budget, including JSON escaping, independent of plumbing output.
const DIAGNOSTIC_LIMIT: usize = 16 * 1024;
/// Stdout bound for snapshot plumbing, which streams trees and patches.
const SNAPSHOT_OUTPUT_LIMIT: usize = 128 * 1024 * 1024;
mod index_audit;
pub(crate) use index_audit::IndexAudit;

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
    /// Audit index headers while discarding the unbounded tracked-path inventory.
    AuditIndex,
}

/// Optional knobs for one Git subprocess. Defaults describe isolated maintenance.
#[derive(Default)]
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
    /// Fail on diagnostics even after exit zero, for ref inventories where Git
    /// may warn and omit an unreadable or broken reference.
    pub reject_stderr: bool,
    /// Validate while an update-ref transaction is prepared and its refs locked.
    /// `stdin` supplies only the prefix through `prepare`; success sends commit,
    /// failure closes stdin to abort. The callback must honor the remaining budget.
    pub prepared: Option<Box<dyn FnOnce(Duration) -> Result<()> + 'a>>,
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
    pub fn audit_index(&self, cwd: &Path, mut invocation: Invocation<'_>) -> Result<IndexAudit> {
        invocation.output = OutputMode::AuditIndex;
        let args = ["ls-files", "-v", "--stage", "-z"]
            .into_iter()
            .map(OsStr::new);
        let audit = self.spawn(cwd, args, invocation)?.audit;
        if audit.malformed {
            return Err(Error::new(
                crate::ErrorCode::GitError,
                "Malformed Git index inventory",
            ));
        }
        Ok(audit)
    }
    fn spawn<'a>(
        &self,
        cwd: &Path,
        args: impl IntoIterator<Item = &'a OsStr>,
        mut invocation: Invocation<'_>,
    ) -> Result<Capture> {
        tracing::trace!(event = "git.started", "Starting Git operation");
        let output_mode = invocation.output;
        let scanning = output_mode != OutputMode::Bounded;
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
        let prepared_transaction = invocation.prepared.is_some();
        let mut transaction_input = None;
        if prepared_transaction {
            transaction_input = Some(invocation.stdin.take().ok_or_else(|| {
                Error::new(crate::ErrorCode::GitError, "Missing ref transaction input")
            })?);
            cmd.stdin(Stdio::piped());
        } else if let Some(input) = invocation.stdin.take() {
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
        let start = Instant::now();
        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout is piped above");
        let stderr = child.stderr.take().expect("stderr is piped above");
        let limit = invocation.stdout_limit.unwrap_or(OUTPUT_LIMIT);
        let (prepare_sender, prepare_receiver) = match prepared_transaction.then(mpsc::channel) {
            Some((sender, receiver)) => (Some(sender), Some(receiver)),
            None => (None, None),
        };
        let out_thread = thread::spawn(move || capture(stdout, output_mode, limit, prepare_sender));
        let err_thread =
            thread::spawn(move || capture(stderr, OutputMode::Bounded, OUTPUT_LIMIT, None));
        let (mut commit_sender, input_thread) = if let Some(mut input) = transaction_input {
            let (sender, receiver) = mpsc::channel();
            // Only prepared transactions request piped stdin above.
            let mut stdin = child
                .stdin
                .take()
                .expect("transaction stdin is piped above");
            let writer = thread::spawn(move || -> std::io::Result<()> {
                std::io::copy(&mut input, &mut stdin)?;
                if receiver.recv().is_ok() {
                    stdin.write_all(b"commit\n")?;
                }
                // EOF aborts a prepared transaction when validation failed.
                Ok(())
            });
            (Some(sender), Some(writer))
        } else {
            (None, None)
        };
        let mut operation_error = None;
        let mut validation_panic = None;
        let mut committed = false;
        // Observe exit without reaping: the unreaped leader reserves its PID and
        // process-group ID, so the group kill below can never reach a process
        // that reused that identity.
        let timed_out =
            loop {
                match crate::platform::process::exited(&child) {
                    Ok(true) => break false,
                    Ok(false) => (),
                    Err(e) => {
                        operation_error = Some(e);
                        break false;
                    }
                }
                if start.elapsed() >= self.timeout {
                    break true;
                }
                if invocation.prepared.is_some() {
                    match prepare_receiver
                        .as_ref()
                        .expect("prepared transaction has a receiver")
                        .try_recv()
                    {
                        Ok(true) => {
                            // The callback is taken once, only after the exact
                            // protocol prefix has been drained by the stdout reader.
                            let validate = invocation.prepared.take().expect("checked above");
                            let validation =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    validate(self.timeout.saturating_sub(start.elapsed()))
                                }));
                            match validation {
                                Ok(Ok(())) if start.elapsed() < self.timeout => {
                                    if let Some(sender) = commit_sender.take() {
                                        match sender.send(()) {
                                            Ok(()) => committed = true,
                                            Err(error) => {
                                                operation_error = Some(Error::new(
                                                crate::ErrorCode::GitError,
                                                "Ref transaction stdin closed before commit",
                                            ).with_source(error));
                                            }
                                        }
                                    }
                                }
                                Ok(Ok(())) => break true,
                                Ok(Err(error)) => operation_error = Some(error),
                                Err(payload) => validation_panic = Some(payload),
                            }
                            // Disconnecting tells the writer to close stdin on error.
                            drop(commit_sender.take());
                        }
                        Ok(false) | Err(mpsc::TryRecvError::Disconnected) => {
                            operation_error = Some(Error::new(
                                crate::ErrorCode::GitError,
                                "Invalid ref transaction preparation response",
                            ));
                            invocation.prepared.take();
                            drop(commit_sender.take());
                        }
                        Err(mpsc::TryRecvError::Empty) => (),
                    }
                }
                thread::sleep(Duration::from_millis(2));
            };
        drop(commit_sender);
        // Stop a timed-out mutation, and close pipe-owning descendants too;
        // background processes are unsupported in maintenance.
        // SAFETY: the leader is still unreaped, so this is the dedicated group established above.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let status = child.wait()?;
        let status = (!timed_out).then_some(status);
        let out = out_thread
            .join()
            .map_err(|_| Error::new(crate::ErrorCode::IoError, "stdout reader panicked"))??;
        let err = err_thread
            .join()
            .map_err(|_| Error::new(crate::ErrorCode::IoError, "stderr reader panicked"))??;
        if let Some(input_thread) = input_thread {
            let result = input_thread.join().map_err(|_| {
                Error::new(
                    crate::ErrorCode::IoError,
                    "transaction stdin writer panicked",
                )
            })?;
            if operation_error.is_none() && !timed_out {
                if let Err(error) = result {
                    operation_error = Some(
                        Error::new(
                            crate::ErrorCode::GitError,
                            "Writing ref transaction input failed",
                        )
                        .with_source(error),
                    );
                }
            }
        }
        if let Some(payload) = validation_panic {
            // Reap Git and join its pipe threads before resuming an internal
            // callback panic; unwinding must not strand a prepared ref lock.
            std::panic::resume_unwind(payload);
        }
        if operation_error.is_none()
            && prepared_transaction
            && !timed_out
            && (!committed || out.bytes != b"start: ok\nprepare: ok\ncommit: ok\n")
        {
            operation_error = Some(Error::new(
                crate::ErrorCode::GitError,
                "Incomplete ref transaction response",
            ));
        }
        tracing::debug!(
            event = "git.completed",
            elapsed_ms = start.elapsed().as_millis() as u64,
            exit_code = status.and_then(|s| s.code()),
            timed_out = status.is_none(),
            "Git operation finished"
        );
        if operation_error.is_some()
            || !status.is_some_and(|s| s.success())
            || (out.large && !scanning)
            || err.large
            || (invocation.reject_stderr && !err.bytes.is_empty())
        {
            return Err(operation_error
                .unwrap_or_else(|| {
                    Error::new(
                        crate::ErrorCode::GitError,
                        if status.is_none() {
                            "Git operation timed out"
                        } else if (out.large && !scanning) || err.large {
                            "Git operation exceeded its output limit"
                        } else if status.is_some_and(|status| status.success()) {
                            "Git operation returned warnings; its inventory may be incomplete"
                        } else {
                            "Git operation failed"
                        },
                    )
                })
                .with_details(|d| {
                    // Keep a failed validation subprocess's diagnostics intact.
                    if d.git.is_none() {
                        let (stdout, stdout_truncated) = diagnostic(&out.bytes);
                        let (stderr, stderr_truncated) = diagnostic(&err.bytes);
                        d.git = Some(crate::GitFailure {
                            exit_code: status.and_then(|s| s.code()),
                            stdout,
                            stderr,
                            output_truncated: out.large
                                || err.large
                                || stdout_truncated
                                || stderr_truncated,
                        });
                    }
                }));
        }
        Ok(out)
    }
}
use std::os::unix::ffi::OsStrExt;

fn diagnostic(bytes: &[u8]) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes);
    let mut result = String::new();
    let mut encoded = 0;
    for character in text.chars() {
        let size = match character {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{08}' | '\u{0c}' => 2,
            '\u{00}'..='\u{1f}' => 6,
            _ => character.len_utf8(),
        };
        if encoded + size > DIAGNOSTIC_LIMIT {
            return (result, true);
        }
        encoded += size;
        result.push(character);
    }
    (result, false)
}

struct Capture {
    bytes: Vec<u8>,
    large: bool,
    gitlink: bool,
    audit: IndexAudit,
}
fn capture(
    mut reader: impl Read,
    mode: OutputMode,
    limit: usize,
    mut prepare: Option<mpsc::Sender<bool>>,
) -> std::io::Result<Capture> {
    let mut out = Vec::new();
    let mut large = false;
    let mut buffer = [0; 8192];
    let mut prefix = [0; 7];
    let mut prefix_len = 0;
    let mut gitlink = false;
    let mut audit = index_audit::Scanner::default();
    let mut prepared_bytes = 0;
    const PREPARED: &[u8] = b"start: ok\nprepare: ok\n";
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        let keep = n.min(limit.saturating_sub(out.len()));
        out.extend_from_slice(&buffer[..keep]);
        large |= keep != n;
        if prepare.is_some() {
            for &byte in &buffer[..n] {
                let matches = byte == PREPARED[prepared_bytes];
                prepared_bytes += 1;
                if !matches || prepared_bytes == PREPARED.len() {
                    if let Some(sender) = prepare.take() {
                        let _ = sender.send(matches);
                    }
                    break;
                }
            }
        }
        if mode == OutputMode::AuditIndex {
            audit.feed(&buffer[..n]);
        }
        if mode == OutputMode::ScanGitlinks {
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
        audit: audit.finish(),
    })
}

#[cfg(test)]
mod diagnostic_tests {
    use super::*;
    #[test]
    fn diagnostics_bound_json_escaping_and_lossy_unicode_without_cutting_characters() {
        for input in [
            vec![b'x'; 100_000],
            vec![0; 100_000],
            vec![0xff; 100_000],
            "é🦀\"\n".repeat(10_000).into_bytes(),
        ] {
            let (text, truncated) = diagnostic(&input);
            assert!(truncated);
            assert!(!text.is_empty());
            assert!(serde_json::to_vec(&text).unwrap().len() <= DIAGNOSTIC_LIMIT + 2);
        }
        assert_eq!(
            diagnostic(b"short warning"),
            ("short warning".into(), false)
        );
    }
}
