use crate::{CommitOid, LeaseId, SlotId};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};

/// Largest supported pool capacity, and therefore the highest slot number.
pub const MAX_CAPACITY: usize = 4096;

/// Default bound for each Git subprocess and administrative lock wait.
pub const DEFAULT_GIT_TIMEOUT: Duration = Duration::from_secs(120);

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
/// Options used to create a pool or explicitly resume interrupted initialization.
pub struct PoolOptions {
    /// Explicitly resume matching interrupted initialization; never reopen a ready pool.
    pub resume: bool,
    /// Maximum concurrent leases, `1..=`[`MAX_CAPACITY`].
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
            resume: false,
            capacity: 4,
            cleanup_policy: CleanupPolicy::Pristine,
            worktree_root: None,
            git_timeout: DEFAULT_GIT_TIMEOUT,
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
            git_timeout: DEFAULT_GIT_TIMEOUT,
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
    /// Replaced an existing checkout that could not be normalized in place.
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

/// What keeps a lease from being reused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Ownership {
    /// A live [`Lease`](crate::Lease) handle retains the slot lock descriptor.
    InProcess,
    /// A durable reservation survives process exit until explicit stopped-writer release.
    Durable,
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
/// Serializable lease metadata. An in-process lease is owned by its live handle;
/// a durable reservation requires explicit release after all writers stop.
pub struct LeaseInfo {
    /// Permanent slot name, such as `wt-0001`.
    pub slot_id: SlotId,
    /// Unique acquisition generation used to reject stale releases.
    pub lease_id: LeaseId,
    /// Absolute path of the verified checkout.
    pub worktree_path: PathBuf,
    /// Full SHA-1 or SHA-256 commit OID requested by this lease.
    pub base_oid: CommitOid,
    /// HEAD mode; always `detached` at the base.
    pub head_mode: HeadMode,
    /// Preparation performed before handing out this lease.
    pub recovery: RecoveryOutcome,
    /// Configured handling of ignored residual files.
    pub cleanup_policy: CleanupPolicy,
    /// Whether a live handle or a durable reservation owns the slot.
    pub ownership: Ownership,
    /// Optional caller-supplied task label; diagnostic, never proof of ownership.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Durable idempotency key for this reservation, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_key: Option<String>,
    /// Original base expression used to reject conflicting request-key retries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_base: Option<String>,
    /// Acquisition timestamp for diagnostics; never authorizes reclamation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquired_at_unix_ms: Option<u64>,
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
    /// Maximum concurrent leases, `1..=`[`MAX_CAPACITY`].
    pub capacity: usize,
    /// Names of the configured permanent slots.
    pub slots: Vec<SlotId>,
    /// Configured handling of ignored residual files.
    pub cleanup_policy: CleanupPolicy,
    /// Absolute root containing managed checkout directories.
    pub worktree_root: PathBuf,
}

/// Current repository-local pool settings, refreshed from durable metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSettings {
    /// Maximum reservations; growth takes effect for subsequent acquisition scans.
    pub capacity: usize,
    /// Cleanup applied to the next acquisition; existing leases keep their policy.
    pub cleanup_policy: CleanupPolicy,
    /// Immutable checkout root.
    pub worktree_root: PathBuf,
    /// Repository-local metadata directory.
    pub state_dir: PathBuf,
}

/// An atomic pool settings update. The checkout root cannot be changed.
#[derive(Debug, Clone)]
pub struct PoolUpdate {
    /// New capacity, `1..=`[`MAX_CAPACITY`]. Shrinking is rejected.
    pub capacity: Option<usize>,
    /// Cleanup policy for future acquisitions.
    pub cleanup_policy: Option<CleanupPolicy>,
    /// Positive administrative-lock wait budget.
    pub timeout: Duration,
}
impl Default for PoolUpdate {
    fn default() -> Self {
        Self {
            capacity: None,
            cleanup_policy: None,
            timeout: DEFAULT_GIT_TIMEOUT,
        }
    }
}

/// Published pool settings and whether this call changed them.
#[derive(Debug, Serialize, Deserialize)]
pub struct PoolUpdateResult {
    /// False when the requested values were already configured.
    pub updated: bool,
    /// Current durable settings.
    pub pool: PoolSettings,
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
