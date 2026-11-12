use crate::{LeaseId, SlotId};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt, path::PathBuf};

/// A Groove operation result with a machine-readable error.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable v1 error categories, serialized as their uppercase protocol names.
///
/// Match variants in Rust; use [`Self::as_str`] at text boundaries. New categories
/// may be added, so downstream exhaustive matches need a fallback arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[non_exhaustive]
pub enum ErrorCode {
    /// Invalid caller input.
    InvalidArgument,
    /// Unsupported platform, filesystem or repository configuration.
    UnsupportedConfiguration,
    /// All usable slots are currently leased.
    PoolExhausted,
    /// The capacity contention deadline elapsed.
    AcquireTimeout,
    /// No ready pool manifest exists.
    NotInitialized,
    /// Requested or stored configuration conflicts with the pool.
    ConfigConflict,
    /// The requested revision does not resolve to a commit.
    InvalidRevision,
    /// The descriptor or slot cannot identify the requested ownership.
    InvalidLease,
    /// The supplied acquisition generation is stale or inactive.
    LeaseMismatch,
    /// A slot could not be restored or verified.
    RecoveryFailed,
    /// No healthy or busy candidate remains.
    PoolUnhealthy,
    /// A filesystem or operating-system operation failed.
    IoError,
    /// A Git operation failed, timed out, or returned invalid output.
    GitError,
    /// Metadata or transport data is malformed or incompatible.
    ProtocolError,
    /// The workspace has staged, modified or untracked files that would be lost.
    UnsavedChanges,
    /// Other processes still have the workspace as their directory or open files in it.
    WorkspaceInUse,
}
impl ErrorCode {
    /// Return the stable v1 wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::UnsupportedConfiguration => "UNSUPPORTED_CONFIGURATION",
            Self::PoolExhausted => "POOL_EXHAUSTED",
            Self::AcquireTimeout => "ACQUIRE_TIMEOUT",
            Self::NotInitialized => "NOT_INITIALIZED",
            Self::ConfigConflict => "CONFIG_CONFLICT",
            Self::InvalidRevision => "INVALID_REVISION",
            Self::InvalidLease => "INVALID_LEASE",
            Self::LeaseMismatch => "LEASE_MISMATCH",
            Self::RecoveryFailed => "RECOVERY_FAILED",
            Self::PoolUnhealthy => "POOL_UNHEALTHY",
            Self::IoError => "IO_ERROR",
            Self::GitError => "GIT_ERROR",
            Self::ProtocolError => "PROTOCOL_ERROR",
            Self::UnsavedChanges => "UNSAVED_CHANGES",
            Self::WorkspaceInUse => "WORKSPACE_IN_USE",
        }
    }
}
impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed or timed-out Git subprocess, with bounded captured output.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFailure {
    /// Exit status; absent when Git was killed after its timeout.
    pub exit_code: Option<i32>,
    /// Captured standard output, lossily decoded.
    pub stdout: String,
    /// Captured standard error, lossily decoded.
    pub stderr: String,
    /// Whether either stream exceeded its capture bound.
    pub output_truncated: bool,
}

/// Why acquisition found no usable slot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapacityFailure {
    /// Configured slot count during the final scan.
    pub capacity: usize,
    /// Slots owned or durably reserved by other tasks.
    pub busy_slots: usize,
    /// Slots that could not be prepared, with their failure reasons.
    pub failed_slots: BTreeMap<SlotId, String>,
}

/// Structured context attached to an [`Error`]. Every field is optional; only
/// fields relevant to the failure are populated and serialized.
///
/// Fields describe *partial progress* so callers can recover without guessing,
/// e.g. a `finish` whose snapshot was pinned before release failed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ErrorDetails {
    /// Executable whose managed spawn failed (arguments are never included).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Execution history entry associated with the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<crate::RunId>,
    /// Filesystem path the failing operation was acting on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Failed Git subprocess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git: Option<GitFailure>,
    /// Capacity scan outcome for exhausted or unhealthy pools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity: Option<CapacityFailure>,
    /// Reservation that remains protected (and therefore yours) after the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_id: Option<LeaseId>,
    /// Checkout path of that reservation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<PathBuf>,
    /// Whether the reservation was already released before the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released: Option<bool>,
    /// Snapshot that was durably saved before the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_id: Option<String>,
    /// Safety snapshot captured before a failed restore.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safety_snapshot_id: Option<String>,
    /// Setup readiness recorded at the time of the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preparation: Option<Box<crate::Preparation>>,
    /// Unsaved changes found by a release check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<crate::WorkspaceGitStatus>,
    /// Processes found using the workspace by a release or finish check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub processes: Vec<crate::WorkspaceProcess>,
    /// A secondary failure encountered while handling the primary one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_error: Option<String>,
}

/// An operation failure. Branch on [`ErrorCode`], not human-readable messages.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
#[non_exhaustive]
pub struct Error {
    /// Stable, typed protocol error category.
    pub code: ErrorCode,
    /// Human-readable diagnostic; wording is not a compatibility guarantee.
    pub message: String,
    /// Whether retrying without configuration changes may succeed.
    pub retryable: bool,
    /// Structured context such as Git output or partially completed work.
    pub details: Box<ErrorDetails>,
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}
impl Error {
    /// Construct a non-retryable error with empty diagnostic details.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            details: Box::default(),
            source: None,
        }
    }
    /// Preserve an underlying error for [`std::error::Error::source`] inspection.
    /// Sources are not serialized into CLI responses.
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
    /// Amend structured context in place, keeping fields set by earlier layers.
    ///
    /// ```
    /// use groove_core::{Error, ErrorCode};
    /// let error = Error::new(ErrorCode::IoError, "disk full")
    ///     .with_details(|d| d.released = Some(false));
    /// assert_eq!(error.details.released, Some(false));
    /// ```
    pub fn with_details(mut self, amend: impl FnOnce(&mut ErrorDetails)) -> Self {
        amend(&mut self.details);
        self
    }
    /// Exit status of the failed Git subprocess, if this is a Git failure.
    pub fn git_exit_code(&self) -> Option<i32> {
        self.details.git.as_ref().and_then(|git| git.exit_code)
    }
    /// One-line diagnostic for per-slot failure summaries: the message plus the
    /// last line Git wrote to stderr, which usually names the real cause.
    pub(crate) fn summary(&self) -> String {
        const MAX_CHARS: usize = 400;
        let cause = self.details.git.as_ref().and_then(|git| {
            git.stderr
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
        });
        match cause {
            Some(line) => {
                // Truncate by characters, never by byte index, so multibyte text cannot panic.
                let line: String = line.chars().take(MAX_CHARS).collect();
                format!("{}: {line}", self.message)
            }
            None => self.message.clone(),
        }
    }
    pub(crate) fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(ErrorCode::IoError, e.to_string()).with_source(e)
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(ErrorCode::ProtocolError, e.to_string()).with_source(e)
    }
}

/// Attach the path an I/O operation was acting on to its error.
pub(crate) trait PathContext<T> {
    /// Convert into a Groove error whose message and details name `path`.
    fn at(self, path: &std::path::Path) -> Result<T>;
}
impl<T, E: Into<Error>> PathContext<T> for std::result::Result<T, E> {
    fn at(self, path: &std::path::Path) -> Result<T> {
        self.map_err(|error| {
            let mut error = error.into();
            if error.details.path.is_none() {
                error.message = format!("{}: {}", path.display(), error.message);
                error.details.path = Some(path.to_owned());
            }
            error
        })
    }
}
