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
    pool.repo
        .root
        .join("slots")
        .join(format!("finish-{id}.json"))
}

pub(crate) fn released_files_saved(pool: &Pool, lease: &crate::LeaseInfo) -> Result<bool> {
    let path = receipt_path(pool, lease.lease_id);
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
    Snapshots::open(&lease.worktree_path, pool.git_timeout)?
        .matches_saved_files(&receipt.snapshot.id)
}

impl Lease {
    /// Save and pin a local checkpoint before releasing this reservation.
    /// A failed checkpoint leaves the reservation protected. Until release completes,
    /// each retry saves the current files again, preserving edits made after a failed attempt.
    /// All external writers must have stopped before calling.
    ///
    /// # Errors
    /// Reports legacy execution protection, snapshot or release failures. Error details retain a saved snapshot ID.
    pub fn finish(&mut self, options: SaveOptions) -> Result<FinishResult> {
        let _operation = self.pool.operation()?;
        self.protect_from_reuse()?;
        let execution = self.pool.execution_guard(&self.info.slot_id)?;
        self.pool.require_execution_clear(&self.info.slot_id)?;
        let path = receipt_path(&self.pool, self.info.lease_id);
        match state::read::<FinishReceipt>(&path) {
            Ok(receipt)
                if receipt.schema_version == state::SCHEMA_VERSION
                    && receipt.lease_id == self.info.lease_id
                    && receipt.slot_id == self.info.slot_id => {}
            Ok(_) => {
                return Err(Error::new(
                    ErrorCode::ProtocolError,
                    "Invalid finish receipt",
                ))
            }
            Err(e) if !path.exists() && e.code == ErrorCode::IoError => {}
            Err(e) => return Err(e),
        }
        // A receipt is history, not proof that this still-reserved checkout is
        // unchanged or that its old snapshot still exists. Re-capture under the
        // slot/execution guards before authorizing reuse. Retain older pins.
        crate::quiescence::missing_checkout(&self.info.worktree_path)?;
        let snapshots = Snapshots::open(&self.info.worktree_path, self.pool.git_timeout)?;
        let mut snapshot = snapshots.save(options)?;
        snapshots.pin(&snapshot.id)?;
        snapshot.pinned = true;
        let mut receipt = FinishReceipt {
            schema_version: state::SCHEMA_VERSION,
            lease_id: self.info.lease_id,
            slot_id: self.info.slot_id.clone(),
            snapshot,
            released: false,
        };
        state::write(&path, &receipt)?;
        state::checkpoint("finish-snapshot-saved");
        drop(execution);
        state::checkpoint("finish-before-release");
        let release = self
            .release()
            .map_err(|e| e.with_details(|d| d.snapshot_id = Some(receipt.snapshot.id.clone())))?;
        state::checkpoint("finish-released");
        receipt.released = true;
        state::write(&path, &receipt)?;
        Ok(FinishResult {
            release,
            snapshot: receipt.snapshot,
        })
    }
}

impl Pool {
    /// Checkpoint and release by lease ID.
    /// Retries reuse completed finish receipts, so a lost response is safe to retry.
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
    /// Rejects active owners, invalid IDs or failed snapshots without authorizing reuse.
    pub fn finish(&self, id: LeaseId, options: SaveOptions) -> Result<FinishResult> {
        let _operation = self.retained_operation()?;
        match self.completed_finish(id)? {
            Some(result) => Ok(result),
            None => self.hold_for_finish(id)?.finish(options),
        }
    }

    /// Resume a finish whose checkpoint was already saved, completing release if needed.
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
        Ok(Some(FinishResult {
            release,
            snapshot: receipt.snapshot,
        }))
    }
}
