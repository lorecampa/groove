use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;

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
        }
    }
    /// Return the CLI exit category for this code.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::InvalidArgument | Self::UnsupportedConfiguration => 2,
            Self::PoolExhausted | Self::AcquireTimeout => 3,
            Self::NotInitialized | Self::ConfigConflict | Self::InvalidRevision => 4,
            Self::InvalidLease | Self::LeaseMismatch => 5,
            Self::RecoveryFailed | Self::PoolUnhealthy => 6,
            Self::IoError | Self::GitError | Self::ProtocolError => 7,
        }
    }
}
impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
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
    /// Structured context such as Git exit status and bounded captured output.
    pub details: Value,
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
            details: json!({}),
            source: None,
        }
    }
    /// Preserve an underlying error for [`std::error::Error::source`] inspection.
    /// Sources are not serialized into CLI responses.
    pub fn with_source(mut self, source: impl std::error::Error + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }
    /// Attach structured diagnostic context.
    pub fn details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
    /// Map this error to the CLI's documented exit category.
    pub fn exit_code(&self) -> u8 {
        self.code.exit_code()
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
