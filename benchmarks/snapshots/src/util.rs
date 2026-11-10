use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
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
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
}
pub(crate) struct Output {
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
    pub(crate) fn new() -> Self {
        let mut env: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(k, _)| {
                let k = k.to_string_lossy();
                !k.starts_with("GIT_") && !k.starts_with("GROOVE_")
            })
            .collect();
        for (k, v) in [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_AUTHOR_DATE", "2025-01-01T00:00:00Z"),
            ("GIT_COMMITTER_DATE", "2025-01-01T00:00:00Z"),
            ("LC_ALL", "C"),
            ("TZ", "UTC"),
        ] {
            env.insert(k.into(), v.into());
        }
        Self {
            env,
            timeout: Duration::from_secs(900),
            output_limit: 128 * 1024 * 1024,
        }
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
        // Files drain both streams without pipe deadlocks or unbounded RAM use.
        let stdout = tempfile::NamedTempFile::new()?;
        let stderr = tempfile::NamedTempFile::new()?;
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
        let status = loop {
            if started.elapsed() > self.timeout
                || stdout.as_file().metadata()?.len() > self.output_limit
                || stderr.as_file().metadata()?.len() > self.output_limit
            {
                return Err(Error::Invalid(format!(
                    "{} exceeded time or output limit",
                    program.as_ref().to_string_lossy()
                )));
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
            stdout: fs::read(stdout.path())?,
            stderr: fs::read(stderr.path())?,
        };
        require(
            status.success(),
            format!(
                "{} exited {status}: {}{}",
                program.as_ref().to_string_lossy(),
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
        let mut all: Vec<OsString> = ["-c", "core.hooksPath=/dev/null", "-c", "gc.auto=0"]
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
}
