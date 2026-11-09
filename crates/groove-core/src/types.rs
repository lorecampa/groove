use crate::{CommitOid, LeaseId, SlotId};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// How acquisition cleans residual worktree contents.
pub enum CleanupPolicy {
    #[default]
    /// Remove all untracked contents, including ignored files.
    Pristine,
    /// Preserve ignored files when sanitation succeeds; rebuilding can discard them.
    Warm,
}

#[derive(Debug, Clone)]
/// Immutable pool configuration used during initialization.
pub struct PoolOptions {
    /// Maximum concurrent leases (1 through 4096).
    pub capacity: usize,
    /// Configured handling of ignored residual files.
    pub cleanup_policy: CleanupPolicy,
    /// Optional dedicated checkout root; defaults to the pool state directory.
    pub worktree_root: Option<PathBuf>,
    /// Positive timeout for each Git operation, not the entire acquisition.
    pub git_timeout: Duration,
}
impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            capacity: 4,
            cleanup_policy: CleanupPolicy::Pristine,
            worktree_root: None,
            git_timeout: Duration::from_secs(120),
        }
    }
}
#[derive(Debug, Clone)]
/// Revision, capacity wait, and Git timeout for one acquisition.
pub struct AcquireOptions {
    /// Git revision resolved once to a full commit OID before allocation.
    pub base: String,
    /// Capacity contention budget; zero fails immediately when all slots are busy.
    /// Preparation has separate Git timeouts and may outlast this budget.
    pub wait: Duration,
    /// Positive timeout for each Git operation, not the entire acquisition.
    pub git_timeout: Duration,
}
impl Default for AcquireOptions {
    fn default() -> Self {
        Self {
            base: "HEAD".into(),
            wait: Duration::ZERO,
            git_timeout: Duration::from_secs(120),
        }
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
/// How a slot reached its verified baseline.
pub enum RecoveryOutcome {
    /// Materialized a previously absent checkout.
    Created,
    /// Normalized and verified an existing checkout.
    Sanitized,
    /// Replaced a damaged or interrupted checkout.
    Rebuilt,
}

/// How a leased worktree's HEAD is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HeadMode {
    /// HEAD points directly to the requested commit.
    Detached,
}

/// How ownership reaches the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LeaseTransport {
    /// An owned Rust lease retains the descriptor.
    InProcess,
    /// A Unix socket transfers the descriptor using SCM_RIGHTS.
    ScmRights,
}

/// When a released slot is cleaned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CleanupTiming {
    /// Cleanup runs during the next acquisition.
    Deferred,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Serializable lease metadata. Retain the owning descriptor separately.
pub struct LeaseInfo {
    /// Permanent slot name, such as `wt-0001`.
    pub slot_id: SlotId,
    /// Unique acquisition generation used to reject stale releases.
    pub lease_id: LeaseId,
    /// Absolute path of the verified checkout.
    pub worktree_path: PathBuf,
    /// Full SHA-1 or SHA-256 commit OID requested by this lease.
    pub base_oid: CommitOid,
    /// Protocol HEAD mode; currently always `detached`.
    pub head_mode: HeadMode,
    /// Preparation performed before handing out this lease.
    pub recovery: RecoveryOutcome,
    /// Configured handling of ignored residual files.
    pub cleanup_policy: CleanupPolicy,
    /// Ownership transport: `in_process` or `scm_rights`.
    pub lease_transport: LeaseTransport,
    /// Number of ownership descriptors; currently always one.
    pub descriptor_count: usize,
}
#[derive(Debug, Serialize, Deserialize)]
/// The initialized pool configuration and whether it was newly published.
pub struct InitResult {
    /// Whether initialization published a new ready manifest.
    pub created: bool,
    /// Canonical shared Git administration directory.
    pub common_git_dir: PathBuf,
    /// Absolute directory containing Groove metadata and permanent locks.
    pub state_dir: PathBuf,
    /// Maximum concurrent leases (1 through 4096).
    pub capacity: usize,
    /// Names of the configured permanent slots.
    pub slots: Vec<SlotId>,
    /// Configured handling of ignored residual files.
    pub cleanup_policy: CleanupPolicy,
    /// Absolute root containing managed checkout directories.
    pub worktree_root: PathBuf,
}
#[derive(Debug, Serialize, Deserialize)]
/// Confirmation of explicit unlock; cleanup is deferred to acquisition.
pub struct ReleaseResult {
    /// Permanent slot name, such as `wt-0001`.
    pub slot_id: SlotId,
    /// Unique acquisition generation used to reject stale releases.
    pub lease_id: LeaseId,
    /// Whether explicit unlock completed successfully.
    pub released: bool,
    /// Cleanup timing; currently always `deferred`.
    pub cleanup: CleanupTiming,
}
