use crate::{
    locking::{self, SlotGuard},
    state::{self, SlotRecord},
    Error, ErrorCode, Pool, PoolSettings, PoolUpdate, PoolUpdateResult, Result, SlotId,
};
use std::{collections::BTreeMap, fs};

impl Pool {
    pub(crate) fn refreshed(&self) -> Result<Self> {
        Self::load(self.repo.clone(), self.git_timeout)
    }

    /// Read current settings, including updates made after this handle was opened.
    ///
    /// # Errors
    /// Rejects incompatible/corrupt metadata or changed permanent lock identities.
    pub fn settings(&self) -> Result<PoolSettings> {
        let current = self.refreshed()?;
        Ok(current.settings_snapshot())
    }

    fn settings_snapshot(&self) -> PoolSettings {
        PoolSettings {
            capacity: self.config.capacity,
            cleanup_policy: self.config.cleanup_policy,
            worktree_root: self.config.worktree_root.clone(),
            state_dir: self.repo.root.clone(),
        }
    }

    /// Grow capacity and/or change future cleanup policy in one durable update.
    ///
    /// Existing reservations, writers and permanent lock files are preserved.
    /// A crash before manifest publication leaves the old settings usable; retrying
    /// adopts only validated empty new-slot infrastructure. In-flight acquisitions
    /// may finish with their previously observed policy; current handles refresh on
    /// acquisition and inspection.
    ///
    /// # Errors
    /// Rejects shrinking, invalid capacity/timeouts,
    /// unsafe or nonempty unpublished slots, changed identities and I/O failures.
    ///
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// use groove_core::{Pool, PoolUpdate};
    /// Pool::open("/path/to/repo")?.configure(PoolUpdate {
    ///     capacity: Some(8), ..Default::default()
    /// })?;
    /// # Ok(()) }
    /// ```
    pub fn configure(&self, update: PoolUpdate) -> Result<PoolUpdateResult> {
        if update.timeout.is_zero()
            || update
                .capacity
                .is_some_and(|n| !(1..=crate::MAX_CAPACITY).contains(&n))
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Capacity must be 1..=4096 and timeout positive",
            ));
        }
        let _admin = SlotGuard::wait(
            &self.repo.admin_lock_path(),
            Some(self.config.admin_identity),
            update.timeout,
        )?;
        let mut current = self.refreshed()?;
        let capacity = update.capacity.unwrap_or(current.config.capacity);
        let cleanup = update
            .cleanup_policy
            .unwrap_or(current.config.cleanup_policy);
        if capacity < current.config.capacity {
            return Err(Error::new(
                ErrorCode::UnsupportedConfiguration,
                "Pool capacity can grow; shrinking is not supported",
            ));
        }
        if capacity == current.config.capacity && cleanup == current.config.cleanup_policy {
            return Ok(PoolUpdateResult {
                updated: false,
                pool: current.settings_snapshot(),
            });
        }
        // Preserve all existing identities. Unpublished growth creates only empty
        // slot infrastructure; no worktree or existing lease is modified.
        let mut identities = BTreeMap::new();
        let mut new_guards = Vec::new();
        for i in 1..=capacity {
            let slot = SlotId::new(i)?;
            let new = i > current.config.capacity;
            let file = locking::open(&self.repo.lock_path(&slot), new)?;
            let identity = locking::identity(&file)?;
            if !new && current.config.slots.get(&slot) != Some(&identity) {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    "Permanent slot lock identity changed",
                ));
            }
            file.sync_all()?;
            if new {
                // Never wait on slot ownership while holding administrative
                // exclusion. Unpublished slots must be immediately unowned.
                let slot_guard = SlotGuard::try_lock(&self.repo.lock_path(&slot), Some(identity))?
                    .ok_or_else(|| {
                        Error::new(
                            ErrorCode::ConfigConflict,
                            "Unpublished slot has a live owner",
                        )
                    })?;
                new_guards.push(slot_guard);
                match fs::symlink_metadata(current.config.worktree_root.join(slot.as_str())) {
                    Ok(_) => {
                        return Err(Error::new(
                            ErrorCode::ConfigConflict,
                            "Unpublished checkout path already exists",
                        ))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                    Err(error) => return Err(error.into()),
                }
                let path = self.repo.record_path(&slot);
                crate::repository::no_symlinks(&path)?;
                if path.try_exists()? {
                    let record: SlotRecord = state::read(&path)?;
                    if record.schema_version != state::SCHEMA_VERSION
                        || record.lease.is_some()
                        || record.phase != crate::state::SlotPhase::Available
                        || record.failure.is_some()
                    {
                        return Err(Error::new(
                            ErrorCode::ConfigConflict,
                            "Unpublished slot metadata is not empty",
                        ));
                    }
                } else {
                    state::write(&path, &SlotRecord::empty())?;
                }
                for path in [
                    current.repo.protection_path(&slot),
                    current.repo.execution_path(&slot),
                ] {
                    match fs::symlink_metadata(path) {
                        Ok(_) => {
                            return Err(Error::new(
                                ErrorCode::ConfigConflict,
                                "Unpublished slot is protected",
                            ))
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            let execution = locking::open(&self.repo.execution_lock_path(&slot), new)?;
            let execution_identity = locking::identity(&execution)?;
            if !new && current.config.execution_locks.get(&slot) != Some(&execution_identity) {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    "Permanent execution lock identity changed",
                ));
            }
            execution.sync_all()?;
            current
                .config
                .execution_locks
                .insert(slot.clone(), execution_identity);
            identities.insert(slot, identity);
            if new {
                state::checkpoint("pool-slot-added");
            }
        }
        current.config.slots = identities;
        current.config.capacity = capacity;
        current.config.cleanup_policy = cleanup;
        state::sync_dir(&self.repo.root.join("locks"))?;
        state::checkpoint("before-pool-update");
        state::write(&self.repo.config_path(), &current.config)?;
        state::checkpoint("pool-updated");
        Ok(PoolUpdateResult {
            updated: true,
            pool: current.settings_snapshot(),
        })
    }
}
