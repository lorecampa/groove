use crate::{
    git::Git,
    lease::Lease,
    locking::{self, SlotGuard},
    recovery,
    repository::{self, Repository},
    state::{self, Config, SlotRecord},
    AcquireOptions, Error, InitResult, PoolOptions, Result,
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Debug, Clone)]
/// A repository's fixed-capacity pool. Cloning does not acquire a slot.
pub struct Pool {
    pub(crate) repo: Repository,
    pub(crate) config: Config,
}
impl Pool {
    /// Initialize a pool, or resume matching interrupted initialization.
    ///
    /// # Errors
    /// Rejects invalid options, conflicting configuration, unsafe managed paths,
    /// unsupported environments, and Git or filesystem failures.
    pub fn init(repo: impl AsRef<Path>, options: PoolOptions) -> Result<InitResult> {
        if options.capacity == 0 || options.capacity > 4096 || options.git_timeout.is_zero() {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Capacity must be 1..=4096 and Git timeout positive",
            ));
        }
        let repo = Repository::discover(repo.as_ref(), options.git_timeout)?;
        repository::mkdir(&repo.root)?;
        repository::owned_dir(&repo.root)?;
        repository::mkdir(&repo.root.join("locks"))?;
        let admin_file = locking::open(&repo.admin_lock_path(), !repo.config_path().exists())?;
        admin_file.sync_all()?;
        state::sync_dir(&repo.root.join("locks"))?;
        let admin_id = locking::identity(&admin_file)?;
        let _admin = SlotGuard::wait(&repo.admin_lock_path(), Some(admin_id), options.git_timeout)?;
        let requested = match options.worktree_root {
            Some(path) => {
                let path = if path.is_absolute() {
                    path
                } else {
                    std::env::current_dir()?.join(path)
                };
                repository::no_symlinks(&path)?;
                repository::utf8(&path)?;
                path
            }
            None => repo.root.join("worktrees"),
        };
        if repo.config_path().exists() {
            let pool = Self::load(repo)?;
            if pool.config.capacity != options.capacity
                || pool.config.cleanup_policy != options.cleanup_policy
                || pool.config.worktree_root != requested
            {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Existing pool configuration differs",
                ));
            }
            return Ok(pool.init_result(false));
        }
        if requested == repo.common
            || requested == repo.root
            || repo.common.starts_with(&requested)
            || (requested.starts_with(&repo.common) && requested != repo.root.join("worktrees"))
        {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Worktree root overlaps repository administration",
            ));
        }
        // Persist initialization intent before creating external storage. Conflicting
        // resumptions must fail, even when the ready manifest was never published.
        let intent = repo.root.join("initializing.json");
        let expected = json!({"schema_version": 1, "capacity": options.capacity, "cleanup_policy": options.cleanup_policy, "worktree_root": requested});
        let resumed = intent.exists();
        if resumed {
            let old: serde_json::Value = state::read(&intent)?;
            if old != expected {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Interrupted initialization has a different configuration",
                ));
            }
        } else {
            if requested.exists() && fs::read_dir(&requested)?.next().is_some() {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Worktree root must initially be empty",
                ));
            }
            state::write(&intent, &expected)?;
        }
        repository::mkdir(&requested)?;
        repository::owned_dir(&requested)?;
        for name in ["slots", "tmp"] {
            repository::mkdir(&repo.root.join(name))?;
        }
        let mut slots = BTreeMap::new();
        for i in 1..=options.capacity {
            let slot = crate::SlotId::new(i as u16)?;
            let file = locking::open(&repo.lock_path(&slot), true)?;
            file.sync_all()?;
            slots.insert(slot.clone(), locking::identity(&file)?);
            if !repo.record_path(&slot).exists() {
                state::write(&repo.record_path(&slot), &SlotRecord::empty())?;
            }
        }
        state::sync_dir(&repo.root.join("locks"))?;
        state::sync_dir(&repo.root)?;
        let config = Config {
            schema_version: 1,
            capacity: options.capacity,
            cleanup_policy: options.cleanup_policy,
            worktree_root: requested,
            admin_identity: admin_id,
            slots,
        };
        state::checkpoint("before-ready-manifest");
        state::write(&repo.config_path(), &config)?;
        fs::remove_file(intent)?;
        state::sync_dir(&repo.root)?;
        Ok(Self { repo, config }.init_result(true))
    }
    /// Discover an initialized pool from any checkout of its repository.
    ///
    /// # Errors
    /// Rejects missing/incompatible metadata, changed lock identities, unsafe paths,
    /// and Git or filesystem failures. Discovery uses a 30-second Git timeout.
    pub fn open(repo: impl AsRef<Path>) -> Result<Self> {
        Self::load(Repository::discover(
            repo.as_ref(),
            Duration::from_secs(30),
        )?)
    }
    fn load(repo: Repository) -> Result<Self> {
        repository::owned_dir(&repo.root).map_err(|e| {
            if !repo.root.exists() {
                Error::new(crate::ErrorCode::NotInitialized, "Pool is not initialized")
            } else {
                e
            }
        })?;
        if !repo.config_path().exists() {
            return Err(Error::new(
                crate::ErrorCode::NotInitialized,
                "Pool is not initialized",
            ));
        }
        let raw: serde_json::Value = state::read(&repo.config_path())?;
        if !matches!(raw["schema_version"].as_u64(), Some(1 | 2)) {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Unsupported manifest schema version",
            ));
        }
        let config: Config = serde_json::from_value(raw)?;
        if config.capacity == 0 || config.capacity > 4096 || config.slots.len() != config.capacity {
            return Err(Error::new(
                crate::ErrorCode::ConfigConflict,
                "Invalid manifest capacity",
            ));
        }
        for i in 1..=config.capacity {
            if !config.slots.contains_key(&crate::SlotId::new(i as u16)?) {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Invalid manifest slot set",
                ));
            }
        }
        if !config.worktree_root.is_absolute()
            || config.worktree_root == repo.root
            || repo.common.starts_with(&config.worktree_root)
            || (config.worktree_root.starts_with(&repo.common)
                && config.worktree_root != repo.root.join("worktrees"))
        {
            return Err(Error::new(
                crate::ErrorCode::ConfigConflict,
                "Unsafe manifest worktree root",
            ));
        }
        repository::owned_dir(&config.worktree_root)?;
        repository::owned_dir(&repo.root.join("locks"))?;
        repository::owned_dir(&repo.root.join("slots"))?;
        let admin = locking::open(&repo.admin_lock_path(), false)?;
        if locking::identity(&admin)? != config.admin_identity {
            return Err(Error::new(
                crate::ErrorCode::IoError,
                "Administrative lock identity changed",
            ));
        }
        Ok(Self { repo, config })
    }
    fn init_result(&self, created: bool) -> InitResult {
        InitResult {
            created,
            common_git_dir: self.repo.common.clone(),
            state_dir: self.repo.root.clone(),
            capacity: self.config.capacity,
            slots: self.config.slots.keys().cloned().collect(),
            cleanup_policy: self.config.cleanup_policy,
            worktree_root: self.config.worktree_root.clone(),
        }
    }
    /// Return the configured maximum number of simultaneously leased slots.
    pub fn capacity(&self) -> usize {
        self.config.capacity
    }
    /// Acquire and verify a detached checkout of the requested commit.
    ///
    /// Previous slot contents are disposable and may be deleted or rebuilt.
    ///
    /// # Errors
    /// Rejects invalid revisions and unsupported Git configuration. Reports exhausted
    /// capacity, capacity wait timeout, unusable slots, and Git or filesystem failures.
    pub fn acquire(&self, options: AcquireOptions) -> Result<Lease> {
        if options.git_timeout.is_zero() {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Git timeout must be positive",
            ));
        }
        let git = Git::new(options.git_timeout);
        let revision = format!("{}^{{commit}}", options.base);
        let bytes = git
            .run(
                &self.repo.common,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &revision,
                ],
                &[],
            )
            .map_err(|e| {
                if e.code == crate::ErrorCode::GitError && e.details["exit_code"] == 1 {
                    Error::new(
                        crate::ErrorCode::InvalidRevision,
                        "Base does not resolve to a commit",
                    )
                    .details(e.details.clone())
                    .with_source(e)
                } else {
                    e
                }
            })?;
        let oid = String::from_utf8(bytes)
            .map_err(|e| {
                Error::new(crate::ErrorCode::GitError, "Git returned an invalid OID").with_source(e)
            })?
            .trim_end_matches('\n')
            .to_owned();
        let oid = oid.parse::<crate::CommitOid>().map_err(|e| {
            Error::new(crate::ErrorCode::GitError, "Git returned an invalid OID").with_source(e)
        })?;
        recovery::validate_config(&git, &self.repo, &oid)?;
        let slots: Vec<_> = self.config.slots.iter().collect();
        let start_slot = (Uuid::new_v4().as_u128() % slots.len() as u128) as usize;
        let start = Instant::now();
        let mut failed = BTreeMap::new();
        loop {
            let mut busy = 0;
            for n in 0..slots.len() {
                let (slot, expected) = slots[(start_slot + n) % slots.len()];
                if failed.contains_key(slot) {
                    continue;
                }
                let guard = match SlotGuard::try_lock(&self.repo.lock_path(slot), Some(*expected)) {
                    Ok(Some(g)) => g,
                    Ok(None) => {
                        busy += 1;
                        continue;
                    }
                    Err(e) => {
                        failed.insert(slot.clone(), e.to_string());
                        continue;
                    }
                };
                // Protection is a durable prohibition on cleanup, not evidence of
                // lock ownership. It survives a tool server's death while external
                // writers may still be using a path that server exposed.
                match fs::symlink_metadata(self.repo.protection_path(slot)) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    _ => {
                        failed.insert(slot.clone(), "Reuse protected; stop writers and explicitly release the protected lease".into());
                        continue;
                    }
                }
                if let Ok(record) = state::read::<serde_json::Value>(&self.repo.record_path(slot)) {
                    if record["schema_version"].as_u64().is_some_and(|v| v != 1) {
                        return Err(Error::new(
                            crate::ErrorCode::UnsupportedConfiguration,
                            "Unsupported slot metadata version",
                        ));
                    }
                }
                match recovery::prepare(
                    &self.repo,
                    &self.config,
                    slot,
                    &guard,
                    &oid,
                    options.git_timeout,
                ) {
                    Ok(recovery) => {
                        let info = crate::LeaseInfo {
                            slot_id: slot.clone(),
                            lease_id: crate::LeaseId::generate(),
                            worktree_path: self.config.worktree_root.join(slot.as_str()),
                            base_oid: oid.clone(),
                            head_mode: crate::HeadMode::Detached,
                            recovery,
                            cleanup_policy: self.config.cleanup_policy,
                            lease_transport: crate::LeaseTransport::InProcess,
                            descriptor_count: 1,
                        };
                        let record = SlotRecord {
                            schema_version: 1,
                            phase: crate::state::SlotPhase::Leased,
                            lease: Some(info.clone()),
                            failure: None,
                        };
                        state::write(&self.repo.record_path(slot), &record)?;
                        state::checkpoint("lease-recorded");
                        return Ok(Lease::new(self.clone(), guard, info));
                    }
                    Err(e) => {
                        let record = SlotRecord {
                            schema_version: 1,
                            phase: crate::state::SlotPhase::Quarantined,
                            lease: None,
                            failure: Some(e.to_string()),
                        };
                        let _ = state::write(&self.repo.record_path(slot), &record);
                        failed.insert(slot.clone(), e.to_string());
                    }
                }
            }
            if busy == 0 || options.wait.is_zero() || start.elapsed() >= options.wait {
                let code = if busy == 0 {
                    crate::ErrorCode::PoolUnhealthy
                } else if options.wait.is_zero() {
                    crate::ErrorCode::PoolExhausted
                } else {
                    crate::ErrorCode::AcquireTimeout
                };
                let mut e = Error::new(code, "No verified worktree slot is available").details(json!({"capacity": self.config.capacity, "busy_slots": busy, "failed_slots": failed}));
                e.retryable = busy > 0;
                return Err(e);
            }
            std::thread::sleep(
                Duration::from_millis(10).min(options.wait.saturating_sub(start.elapsed())),
            );
        }
    }
    /// Release a transported descriptor after every writer has stopped.
    ///
    /// A retry for the same generation can finish an interrupted release. Explicit
    /// unlock affects all duplicate descriptors; do not use the worktree afterward.
    ///
    /// # Errors
    /// Rejects invalid descriptors and stale generations, or reports metadata I/O
    /// failures. A successor's lease cannot be released with an older generation.
    pub fn release_fd(
        &self,
        slot: &crate::SlotId,
        lease_id: crate::LeaseId,
        fd: std::os::fd::BorrowedFd<'_>,
    ) -> Result<crate::ReleaseResult> {
        crate::lease::release(self, slot, lease_id, fd)
    }

    /// List durable reuse protections, including those left by a crashed MCP server.
    /// This is diagnostic metadata; it does not establish current lock ownership.
    ///
    /// # Errors
    /// Reports malformed/unsupported protection records and filesystem errors.
    pub fn protected_leases(&self) -> Result<Vec<crate::LeaseInfo>> {
        let mut leases = Vec::new();
        for slot in self.config.slots.keys() {
            let path = self.repo.protection_path(slot);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
                Ok(_) => (),
            }
            match state::read::<state::ReuseProtection>(&path) {
                Ok(record) => {
                    if record.schema_version != 1 || &record.lease.slot_id != slot {
                        return Err(Error::new(
                            crate::ErrorCode::ProtocolError,
                            "Invalid reuse protection record",
                        ));
                    }
                    leases.push(record.lease);
                }
                // An explicit release may remove the record during a diagnostic scan.
                Err(e)
                    if e.code == crate::ErrorCode::IoError
                        && matches!(fs::symlink_metadata(&path), Err(ref io) if io.kind() == std::io::ErrorKind::NotFound) =>
                    {}
                Err(e) => return Err(e),
            }
        }
        Ok(leases)
    }

    /// Release a protected lease abandoned by its owner, after stopping ALL writers.
    ///
    /// This intentionally does not return a usable checkout. The next acquisition
    /// normalizes and verifies it. A live lock owner must release its own lease.
    ///
    /// # Errors
    /// Rejects busy locks, missing protections, stale generations and I/O failures.
    pub fn release_protected(
        &self,
        slot: &crate::SlotId,
        lease_id: crate::LeaseId,
    ) -> Result<crate::ReleaseResult> {
        let identity = self
            .config
            .slots
            .get(slot)
            .ok_or_else(|| Error::new(crate::ErrorCode::InvalidLease, "Unknown slot"))?;
        let guard =
            SlotGuard::try_lock(&self.repo.lock_path(slot), Some(*identity))?.ok_or_else(|| {
                Error::new(
                    crate::ErrorCode::InvalidLease,
                    "Protected lease still has a live lock owner",
                )
            })?;
        let record: state::ReuseProtection = state::read(&self.repo.protection_path(slot))?;
        if record.schema_version != 1
            || &record.lease.slot_id != slot
            || record.lease.lease_id != lease_id
        {
            return Err(Error::new(
                crate::ErrorCode::LeaseMismatch,
                "Protected lease generation mismatch",
            ));
        }
        use std::os::fd::AsFd;
        crate::lease::release(self, slot, lease_id, guard.file.as_fd())
    }
}
