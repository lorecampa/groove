//! Finishing a reservation: pin a checkpoint and release.

use crate::{
    snapshot::{SaveOptions, Snapshot, Snapshots},
    state::{self, SlotPhase, SlotRecord},
    Error, ErrorCode, Lease, LeaseId, Pool, ReleaseResult, Result, SlotId,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Crash-safe record of a finish: written after the snapshot is pinned, updated after release.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct FinishReceipt {
    schema_version: u32,
    lease_id: LeaseId,
    slot_id: SlotId,
    snapshot: Snapshot,
    released: bool,
    /// Cleanup journal retained until every superseded pin has been retired.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    superseded: Vec<String>,
}

/// A pinned checkpoint and its completed release.
#[derive(Debug, Serialize, Deserialize)]
pub struct FinishResult {
    /// Release receipt.
    pub release: ReleaseResult,
    /// Pinned local checkpoint, independently recoverable after slot reuse.
    pub snapshot: Snapshot,
}

fn receipt_path(pool: &Pool, id: LeaseId) -> PathBuf {
    receipt_path_in(&pool.repo, id)
}

fn receipt_path_in(repo: &crate::repository::Repository, id: LeaseId) -> PathBuf {
    repo.root.join("slots").join(format!("finish-{id}.json"))
}

/// Called under exclusive lifecycle admission after durable deletion. Permanent
/// locks and request tombstones are retained; they protect ownership and retries.
pub(crate) fn prune_missing_receipts(
    repo: &crate::repository::Repository,
    timeout: std::time::Duration,
) -> Result<()> {
    let slots = repo.root.join("slots");
    if !slots.try_exists()? {
        return Ok(());
    }
    crate::repository::owned_dir(&slots)?;
    let candidates = std::fs::read_dir(&slots)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    if !candidates.iter().any(|path| {
        path.file_name()
            .is_some_and(|name| name.as_encoded_bytes().starts_with(b"finish-"))
    }) {
        return Ok(());
    }
    let retained = Snapshots::retained_ids(repo, timeout)?;
    let mut removed = false;
    for path in candidates {
        let Some(id) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("finish-"))
            .and_then(|name| name.strip_suffix(".json"))
            .and_then(|id| id.parse::<LeaseId>().ok())
        else {
            continue;
        };
        let receipt = match state::read::<FinishReceipt>(&path) {
            Ok(receipt) => receipt,
            // Unknown or damaged records cannot establish that cleanup is safe.
            Err(_) => continue,
        };
        if receipt.schema_version == state::SCHEMA_VERSION
            && receipt.lease_id == id
            && receipt.released
            && receipt.superseded.is_empty()
            && !retained.contains(&receipt.snapshot.id)
        {
            std::fs::remove_file(path)?;
            removed = true;
        }
    }
    if removed {
        state::sync_dir(&slots)?;
    }
    Ok(())
}

fn retire_superseded(pool: &Pool, receipt: &mut FinishReceipt) -> Result<()> {
    for id in &receipt.superseded {
        if *id == receipt.snapshot.id {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Current saved release cannot supersede itself",
            ));
        }
        Snapshots::unpin_saved_release_snapshot(&pool.repo, pool.git_timeout, id)?;
    }
    receipt.superseded.clear();
    state::write(&receipt_path(pool, receipt.lease_id), receipt)
}

pub(crate) fn released_files_saved(pool: &Pool, lease: &crate::LeaseInfo) -> Result<bool> {
    released_files_saved_in(&pool.repo, pool.git_timeout, lease, false)
}

pub(crate) fn released_files_saved_in(
    repo: &crate::repository::Repository,
    timeout: std::time::Duration,
    lease: &crate::LeaseInfo,
    admitted: bool,
) -> Result<bool> {
    let path = receipt_path_in(repo, lease.lease_id);
    if !path.try_exists()? {
        return Ok(false);
    }
    let receipt: FinishReceipt = state::read(&path)?;
    if receipt.schema_version != state::SCHEMA_VERSION
        || receipt.lease_id != lease.lease_id
        || receipt.slot_id != lease.slot_id
    {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Invalid saved release receipt",
        ));
    }
    let snapshots = if admitted {
        Snapshots::open_admitted(&lease.worktree_path, timeout)?
    } else {
        Snapshots::open(&lease.worktree_path, timeout)?
    };
    snapshots.matches_saved_files(&receipt.snapshot.id)
}

impl Lease {
    /// Save and pin a local checkpoint before releasing this reservation.
    ///
    /// Stop all external writers before calling. A failed checkpoint leaves the
    /// reservation protected. Until release completes, each retry captures the
    /// current files, including edits made after a failed attempt.
    ///
    /// # Errors
    /// Reports protection, legacy execution, snapshot, receipt or release failures.
    /// If release fails after capture, error details include the saved snapshot ID.
    /// Earlier failures may occur before a checkpoint exists. A receipt write can
    /// also fail after release has completed; retry through [`Pool::finish`] to
    /// recover the result.
    pub fn finish(&mut self, options: SaveOptions) -> Result<FinishResult> {
        let _operation = self.pool.operation()?;
        self.protect_from_reuse()?;
        let execution = self.pool.execution_guard(&self.info.slot_id)?;
        self.pool.require_execution_clear(&self.info.slot_id)?;
        let path = receipt_path(&self.pool, self.info.lease_id);
        let previous = match state::read::<FinishReceipt>(&path) {
            Ok(receipt)
                if receipt.schema_version == state::SCHEMA_VERSION
                    && receipt.lease_id == self.info.lease_id
                    && receipt.slot_id == self.info.slot_id =>
            {
                Some(receipt)
            }
            Ok(_) => {
                return Err(Error::new(
                    ErrorCode::ProtocolError,
                    "Invalid finish receipt",
                ))
            }
            Err(e) if !path.exists() && e.code == ErrorCode::IoError => None,
            Err(e) => return Err(e),
        };
        // A receipt is history, not proof that this still-reserved checkout is
        // unchanged or that its old snapshot still exists. Re-capture under the
        // slot/execution guards before authorizing reuse. Journal older pins so
        // a crash during retry or cleanup cannot forget the pending retirement.
        crate::quiescence::missing_checkout(&self.info.worktree_path)?;
        let snapshots = Snapshots::open(&self.info.worktree_path, self.pool.git_timeout)?;
        let mut snapshot = snapshots.save_for_release(options)?;
        snapshot.pinned = true;
        let mut receipt = FinishReceipt {
            schema_version: state::SCHEMA_VERSION,
            lease_id: self.info.lease_id,
            slot_id: self.info.slot_id.clone(),
            snapshot,
            released: false,
            superseded: previous
                .map(|mut old| {
                    old.superseded.push(old.snapshot.id);
                    old.superseded
                })
                .unwrap_or_default(),
        };
        state::write(&path, &receipt)?;
        state::checkpoint("finish-before-pin");
        snapshots.pin(&receipt.snapshot.id)?;
        state::checkpoint("finish-snapshot-saved");
        drop(execution);
        state::checkpoint("finish-before-release");
        let release = self
            .release()
            .map_err(|e| e.with_details(|d| d.snapshot_id = Some(receipt.snapshot.id.clone())))?;
        state::checkpoint("finish-released");
        receipt.released = true;
        state::write(&path, &receipt)?;
        state::checkpoint("finish-before-pin-retirement");
        retire_superseded(&self.pool, &mut receipt).map_err(|error| {
            error.with_details(|details| {
                details.released = Some(true);
                details.snapshot_id = Some(receipt.snapshot.id.clone());
            })
        })?;
        Ok(FinishResult {
            release,
            snapshot: receipt.snapshot,
        })
    }
}

impl Pool {
    /// Whether this pool retains a validated saved-release receipt for `id`.
    ///
    /// This is a read-only routing hint, never proof of descriptor ownership or
    /// authorization to release a successor. [`Self::finish`] rechecks recovery
    /// and generation state when a caller retries the saved release.
    ///
    /// # Errors
    /// Reports filesystem failures and malformed or mismatched receipt records.
    pub fn has_saved_release(&self, id: LeaseId) -> Result<bool> {
        let path = receipt_path(self, id);
        if !path.try_exists()? {
            return Ok(false);
        }
        let receipt: FinishReceipt = state::read(&path)?;
        if receipt.schema_version != state::SCHEMA_VERSION || receipt.lease_id != id {
            return Err(Error::new(
                ErrorCode::RecoveryFailed,
                "Invalid saved release receipt",
            ));
        }
        Ok(true)
    }

    /// Save a pinned checkpoint and release the reservation identified by `id`.
    ///
    /// Stop all external writers before calling. While the checkout remains
    /// reserved, retries capture its current files again. Once release completes,
    /// retries return the saved result without touching a successor's workspace.
    ///
    /// ```no_run
    /// use groove_core::{snapshot::SaveOptions, Pool};
    /// let pool = Pool::open("/path/to/repo")?;
    /// # let id = pool.reserve(Default::default())?.lease_id;
    /// // After every writer in the workspace has stopped:
    /// let finished = pool.finish(id, SaveOptions::default())?;
    /// println!("recover with snapshot {}", finished.snapshot.id);
    /// # Ok::<(), groove_core::Error>(())
    /// ```
    ///
    /// # Errors
    /// Rejects active descriptor owners, invalid IDs and failed captures. Completed
    /// retries fail if their saved snapshot is missing or no longer pinned. Errors
    /// while checking a completed release do not restore its reservation; snapshot
    /// validation errors include `snapshot_id` and `released: true` in their details.
    pub fn finish(&self, id: LeaseId, options: SaveOptions) -> Result<FinishResult> {
        let _operation = self.retained_operation()?;
        match self.completed_finish(id)? {
            Some(result) => Ok(result),
            None => self.hold_for_finish(id)?.finish(options),
        }
    }

    /// Return a completed release, or `None` when the caller must capture again.
    /// A saved receipt cannot prove that a still-reserved checkout is unchanged.
    fn completed_finish(&self, id: LeaseId) -> Result<Option<FinishResult>> {
        let path = receipt_path(self, id);
        if !path.exists() {
            return Ok(None);
        }
        let mut receipt: FinishReceipt = state::read(&path)?;
        if receipt.schema_version != state::SCHEMA_VERSION || receipt.lease_id != id {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Invalid finish receipt",
            ));
        }
        let record: SlotRecord = state::read(&self.repo.record_path(&receipt.slot_id))?;
        let current_generation = record.lease.as_ref().map(|i| i.lease_id) == Some(id);
        // A successor proves release completed, even if the process died before
        // publishing the final receipt. Return history without touching its lock.
        let deleted = crate::lifecycle::read(&self.repo)?
            .is_some_and(|r| r.phase == crate::lifecycle::Phase::Deleted);
        let release = if receipt.released
            || !current_generation
            || (deleted && record.phase == SlotPhase::Released)
        {
            ReleaseResult {
                slot_id: receipt.slot_id.clone(),
                lease_id: id,
                released: true,
                cleanup: crate::CleanupTiming::Deferred,
            }
        } else if record.phase == SlotPhase::Released {
            // Release records its result before removing protection. A crash
            // between those writes leaves a reservation whose files can change.
            // Reacquire its verified slot lock and capture again before reuse.
            if self.repo.protection_path(&receipt.slot_id).try_exists()? {
                return Ok(None);
            }
            self.release_lease(id)?
        } else {
            return Ok(None);
        };
        // Completed retries are historical: never capture or mutate a successor.
        // Verify recoverability instead of claiming a deleted snapshot is pinned.
        receipt.snapshot =
            Snapshots::saved_release_snapshot(&self.repo, self.git_timeout, &receipt.snapshot.id)
                .map_err(|e| {
                e.with_details(|d| {
                    d.snapshot_id = Some(receipt.snapshot.id.clone());
                    d.released = Some(true);
                })
            })?;
        receipt.released = true;
        state::write(&path, &receipt)?;
        retire_superseded(self, &mut receipt).map_err(|error| {
            error.with_details(|details| {
                details.released = Some(true);
                details.snapshot_id = Some(receipt.snapshot.id.clone());
            })
        })?;
        Ok(Some(FinishResult {
            release,
            snapshot: receipt.snapshot,
        }))
    }
}
