use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// A release configuration, I/O or command failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A failed release invariant or external command.
    #[error("{0}")]
    Invalid(String),
    /// Filesystem or process I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Invalid manifest or lockfile.
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    /// Manifest editing failed.
    #[error(transparent)]
    Edit(#[from] toml_edit::TomlError),
    /// Invalid registry or command JSON.
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// Invalid semantic version.
    #[error(transparent)]
    Version(#[from] semver::Error),
}
/// Result of a release operation.
pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::Invalid(message.into()))
    }
}

#[derive(Clone)]
pub(crate) struct Runner {
    env: BTreeMap<OsString, OsString>,
}

struct ChildGuard(Option<Child>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            // SAFETY: this unreaped child owns the process group created by process_group(0).
            // The process ID cannot be recycled until wait reaps the child.
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
            .filter(|(key, _)| {
                let key = key.to_string_lossy();
                !key.starts_with("GIT_") && !key.starts_with("GROOVE_")
            })
            .collect();
        for (key, value) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("LC_ALL", "C"),
        ] {
            env.insert(key.into(), value.into());
        }
        Self { env }
    }

    pub(crate) fn isolate(&mut self, root: &Path) -> Result<()> {
        self.env
            .retain(|key, _| !key.to_string_lossy().starts_with("XDG_"));
        for (key, name) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
        ] {
            let path = root.join(name);
            fs::create_dir_all(&path)?;
            self.env.insert(key.into(), path.into_os_string());
        }
        Ok(())
    }

    pub(crate) fn run(
        &self,
        cwd: &Path,
        program: impl AsRef<OsStr>,
        arguments: Vec<OsString>,
    ) -> Result<Vec<u8>> {
        eprintln!("+ {} {:?}", program.as_ref().to_string_lossy(), arguments);
        // File-backed capture drains both streams without pipe deadlocks or unbounded memory.
        let stdout = tempfile::NamedTempFile::new()?;
        let stderr = tempfile::NamedTempFile::new()?;
        let child = Command::new(program.as_ref())
            .args(arguments)
            .current_dir(cwd)
            .env_clear()
            .envs(&self.env)
            .stdin(Stdio::null())
            .stdout(stdout.reopen()?)
            .stderr(stderr.reopen()?)
            .process_group(0)
            .spawn()?;
        let mut guard = ChildGuard(Some(child));
        let started = Instant::now();
        let output_limit = 64 * 1024 * 1024;
        let status = loop {
            require(
                started.elapsed() < Duration::from_secs(1800)
                    && stdout.as_file().metadata()?.len() <= output_limit
                    && stderr.as_file().metadata()?.len() <= output_limit,
                "Release command exceeded time or output limit",
            )?;
            if let Some(status) = guard
                .0
                .as_mut()
                .ok_or_else(|| Error::Invalid("Missing release process".into()))?
                .try_wait()?
            {
                guard.0 = None;
                break status;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        require(
            stdout.as_file().metadata()?.len() <= output_limit
                && stderr.as_file().metadata()?.len() <= output_limit,
            "Release command output limit exceeded",
        )?;
        let output = fs::read(stdout.path())?;
        let diagnostics = fs::read(stderr.path())?;
        eprint!("{}", String::from_utf8_lossy(&diagnostics));
        require(
            status.success(),
            format!(
                "{} failed ({status}): {}",
                program.as_ref().to_string_lossy(),
                String::from_utf8_lossy(&output)
            ),
        )?;
        Ok(output)
    }

    pub(crate) fn text(
        &self,
        cwd: &Path,
        program: impl AsRef<OsStr>,
        arguments: Vec<OsString>,
    ) -> Result<String> {
        String::from_utf8(self.run(cwd, program, arguments)?)
            .map_err(|e| Error::Invalid(format!("Non-UTF-8 command response: {e}")))
    }
}
