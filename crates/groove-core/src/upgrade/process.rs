//! Bounded noninteractive installer supervision. No shell evaluation.
use crate::{Error, ErrorCode, Result};
use std::{
    fs::File,
    io::Read,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

pub(super) const LIMIT: usize = 8 * 1024 * 1024;

pub(super) struct Running {
    child: Child,
    stdout: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    stderr: mpsc::Receiver<std::io::Result<Vec<u8>>>,
    started: Instant,
    timeout: Duration,
    reaped: bool,
}
fn capture(
    mut input: impl Read + Send + 'static,
) -> Result<mpsc::Receiver<std::io::Result<Vec<u8>>>> {
    let (send, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("groove-upgrade-output".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let mut chunk = [0; 8192];
            let result = loop {
                match input.read(&mut chunk) {
                    Ok(0) => break Ok(bytes),
                    Ok(n) => {
                        let keep = n.min((LIMIT + 1).saturating_sub(bytes.len()));
                        bytes.extend_from_slice(&chunk[..keep]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => break Err(error),
                }
            };
            let _ = send.send(result);
        })?;
    Ok(receive)
}
impl Running {
    pub fn start(
        program: &str,
        args: &[String],
        timeout: Duration,
        lock: Option<&File>,
    ) -> Result<Self> {
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GCM_INTERACTIVE", "Never")
            .env("CARGO_TERM_COLOR", "never")
            .env("CI", "1")
            .process_group(0);
        // Run away from repository configuration and never select a caller's Git repository.
        command.current_dir("/");
        for (key, _) in std::env::vars_os() {
            if key.to_str().is_some_and(|key| key.starts_with("GIT_")) {
                command.env_remove(key);
            }
        }
        command.env("GIT_TERMINAL_PROMPT", "0");
        if let Some(lock) = lock {
            let fd = lock.as_raw_fd();
            // SAFETY: pre_exec only calls async-signal-safe fcntl. The borrowed
            // lock outlives spawn. Its inherited descriptor keeps exclusion if
            // the coordinator exits while Cargo or a native manager is writing.
            unsafe {
                command.pre_exec(move || {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = command.spawn().map_err(|error| {
            Error::new(ErrorCode::IoError, format!("Cannot run {program}: {error}"))
                .with_source(error)
        })?;
        // Set up the guard before fallible reader-thread creation.
        let (_send, empty) = mpsc::channel();
        let (_send, empty_err) = mpsc::channel();
        let mut running = Self {
            child,
            stdout: empty,
            stderr: empty_err,
            started: Instant::now(),
            timeout,
            reaped: false,
        };
        running.stdout = capture(running.child.stdout.take().expect("piped stdout"))?;
        running.stderr = capture(running.child.stderr.take().expect("piped stderr"))?;
        Ok(running)
    }
    pub fn poll(&mut self) -> Result<Option<Vec<u8>>> {
        if !crate::platform::process::exited(&self.child)? {
            if self.started.elapsed() >= self.timeout {
                return Err(Error::new(
                    ErrorCode::IoError,
                    "Upgrade subprocess timed out; inspect its installation before retrying",
                ));
            }
            return Ok(None);
        }
        self.kill_group();
        let status = self.child.wait()?;
        self.reaped = true;
        let stdout = receive(&self.stdout)?;
        let stderr = receive(&self.stderr)?;
        if !status.success() {
            let diagnostic: String = String::from_utf8_lossy(&stderr)
                .chars()
                .take(4096)
                .collect();
            return Err(Error::new(
                ErrorCode::IoError,
                format!("Upgrade subprocess exited with {status}: {diagnostic}"),
            ));
        }
        if stdout.len() > LIMIT {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Upgrade subprocess output exceeded 8 MiB",
            ));
        }
        Ok(Some(stdout))
    }
    fn kill_group(&self) {
        if let Ok(pid) = i32::try_from(self.child.id()) {
            // SAFETY: the child is unreaped, reserving its PID/PGID. It was
            // placed in its own process group; this never signals the caller.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
    }
    pub fn wait(mut self) -> Result<Vec<u8>> {
        loop {
            if let Some(output) = self.poll()? {
                return Ok(output);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
fn receive(receiver: &mpsc::Receiver<std::io::Result<Vec<u8>>>) -> Result<Vec<u8>> {
    receiver
        .recv_timeout(Duration::from_millis(100))
        .map_err(|error| {
            Error::new(ErrorCode::IoError, "Upgrade output reader did not finish")
                .with_source(error)
        })?
        .map_err(Into::into)
}
impl Drop for Running {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill_group();
            let _ = self.child.wait();
        }
    }
}
pub(super) fn run(
    program: &str,
    args: &[String],
    timeout: Duration,
    lock: Option<&File>,
) -> Result<Vec<u8>> {
    Running::start(program, args, timeout, lock)?.wait()
}
pub(super) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).into()).collect()
}
pub(super) fn index_request(timeout: Duration) -> Result<Running> {
    let seconds = timeout.as_secs().max(1).to_string();
    Running::start(
        "curl",
        &strings(&[
            "--disable",
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--max-redirs",
            "3",
            "--max-filesize",
            "8388608",
            "--connect-timeout",
            &seconds,
            "--max-time",
            &seconds,
            "--user-agent",
            concat!("groove/", env!("CARGO_PKG_VERSION")),
            "https://index.crates.io/gr/oo/groove-cli",
        ]),
        timeout + Duration::from_secs(1),
        None,
    )
}
