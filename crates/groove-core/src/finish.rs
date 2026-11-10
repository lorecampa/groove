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

impl Lease {
    /// Save and pin a local checkpoint before releasing this reservation.
    /// A failed checkpoint leaves the reservation protected; retries reuse its saved checkpoint.
    /// All external writers must have stopped before calling.
    ///
    /// # Errors
    /// Reports active managed execution, snapshot or release failures. Error details retain a saved snapshot ID.
    pub fn finish(&mut self, options: SaveOptions) -> Result<FinishResult> {
        self.protect_from_reuse()?;
        let execution = self.pool.execution_guard(&self.info.slot_id)?;
        self.pool.require_execution_clear(&self.info.slot_id)?;
        let path = receipt_path(&self.pool, self.info.lease_id);
        let mut receipt = match state::read::<FinishReceipt>(&path) {
            Ok(receipt)
                if receipt.schema_version == state::SCHEMA_VERSION
                    && receipt.lease_id == self.info.lease_id =>
            {
                receipt
            }
            Ok(_) => {
                return Err(Error::new(
                    ErrorCode::ProtocolError,
                    "Invalid finish receipt",
                ))
            }
            Err(e) if !path.exists() && e.code == ErrorCode::IoError => {
                let snapshots = Snapshots::open(&self.info.worktree_path, self.pool.git_timeout)?;
                let mut snapshot = snapshots.save(options)?;
                snapshots.pin(&snapshot.id)?;
                snapshot.pinned = true;
                let receipt = FinishReceipt {
                    schema_version: state::SCHEMA_VERSION,
                    lease_id: self.info.lease_id,
                    slot_id: self.info.slot_id.clone(),
                    snapshot,
                    released: false,
                };
                state::write(&path, &receipt)?;
                state::checkpoint("finish-snapshot-saved");
                receipt
            }
            Err(e) => return Err(e),
        };
        drop(execution);
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
        match self.completed_finish(id)? {
            Some(result) => Ok(result),
            None => self.hold_reserved(id)?.finish(options),
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
        let release = if receipt.released || !current_generation {
            ReleaseResult {
                slot_id: receipt.slot_id.clone(),
                lease_id: id,
                released: true,
                cleanup: crate::CleanupTiming::Deferred,
            }
        } else if record.phase == SlotPhase::Released {
            self.release_lease(id)?
        } else {
            return Ok(None);
        };
        receipt.released = true;
        state::write(&path, &receipt)?;
        Ok(Some(FinishResult {
            release,
            snapshot: receipt.snapshot,
        }))
    }
}
