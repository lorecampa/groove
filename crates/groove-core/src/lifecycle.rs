//! Pool-wide admission, distinct from slot ownership. Locks are never replaced.
use crate::{locking, repository::Repository, state, Error, ErrorCode, Pool, Result};
use serde::{Deserialize, Serialize};
use std::{fs::File, os::fd::AsFd};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Durable lifecycle phase of one pool namespace.
pub enum Phase {
    /// Initialization has not yet published a ready pool.
    Initializing,
    /// Reservations and managed work are allowed.
    Ready,
    /// Removal must be explicitly retried before the pool can be used again.
    Deleting,
    /// Workspaces were removed; history and permanent coordination remain.
    Deleted,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Lifecycle {
    pub schema_version: u32,
    pub pool_id: crate::PoolId,
    pub phase: Phase,
    pub lock_identity: locking::Identity,
    /// Persist the destructive acknowledgement so a retry has the same policy.
    pub discard_changes: bool,
}

pub(crate) fn read(repo: &Repository) -> Result<Option<Lifecycle>> {
    let path = repo.root.join("lifecycle.json");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    let record: Lifecycle = state::read(&path)?;
    if record.schema_version != state::SCHEMA_VERSION {
        return Err(Error::new(
            ErrorCode::UnsupportedConfiguration,
            "Unsupported pool lifecycle record",
        ));
    }
    let lock = locking::open(&repo.root.join("locks/lifecycle.lock"), false)?;
    if locking::identity(&lock)? != record.lock_identity {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Pool lifecycle lock identity changed",
        ));
    }
    Ok(Some(record))
}

pub(crate) fn write(repo: &Repository, record: &Lifecycle) -> Result<()> {
    state::write(&repo.root.join("lifecycle.json"), record)
}

pub(crate) fn validate_config_locks(repo: &Repository, config: &state::Config) -> Result<()> {
    if locking::identity(&locking::open(&repo.admin_lock_path(), false)?)? != config.admin_identity
    {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Recorded administrative lock identity changed",
        ));
    }
    for (slot, expected) in &config.slots {
        for (path, identity) in [
            (repo.lock_path(slot), *expected),
            (repo.execution_lock_path(slot), config.execution_locks[slot]),
        ] {
            if locking::identity(&locking::open(&path, false)?)? != identity {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    "Recorded permanent lock identity changed",
                )
                .with_details(|d| d.path = Some(path)));
            }
        }
    }
    Ok(())
}

pub(crate) fn retired_configs(repo: &Repository) -> Result<Vec<state::Config>> {
    let root = repo.root.join("retired");
    crate::repository::no_symlinks(&root)?;
    if !root.try_exists()? {
        return Ok(Vec::new());
    }
    crate::repository::owned_dir(&root)?;
    let mut configs = Vec::new();
    for item in std::fs::read_dir(root)? {
        let path = item?.path();
        if path
            .file_stem()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.parse::<crate::PoolId>().is_err())
            || path.extension().is_none_or(|s| s != "json")
        {
            // Atomic publication can leave temporary files after process death.
            // Preserve unknown entries; only UUID-named manifests are authority.
            continue;
        }
        let config: state::Config = state::read(&path)?;
        config.validate()?;
        configs.push(config);
    }
    Ok(configs)
}

pub(crate) fn validate_retired_locks(repo: &Repository) -> Result<()> {
    for config in retired_configs(repo)? {
        validate_config_locks(repo, &config)?;
    }
    Ok(())
}

/// No exclusive waiter is queued: nested shared operations cannot deadlock behind
/// deletion. Surviving children retain slot/execution/admin locks, checked separately.
pub(crate) fn gate(repo: &Repository, exclusive: bool) -> Result<File> {
    let path = repo.root.join("locks/lifecycle.lock");
    let created = !path.try_exists()?;
    // Once a lifecycle record exists, a missing lock is corruption, never an
    // invitation to create a second ownership domain on a new inode.
    let create = match std::fs::symlink_metadata(repo.root.join("lifecycle.json")) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(e.into()),
        Ok(_) => false,
    };
    let file = locking::open(&path, create)?;
    if created {
        file.sync_all()?;
        state::sync_dir(&repo.root.join("locks"))?;
    }
    let op = if exclusive {
        libc::LOCK_EX
    } else {
        libc::LOCK_SH
    };
    if !locking::flock(file.as_fd(), op | libc::LOCK_NB)? {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Another pool operation is active; retry when it finishes",
        )
        .retryable(true));
    }
    // Validate the pathname after acquiring, as for permanent slot locks.
    let current = locking::open(&path, false)?;
    if locking::identity(&current)? != locking::identity(&file)? {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Pool lifecycle lock changed during admission",
        ));
    }
    Ok(file)
}

impl Pool {
    pub(crate) fn operation(&self) -> Result<File> {
        self.admit(false)
    }

    pub(crate) fn retained_operation(&self) -> Result<File> {
        self.admit(true)
    }

    fn admit(&self, retained: bool) -> Result<File> {
        let gate = gate(&self.repo, false)?;
        self.check_lifecycle(retained)?;
        Ok(gate)
    }

    pub(crate) fn check_lifecycle(&self, retained: bool) -> Result<()> {
        let record = read(&self.repo)?;
        if record.as_ref().map(|r| r.pool_id) != self.pool_id {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "This pool handle belongs to an earlier pool; open the repository again",
            ));
        }
        require_phase(record.as_ref(), retained)
    }
}

pub(crate) fn require_phase(record: Option<&Lifecycle>, retained: bool) -> Result<()> {
    match record.map(|r| r.phase) {
        None | Some(Phase::Ready) => Ok(()),
        Some(Phase::Deleted) if retained => Ok(()),
        Some(Phase::Deleted) => Err(Error::new(ErrorCode::NotInitialized, "Pool was deleted; retained history and snapshots remain available, or run init to create a new pool")),
        Some(Phase::Deleting) => Err(Error::new(ErrorCode::ConfigConflict, "Pool deletion is incomplete; retry destroy with the original options")),
        Some(Phase::Initializing) => Err(Error::new(ErrorCode::ConfigConflict, "Pool initialization is incomplete; retry init --resume with the original settings")),
    }
}
