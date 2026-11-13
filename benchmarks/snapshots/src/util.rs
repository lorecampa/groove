use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
extern "C" fn interrupt(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::Relaxed);
}
pub(crate) fn install_signals() -> Result<()> {
    // SAFETY: sigaction is initialized before use; the handler only stores an atomic flag.
    // Installation happens before benchmark worker threads are started.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = interrupt as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        for signal in [libc::SIGINT, libc::SIGTERM] {
            if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("Benchmark worker panicked")]
    WorkerPanic,
    #[error(transparent)]
    WorkerReceive(#[from] std::sync::mpsc::RecvTimeoutError),
    #[error(transparent)]
    WorkerSend(#[from] std::sync::mpsc::SendError<()>),
    #[error("Benchmark interrupted; owned processes are being stopped")]
    Interrupted,
    #[error("{program} exceeded time or output limit")]
    Limit { program: String },
    #[error("{source}; retained interrupted fixture at {path}")]
    Retained {
        path: PathBuf,
        #[source]
        source: Box<Error>,
    },
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
pub(crate) type Result<T> = std::result::Result<T, Error>;
pub(crate) fn require(ok: bool, message: impl Into<String>) -> Result<()> {
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}
pub(crate) fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("project root exists")
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0xf)]));
    }
    encoded
}
pub(crate) fn hash(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
pub(crate) fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut h = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        h.update(&buffer[..n]);
    }
    Ok(hex(&h.finalize()))
}
pub(crate) fn write_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    writeln!(file)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

#[derive(Clone)]
pub(crate) struct Runner {
    pub(crate) env: BTreeMap<OsString, OsString>,
    pub(crate) timeout: Duration,
    pub(crate) output_limit: u64,
    pub(crate) interruptible: bool,
    pub(crate) progress_path: Option<PathBuf>,
}
#[derive(Debug)]
pub(crate) struct Output {
    pub(crate) status: std::process::ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}
// Reap the process group even if polling or output inspection returns an error.
struct ChildGuard(Option<std::process::Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            // SAFETY: this unreaped child owns the process group created by process_group(0).
            // Its PID cannot be recycled before wait reaps it.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
        }
    }
}
impl Runner {
    pub(crate) fn temporary_file(&self) -> Result<tempfile::NamedTempFile> {
        Ok(if let Some(root) = self.env.get(OsStr::new("TMPDIR")) {
            tempfile::NamedTempFile::new_in(root)?
        } else {
            tempfile::NamedTempFile::new()?
        })
    }
    pub(crate) fn new() -> Self {
        let mut env = BTreeMap::new();
        // Only executable/toolchain discovery is inherited for exploratory host runs.
        // Measured canonical processes replace PATH with the image's fixed tool paths.
        for key in ["PATH", "RUSTUP_HOME", "CARGO_HOME"] {
            if let Some(value) = std::env::var_os(key) {
                env.insert(key.into(), value);
            }
        }
        // Rustup proxies need their explicit toolchain root after HOME isolation.
        if !env.contains_key(OsStr::new("RUSTUP_HOME")) {
            if let Some(home) = std::env::var_os("HOME") {
                let rustup = PathBuf::from(home).join(".rustup");
                if rustup.is_dir() {
                    env.insert("RUSTUP_HOME".into(), rustup.into_os_string());
                }
            }
        }
        for (k, v) in [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_AUTHOR_DATE", "2025-01-01T00:00:00Z"),
            ("GIT_COMMITTER_DATE", "2025-01-01T00:00:00Z"),
            ("LC_ALL", "C"),
            ("TZ", "UTC"),
            ("HOME", "/nonexistent"),
            ("XDG_CONFIG_HOME", "/nonexistent"),
            ("XDG_STATE_HOME", "/nonexistent"),
            ("GIT_CONFIG_COUNT", "0"),
            ("GIT_CONFIG_SYSTEM", "/dev/null"),
            ("GIT_TEMPLATE_DIR", "/nonexistent"),
        ] {
            env.insert(k.into(), v.into());
        }
        Self {
            env,
            timeout: Duration::from_secs(900),
            output_limit: 128 * 1024 * 1024,
            interruptible: true,
            progress_path: None,
        }
    }
    pub(crate) fn launcher() -> Self {
        let mut runner = Self::new();
        // Docker client configuration never reaches measured processes.
        for key in [
            "HOME",
            "DOCKER_HOST",
            "DOCKER_CONTEXT",
            "DOCKER_CONFIG",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "XDG_RUNTIME_DIR",
            "SSH_AUTH_SOCK",
        ] {
            if let Some(value) = std::env::var_os(key) {
                runner.env.insert(key.into(), value);
            }
        }
        runner.env.insert("DOCKER_BUILDKIT".into(), "1".into());
        runner
    }
    pub(crate) fn isolated(&self, root: &Path) -> Result<Self> {
        let mut runner = self.clone();
        for key in ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "TMPDIR"] {
            let path = root.join(key.to_lowercase());
            fs::create_dir_all(&path)?;
            runner.env.insert(key.into(), path.into_os_string());
        }
        Ok(runner)
    }
    pub(crate) fn capture<I, S>(
        &self,
        program: impl AsRef<OsStr>,
        args: I,
        cwd: &Path,
    ) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        // Files drain both streams without pipe deadlocks or unbounded RAM use.
        let stdout = self.temporary_file()?;
        let stderr = self.temporary_file()?;
        let mut command = Command::new(program.as_ref());
        command
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?)
            .process_group(0);
        let mut guard = ChildGuard(Some(command.spawn()?));
        let started = Instant::now();
        let mut progress_tick = Instant::now();
        let mut last_progress = None;
        let status = loop {
            // Launcher-only feedback. This never runs in measured child runners.
            if let Some(path) = &self.progress_path {
                if progress_tick.elapsed() >= Duration::from_secs(5) {
                    progress_tick = Instant::now();
                    if let Some(progress) = fs::read(path)
                        .ok()
                        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                        .and_then(|v| v.get("progress").cloned())
                        .filter(|v| v.is_object())
                    {
                        if last_progress.as_ref() != Some(&progress) {
                            eprintln!(
                                "{}/{}: {} {}/{} ({})",
                                progress["profile"].as_str().unwrap_or("?"),
                                progress["case"].as_str().unwrap_or("?"),
                                progress["phase"].as_str().unwrap_or("?"),
                                progress["block"],
                                progress["blocks"],
                                progress["binary"].as_str().unwrap_or("?")
                            );
                            last_progress = Some(progress);
                        }
                    }
                }
            }
            if self.interruptible && INTERRUPTED.load(Ordering::Relaxed) {
                return Err(Error::Interrupted);
            }
            if started.elapsed() > self.timeout
                || stdout.as_file().metadata()?.len() > self.output_limit
                || stderr.as_file().metadata()?.len() > self.output_limit
            {
                return Err(Error::Limit {
                    program: program.as_ref().to_string_lossy().into_owned(),
                });
            }
            if let Some(status) = guard.0.as_mut().expect("running child").try_wait()? {
                guard.0 = None;
                break status;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        require(
            stdout.as_file().metadata()?.len() <= self.output_limit
                && stderr.as_file().metadata()?.len() <= self.output_limit,
            "Command output limit exceeded",
        )?;
        let out = Output {
            status,
            stdout: fs::read(stdout.path())?,
            stderr: fs::read(stderr.path())?,
        };
        Ok(out)
    }
    pub(crate) fn run<I, S>(
        &self,
        program: impl AsRef<OsStr>,
        args: I,
        cwd: &Path,
    ) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.capture(&program, args, cwd)?;
        require(
            out.status.success(),
            format!(
                "{} exited {}: {}{}",
                program.as_ref().to_string_lossy(),
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        )?;
        Ok(out)
    }
    pub(crate) fn text<I, S>(
        &self,
        program: impl AsRef<OsStr>,
        args: I,
        cwd: &Path,
    ) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let out = self.run(program, args, cwd)?;
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
    pub(crate) fn git<I, S>(&self, repo: &Path, args: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut all: Vec<OsString> = [
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "gc.auto=0",
            "-c",
            "maintenance.auto=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.fsmonitor=false",
        ]
        .map(Into::into)
        .to_vec();
        all.extend(args.into_iter().map(|s| s.as_ref().to_owned()));
        Ok(self.run("git", all, repo)?.stdout)
    }
    pub(crate) fn git_text(&self, repo: &Path, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.git(repo, args)?)
            .trim()
            .to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_is_atomic_and_hash_is_standard() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("report.json");
        let value = serde_json::json!({"status": "passed"});
        write_json(&path, &value).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&path).unwrap()).unwrap(),
            value
        );
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
        assert_eq!(
            hash(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(digest(&path).unwrap(), hash(&fs::read(path).unwrap()));
    }
    #[test]
    fn commands_bound_time_and_output_and_propagate_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let mut runner = Runner::new();
        runner.timeout = Duration::from_millis(50);
        assert!(runner.run("sleep", ["10"], tmp.path()).is_err());
        assert!(runner.run("false", [] as [&str; 0], tmp.path()).is_err());
        runner.output_limit = 32;
        assert!(runner.run("yes", [] as [&str; 0], tmp.path()).is_err());
    }
    #[test]
    fn measured_environment_ignores_parent_configuration() {
        let child_key = "GROOVE_BENCH_ENV_TEST";
        if let Some(root) = std::env::var_os(child_key) {
            let runner = Runner::new().isolated(Path::new(&root)).unwrap();
            runner.run("/bin/bash",["-c","test -z \"${RUSTFLAGS+x}\" && test -z \"${BASH_ENV+x}\" && test \"$GIT_CONFIG_GLOBAL\" = /dev/null && test \"$GIT_CONFIG_COUNT\" = 0"],Path::new(&root)).unwrap();
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let poison = temp.path().join("poison.bash");
        fs::write(&poison, "exit 99\n").unwrap();
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "util::tests::measured_environment_ignores_parent_configuration",
            ])
            .env(child_key, temp.path())
            .env("BASH_ENV", &poison)
            .env("RUSTFLAGS", "parent")
            .env("GIT_CONFIG_GLOBAL", &poison)
            .env("GIT_CONFIG_COUNT", "invalid")
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(fs::read_to_string(poison).unwrap(), "exit 99\n");
    }
    #[test]
    fn interruption_reaps_the_owned_child_before_returning_failure() {
        let child_key = "GROOVE_BENCH_SIGNAL_TEST";
        if let Some(root) = std::env::var_os(child_key) {
            install_signals().unwrap();
            let root = Path::new(&root);
            let result = Runner::new().run(
                "/bin/bash",
                [
                    OsStr::new("-c"),
                    OsStr::new("printf ready > \"$1\"; exec sleep 30"),
                    OsStr::new("bench-peer"),
                    root.join("ready").as_os_str(),
                ],
                root,
            );
            assert!(result.unwrap_err().to_string().contains("interrupted"));
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "util::tests::interruption_reaps_the_owned_child_before_returning_failure",
            ])
            .env(child_key, temp.path())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut guard = ChildGuard(Some(child));
        let started = Instant::now();
        while !temp.path().join("ready").exists() {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "Peer never became ready"
            );
            if guard.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                guard.0 = None;
                panic!("Peer exited before readiness");
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        // SAFETY: guard owns this unreaped direct child.
        let signalled = unsafe { libc::kill(guard.0.as_ref().unwrap().id() as i32, libc::SIGTERM) };
        assert_eq!(signalled, 0);
        loop {
            if let Some(status) = guard.0.as_mut().unwrap().try_wait().unwrap() {
                guard.0 = None;
                assert!(status.success());
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "Interrupted runner did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
