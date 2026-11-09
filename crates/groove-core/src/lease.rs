use crate::{
    locking::{self, SlotGuard},
    state::{self, SlotRecord},
    Error, LeaseInfo, Pool, ReleaseResult, Result,
};
use std::{
    fs::File,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        unix::process::CommandExt,
    },
    process::{Command, ExitStatus},
};

/// Exclusive ownership of a verified worktree. Drop closes only; it never runs Git
/// or explicitly unlocks copies held by a surviving participating child.
#[derive(Debug)]
#[must_use = "keep the lease alive until all worktree writers have stopped"]
pub struct Lease {
    pool: Pool,
    guard: Option<SlotGuard>,
    info: LeaseInfo,
}
impl Lease {
    pub(crate) fn new(pool: Pool, guard: SlotGuard, info: LeaseInfo) -> Self {
        Self {
            pool,
            guard: Some(guard),
            info,
        }
    }
    /// Return descriptive metadata; this metadata alone does not confer ownership.
    pub fn info(&self) -> &LeaseInfo {
        &self.info
    }
    /// Whether this handle still retains its descriptor.
    ///
    /// External explicit unlock through a duplicate descriptor affects all copies.
    pub fn is_active(&self) -> bool {
        self.guard.is_some()
    }
    /// Durably block automatic reuse even if this handle or its server disappears.
    ///
    /// Use before exposing a checkout to tools whose writers do not inherit its
    /// descriptor. Explicit release removes protection after writers have stopped.
    /// Drop leaves protection intact; a later owner can use [`Pool::release_protected`].
    ///
    /// # Errors
    /// Rejects inactive handles or reports metadata I/O failures. On failure, do
    /// not expose the path: persistence may have partially completed.
    pub fn protect_from_reuse(&self) -> Result<()> {
        if self.guard.is_none() {
            return Err(Error::new(
                crate::ErrorCode::InvalidLease,
                "Lease was released",
            ));
        }
        // Upgrade before publishing protection. Earlier binaries reject v2 rather
        // than ignoring the new prohibition and cleaning an abandoned MCP slot.
        // Existing processes using older code must be stopped before enabling MCP.
        let _admin = SlotGuard::wait(
            &self.pool.repo.admin_lock_path(),
            Some(self.pool.config.admin_identity),
            std::time::Duration::from_secs(30),
        )?;
        let mut config: state::Config = state::read(&self.pool.repo.config_path())?;
        match config.schema_version {
            1 => {
                config.schema_version = 2;
                state::write(&self.pool.repo.config_path(), &config)?;
            }
            2 => (),
            _ => {
                return Err(Error::new(
                    crate::ErrorCode::UnsupportedConfiguration,
                    "Unsupported pool protection schema",
                ))
            }
        }
        state::write(
            &self.pool.repo.protection_path(&self.info.slot_id),
            &state::ReuseProtection {
                schema_version: 1,
                lease: self.info.clone(),
            },
        )
    }
    /// Explicit release invalidates this handle on success and retains it on failure.
    /// All writers must already have stopped; unlock also affects duplicate descriptors.
    ///
    /// # Errors
    /// Returns an error for an inactive handle, stale generation, invalid descriptor,
    /// or metadata I/O failure. Retrying can finish a partially recorded release.
    pub fn release(&mut self) -> Result<ReleaseResult> {
        let guard = self.guard.as_ref().ok_or_else(|| {
            Error::new(crate::ErrorCode::InvalidLease, "Lease was already released")
        })?;
        let result = release(
            &self.pool,
            &self.info.slot_id,
            self.info.lease_id,
            guard.file.as_fd(),
        )?;
        self.guard.take();
        Ok(result)
    }
    /// Run one participating writer in the leased worktree, retaining ownership in
    /// that child if this process dies. Callers must account for detached descendants.
    ///
    /// # Errors
    /// Returns an error if the lease is inactive or spawning/waiting fails. A worker's
    /// nonzero exit is returned as an [`ExitStatus`], not a library error.
    pub fn run_worker(&mut self, mut command: Command) -> Result<ExitStatus> {
        let fd = self
            .guard
            .as_ref()
            .ok_or_else(|| Error::new(crate::ErrorCode::InvalidLease, "Lease was released"))?
            .file
            .as_raw_fd();
        command
            .current_dir(&self.info.worktree_path)
            .process_group(0);
        // SAFETY: only async-signal-safe fcntl calls execute in the child closure.
        // The lease remains borrowed until the spawned child has terminated.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        loop {
            match child.wait() {
                Ok(status) => return Ok(status),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // SAFETY: the child has a dedicated process group created above.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(e.into());
                }
            }
        }
    }
    /// Consume the handle for transport. The returned descriptor owns the lock.
    ///
    /// # Errors
    /// Returns an error if the handle has already been released.
    pub fn into_descriptor(mut self) -> Result<(LeaseInfo, OwnedFd)> {
        let guard = self
            .guard
            .take()
            .ok_or_else(|| Error::new(crate::ErrorCode::InvalidLease, "Lease was released"))?;
        Ok((self.info.clone(), guard.file.into()))
    }
}

pub(crate) fn release(
    pool: &Pool,
    slot: &crate::SlotId,
    lease_id: crate::LeaseId,
    fd: BorrowedFd<'_>,
) -> Result<ReleaseResult> {
    let expected = pool
        .config
        .slots
        .get(slot)
        .ok_or_else(|| Error::new(crate::ErrorCode::InvalidLease, "Unknown slot"))?;
    // Duplicate for RAII inspection without taking ownership of the caller's descriptor.
    let file = File::from(fd.try_clone_to_owned()?);
    if locking::identity(&file)? != *expected {
        return Err(Error::new(
            crate::ErrorCode::InvalidLease,
            "Descriptor does not identify the slot lock",
        ));
    }
    let path_file = locking::open(&pool.repo.lock_path(slot), false)?;
    if locking::identity(&path_file)? != *expected {
        return Err(Error::new(
            crate::ErrorCode::InvalidLease,
            "Permanent lock identity changed",
        ));
    }
    fn checked(pool: &Pool, slot: &crate::SlotId, lease_id: crate::LeaseId) -> Result<SlotRecord> {
        let record: SlotRecord = state::read(&pool.repo.record_path(slot))?;
        if record.schema_version != 1
            || !matches!(
                record.phase,
                crate::state::SlotPhase::Leased | crate::state::SlotPhase::Released
            )
            || !record
                .lease
                .as_ref()
                .is_some_and(|l| l.lease_id == lease_id && &l.slot_id == slot)
        {
            return Err(Error::new(
                crate::ErrorCode::LeaseMismatch,
                "Lease generation is stale or no longer active",
            ));
        }
        Ok(record)
    }
    checked(pool, slot, lease_id)?;
    // Accept the same generation's released record to finish an interrupted release.
    // On the owning open file description flock is a no-op. An independent open
    // conflicts with a retained lock. After a completed release, reacquiring an idle
    // slot is harmless; the second generation check protects any successor.
    if !locking::flock(fd, libc::LOCK_EX | libc::LOCK_NB)? {
        return Err(Error::new(
            crate::ErrorCode::InvalidLease,
            "Descriptor does not own the active lock",
        ));
    }
    let mut record = match checked(pool, slot, lease_id) {
        Ok(record) => record,
        Err(e) => {
            locking::flock(fd, libc::LOCK_UN)?;
            return Err(e);
        }
    };
    record.phase = crate::state::SlotPhase::Released;
    state::write(&pool.repo.record_path(slot), &record)?;
    state::checkpoint("release-recorded");
    let protection_path = pool.repo.protection_path(slot);
    match std::fs::symlink_metadata(&protection_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
        Ok(_) => {
            let record: state::ReuseProtection = state::read(&protection_path)?;
            if record.schema_version != 1
                || &record.lease.slot_id != slot
                || record.lease.lease_id != lease_id
            {
                return Err(Error::new(
                    crate::ErrorCode::LeaseMismatch,
                    "Reuse protection generation mismatch",
                ));
            }
            std::fs::remove_file(&protection_path)?;
            state::sync_dir(protection_path.parent().expect("slot protection parent"))?;
        }
    }
    locking::flock(fd, libc::LOCK_UN)?;
    state::checkpoint("release-unlocked");
    Ok(ReleaseResult {
        slot_id: slot.clone(),
        lease_id,
        released: true,
        cleanup: crate::CleanupTiming::Deferred,
    })
}
