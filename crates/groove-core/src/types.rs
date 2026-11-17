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
/// Both policies reset tracked files. Retained ignored files are not verified
/// build outputs and can be discarded if the checkout must be rebuilt.
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
    /// Persisted foreground Git budget. Per-operation overrides do not change it.
    pub default_git_timeout: crate::ReadyGitTimeout,
    /// Automatic spare-checkout policy; defaults to `None` for library callers,
    /// which supply their own background execution. The CLI copies user defaults.
    pub ready: Option<crate::ReadySettings>,
    /// Explicitly resume matching interrupted initialization; never reopen a ready pool.
    pub resume: bool,
    /// Maximum concurrent leases, `1..=`[`MAX_CAPACITY`].
    pub capacity: usize,
    /// Configured handling of ignored residual files.
    pub cleanup_policy: CleanupPolicy,
    /// Exact dedicated checkout root, overriding [`Self::worktree_base`].
    pub worktree_root: Option<PathBuf>,
    /// Parent for automatically named roots: `<base>/<project>-<repo-id>/<pool>`.
    /// Defaults to `$HOME/.worktrees` (`.worktrees.noindex` on macOS).
    /// Must be absolute; only new pools use it.
    pub worktree_base: Option<PathBuf>,
    /// Positive timeout for each Git operation, not the entire acquisition.
    pub git_timeout: Duration,
}
impl Default for PoolOptions {
    fn default() -> Self {
        Self {
            default_git_timeout: crate::ReadyGitTimeout::default(),
            ready: None,
            resume: false,
            capacity: 4,
            cleanup_policy: CleanupPolicy::Pristine,
            worktree_root: None,
            worktree_base: None,
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
    /// Cleanup runs later, during background preparation or the next acquisition.
    Deferred,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Serializable metadata recorded for a lease, not a live view of its checkout.
///
/// Metadata alone does not grant ownership. An in-process lease needs its live
/// handle; a durable reservation requires explicit release after all writers stop.
pub struct LeaseInfo {
    /// Permanent slot name, such as `wt-0001`.
    pub slot_id: SlotId,
    /// Unique acquisition generation used to reject stale releases.
    pub lease_id: LeaseId,
    /// Absolute path of the verified checkout.
    pub worktree_path: PathBuf,
    /// Full SHA-1 or SHA-256 commit OID requested by this lease.
    pub base_oid: CommitOid,
    /// HEAD mode at acquisition: `detached` at the base, even if the task later switches branches.
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
    /// Persisted automatic checkout policy, absent for legacy lazy pools.
    #[serde(flatten)]
    pub ready: Option<crate::ReadySettings>,
    /// Namespace initialized; `default` uses the original layout.
    #[serde(default)]
    pub pool_name: crate::PoolName,
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
    /// Persisted foreground Git budget; defaults to two minutes in older pools.
    #[serde(default)]
    pub git_timeout: crate::ReadyGitTimeout,
    /// Persisted automatic checkout policy, absent for legacy lazy pools.
    #[serde(flatten)]
    pub ready: Option<crate::ReadySettings>,
    /// Selected pool namespace.
    #[serde(default)]
    pub pool_name: crate::PoolName,
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
    /// Persisted foreground Git budget; None leaves the setting unchanged.
    pub git_timeout: Option<crate::ReadyGitTimeout>,
    /// Desired spare count; validated against the resulting capacity.
    pub ready_target: Option<usize>,
    /// Idle eviction duration, or `never`.
    pub idle_timeout: Option<crate::IdleTimeout>,
    /// Local revision used to prepare unused checkouts. Never fetches.
    pub ready_base: Option<String>,
    /// Persisted background Git budget; None leaves the current setting unchanged.
    pub ready_git_timeout: Option<crate::ReadyGitTimeout>,
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
            git_timeout: None,
            ready_target: None,
            idle_timeout: None,
            ready_base: None,
            ready_git_timeout: None,
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
/// Confirmation of explicit unlock; cleanup is deferred to preparation/reuse.
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
