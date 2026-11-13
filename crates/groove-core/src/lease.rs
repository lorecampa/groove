use crate::{
    locking::{self, SlotGuard},
    state::{self, SlotRecord},
    Error, LeaseInfo, Pool, ReleaseResult, Result,
};
use std::{
    fs::File,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd},
        unix::process::CommandExt,
    },
    process::{Command, ExitStatus},
};

/// Exclusive ownership of a verified worktree. Drop closes only; it never runs Git
/// or explicitly unlocks copies held by a surviving participating child.
#[derive(Debug)]
#[must_use = "keep the lease alive until all worktree writers have stopped"]
pub struct Lease {
    pub(crate) pool: Pool,
    pub(crate) guard: Option<SlotGuard>,
    pub(crate) info: LeaseInfo,
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
    #[must_use]
    pub fn info(&self) -> &LeaseInfo {
        &self.info
    }
    /// Whether this handle still retains its descriptor.
    ///
    /// External explicit unlock through a duplicate descriptor affects all copies.
    #[must_use]
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
        let _operation = self.pool.operation()?;
        if self.guard.is_none() {
            return Err(Error::new(
                crate::ErrorCode::InvalidLease,
                "Lease was released",
            ));
        }
        state::write(
            &self.pool.repo.protection_path(&self.info.slot_id),
            &state::ReuseProtection {
                schema_version: state::SCHEMA_VERSION,
                lease: self.info.clone(),
            },
        )?;
        state::checkpoint("reuse-protected");
        Ok(())
    }
    /// Consume this handle into a durable reservation usable across shell calls.
    ///
    /// Persists protection before returning any path and closes the kernel lease
    /// descriptor. The checkout remains unavailable to other acquisitions until
    /// [`Pool::release_protected`] is called after saving results and stopping ALL
    /// writers. Process exit, timeout or age never releases this reservation.
    /// If a response is lost, inspect [`Pool::protected_leases`] before retrying.
    ///
    /// # Errors
    /// Reports inactive handles or persistence failures. A failed call may have
    /// partially persisted protection; inspect protected leases before recovery.
    pub fn into_protected(mut self) -> Result<LeaseInfo> {
        self.info.ownership = crate::Ownership::Durable;
        self.protect_from_reuse()?;
        Ok(self.info.clone())
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
        let _operation = self.pool.operation()?;
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
}

pub(crate) fn release(
    pool: &Pool,
    slot: &crate::SlotId,
    lease_id: crate::LeaseId,
    fd: BorrowedFd<'_>,
) -> Result<ReleaseResult> {
    let _operation = pool.operation()?;
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
        if record.schema_version != state::SCHEMA_VERSION
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
    // Managed execution is serialized independently of the owner's descriptor.
    // Even a live lease owner cannot explicitly unlock while a managed worker is active.
    let _execution = pool.execution_guard(slot)?;
    pool.require_execution_clear(slot)?;
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
    // Free any branch the task left checked out here. Writers have stopped and
    // both locks are held; the mutating child inherits the slot descriptor.
    if let Err(error) = crate::recovery::detach_head(
        &pool.repo,
        &pool.config,
        slot,
        pool.git_timeout,
        &[fd.as_raw_fd()],
    ) {
        tracing::warn!(slot = %slot, %error, "Could not detach HEAD in the released checkout; the next acquisition resets it");
    }
    record.phase = crate::state::SlotPhase::Released;
    if let Some(info) = &record.lease {
        pool.record_request(info, true)?;
    }
    state::write(&pool.repo.record_path(slot), &record)?;
    state::checkpoint("release-recorded");
    let protection_path = pool.repo.protection_path(slot);
    match std::fs::symlink_metadata(&protection_path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
        Ok(_) => {
            let record: state::ReuseProtection = state::read(&protection_path)?;
            if record.schema_version != state::SCHEMA_VERSION
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
