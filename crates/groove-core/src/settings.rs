use crate::{
    locking::{self, SlotGuard},
    state::{self, SlotRecord},
    Error, ErrorCode, Pool, PoolSettings, PoolUpdate, PoolUpdateResult, Result, SlotId,
};
use std::{collections::BTreeMap, fs};

impl Pool {
    pub(crate) fn refreshed(&self) -> Result<Self> {
        self.check_lifecycle(self.retained)?;
        let current = Self::load_mode(self.repo.clone(), self.git_timeout, self.retained)?;
        if current.pool_id != self.pool_id {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "Pool was reinitialized; open it again",
            ));
        }
        Ok(current)
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
            ready: self.config.ready.clone(),
            pool_name: self.repo.name.clone(),
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
    /// adopts only validated empty or retired-slot infrastructure. Retired diagnostic
    /// records are reset after ownership/protection checks; history and receipts remain.
    /// Descriptor use is bounded independently of the requested capacity.
    /// In-flight acquisitions may finish with their previously observed policy;
    /// current handles refresh on acquisition and inspection.
    ///
    /// # Errors
    /// Rejects shrinking, invalid capacity/timeouts,
    /// unsafe or unrecognized unpublished state, protected/busy retired slots,
    /// changed identities and I/O failures.
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
        let _operation = self.operation()?;
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
        let retired = crate::lifecycle::retired_configs(&self.repo)?;
        for config in &retired {
            crate::lifecycle::validate_config_locks(&self.repo, config)?;
        }
        let capacity = update.capacity.unwrap_or(current.config.capacity);
        let mut ready = current.config.ready.clone();
        if update.ready_target.is_some()
            || update.idle_timeout.is_some()
            || update.ready_base.is_some()
            || update.ready_git_timeout.is_some()
        {
            let candidate = ready.get_or_insert_with(crate::ReadySettings::default);
            if let Some(target) = update.ready_target {
                candidate.ready_target = target;
            }
            if let Some(timeout) = update.idle_timeout {
                candidate.idle_timeout = timeout;
            }
            if let Some(base) = update.ready_base {
                candidate.ready_base = base;
            }
            if let Some(timeout) = update.ready_git_timeout {
                candidate.ready_git_timeout = Some(timeout);
            }
            candidate.validate(capacity)?;
            candidate.resolve_head(&self.repo, self.git_timeout)?;
            if self.pool_id.is_none() {
                return Err(Error::new(ErrorCode::UnsupportedConfiguration, "Legacy pool has no lifecycle generation; explicitly recreate it before enabling automatic checkout preparation"));
            }
        }
        let cleanup = update
            .cleanup_policy
            .unwrap_or(current.config.cleanup_policy);
        if capacity < current.config.capacity {
            return Err(Error::new(
                ErrorCode::UnsupportedConfiguration,
                "Pool capacity can grow; shrinking is not supported",
            ));
        }
        if capacity == current.config.capacity
            && cleanup == current.config.cleanup_policy
            && ready == current.config.ready
        {
            return Ok(PoolUpdateResult {
                updated: false,
                pool: current.settings_snapshot(),
            });
        }
        // Preserve all existing identities. Unpublished growth creates only empty
        // slot infrastructure; no worktree or existing lease is modified.
        let mut identities = BTreeMap::new();
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
            let execution = locking::open(&self.repo.execution_lock_path(&slot), new)?;
            let execution_identity = locking::identity(&execution)?;
            if !new && current.config.execution_locks.get(&slot) != Some(&execution_identity) {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    "Permanent execution lock identity changed",
                ));
            }
            execution.sync_all()?;
            if new {
                // Never wait on slot ownership while holding administrative
                // exclusion. The published manifest excludes these slots, and the
                // admin guard serializes growth. Old-generation handles cannot
                // allocate them, so probes need not accumulate until publication.
                let _slot = SlotGuard::try_lock(&self.repo.lock_path(&slot), Some(identity))?
                    .ok_or_else(|| {
                        Error::new(
                            ErrorCode::ConfigConflict,
                            "Unpublished slot has a live owner",
                        )
                    })?;
                let _execution = SlotGuard::try_lock(
                    &self.repo.execution_lock_path(&slot),
                    Some(execution_identity),
                )?
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::ConfigConflict,
                        "Unpublished slot has a live execution owner",
                    )
                })?;
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
                let path = self.repo.record_path(&slot);
                crate::repository::no_symlinks(&path)?;
                if path.try_exists()? {
                    let record: SlotRecord = state::read(&path)?;
                    let was_retired = retired.iter().any(|c| c.slots.contains_key(&slot));
                    if record.schema_version != state::SCHEMA_VERSION
                        || record.lease.as_ref().is_some_and(|lease| {
                            lease.slot_id != slot
                                || (was_retired
                                    && !retired.iter().any(|c| {
                                        c.slots.contains_key(&slot)
                                            && c.worktree_root.join(slot.as_str())
                                                == lease.worktree_path
                                    }))
                        })
                    {
                        return Err(Error::new(
                            ErrorCode::ConfigConflict,
                            "Invalid retired or unpublished slot metadata",
                        )
                        .with_details(|d| d.path = Some(path)));
                    }
                    let empty = record.lease.is_none()
                        && record.phase == crate::state::SlotPhase::Available
                        && record.failure.is_none();
                    if !empty {
                        if !was_retired {
                            return Err(Error::new(
                                ErrorCode::ConfigConflict,
                                "Unpublished slot metadata is not empty",
                            ));
                        }
                        // A completed deletion archived this slot's original locks.
                        // Exclusion and absence of protection above permit resetting
                        // diagnostics; independent receipts/journals/history survive.
                        state::write(&path, &SlotRecord::empty())?;
                    }
                } else {
                    state::write(&path, &SlotRecord::empty())?;
                }
            }
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
        if ready.is_some() && current.config.ready_lock.is_none() {
            let file = locking::open(&self.repo.root.join("locks/ready.lock"), true)?;
            file.sync_all()?;
            current.config.ready_lock = Some(locking::identity(&file)?);
        }
        current.config.ready = ready;
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
