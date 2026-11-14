//! User preferences are independent of repository pool metadata.
use groove_core::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub pool_defaults: PoolDefaults,
    pub logging: Logging,
    pub worktrees: Worktrees,
    #[serde(skip_serializing_if = "groove_core::experimental::ExperimentalConfig::is_empty")]
    pub experimental: groove_core::experimental::ExperimentalConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PoolDefaults {
    pub ready_target: usize,
    pub idle_timeout: groove_core::IdleTimeout,
}
impl PoolDefaults {
    fn validate(&self) -> Result<()> {
        if self.ready_target > groove_core::MAX_CAPACITY {
            return Err(invalid("pool_defaults.ready_target cannot exceed 4096"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Worktrees {
    pub root: Option<PathBuf>,
}

impl Worktrees {
    fn validate(&self) -> Result<()> {
        if self.root.as_ref().is_some_and(|path| !path.is_absolute()) {
            return Err(invalid(
                "worktrees.root must be an absolute path (TOML paths do not expand ~)",
            ));
        }
        Ok(())
    }
}

/// A requested change to the preferences file.
pub enum Change {
    /// Open the file in the user's editor.
    Edit,
    /// Set (`Some`) or remove (`None`) dotted keys.
    Update(Vec<(String, Option<String>)>),
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Logging {
    pub enabled: bool,
    pub level: LogLevel,
    pub directory: Option<PathBuf>,
    pub max_file_mib: u64,
    pub max_total_mib: u64,
    pub max_files: usize,
    pub max_age_days: u64,
}

/// Persistent diagnostic verbosity, independent of terminal flags.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    #[default]
    Debug,
    Trace,
}
impl LogLevel {
    pub fn allows(self, level: tracing::Level) -> bool {
        level
            <= match self {
                Self::Error => tracing::Level::ERROR,
                Self::Warn => tracing::Level::WARN,
                Self::Info => tracing::Level::INFO,
                Self::Debug => tracing::Level::DEBUG,
                Self::Trace => tracing::Level::TRACE,
            }
    }
}
impl Logging {
    /// Settings that keep file logging off, for commands that must not create logs.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}
impl Default for Logging {
    fn default() -> Self {
        Self {
            enabled: true,
            level: LogLevel::Debug,
            directory: None,
            max_file_mib: 5,
            max_total_mib: 100,
            max_files: 100,
            max_age_days: 7,
        }
    }
}
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}
pub fn path() -> Result<PathBuf> {
    let root = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(".config"))
        })
        .ok_or_else(|| {
            invalid("Set HOME or an absolute XDG_CONFIG_HOME to locate user configuration")
        })?;
    Ok(root.join("groove/config.toml"))
}
fn read(path: &PathBuf) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}
fn parse(text: &str) -> Result<Config> {
    let config: Config = toml::from_str(text)
        .map_err(|e| invalid(format!("Invalid user configuration: {e}")).with_source(e))?;
    config.logging.validate()?;
    config.worktrees.validate()?;
    config.pool_defaults.validate()?;
    config.experimental.validate()?;
    Ok(config)
}
impl Logging {
    fn validate(&self) -> Result<()> {
        if self.max_file_mib == 0
            || self.max_file_mib > 1_048_576
            || self.max_total_mib < self.max_file_mib
            || self.max_total_mib > 1_048_576
            || self.max_files == 0
            || self.max_files > 100_000
            || self.max_age_days == 0
            || self.max_age_days > 36500
        {
            return Err(invalid("Logging limits must be positive: file <= total <= 1048576 MiB, files <= 100000, age <= 36500 days"));
        }
        if self.directory.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err(invalid("Logging directory must be an absolute path"));
        }
        Ok(())
    }
}
pub fn load() -> Result<Config> {
    let mut config = match path() {
        Ok(path) => read(&path)
            .and_then(|text| parse(&text))
            .map_err(|mut error| {
                error.message = format!("{}: {}", path.display(), error.message);
                error.details.path = Some(path);
                error
            })?,
        Err(_) => Config::default(),
    };
    if let Some(value) = std::env::var_os("GROOVE_LOG_ENABLED") {
        config.logging.enabled = match value.to_str() {
            Some("true") => true,
            Some("false") => false,
            _ => return Err(invalid("GROOVE_LOG_ENABLED must be true or false")),
        };
    }
    if let Some(value) = std::env::var_os("GROOVE_LOG_DIR") {
        config.logging.directory = Some(value.into());
    }
    config.logging.validate()?;
    config.worktrees.validate()?;
    Ok(config)
}
const KEYS: &[&str] = &[
    "enabled",
    "level",
    "directory",
    "max_file_mib",
    "max_total_mib",
    "max_files",
    "max_age_days",
];
/// Effective preferences, their file, and which environment variables override them.
pub fn show() -> Result<serde_json::Value> {
    let path = path()?;
    Ok(
        serde_json::json!({"config_path": path, "config": load()?, "environment_overrides": {
            "logging.enabled": std::env::var_os("GROOVE_LOG_ENABLED").is_some(),
            "logging.directory": std::env::var_os("GROOVE_LOG_DIR").is_some()
        }}),
    )
}

/// Apply `change` atomically, refusing to overwrite a concurrent edit.
pub fn change(change: Change) -> Result<serde_json::Value> {
    let path = path()?;
    let parent = path.parent().expect("configuration has parent");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(parent.join("config.lock"))?;
    // SAFETY: flock borrows a valid file descriptor; contention never blocks an editor.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Another configuration edit is in progress",
        ));
    }
    let original = read(&path)?;
    let mut candidate = tempfile::Builder::new()
        .prefix(".config-")
        .suffix(".toml")
        .tempfile_in(parent)?;
    let text = match change {
        Change::Edit => {
            let initial = if original.is_empty() {
                toml::to_string_pretty(&Config::default()).map_err(|e| invalid(e.to_string()))?
            } else {
                original.clone()
            };
            candidate.write_all(initial.as_bytes())?;
            candidate.flush()?;
            let editor = std::env::var("VISUAL")
                .ok()
                .filter(|s| !s.is_empty())
                .or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.is_empty()))
                .unwrap_or_else(|| "vi".into());
            let words = shell_words::split(&editor)
                .map_err(|e| invalid(format!("Invalid editor command: {e}")))?;
            let (program, arguments) = words
                .split_first()
                .ok_or_else(|| invalid("Empty editor command"))?;
            // Keep editor UI off stdout, which carries the response. No shell evaluation.
            let status = Command::new(program)
                .args(arguments)
                .arg(candidate.path())
                .stdin(Stdio::inherit())
                .stdout(Stdio::from(std::io::stderr()))
                .stderr(Stdio::inherit())
                .status()?;
            if !status.success() {
                return Err(invalid("Editor failed; configuration unchanged"));
            }
            fs::read_to_string(candidate.path())?
        }
        Change::Update(settings) => {
            let mut doc = original
                .parse::<toml_edit::DocumentMut>()
                .map_err(|e| invalid(e.to_string()))?;
            let mut seen = std::collections::BTreeSet::new();
            for (key, value) in &settings {
                if !seen.insert(key) {
                    return Err(invalid(format!("Setting specified twice: {key}")));
                }
                let (section, field) = key
                    .split_once('.')
                    .filter(|(section, field)| {
                        (*section == "logging" && KEYS.contains(field))
                            || (*section == "worktrees" && *field == "root")
                            || (*section == "pool_defaults"
                                && ["ready_target", "idle_timeout"].contains(field))
                            || (*section == "experimental" && *field == "features")
                    })
                    .ok_or_else(|| invalid(format!("Unknown setting: {key}")))?;
                if let Some(value) = value {
                    // Accept TOML literals (true, 50, "text"); anything else is a bare string.
                    let mut value = value
                        .parse::<toml_edit::Value>()
                        .unwrap_or_else(|_| toml_edit::Value::from(value.as_str()));
                    // Decode this supplied value against the actual preference
                    // types, without validating cross-field limits in isolation.
                    // Report serde's expected values without TOML source positions.
                    let mut supplied = toml_edit::DocumentMut::new();
                    supplied[section][field] = toml_edit::Item::Value(value.clone());
                    toml::from_str::<Config>(&supplied.to_string()).map_err(|error| {
                        invalid(format!("Invalid value for {key}: {}", error.message()))
                            .with_source(error)
                    })?;
                    if doc.get(section).is_none() {
                        doc[section] = toml_edit::Item::Table(toml_edit::Table::new());
                    }
                    let table = doc[section]
                        .as_table_like_mut()
                        .ok_or_else(|| invalid(format!("{section} must be a table")))?;
                    if let Some(previous) = table.get(field).and_then(toml_edit::Item::as_value) {
                        *value.decor_mut() = previous.decor().clone();
                    }
                    table.insert(field, toml_edit::Item::Value(value));
                } else if let Some(table) = doc.get_mut(section).and_then(|v| v.as_table_like_mut())
                {
                    table.remove(field);
                }
            }
            doc.to_string()
        }
    };
    parse(&text)?;
    if read(&path)? != original {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Configuration changed while editing; refusing to overwrite",
        ));
    }
    // Use a fresh file: an editor may replace its temporary file by rename.
    let mut saved = tempfile::NamedTempFile::new_in(parent)?;
    saved.write_all(text.as_bytes())?;
    saved.as_file().sync_all()?;
    saved.persist(&path).map_err(|e| Error::from(e.error))?;
    fs::File::open(parent)?.sync_all()?;
    Ok(serde_json::json!({"config_path": path, "updated": true}))
}
