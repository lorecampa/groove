//! Compatibility guards for permanent locks and execution records from older versions.

use crate::{state, Error, ErrorCode, Pool, Result, SlotId};

impl Pool {
    /// Take the permanent legacy execution lock without waiting.
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
        crate::locking::SlotGuard::try_lock(&self.repo.execution_lock_path(slot), Some(identity))?
            .ok_or_else(|| {
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
