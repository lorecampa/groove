use crate::{git::Git, repository, Error, ErrorCode, LeaseInfo, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::Read,
    os::unix::fs::OpenOptionsExt,
    path::{Component, Path, PathBuf},
    time::Duration,
};

/// Versioned, explicitly invoked project workflows from `.groove.toml`.
/// Reading this file never executes a program or changes the pool configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Project {
    /// Project format; only version 1 is supported.
    pub schema_version: u32,
    /// Stable workspace environment and optional port mapping.
    pub workspace: WorkspaceConfig,
    /// Named ordered setup recipes.
    pub setup: BTreeMap<String, SetupRecipe>,
    /// Named one-command workflows invoked through exec.
    pub recipes: BTreeMap<String, CommandRecipe>,
    /// Approved artifact paths and compatibility inputs.
    pub caches: BTreeMap<String, CacheProfile>,
    /// Named agent argv prefixes, invoked through exec.
    pub agents: BTreeMap<String, CommandRecipe>,
}
impl Default for Project {
    fn default() -> Self {
        Self {
            schema_version: 1,
            workspace: WorkspaceConfig::default(),
            setup: BTreeMap::new(),
            recipes: BTreeMap::new(),
            caches: BTreeMap::new(),
            agents: BTreeMap::new(),
        }
    }
}

/// Environment attached to explicitly launched project commands.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    /// Optional base plus the one-based slot number, exposed as `GROOVE_PORT`.
    /// This derives a predictable number, not an operating-system port reservation.
    pub port_base: Option<u16>,
    /// Values may contain `{slot}`, `{slot_index}`, `{worktree}`, `{lease}` and `{task}`.
    /// Substitutions are data; no shell expansion occurs.
    pub environment: BTreeMap<String, String>,
}

/// A directly executed argv vector, with optional preparation prerequisite.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandRecipe {
    /// Program and arguments; shell syntax requires an explicit shell program.
    pub command: Vec<String>,
    /// Runtime bound in milliseconds; defaults to ten minutes, zero disables it.
    #[serde(default = "command_timeout")]
    pub timeout_ms: u64,
    /// Required successful setup profile; checked without implicitly executing it.
    #[serde(default)]
    pub requires_setup: Option<String>,
}
fn command_timeout() -> u64 {
    600_000
}

/// An ordered project setup workflow, invoked only on explicit request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SetupRecipe {
    /// Optional approved cache to seed before commands and publish after success.
    pub cache: Option<String>,
    /// Commands run sequentially; failure stops later steps and retains the reservation.
    pub steps: Vec<SetupStep>,
}

/// One named setup command; every run is captured by managed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStep {
    /// Stable descriptive name, unique within this setup recipe.
    pub name: String,
    /// Direct argv, never implicitly interpreted by a shell.
    pub command: Vec<String>,
    /// Maximum runtime in milliseconds; defaults to ten minutes.
    #[serde(default = "command_timeout")]
    pub timeout_ms: u64,
    /// Explicitly allow skipping this step on a compatible cache hit.
    #[serde(default)]
    pub when: StepWhen,
}

/// When a setup step is required.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepWhen {
    /// Always run, including after cache seeding.
    #[default]
    Always,
    /// Run only if no compatible artifact cache was seeded.
    CacheMiss,
}

/// Explicit artifact duplication strategy; no silent fallback occurs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    /// Portable independent byte copies.
    #[default]
    Copy,
    /// Copy-on-write cloning (Linux FICLONE or macOS fclonefileat), requiring
    /// compatible source/destination storage. Unsupported cloning fails explicitly.
    Reflink,
}

/// Allowlisted ignored artifacts, separate from tracked checkout materialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheProfile {
    /// Nonempty relative paths; Git administration, overlapping paths and escapes are rejected.
    pub paths: Vec<PathBuf>,
    /// Regular relative files whose byte content participates in the compatibility key.
    pub inputs: Vec<PathBuf>,
    /// Explicit toolchain/environment version, e.g. `node-22-linux-x86_64`.
    pub key: String,
    /// Independent copy or explicit reflink; default copy.
    #[serde(default)]
    pub mode: CacheMode,
    /// Total regular-file bytes accepted per artifact snapshot; default 20 GiB.
    #[serde(default = "cache_bytes")]
    pub max_bytes: u64,
    /// Maximum artifact entries; default one million.
    #[serde(default = "cache_files")]
    pub max_files: u64,
}
fn cache_bytes() -> u64 {
    20 * 1024 * 1024 * 1024
}
fn cache_files() -> u64 {
    1_000_000
}

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}
pub(crate) fn hex(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let bytes = bytes.as_ref();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0xf)]));
    }
    encoded
}
pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
pub(crate) fn relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path
            .components()
            .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".groove.toml")
    {
        return Err(invalid("Project paths must be nonempty relative paths without Git/project administration or parent components"));
    }
    Ok(())
}
fn command_valid(command: &[String]) -> bool {
    !command.is_empty()
        && !command[0].is_empty()
        && command.len() <= 1024
        && command
            .iter()
            .all(|arg| !arg.contains('\0') && arg.len() <= 1024 * 1024)
}
pub(crate) fn env_valid(key: &str, value: &str) -> bool {
    !key.is_empty()
        && !key.starts_with("GIT_")
        && !key.starts_with("GROOVE_")
        && key
            .bytes()
            .enumerate()
            .all(|(i, b)| b == b'_' || b.is_ascii_alphabetic() || (i > 0 && b.is_ascii_digit()))
        && !value.contains('\0')
}

impl Project {
    /// Parse and validate project TOML. No workflow is executed.
    ///
    /// ```
    /// let project = groove_core::Project::parse(r#"
    /// schema_version = 1
    /// [setup.dev]
    /// [[setup.dev.steps]]
    /// name = "check"
    /// command = ["cargo", "check", "--locked"]
    /// "#)?;
    /// assert_eq!(project.setup["dev"].steps.len(), 1);
    /// # Ok::<(), groove_core::Error>(())
    /// ```
    ///
    /// # Errors
    /// Rejects unsupported versions, unknown fields, invalid commands, paths and references.
    pub fn parse(contents: &str) -> Result<Self> {
        let project: Self = toml::from_str(contents)
            .map_err(|error| invalid("Invalid .groove.toml").with_source(error))?;
        project.validate()?;
        Ok(project)
    }
    /// Read `.groove.toml` at a specific checkout root, or empty defaults when absent.
    ///
    /// # Errors
    /// Rejects symlinks, nonregular/oversized files, invalid TOML and I/O failures.
    pub fn read(root: &Path) -> Result<Self> {
        let path = root.join(".groove.toml");
        repository::no_symlinks(&path)?;
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(error.into()),
        };
        if !file.metadata()?.is_file() || file.metadata()?.len() > 1024 * 1024 {
            return Err(invalid(
                "Project configuration must be a regular file of at most 1 MiB",
            ));
        }
        let mut contents = String::new();
        file.take(1024 * 1024 + 1).read_to_string(&mut contents)?;
        if contents.len() > 1024 * 1024 {
            return Err(invalid("Project configuration exceeded 1 MiB"));
        }
        Self::parse(&contents)
    }
    /// Discover the selected Git checkout and read its project configuration.
    ///
    /// # Errors
    /// Rejects bare roots without a selected checkout, Git discovery and configuration failures.
    pub fn discover(repo: &Path, timeout: Duration) -> Result<(PathBuf, Self)> {
        let root = String::from_utf8(Git::new(timeout).run(
            repo,
            &["rev-parse", "--show-toplevel"],
            &[],
        )?)
        .map_err(|error| invalid("Project root must be UTF-8").with_source(error))?;
        let root = PathBuf::from(root.trim_end_matches('\n'));
        let project = Self::read(&root)?;
        Ok((root.join(".groove.toml"), project))
    }
    /// Stable fingerprint used to detect changed setup definitions on retry.
    ///
    /// # Errors
    /// Reports invalid configuration or serialization failures.
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        Ok(hex(Sha256::digest(serde_json::to_vec(self)?)))
    }
    /// Validate the entire file before executing any step or copying artifacts.
    ///
    /// # Errors
    /// Rejects unknown references, invalid argv/environment and unsafe artifact paths.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(Error::new(
                ErrorCode::UnsupportedConfiguration,
                "Unsupported project configuration version",
            ));
        }
        for (key, value) in &self.workspace.environment {
            if !env_valid(key, value) {
                return Err(invalid(
                    "Project environment names must be ordinary identifiers outside GIT_/GROOVE_",
                ));
            }
        }
        for (name, cache) in &self.caches {
            if !valid_name(name)
                || cache.key.is_empty()
                || cache.key.len() > 256
                || cache.paths.is_empty()
                || cache.inputs.is_empty()
                || cache.max_bytes == 0
                || cache.max_files == 0
            {
                return Err(invalid(
                    "Caches need a valid name, explicit key, paths, inputs and positive limits",
                ));
            }
            for path in cache.paths.iter().chain(&cache.inputs) {
                relative(path)?;
            }
            for (i, path) in cache.paths.iter().enumerate() {
                if cache
                    .paths
                    .iter()
                    .skip(i + 1)
                    .any(|other| path.starts_with(other) || other.starts_with(path))
                    || cache.inputs.iter().any(|input| input.starts_with(path))
                {
                    return Err(invalid(
                        "Cache paths cannot overlap each other or compatibility inputs",
                    ));
                }
            }
        }
        for (name, recipe) in &self.setup {
            if !valid_name(name)
                || recipe.steps.len() > 256
                || recipe
                    .cache
                    .as_ref()
                    .is_some_and(|cache| !self.caches.contains_key(cache))
            {
                return Err(invalid("Invalid setup name, cache reference or step count"));
            }
            let mut names = std::collections::BTreeSet::new();
            for step in &recipe.steps {
                if !valid_name(&step.name) {
                    return Err(invalid(format!(
                        "Setup profile {name:?}, step {:?}: name must contain 1–64 ASCII letters, digits, underscores or hyphens",
                        step.name
                    )));
                }
                if !names.insert(&step.name) {
                    return Err(invalid(format!(
                        "Setup profile {name:?}: duplicate step name {:?}",
                        step.name
                    )));
                }
                if !command_valid(&step.command) {
                    return Err(invalid(format!("Setup profile {name:?}, step {:?}: command must be nonempty direct argv with a nonempty executable, at most 1024 arguments, and no NUL or argument over 1 MiB", step.name)));
                }
                if step.when == StepWhen::CacheMiss && recipe.cache.is_none() {
                    return Err(invalid("cache_miss steps need an associated cache"));
                }
            }
        }
        for (name, recipe) in self.recipes.iter().chain(&self.agents) {
            if !valid_name(name)
                || !command_valid(&recipe.command)
                || recipe
                    .requires_setup
                    .as_ref()
                    .is_some_and(|setup| !self.setup.contains_key(setup))
            {
                return Err(invalid(
                    "Invalid recipe/agent name, command or setup reference",
                ));
            }
        }
        Ok(())
    }
    pub(crate) fn environment(&self, info: &LeaseInfo) -> Result<BTreeMap<String, String>> {
        let mut values = BTreeMap::new();
        for (key, value) in &self.workspace.environment {
            let value = value
                .replace("{slot}", info.slot_id.as_str())
                .replace("{slot_index}", &info.slot_id.number().to_string())
                .replace("{worktree}", repository::utf8(&info.worktree_path)?)
                .replace("{lease}", &info.lease_id.to_string())
                .replace("{task}", info.task.as_deref().unwrap_or(""));
            values.insert(key.clone(), value);
        }
        if let Some(base) = self.workspace.port_base {
            let port = base
                .checked_add(info.slot_id.number())
                .ok_or_else(|| invalid("Workspace port_base + slot number exceeds 65535"))?;
            values.insert("GROOVE_PORT".into(), port.to_string());
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_encoding_preserves_zero_padding_case_and_high_bytes() {
        assert_eq!(super::hex([]), "");
        assert_eq!(
            super::hex([0, 1, 15, 16, 127, 128, 254, 255]),
            "00010f107f80feff"
        );
    }
}
