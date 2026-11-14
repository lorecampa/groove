//! Spare checkout preparation and opportunistic idle eviction.
use crate::{Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr, time::Duration};

/// Positive idle duration (`s`, `m`, `h`, `d`) or `never` to disable eviction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct IdleTimeout(Option<Duration>);
impl Default for IdleTimeout {
    fn default() -> Self {
        Self(Some(Duration::from_secs(7 * 86400)))
    }
}
impl FromStr for IdleTimeout {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        if value == "never" {
            return Ok(Self(None));
        }
        let invalid = || {
            Error::new(
                ErrorCode::InvalidArgument,
                "Idle timeout must be a positive integer followed by s, m, h or d, or never",
            )
        };
        let split = value.len().saturating_sub(1);
        let number = value.get(..split).ok_or_else(invalid)?;
        let unit = value.get(split..).ok_or_else(invalid)?;
        if !number.starts_with(|c: char| matches!(c, '1'..='9'))
            || !number.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(invalid());
        }
        let multiplier = match unit {
            "s" => 1,
            "m" => 60,
            "h" => 3600,
            "d" => 86400,
            _ => return Err(invalid()),
        };
        let seconds = number
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(multiplier))
            .filter(|n| *n > 0 && *n <= u64::MAX / 1000)
            .ok_or_else(invalid)?;
        Ok(Self(Some(Duration::from_secs(seconds))))
    }
}
impl TryFrom<String> for IdleTimeout {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}
impl From<IdleTimeout> for String {
    fn from(value: IdleTimeout) -> Self {
        value.to_string()
    }
}
impl fmt::Display for IdleTimeout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            None => f.write_str("never"),
            Some(duration) => {
                let seconds = duration.as_secs();
                for (unit, divisor) in [("d", 86400), ("h", 3600), ("m", 60), ("s", 1)] {
                    if seconds % divisor == 0 {
                        return write!(f, "{}{unit}", seconds / divisor);
                    }
                }
                unreachable!("every integer is divisible by one")
            }
        }
    }
}

/// Pool-specific automatic preparation and retention policy.
/// Global CLI preferences are copied at initialization, never inherited at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadySettings {
    /// Desired unreserved prepared checkouts, bounded by pool capacity.
    pub ready_target: usize,
    /// Excess released checkouts become eligible after this idle duration.
    pub idle_timeout: IdleTimeout,
    /// Local revision for preparation; HEAD is resolved at initialization.
    pub ready_base: String,
}
impl Default for ReadySettings {
    fn default() -> Self {
        Self {
            ready_target: 1,
            idle_timeout: IdleTimeout::default(),
            ready_base: "HEAD".into(),
        }
    }
}
impl ReadySettings {
    pub(crate) fn resolve_head(
        &mut self,
        repo: &crate::repository::Repository,
        timeout: Duration,
    ) -> Result<()> {
        if self.ready_base == "HEAD" {
            let git = Git::new(timeout);
            let base = git
                .run(&repo.origin, &["symbolic-ref", "--quiet", "HEAD"], &[])
                .or_else(|_| git.run(&repo.origin, &["rev-parse", "--verify", "HEAD"], &[]))?;
            self.ready_base = String::from_utf8_lossy(&base).trim().to_owned();
        }
        Ok(())
    }
    /// Validate the policy against the complete candidate pool capacity.
    /// # Errors
    /// Rejects targets above capacity and empty or option-like revisions.
    pub fn validate(&self, capacity: usize) -> Result<()> {
        if self.ready_target > capacity
            || self.ready_base.is_empty()
            || self.ready_base.starts_with('-')
            || self.ready_base.chars().any(char::is_control)
        {
            return Err(Error::new(ErrorCode::InvalidArgument, "ready_target must not exceed capacity and ready_base must be a nonempty local revision"));
        }
        Ok(())
    }
}

use crate::{
    git::Git,
    locking::SlotGuard,
    state::{self, SlotPhase, SlotRecord},
    Pool, SlotId,
};
use std::{collections::BTreeMap, fs};

/// Latest background reconciliation observation. It never grants ownership.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReadyReport {
    /// Last attempt timestamp, used only for diagnostics/backoff.
    pub attempted_at_unix_ms: u64,
    /// Whether a reconciler is currently observed holding its lock.
    pub running: bool,
    /// Successfully inspected ready checkouts at the end of the pass.
    pub ready: usize,
    /// Slots whose preparation or cleanup could not safely complete.
    pub failures: BTreeMap<SlotId, String>,
    /// Failure affecting the entire pass, such as an invalid base revision.
    pub error: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Work {
    schema_version: u32,
    pool_id: Option<crate::PoolId>,
    lease_id: Option<crate::LeaseId>,
    phase: WorkPhase,
    oid: crate::CommitOid,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkPhase {
    Preparing,
    Ready,
    Evicting,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportState {
    schema_version: u32,
    pool_id: Option<crate::PoolId>,
    policy: Option<ReadySettings>,
    report: ReadyReport,
}
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    pool_id: Option<crate::PoolId>,
    nonce: String,
}
impl Pool {
    pub(crate) fn validate_ready_state(&self, slot: &SlotId) -> Result<()> {
        if self.config.ready.is_some() && self.ready_path(slot).try_exists()? {
            let work: Work = state::read(&self.ready_path(slot))?;
            if work.schema_version != state::SCHEMA_VERSION {
                return Err(Error::new(
                    ErrorCode::UnsupportedConfiguration,
                    "Unsupported ready slot state",
                ));
            }
        }
        Ok(())
    }
    /// Current generation when this pool enables any automatic checkout work.
    /// This observation does not acquire ownership or publish a work request.
    /// # Errors
    /// Reports unreadable settings or a replaced pool generation.
    pub fn ready_generation(&self) -> Result<Option<crate::PoolId>> {
        let current = self.refreshed()?;
        Ok(current
            .config
            .ready
            .as_ref()
            .filter(|p| p.ready_target > 0 || p.idle_timeout.0.is_some())
            .and(current.pool_id))
    }
    pub(crate) fn ready_hint(&self, slot: &SlotId) -> bool {
        state::read::<Work>(&self.ready_path(slot)).is_ok_and(|work| {
            work.schema_version == state::SCHEMA_VERSION
                && work.pool_id == self.pool_id
                && work.phase == WorkPhase::Ready
        })
    }
    fn ready_admin(&self) -> Result<SlotGuard> {
        SlotGuard::wait(
            &self.repo.admin_lock_path(),
            Some(self.config.admin_identity),
            self.git_timeout,
        )
    }
    fn ready_path(&self, slot: &SlotId) -> std::path::PathBuf {
        self.repo
            .root
            .join("slots")
            .join(format!("{slot}.ready.json"))
    }
    /// Request a background pass and return its immutable pool generation.
    /// The CLI launches its own executable; the core never starts a daemon.
    /// `None` means a legacy pool has no automatic policy.
    /// # Errors
    /// Reports lifecycle, lock or durable request publication failures.
    pub fn request_ready(&self) -> Result<Option<crate::PoolId>> {
        let _operation = self.operation()?;
        let _admin = self.ready_admin()?;
        let current = self.refreshed()?;
        if current.config.ready.is_none() {
            return Ok(None);
        }
        if current
            .config
            .ready
            .as_ref()
            .is_some_and(|p| p.ready_target == 0 && p.idle_timeout.0.is_none())
        {
            return Ok(None);
        }
        state::write(
            &self.repo.root.join("ready-request.json"),
            &Request {
                schema_version: state::SCHEMA_VERSION,
                pool_id: self.pool_id,
                nonce: uuid::Uuid::new_v4().to_string(),
            },
        )?;
        Ok(self.pool_id)
    }
    /// Read background diagnostics without starting work.
    /// # Errors
    /// Reports invalid metadata or lock identity changes.
    pub fn ready_report(&self) -> Result<ReadyReport> {
        let current = self.refreshed()?;
        let path = self.repo.root.join("ready-report.json");
        let mut report = if path.try_exists()? {
            let saved: ReportState = state::read(&path)?;
            if saved.schema_version != state::SCHEMA_VERSION {
                return Err(Error::new(
                    ErrorCode::UnsupportedConfiguration,
                    "Unsupported ready report",
                ));
            }
            if saved.pool_id == self.pool_id {
                saved.report
            } else {
                ReadyReport::default()
            }
        } else {
            ReadyReport::default()
        };
        if let Some(identity) = current.config.ready_lock {
            report.running =
                SlotGuard::try_lock(&self.repo.root.join("locks/ready.lock"), Some(identity))?
                    .is_none();
        }
        Ok(report)
    }
    /// Reconcile spare checkouts for exactly the supplied pool generation.
    /// Concurrent calls coalesce; a request arriving during a pass causes another
    /// pass before exit. Reservations and active descriptor owners are never reused.
    /// Libraries may call this on their own background thread after operations.
    /// # Errors
    /// Rejects stale generations, unsafe coordination state and I/O failures.
    /// Per-slot failures are retained in [`Self::ready_report`] and do not release tasks.
    pub fn run_ready(&self, generation: crate::PoolId) -> Result<()> {
        let _operation = self.operation()?;
        if self.pool_id != Some(generation) {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "Stale ready worker generation",
            ));
        }
        let admin = self.ready_admin()?;
        let current = self.refreshed()?;
        let Some(identity) = current.config.ready_lock else {
            return Ok(());
        };
        let Some(worker) =
            SlotGuard::try_lock(&self.repo.root.join("locks/ready.lock"), Some(identity))?
        else {
            return Ok(());
        };
        drop(admin);
        loop {
            // Never overwrite a report from a format this binary cannot read.
            self.ready_report()?;
            let request: Request = state::read(&self.repo.root.join("ready-request.json"))?;
            if request.schema_version != state::SCHEMA_VERSION || request.pool_id != self.pool_id {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    "Invalid ready request generation",
                ));
            }
            let current = self.refreshed()?;
            let mut report = ReadyReport {
                attempted_at_unix_ms: crate::reservation::now_ms(),
                ..Default::default()
            };
            if let Err(error) = current.ready_pass(&mut report) {
                report.error = Some(error.summary());
            }
            state::write(
                &self.repo.root.join("ready-report.json"),
                &ReportState {
                    schema_version: state::SCHEMA_VERSION,
                    pool_id: self.pool_id,
                    policy: current.config.ready.clone(),
                    report,
                },
            )?;
            state::checkpoint("ready-pass-completed");
            let admin = self.ready_admin()?;
            let latest: Request = state::read(&self.repo.root.join("ready-request.json"))?;
            if latest == request {
                // Request publication and final worker closure share admin exclusion:
                // a trigger either belongs to this pass or can start a new owner.
                drop(worker);
                drop(admin);
                return Ok(());
            }
        }
    }
    fn ready_pass(&self, report: &mut ReadyReport) -> Result<()> {
        let Some(policy) = &self.config.ready else {
            return Ok(());
        };
        if policy.ready_target == 0 && policy.idle_timeout.0.is_none() {
            return Ok(());
        }
        let previous: Option<ReportState> =
            if self.repo.root.join("ready-report.json").try_exists()? {
                Some(state::read(&self.repo.root.join("ready-report.json"))?)
            } else {
                None
            };
        let recent = previous.filter(|saved| {
            saved.schema_version == state::SCHEMA_VERSION
                && saved.pool_id == self.pool_id
                && saved.policy.as_ref() == Some(policy)
                && report
                    .attempted_at_unix_ms
                    .checked_sub(saved.report.attempted_at_unix_ms)
                    .is_some_and(|age| age < 30_000)
        });
        if let Some(saved) = &recent {
            if saved.report.error.is_some() {
                report.attempted_at_unix_ms = saved.report.attempted_at_unix_ms;
                report.error = saved.report.error.clone();
                return Ok(());
            }
        }
        let git = Git::new(self.git_timeout);
        let revision = format!("{}^{{commit}}", policy.ready_base);
        let raw = git.run(
            &self.repo.common,
            &["rev-parse", "--verify", "--end-of-options", &revision],
            &[],
        )?;
        let oid: crate::CommitOid = String::from_utf8_lossy(&raw).trim().parse()?;
        crate::recovery::validate_config(&git, &self.repo, &oid)?;
        // Keep the newest idle slots; ordering is advisory and revalidated under
        // ownership. No descriptors accumulate with pool capacity.
        let mut slots = Vec::new();
        for slot in self.config.slots.keys() {
            match state::read::<SlotRecord>(&self.repo.record_path(slot)) {
                Ok(record) => slots.push((slot.clone(), record.idle_since_unix_ms.unwrap_or(0))),
                Err(error) => {
                    report.failures.insert(slot.clone(), error.summary());
                }
            }
        }
        slots.sort_by_key(|(_, age)| std::cmp::Reverse(*age));
        for (slot, _) in slots {
            if let Some(failure) = recent
                .as_ref()
                .and_then(|saved| saved.report.failures.get(&slot))
            {
                report.attempted_at_unix_ms = recent
                    .as_ref()
                    .expect("failure came from recent report")
                    .report
                    .attempted_at_unix_ms;
                report.failures.insert(slot, failure.clone());
                continue;
            }
            let outcome = self.ready_slot(&slot, &oid, policy, report.ready);
            match outcome {
                Ok(true) => report.ready += 1,
                Ok(false) => (),
                Err(error) => {
                    report.failures.insert(slot, error.summary());
                }
            }
        }
        Ok(())
    }
    fn ready_slot(
        &self,
        slot: &SlotId,
        oid: &crate::CommitOid,
        policy: &ReadySettings,
        retained: usize,
    ) -> Result<bool> {
        let protection = self.repo.protection_path(slot);
        if fs::symlink_metadata(&protection).is_ok() {
            return Ok(false);
        }
        let Some(guard) = SlotGuard::try_lock(
            &self.repo.lock_path(slot),
            self.config.slots.get(slot).copied(),
        )?
        else {
            return Ok(false);
        };
        if protection.try_exists()? {
            return Ok(false);
        }
        let _execution = self.execution_guard(slot)?;
        self.require_execution_clear(slot)?;
        let mut record: SlotRecord = state::read(&self.repo.record_path(slot))?;
        if record.schema_version != state::SCHEMA_VERSION {
            return Err(Error::new(
                ErrorCode::UnsupportedConfiguration,
                "Unsupported slot record",
            ));
        }
        if !matches!(record.phase, SlotPhase::Available | SlotPhase::Released) {
            return Ok(false);
        }
        crate::recovery::validate_slot_path(&self.repo, &self.config, slot)?;
        let path = self.config.worktree_root.join(slot.as_str());
        let work_path = self.ready_path(slot);
        let prior: Option<Work> = if work_path.try_exists()? {
            Some(state::read(&work_path)?)
        } else {
            None
        };
        if prior
            .as_ref()
            .is_some_and(|work| work.schema_version != state::SCHEMA_VERSION)
        {
            return Err(Error::new(
                ErrorCode::UnsupportedConfiguration,
                "Unsupported ready slot state",
            ));
        }
        let lease_id = record.lease.as_ref().map(|lease| lease.lease_id);
        let prior = prior.filter(|work| work.pool_id == self.pool_id && work.lease_id == lease_id);
        let now = crate::reservation::now_ms();
        let expired = policy.idle_timeout.0.is_some_and(|timeout| {
            record.idle_since_unix_ms.is_some_and(|idle_since| {
                now.checked_sub(idle_since)
                    .is_some_and(|age| age >= timeout.as_millis() as u64)
            })
        });
        let evicting = prior
            .as_ref()
            .is_some_and(|work| work.phase == WorkPhase::Evicting);
        // Once the spare target is met, excess checkouts need no inspection or
        // normalization until eviction is due. In particular, pristine cleanup
        // must not erase their ignored caches merely because they were released.
        if retained >= policy.ready_target && !expired && !evicting {
            return Ok(false);
        }
        if path.try_exists()? && !crate::quiescence::all_processes_using(&path)?.is_empty() {
            return Err(Error::new(
                ErrorCode::WorkspaceInUse,
                "Workspace still has external users",
            ));
        }
        if path.try_exists()? {
            use std::os::unix::fs::MetadataExt;
            crate::destruction::validate_tree(
                &path,
                fs::metadata(&self.config.worktree_root)?.dev(),
            )?;
        }
        // Recover only operations whose disposal/preparation intent was recorded
        // while the slot was owned and its contents were independently checked.
        let interrupted = prior
            .as_ref()
            .is_some_and(|work| matches!(work.phase, WorkPhase::Preparing | WorkPhase::Evicting));
        if path.try_exists()? && !interrupted {
            crate::recovery::registration(&self.repo, &path)?;
            let git = Git::new(self.git_timeout);
            let index = git.run(&path, &["ls-files", "-v", "-z"], &[])?;
            if index
                .split(|b| *b == 0)
                .filter(|entry| !entry.is_empty())
                .any(|entry| entry.first() != Some(&b'H'))
            {
                return Err(Error::new(
                    ErrorCode::UnsupportedConfiguration,
                    "Idle checkout has index flags or conflicts that can hide changes",
                ));
            }
            let head = git.run(&path, &["rev-parse", "--verify", "HEAD"], &[])?;
            let expected_head = prior
                .as_ref()
                .map(|work| &work.oid)
                .or(record.released_head.as_ref());
            if expected_head
                .is_some_and(|expected| String::from_utf8_lossy(&head).trim() != expected.as_str())
            {
                return Err(Error::new(
                    ErrorCode::UnsavedChanges,
                    "Idle checkout HEAD changed after release or preparation",
                ));
            }
            if record.phase == SlotPhase::Released
                && record.idle_since_unix_ms.is_some()
                && expected_head.is_none()
            {
                return Err(Error::new(
                    ErrorCode::RecoveryFailed,
                    "Release could not record a safe HEAD for background preparation",
                ));
            }
            let changes = self.git_status(slot, &path, self.git_timeout)?;
            if changes.error.is_some() {
                return Err(Error::new(
                    ErrorCode::RecoveryFailed,
                    "Cannot inspect idle checkout",
                ));
            }
            if changes.has_changes()
                && !match &record.lease {
                    Some(lease) => crate::finish::released_files_saved(self, lease)?,
                    None => false,
                }
            {
                return Err(Error::new(
                    ErrorCode::UnsavedChanges,
                    "Idle checkout changed after release; preserve its contents before reuse",
                ));
            }
        }
        if !path.try_exists()? {
            record.idle_since_unix_ms = Some(now);
        }
        record.idle_since_unix_ms.get_or_insert(now);
        if evicting || (retained >= policy.ready_target && expired && path.try_exists()?) {
            let _admin = self.ready_admin()?;
            crate::recovery::validate_disposal(&self.repo, &path)?;
            state::write(
                &work_path,
                &Work {
                    schema_version: state::SCHEMA_VERSION,
                    pool_id: self.pool_id,
                    lease_id,
                    phase: WorkPhase::Evicting,
                    oid: oid.clone(),
                },
            )?;
            state::checkpoint("ready-before-eviction");
            crate::recovery::remove_owned(&self.repo, &path)?;
            fs::remove_file(&work_path)?;
            state::sync_dir(work_path.parent().expect("slot state parent"))?;
            if retained >= policy.ready_target {
                return Ok(false);
            }
            // A changed target may need this slot again. Complete the recorded
            // eviction, then replenish in this pass after admin exclusion ends.
            record.idle_since_unix_ms = Some(now);
        }
        if !path.try_exists()? && retained >= policy.ready_target {
            return Ok(false);
        }
        if prior
            .as_ref()
            .is_some_and(|work| work.phase == WorkPhase::Ready && work.oid == *oid)
            && crate::recovery::verify(
                &Git::new(self.git_timeout),
                &self.repo,
                &path,
                oid,
                self.config.cleanup_policy,
            )
            .is_ok()
        {
            return Ok(true);
        }
        state::write(&self.repo.record_path(slot), &record)?;
        state::write(
            &work_path,
            &Work {
                schema_version: state::SCHEMA_VERSION,
                pool_id: self.pool_id,
                lease_id,
                phase: WorkPhase::Preparing,
                oid: oid.clone(),
            },
        )?;
        state::checkpoint("ready-before-preparation");
        crate::recovery::prepare(
            &self.repo,
            &self.config,
            slot,
            &guard,
            oid,
            self.git_timeout,
        )?;
        state::write(
            &work_path,
            &Work {
                schema_version: state::SCHEMA_VERSION,
                pool_id: self.pool_id,
                lease_id,
                phase: WorkPhase::Ready,
                oid: oid.clone(),
            },
        )?;
        Ok(true)
    }
}
