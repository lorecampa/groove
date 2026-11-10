#![allow(dead_code)]
use std::{
    ffi::OsStr,
    fs,
    io::{self, Read},
    os::unix::{
        fs::FileExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

pub struct Repo {
    pub temp: TempDir,
    pub path: PathBuf,
}
impl Repo {
    pub fn new() -> Self {
        Self::format("sha1")
    }
    pub fn format(format: &str) -> Self {
        // Darwin's default TMPDIR can leave too little room for Unix socket
        // names. Keep process fixtures short without changing global env vars.
        let temp = if let Some(root) = std::env::var_os("GROOVE_TEST_ROOT") {
            tempfile::tempdir_in(root)
        } else if cfg!(target_os = "macos") {
            tempfile::tempdir_in("/private/tmp")
        } else {
            tempfile::tempdir()
        }
        .unwrap();
        let path = temp.path().join("repo with spaces è");
        fs::create_dir(&path).unwrap();
        let repo = Self { temp, path };
        repo.git(&["init", "-q", &format!("--object-format={format}")]);
        repo.git(&["config", "user.name", "Groove Test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        fs::write(repo.path.join("tracked.txt"), "baseline\n").unwrap();
        fs::write(repo.path.join(".gitignore"), "ignored/\n").unwrap();
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "baseline"]);
        repo
    }
    pub fn git(&self, args: &[&str]) -> String {
        git(&self.path, args)
    }
    pub fn state(&self) -> PathBuf {
        self.path.join(".git/groove")
    }
    pub fn slot(&self, n: usize) -> PathBuf {
        self.state().join(format!("worktrees/wt-{n:04}"))
    }
}
pub fn command(path: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    for (key, _) in std::env::vars_os() {
        use std::os::unix::ffi::OsStrExt;
        if key.as_bytes().starts_with(b"GIT_") {
            c.env_remove(key);
        }
    }
    c.current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}
pub fn git(path: &Path, args: &[&str]) -> String {
    let output = command(path, "git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .into()
}
pub fn assert_success(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

pub const PROCESS_TIMEOUT: Duration = Duration::from_secs(30);

/// A disconnected stream fails writes with EPIPE on both supported platforms.
/// Rust's stdout deliberately suppresses EBADF, so a read-only file cannot
/// exercise a failed standard-output response.
pub fn failed_output() -> std::os::fd::OwnedFd {
    let (writer, reader) = UnixStream::pair().unwrap();
    drop(reader);
    writer.into()
}

/// Capture to files so a full pipe or inherited pipe writer cannot stall the harness.
/// The direct child remains owned until it is killed/reaped, including during unwind.
pub struct TestChild {
    child: std::process::Child,
    stdout: fs::File,
    stderr: fs::File,
}
impl TestChild {
    pub fn spawn(command: &mut Command) -> Self {
        let stdout = tempfile::tempfile().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        let child = command
            .stdout(Stdio::from(stdout.try_clone().unwrap()))
            .stderr(Stdio::from(stderr.try_clone().unwrap()))
            .spawn()
            .unwrap();
        Self {
            child,
            stdout,
            stderr,
        }
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        self.wait_timeout(PROCESS_TIMEOUT)
    }
    pub fn wait_timeout(&mut self, timeout: Duration) -> io::Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(self.failure(io::ErrorKind::TimedOut, "waiting for child exit"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        let status = self.wait()?;
        Ok(Output {
            status,
            stdout: captured(&self.stdout)?,
            stderr: captured(&self.stderr)?,
        })
    }
    fn failure(&self, kind: io::ErrorKind, context: &str) -> io::Error {
        io::Error::new(
            kind,
            format!(
                "{context}; child={} stdout={} stderr={}",
                self.child.id(),
                String::from_utf8_lossy(&captured(&self.stdout).unwrap_or_default()),
                String::from_utf8_lossy(&captured(&self.stderr).unwrap_or_default()),
            ),
        )
    }
    pub fn checkpoint(
        &mut self,
        listener: &UnixListener,
        name: &str,
        timeout: Duration,
    ) -> io::Result<UnixStream> {
        let deadline = Instant::now() + timeout;
        let stream = accept_child(listener, std::slice::from_mut(self), timeout)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        self.read_checkpoint(stream, name, remaining)
    }
    pub fn read_checkpoint(
        &self,
        mut stream: UnixStream,
        name: &str,
        timeout: Duration,
    ) -> io::Result<UnixStream> {
        let deadline = Instant::now() + timeout;
        let mut bytes = vec![0; name.len()];
        let mut received = 0;
        while received < bytes.len() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(self.failure(io::ErrorKind::TimedOut, name));
            }
            stream.set_read_timeout(Some(remaining))?;
            let count = stream.read(&mut bytes[received..]).map_err(|e| {
                let kind = if e.kind() == io::ErrorKind::WouldBlock {
                    io::ErrorKind::TimedOut
                } else {
                    e.kind()
                };
                self.failure(kind, &format!("reading checkpoint {name}: {e}"))
            })?;
            if count == 0 {
                return Err(self.failure(
                    io::ErrorKind::UnexpectedEof,
                    &format!("incomplete checkpoint {name}"),
                ));
            }
            received += count;
        }
        if bytes != name.as_bytes() {
            return Err(self.failure(
                io::ErrorKind::InvalidData,
                &format!("unexpected checkpoint {bytes:?}; expected {name}"),
            ));
        }
        Ok(stream)
    }
}

fn captured(file: &fs::File) -> io::Result<Vec<u8>> {
    // Bounded diagnostics, with independent offsets even while a child is writing.
    let length = file.metadata()?.len().min(1024 * 1024) as usize;
    let mut bytes = vec![0; length];
    file.read_exact_at(&mut bytes, 0)?;
    Ok(bytes)
}

/// Accept a readiness connection, detecting early exits and bounding both socket directions.
pub fn accept_child(
    listener: &UnixListener,
    children: &mut [TestChild],
    timeout: Duration,
) -> io::Result<UnixStream> {
    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                // Darwin inherits O_NONBLOCK from the listener; Linux does not.
                // Handshake readers use blocking I/O with explicit deadlines.
                stream.set_nonblocking(false)?;
                stream.set_read_timeout(Some(timeout))?;
                stream.set_write_timeout(Some(timeout))?;
                return Ok(stream);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
            Err(e) => return Err(e),
        }
        for child in children.iter_mut() {
            if let Some(status) = child.try_wait()? {
                return Err(child.failure(
                    io::ErrorKind::UnexpectedEof,
                    &format!("child exited {status} before readiness"),
                ));
            }
        }
        if Instant::now() >= deadline {
            let diagnostics: Vec<_> = children
                .iter()
                .map(|child| {
                    child
                        .failure(io::ErrorKind::TimedOut, "waiting for readiness")
                        .to_string()
                })
                .collect();
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                diagnostics.join("\n"),
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Fixtures whose assertions require no unrelated descriptor owners run in separate
/// processes, concurrently. Explicit ownership/concurrency scenarios still arrange
/// their own participating children and are never retried by this helper.
pub fn in_subprocess(name: &str) -> bool {
    if std::env::var("GROOVE_ISOLATED_TEST").as_deref() == Ok(name) {
        return true;
    }
    let output = TestChild::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", name, "--nocapture"])
            .env("GROOVE_ISOLATED_TEST", name),
    )
    .wait_with_output()
    .unwrap();
    assert!(
        output.status.success(),
        "{name}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}
impl std::ops::Deref for TestChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.child
    }
}
impl std::ops::DerefMut for TestChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Path of the groove binary under test; `GROOVE_CLI_BINARY` overrides the built one.
fn groove_binary() -> std::ffi::OsString {
    std::env::var_os("GROOVE_CLI_BINARY").unwrap_or_else(|| {
        match option_env!("CARGO_BIN_EXE_groove") {
            Some(path) => path.into(),
            None => panic!("only groove-cli tests build the groove binary"),
        }
    })
}

/// The groove binary under test, emitting JSON.
pub fn groove() -> Command {
    let mut command = Command::new(groove_binary());
    command.env("GROOVE_OUTPUT", "json");
    command
}

/// [`groove`] with the controlled Git environment of [`command`], run in `path`.
pub fn groove_in(path: &Path) -> Command {
    let mut command = command(path, groove_binary());
    command.env("GROOVE_OUTPUT", "json");
    command
}
