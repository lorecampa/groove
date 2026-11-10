//! Read-only pool observations. Nothing here authorizes reuse or proves ownership.

use crate::{
    locking::{Identity, SlotGuard},
    state::{self, SlotPhase, SlotRecord},
    Error, ErrorCode, LeaseInfo, Pool, PreparationStatus, Result, SlotId,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Duration};

/// Diagnostic state observed while inspecting a slot; it can change after the response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SlotStatus {
    /// Unlocked, unreserved; prepared again on acquisition.
    Available,
    /// A durable reservation prohibits reuse.
    Reserved,
    /// A descriptor owner currently retains the slot lock.
    Locked,
    /// Preparation previously failed.
    Quarantined,
    /// Metadata could not be interpreted safely.
    Unhealthy,
}

/// One slot's diagnostic snapshot. PIDs and timestamps never authorize reclamation.
#[derive(Debug, Serialize, Deserialize)]
pub struct SlotInspection {
    /// Permanent slot identifier.
    pub slot_id: SlotId,
    /// Observation of lock/protection state.
    pub status: SlotStatus,
    /// Reusable directory, which must not be used without acquisition.
    pub worktree_path: PathBuf,
    /// Current or most recent reservation metadata.
    pub lease: Option<LeaseInfo>,
    /// Whether an independent lock probe observed an owner.
    pub lock_held: bool,
    /// Managed execution metadata, including orphaned execution prohibitions.
    pub execution: Option<crate::ExecutionInfo>,
    /// Metadata or preparation failure, if any.
    pub failure: Option<String>,
    /// Current-generation explicit project readiness, absent when not requested.
    pub preparation: Option<crate::Preparation>,
    /// Human-readable explanation derived from observed ownership and readiness.
    pub reason: String,
    /// Suggested explicit next action; never permission for automatic reclamation.
    pub next_action: String,
    /// Optional bounded local Git facts, collected only by detailed inspection.
    pub git: Option<WorkspaceGitStatus>,
}

/// Optional read-only Git facts. Errors are explicit rather than reported as clean.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGitStatus {
    /// Whether the index has changes.
    pub staged: bool,
    /// Whether tracked working files have changes.
    pub modified: bool,
    /// Whether non-ignored untracked files are present.
    pub untracked: bool,
    /// Failure collecting facts; other fields are unknown when this is present.
    pub error: Option<String>,
}
impl WorkspaceGitStatus {
    /// Whether anything would be lost: staged, modified or untracked files.
    pub fn has_changes(&self) -> bool {
        self.staged || self.modified || self.untracked
    }
}

/// A complete, non-transactional pool observation.
#[derive(Debug, Serialize, Deserialize)]
pub struct PoolInspection {
    /// Configured slot count, not a process or resource limit.
    pub capacity: usize,
    /// Per-slot state and task context.
    pub slots: Vec<SlotInspection>,
}

/// Read an optional record: a missing file is `None`, anything else unreadable is an error.
fn read_optional<T: serde::de::DeserializeOwned>(
    path: &std::path::Path,
    limit: u64,
) -> Result<Option<T>> {
    match state::read_bounded::<T>(path, limit) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.code == ErrorCode::IoError && !path.try_exists().unwrap_or(true) => {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

/// Collects the first metadata failure seen while observing a slot.
#[derive(Default)]
struct Failure(Option<String>);
impl Failure {
    fn keep<T>(&mut self, result: Result<T>) -> Option<T> {
        result
            .map_err(|error| {
                self.0.get_or_insert(error.to_string());
            })
            .ok()
    }
    fn note(&mut self, message: &str) {
        self.0.get_or_insert_with(|| message.to_owned());
    }
}

/// Explanation and next step for an observed slot.
fn advice(
    status: &SlotStatus,
    execution: bool,
    preparation: Option<PreparationStatus>,
) -> (&'static str, &'static str) {
    match (status, execution, preparation) {
        (SlotStatus::Unhealthy | SlotStatus::Quarantined, _, _) => (
            "Workspace metadata or checkout needs attention",
            "Inspect the failure; retain reservations and recover only after stopping writers",
        ),
        (_, true, _) => (
            "Managed execution is active or awaiting orphan recovery",
            "Inspect logs; stop the execution, or recover it after all writers stop",
        ),
        (_, _, Some(PreparationStatus::Preparing)) => (
            "Project setup is running or was interrupted",
            "Inspect setup; stop it if live, or explicitly retry after writer recovery",
        ),
        (_, _, Some(PreparationStatus::Ready)) => (
            "Project setup completed for this reservation; named commands revalidate its definition",
            "Run a task; finish after preserving results and stopping writers",
        ),
        (_, _, Some(PreparationStatus::Failed)) => (
            "Project setup failed; the reservation remains protected",
            "Inspect setup logs and explicitly retry, or preserve results and release",
        ),
        (SlotStatus::Reserved, _, None) => (
            "Reserved checkout; project setup has not been requested",
            "Explicitly run setup or a task; finish after saving results and stopping writers",
        ),
        (SlotStatus::Locked, _, None) => (
            "A descriptor owner is using or preparing this workspace",
            "Wait for its owner; metadata and age never authorize reclaiming it",
        ),
        (SlotStatus::Available, _, None) => (
            "Available for a new task; preparation runs on acquisition",
            "Acquire before editing or executing project commands",
        ),
    }
}

impl Pool {
    /// Inspect every slot without interpreting metadata, timestamps or PIDs as ownership.
    ///
    /// # Errors
    /// Reports configuration failures; per-slot failures are included in the diagnostic result.
    pub fn inspect(&self) -> Result<PoolInspection> {
        let pool = self.refreshed()?;
        let slots = pool
            .config
            .slots
            .iter()
            .map(|(slot, identity)| pool.observe_slot(slot, *identity))
            .collect();
        Ok(PoolInspection {
            capacity: pool.config.capacity,
            slots,
        })
    }

    /// Inspect one slot without probing other slots or running Git status.
    ///
    /// This is a nontransactional observation, never authorization to reclaim a
    /// workspace. The manifest is refreshed so newly added slots can be selected.
    ///
    /// # Errors
    /// Reports configuration failures and rejects slots outside this pool.
    /// Per-slot failures are included in the diagnostic result.
    ///
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// let pool = groove_core::Pool::open(".")?;
    /// let slot = "wt-0001".parse()?;
    /// let observation = pool.inspect_slot(&slot)?;
    /// println!("{}", observation.reason);
    /// # Ok(())
    /// # }
    /// ```
    pub fn inspect_slot(&self, slot: &SlotId) -> Result<SlotInspection> {
        let pool = self.refreshed()?;
        let identity = pool.config.slots.get(slot).ok_or_else(|| {
            Error::new(ErrorCode::InvalidArgument, "Slot is not part of this pool")
        })?;
        Ok(pool.observe_slot(slot, *identity))
    }

    fn observe_slot(&self, slot: &SlotId, identity: Identity) -> SlotInspection {
        let mut failure = Failure::default();
        let lock_held = failure
            .keep(SlotGuard::try_lock(
                &self.repo.lock_path(slot),
                Some(identity),
            ))
            .is_some_and(|guard| guard.is_none());
        let record = failure
            .keep(state::read::<SlotRecord>(&self.repo.record_path(slot)))
            .filter(|record| {
                let supported = record.schema_version == state::SCHEMA_VERSION;
                if !supported {
                    failure.note("Unsupported slot record");
                }
                supported
            });
        let mut lease = record.as_ref().and_then(|r| r.lease.clone());
        let protection_path = self.repo.protection_path(slot);
        let protected = match fs::symlink_metadata(&protection_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => {
                failure.note(&e.to_string());
                true
            }
            Ok(_) => {
                match failure.keep(state::read::<state::ReuseProtection>(&protection_path)) {
                    Some(p)
                        if p.schema_version == state::SCHEMA_VERSION
                            && p.lease.slot_id == *slot =>
                    {
                        lease = Some(p.lease)
                    }
                    Some(_) => failure.note("Invalid protection record"),
                    None => {}
                }
                true
            }
        };
        let execution = failure
            .keep(read_optional::<crate::ExecutionInfo>(
                &self.repo.execution_path(slot),
                crate::preparation::RECORD_LIMIT,
            ))
            .flatten();
        let preparation = failure
            .keep(read_optional::<crate::Preparation>(
                &self.preparation_path(slot),
                crate::preparation::RECORD_LIMIT,
            ))
            .flatten()
            .filter(|preparation| {
                if preparation.schema_version != state::SCHEMA_VERSION {
                    failure.note("Unsupported setup metadata");
                    return false;
                }
                protected
                    && lease
                        .as_ref()
                        .is_some_and(|lease| lease.lease_id == preparation.lease_id)
            });
        let status = if failure.0.is_some() {
            SlotStatus::Unhealthy
        } else if lock_held {
            SlotStatus::Locked
        } else if protected {
            SlotStatus::Reserved
        } else if record
            .as_ref()
            .is_some_and(|r| r.phase == SlotPhase::Quarantined)
        {
            SlotStatus::Quarantined
        } else {
            SlotStatus::Available
        };
        let failure = failure.0.or_else(|| record.and_then(|r| r.failure));
        let (reason, next_action) = advice(
            &status,
            execution.is_some(),
            preparation.as_ref().map(|p| p.status),
        );
        SlotInspection {
            slot_id: slot.clone(),
            status,
            worktree_path: self.config.worktree_root.join(slot.as_str()),
            lease,
            lock_held,
            execution,
            failure,
            preparation,
            reason: reason.into(),
            next_action: next_action.into(),
            git: None,
        }
    }

    /// Inspect slots plus explicitly requested bounded local Git working-tree facts.
    /// Does not contact a forge, invoke hooks, modify an index or infer ownership.
    ///
    /// # Errors
    /// Reports pool configuration failures; individual Git failures remain in each slot's facts.
    pub fn inspect_detailed(&self, timeout: Duration) -> Result<PoolInspection> {
        if timeout.is_zero() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Inspection timeout must be positive",
            ));
        }
        let mut inspection = self.inspect()?;
        for slot in &mut inspection.slots {
            self.add_git_status(slot, timeout);
        }
        Ok(inspection)
    }

    /// Inspect one slot and collect bounded local Git status only for that slot.
    ///
    /// Does not contact a remote, invoke hooks, modify an index or infer ownership.
    /// Unmaterialized slots have no Git facts; Git failures are recorded in `git.error`.
    ///
    /// # Errors
    /// Rejects a zero timeout, unknown slots and invalid pool configuration.
    ///
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// let pool = groove_core::Pool::open(".")?;
    /// let slot = "wt-0001".parse()?;
    /// let observation = pool.inspect_slot_detailed(&slot, std::time::Duration::from_secs(5))?;
    /// if let Some(git) = observation.git {
    ///     println!("changes: {}, error: {:?}", git.has_changes(), git.error);
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn inspect_slot_detailed(
        &self,
        slot: &SlotId,
        timeout: Duration,
    ) -> Result<SlotInspection> {
        if timeout.is_zero() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Inspection timeout must be positive",
            ));
        }
        let mut inspection = self.inspect_slot(slot)?;
        self.add_git_status(&mut inspection, timeout);
        Ok(inspection)
    }

    fn add_git_status(&self, slot: &mut SlotInspection, timeout: Duration) {
        if slot.worktree_path.exists() {
            slot.git = Some(
                self.git_status(&slot.slot_id, &slot.worktree_path, timeout)
                    .unwrap_or_else(|error| WorkspaceGitStatus {
                        error: Some(error.to_string()),
                        ..WorkspaceGitStatus::default()
                    }),
            );
        }
    }

    /// Bounded, read-only working-tree facts. Tuned for large repositories: no rename
    /// detection, no ignored-file scan, untracked directories reported without being
    /// walked, no ahead/behind computation, and no index write (optional locks off).
    pub(crate) fn git_status(
        &self,
        slot: &SlotId,
        path: &std::path::Path,
        timeout: Duration,
    ) -> Result<WorkspaceGitStatus> {
        self.validate_workspace(slot)?;
        let bytes = crate::git::Git::new(timeout).run(
            path,
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=normal",
                "--ignored=no",
                "--no-renames",
                "--no-ahead-behind",
                "--ignore-submodules=all",
            ],
            &[],
        )?;
        let mut facts = WorkspaceGitStatus::default();
        for entry in bytes.split(|byte| *byte == 0).filter(|e| !e.is_empty()) {
            if entry.len() < 3 {
                return Err(Error::new(
                    ErrorCode::GitError,
                    "Invalid Git status response",
                ));
            }
            if &entry[..2] == b"??" {
                facts.untracked = true;
            } else {
                facts.staged |= entry[0] != b' ';
                facts.modified |= entry[1] != b' ';
            }
        }
        Ok(facts)
    }

    /// Resolve a materialized permanent workspace for navigation only.
    /// Returning a path never grants a reservation or authorizes edits.
    ///
    /// # Errors
    /// Rejects unknown/unmaterialized slots, changed roots and invalid native registrations.
    pub fn workspace_path(&self, slot: &SlotId) -> Result<PathBuf> {
        self.validate_workspace(slot)
    }

    /// Validate that `slot` is a known, materialized, natively registered workspace.
    fn validate_workspace(&self, slot: &SlotId) -> Result<PathBuf> {
        let current = self.refreshed()?;
        if !current.config.slots.contains_key(slot) {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Unknown workspace slot",
            ));
        }
        crate::recovery::validate_slot_path(&current.repo, &current.config, slot)?;
        let path = current.config.worktree_root.join(slot.as_str());
        crate::recovery::registration(&current.repo, &path)?;
        Ok(path)
    }
}
