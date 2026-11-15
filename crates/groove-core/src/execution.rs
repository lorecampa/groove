//! Compatibility guards for permanent locks and execution records from older versions.

use crate::{state, Error, ErrorCode, Pool, Result, SlotId};

impl Pool {
    /// Exclude legacy execution owners and wait for shared read-only inspectors.
    pub(crate) fn execution_guard(&self, slot: &SlotId) -> Result<crate::locking::SlotGuard> {
        // Re-read the manifest: growth may have added this slot after `self` was opened.
        let config: state::Config = state::read(&self.repo.config_path())?;
        config.validate()?;
        let identity = match config.execution_locks.get(slot) {
            Some(identity) => *identity,
            None => {
                return Err(Error::new(
                    ErrorCode::InvalidLease,
                    "Slot is not in the current pool",
                ))
            }
        };
        let path = self.repo.execution_lock_path(slot);
        if let Some(guard) = crate::locking::SlotGuard::try_lock(&path, Some(identity))? {
            return Ok(guard);
        }
        // A successful shared probe establishes that current holders are
        // inspectors, not an exclusive execution owner. Close it before waiting
        // for exclusive admission; never upgrade a live shared descriptor.
        if let Some(reader) = crate::locking::SlotGuard::try_shared(&path, Some(identity))? {
            drop(reader);
            state::checkpoint("waiting-for-inspection");
            return crate::locking::SlotGuard::wait(&path, Some(identity), self.git_timeout);
        }
        Err({
            Error::new(
                ErrorCode::InvalidLease,
                "Reservation operation lock is busy; retry after the current owner finishes",
            )
            .retryable(true)
        })
    }

    /// Fail while a durable execution record (live or orphaned) still protects the slot.
    pub(crate) fn require_execution_clear(&self, slot: &SlotId) -> Result<()> {
        match std::fs::symlink_metadata(self.repo.execution_path(slot)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
            Ok(_) => Err(Error::new(
                ErrorCode::InvalidLease,
                "Legacy execution remains protected; use the previous Groove version to stop and recover it before upgrading",
            )),
        }
    }
}
