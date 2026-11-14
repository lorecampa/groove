use crate::{
    git::Git,
    lease::Lease,
    locking::{self, SlotGuard},
    recovery,
    repository::{self, Repository},
    state::{self, Config, SlotRecord},
    AcquireOptions, Error, InitResult, PoolOptions, Result,
};
use std::{
    collections::BTreeMap,
    fs,
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[derive(Debug, Clone)]
/// A repository's bounded pool. Cloning does not acquire a slot.
pub struct Pool {
    pub(crate) repo: Repository,
    pub(crate) config: Config,
    pub(crate) git_timeout: Duration,
    pub(crate) pool_id: Option<crate::PoolId>,
    pub(crate) retained: bool,
}
impl Pool {
    /// Create a pool, or explicitly resume matching interrupted initialization.
    ///
    /// # Errors
    /// Rejects invalid options, conflicting configuration, unsafe managed paths,
    /// unsupported environments, and Git or filesystem failures.
    pub fn init(repo: impl AsRef<Path>, options: PoolOptions) -> Result<InitResult> {
        Self::init_named(repo, &crate::PoolName::default(), options)
    }

    /// Initialize a named namespace with independent slots, settings and history.
    /// `default` retains the original state layout. Checkout roots cannot overlap
    /// another namespace, including pending or retained deleted pools.
    /// # Errors
    /// See [`Self::init`]; also rejects overlapping checkout-root claims.
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// let name = "frontend".parse()?;
    /// groove_core::Pool::init_named("/path/to/repo", &name, Default::default())?;
    /// # Ok(()) }
    /// ```
    pub fn init_named(
        repo: impl AsRef<Path>,
        name: &crate::PoolName,
        options: PoolOptions,
    ) -> Result<InitResult> {
        if let Some(ready) = &options.ready {
            ready.validate(options.capacity)?;
        }
        if !(1..=crate::MAX_CAPACITY).contains(&options.capacity) || options.git_timeout.is_zero() {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Capacity must be 1..=4096 and Git timeout positive",
            ));
        }
        let repo = Repository::discover_named(repo.as_ref(), name, options.git_timeout)?;
        if let Some(ready) = &options.ready {
            ready.validate_revision(&repo, options.git_timeout)?;
        }
        repository::mkdir(&repo.root)?;
        repository::owned_dir(&repo.root)?;
        repository::mkdir(&repo.root.join("locks"))?;
        let lifecycle_gate = crate::lifecycle::gate(&repo, true)?;
        let previous = crate::lifecycle::read(&repo)?;
        let _admin = crate::namespace::admin(&repo, options.git_timeout)?;
        let admin_id = locking::identity(&_admin.file)?;
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
            None => repo.default_worktree_root(options.worktree_base)?,
        };
        if previous
            .as_ref()
            .is_some_and(|r| r.phase == crate::lifecycle::Phase::Ready)
            || (repo.config_path().exists()
                && !previous.as_ref().is_some_and(|r| {
                    matches!(
                        r.phase,
                        crate::lifecycle::Phase::Deleted | crate::lifecycle::Phase::Initializing
                    )
                }))
        {
            return Err(Error::new(
                crate::ErrorCode::ConfigConflict,
                "Pool already initialized; inspect or update it instead of running init again",
            ));
        }
        if previous
            .as_ref()
            .is_some_and(|r| r.phase == crate::lifecycle::Phase::Deleting)
        {
            return Err(Error::new(
                crate::ErrorCode::ConfigConflict,
                "Complete destroy before initializing another pool",
            ));
        }
        crate::lifecycle::validate_retired_locks(&repo)?;
        if repo.config_path().try_exists()? {
            let config: Config = state::read(&repo.config_path())?;
            config.validate()?;
            crate::lifecycle::validate_config_locks(&repo, &config)?;
        }
        if overlaps_administration(&repo, &requested) {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Worktree root overlaps repository administration",
            ));
        }
        if let Some(checkout) = enclosing_checkout(&repo, &requested, options.git_timeout)? {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                format!(
                    "Worktree root is inside the checkout {}; `git add -A` or `git clean` there would capture or delete pooled work. Choose a directory outside every checkout",
                    checkout.display()
                ),
            ));
        }
        crate::namespace::validate_root(&repo, &requested)?;
        // Persist initialization intent before creating external storage. Conflicting
        // resumptions must fail, even when the ready manifest was never published.
        let intent = repo.root.join("initializing.json");
        let expected = InitIntent {
            ready: options.ready.clone(),
            schema_version: state::SCHEMA_VERSION,
            capacity: options.capacity,
            cleanup_policy: options.cleanup_policy,
            worktree_root: requested.clone(),
        };
        if intent.exists() {
            if !options.resume {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Interrupted initialization exists; retry the original settings with init --resume",
                ));
            }
            if state::read::<InitIntent>(&intent)? != expected {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Interrupted initialization has a different configuration",
                ));
            }
        } else {
            if options.resume {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "No interrupted initialization to resume",
                ));
            }
            if requested.exists() && crate::namespace::root_has_contents(&repo, &requested)? {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Worktree root must initially be empty",
                ));
            }
            state::write(&intent, &expected)?;
        }
        let mut lifecycle = crate::lifecycle::Lifecycle {
            schema_version: state::SCHEMA_VERSION,
            pool_id: previous
                .as_ref()
                .filter(|r| r.phase == crate::lifecycle::Phase::Initializing)
                .map_or_else(crate::PoolId::generate, |r| r.pool_id),
            phase: crate::lifecycle::Phase::Initializing,
            lock_identity: locking::identity(&lifecycle_gate)?,
            discard_changes: false,
        };
        repository::mkdir(&requested)?;
        repository::owned_dir(&requested)?;
        state::checkpoint("init-before-root-claim");
        if let Err(error) = crate::namespace::claim_root(&repo, &requested) {
            if error.code == crate::ErrorCode::ConfigConflict && !options.resume {
                // Another repository won the root race. This invocation has
                // published only its own intent, so allow a different root on
                // retry without touching the winner's storage or permanent locks.
                fs::remove_file(&intent)?;
                state::sync_dir(&repo.root)?;
            }
            return Err(error);
        }
        crate::lifecycle::write(&repo, &lifecycle)?;
        for name in ["slots", "tmp"] {
            repository::mkdir(&repo.root.join(name))?;
        }
        let mut slots = BTreeMap::new();
        let mut execution_locks = BTreeMap::new();
        for i in 1..=options.capacity {
            let slot = crate::SlotId::new(i)?;
            slots.insert(slot.clone(), create_lock(&repo.lock_path(&slot))?);
            execution_locks.insert(slot.clone(), create_lock(&repo.execution_lock_path(&slot))?);
            let _slot = SlotGuard::try_lock(&repo.lock_path(&slot), slots.get(&slot).copied())?
                .ok_or_else(|| {
                    Error::new(
                        crate::ErrorCode::ConfigConflict,
                        "Slot still has a live owner",
                    )
                })?;
            let _execution = SlotGuard::try_lock(
                &repo.execution_lock_path(&slot),
                execution_locks.get(&slot).copied(),
            )?
            .ok_or_else(|| {
                Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Execution still has a live owner",
                )
            })?;
            for path in [repo.protection_path(&slot), repo.execution_path(&slot)] {
                match fs::symlink_metadata(&path) {
                    Ok(_) => {
                        return Err(Error::new(
                            crate::ErrorCode::ConfigConflict,
                            "Slot retains durable protection",
                        )
                        .with_details(|d| d.path = Some(path)))
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.into()),
                }
            }
            if repo.record_path(&slot).try_exists()? {
                let record: SlotRecord = state::read(&repo.record_path(&slot))?;
                if record.schema_version != state::SCHEMA_VERSION {
                    return Err(Error::new(
                        crate::ErrorCode::UnsupportedConfiguration,
                        "Unsupported slot metadata version",
                    ));
                }
            }
            // New initialization resets diagnostic records; finish receipts and
            // execution history are separate and remain available.
            state::write(&repo.record_path(&slot), &SlotRecord::empty())?;
        }
        state::sync_dir(&repo.root.join("locks"))?;
        state::sync_dir(&repo.root)?;
        let config = Config {
            ready_lock: if options.ready.is_some() {
                Some(create_lock(&repo.root.join("locks/ready.lock"))?)
            } else {
                None
            },
            ready: options
                .ready
                .map(|mut ready| {
                    ready.resolve_head(&repo, options.git_timeout)?;
                    Ok::<_, crate::Error>(ready)
                })
                .transpose()?,
            schema_version: state::SCHEMA_VERSION,
            capacity: options.capacity,
            cleanup_policy: options.cleanup_policy,
            worktree_root: requested,
            admin_identity: admin_id,
            slots,
            execution_locks,
        };
        state::sync_dir(&repo.root.join("locks"))?;
        state::checkpoint("before-ready-manifest");
        state::write(&repo.config_path(), &config)?;
        lifecycle.phase = crate::lifecycle::Phase::Ready;
        crate::lifecycle::write(&repo, &lifecycle)?;
        fs::remove_file(intent)?;
        state::sync_dir(&repo.root)?;
        Ok(Self {
            repo,
            config,
            git_timeout: options.git_timeout,
            pool_id: Some(lifecycle.pool_id),
            retained: false,
        }
        .init_result(true))
    }
    /// Discover an initialized pool from any checkout of its repository, bounding
    /// discovery Git calls by [`DEFAULT_GIT_TIMEOUT`](crate::DEFAULT_GIT_TIMEOUT).
    ///
    /// # Errors
    /// Rejects missing/incompatible metadata, changed lock identities, unsafe paths,
    /// and Git or filesystem failures.
    pub fn open(repo: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_timeout(repo, crate::DEFAULT_GIT_TIMEOUT)
    }
    /// Like [`Self::open`], with an explicit positive bound for discovery Git calls
    /// and for each snapshot subprocess/lock wait when finishing a reservation.
    ///
    /// # Errors
    /// See [`Self::open`]; additionally rejects a zero timeout.
    pub fn open_with_timeout(repo: impl AsRef<Path>, git_timeout: Duration) -> Result<Self> {
        Self::open_named_with_timeout(repo, &crate::PoolName::default(), git_timeout)
    }

    /// Open a named pool with the default discovery/Git timeout.
    /// # Errors
    /// See [`Self::open`]. Selection never falls back to a different namespace.
    pub fn open_named(repo: impl AsRef<Path>, name: &crate::PoolName) -> Result<Self> {
        Self::open_named_with_timeout(repo, name, crate::DEFAULT_GIT_TIMEOUT)
    }

    /// Open a named pool with a positive per-Git-operation timeout.
    /// # Errors
    /// See [`Self::open_with_timeout`].
    pub fn open_named_with_timeout(
        repo: impl AsRef<Path>,
        name: &crate::PoolName,
        git_timeout: Duration,
    ) -> Result<Self> {
        if git_timeout.is_zero() {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Git timeout must be positive",
            ));
        }
        Self::load(
            Repository::discover_named(repo.as_ref(), name, git_timeout)?,
            git_timeout,
        )
    }
    pub(crate) fn load(repo: Repository, git_timeout: Duration) -> Result<Self> {
        Self::load_mode(repo, git_timeout, false)
    }

    /// Open a ready or deleted pool to retry saved-release receipts.
    ///
    /// # Errors
    /// Reports invalid metadata, changed lock identities, unsafe paths and Git/I/O errors.
    pub fn open_retained(repo: impl AsRef<Path>, git_timeout: Duration) -> Result<Self> {
        Self::open_retained_named(repo, &crate::PoolName::default(), git_timeout)
    }

    /// Open a named pool for saved-release retries, including after deletion.
    /// # Errors
    /// See [`Self::open_retained`]; an absent namespace never selects another pool.
    pub fn open_retained_named(
        repo: impl AsRef<Path>,
        name: &crate::PoolName,
        git_timeout: Duration,
    ) -> Result<Self> {
        if git_timeout.is_zero() {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Git timeout must be positive",
            ));
        }
        Self::load_mode(
            Repository::discover_named(repo.as_ref(), name, git_timeout)?,
            git_timeout,
            true,
        )
    }

    pub(crate) fn load_mode(
        repo: Repository,
        git_timeout: Duration,
        retained: bool,
    ) -> Result<Self> {
        let not_initialized =
            || Error::new(crate::ErrorCode::NotInitialized, "Pool is not initialized");
        if !repo.root.exists() {
            return Err(not_initialized());
        }
        repository::owned_dir(&repo.root)?;
        if !repo.config_path().exists() {
            return Err(not_initialized());
        }
        let _gate = crate::lifecycle::gate(&repo, false)?;
        let lifecycle = crate::lifecycle::read(&repo)?;
        crate::lifecycle::require_phase(lifecycle.as_ref(), retained)?;
        let config: Config = state::read(&repo.config_path())?;
        config.validate()?;
        if !config.worktree_root.is_absolute()
            || overlaps_administration(&repo, &config.worktree_root)
        {
            return Err(Error::new(
                crate::ErrorCode::ConfigConflict,
                "Unsafe manifest worktree root",
            ));
        }
        if !config.worktree_root.exists() && !retained {
            // Manifests and native worktree registrations record absolute paths,
            // so a moved repository leaves them pointing at its old location.
            // Legacy default roots lived at `<common>/groove/worktrees`.
            let recorded_common = repo.recorded_default_common(&config.worktree_root);
            let message = match recorded_common {
                Some(old) if old != repo.common => format!(
                    "The pool was created for {} but the repository is now at {}; pools record absolute paths and cannot be moved. Move the repository back to its original location (see docs/operations.md)",
                    old.display(),
                    repo.common.display()
                ),
                _ => format!(
                    "The pool's worktree root {} no longer exists; restore it at that path (see docs/operations.md)",
                    config.worktree_root.display()
                ),
            };
            return Err(Error::new(crate::ErrorCode::ConfigConflict, message)
                .with_details(|d| d.path = Some(config.worktree_root.clone())));
        }
        if config.worktree_root.exists() {
            repository::owned_dir(&config.worktree_root)?;
        }
        repository::owned_dir(&repo.root.join("locks"))?;
        repository::owned_dir(&repo.root.join("slots"))?;
        let admin = locking::open(&repo.admin_lock_path(), false)?;
        if locking::identity(&admin)? != config.admin_identity {
            return Err(Error::new(
                crate::ErrorCode::IoError,
                "Administrative lock identity changed",
            ));
        }
        if let Some(expected) = config.ready_lock {
            if locking::identity(&locking::open(&repo.root.join("locks/ready.lock"), false)?)?
                != expected
            {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Ready worker lock identity changed",
                ));
            }
        }
        Ok(Self {
            repo,
            config,
            git_timeout,
            pool_id: lifecycle.map(|r| r.pool_id),
            retained,
        })
    }
    fn init_result(&self, created: bool) -> InitResult {
        InitResult {
            ready: self.config.ready.clone(),
            pool_name: self.repo.name.clone(),
            created,
            common_git_dir: self.repo.common.clone(),
            state_dir: self.repo.root.clone(),
            capacity: self.config.capacity,
            slots: self.config.slots.keys().cloned().collect(),
            cleanup_policy: self.config.cleanup_policy,
            worktree_root: self.config.worktree_root.clone(),
        }
    }
    /// Return the capacity observed when this handle was opened.
    /// Use [`Self::settings`] for current settings; allocation refreshes each scan.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.config.capacity
    }
    /// Canonical repository administration shared by every pool and checkout.
    #[must_use]
    pub fn common_git_dir(&self) -> &Path {
        &self.repo.common
    }
    /// Return the selected namespace. Names never establish slot ownership.
    #[must_use]
    pub fn name(&self) -> &crate::PoolName {
        &self.repo.name
    }
    /// Acquire and verify a detached checkout of the requested commit.
    ///
    /// Previous slot contents are disposable and may be deleted or rebuilt.
    ///
    /// # Errors
    /// Rejects invalid revisions and unsupported Git configuration. Reports exhausted
    /// capacity, capacity wait timeout, unusable slots, and Git or filesystem failures.
    pub fn acquire(&self, options: AcquireOptions) -> Result<Lease> {
        let _operation = self.operation()?;
        if options.git_timeout.is_zero() {
            return Err(Error::new(
                crate::ErrorCode::InvalidArgument,
                "Git timeout must be positive",
            ));
        }
        let git = Git::new(options.git_timeout);
        let revision = format!("{}^{{commit}}", options.base);
        // Resolve where the caller discovered the pool: `HEAD` in a linked
        // worktree (or inside a slot) names that checkout's HEAD.
        let bytes = git
            .run(
                &self.repo.origin,
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
                if e.code == crate::ErrorCode::GitError && e.git_exit_code() == Some(1) {
                    let details = e.details.clone();
                    let unborn = options.base == "HEAD"
                        && git
                            .run(&self.repo.origin, &["symbolic-ref", "--quiet", "HEAD"], &[])
                            .is_ok();
                    Error::new(
                        crate::ErrorCode::InvalidRevision,
                        if unborn {
                            "HEAD has no commits yet; create a first commit or pass a base commit"
                        } else {
                            "Base does not resolve to a commit"
                        },
                    )
                    .with_details(|d| *d = *details)
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
        let scan_seed = Uuid::new_v4().as_u128();
        let start = Instant::now();
        let mut failed = BTreeMap::new();
        let mut retry_delay = Duration::from_millis(10);
        loop {
            let current = self.refreshed()?;
            let mut slots: Vec<_> = current.config.slots.iter().collect();
            let start_slot = (scan_seed % slots.len() as u128) as usize;
            slots.rotate_left(start_slot);
            // Hints influence preference only; descriptor ownership, protection
            // and normal preparation still validate every selected checkout.
            if current.config.ready.is_some() {
                slots.sort_by_key(|(slot, _)| !current.ready_hint(slot));
            }
            let mut busy = 0;
            let mut observed_processes = Vec::new();
            let mut preparing = false;
            for (slot, expected) in slots {
                if failed.contains_key(slot) {
                    continue;
                }
                // Durable protection prohibits allocation even without an owner.
                // Observe it before probing ownership so capacity waiters do not
                // transiently lock a reservation and spuriously block release.
                // The check under the acquired descriptor below remains required
                // for protection published between this observation and locking.
                if state::read::<state::ReuseProtection>(&self.repo.protection_path(slot))
                    .is_ok_and(|record| {
                        record.schema_version == state::SCHEMA_VERSION
                            && &record.lease.slot_id == slot
                    })
                {
                    busy += 1;
                    continue;
                }
                // Failed/malformed observations retain the original ownership
                // probe and validation below; an active descriptor still counts
                // as contention, and unowned corrupt metadata fails closed.
                let guard = match SlotGuard::try_lock(&self.repo.lock_path(slot), Some(*expected)) {
                    Ok(Some(g)) => g,
                    Ok(None) => {
                        preparing |= current.config.ready.is_some()
                            && state::read::<SlotRecord>(&self.repo.record_path(slot)).is_ok_and(
                                |r| {
                                    matches!(
                                        r.phase,
                                        crate::state::SlotPhase::Available
                                            | crate::state::SlotPhase::Released
                                    )
                                },
                            );
                        tracing::trace!(event = "slot.busy", slot = %slot, "Slot busy");
                        busy += 1;
                        continue;
                    }
                    Err(e) => {
                        failed.insert(slot.clone(), e.summary());
                        continue;
                    }
                };
                // Protection is a durable prohibition on cleanup, not evidence of
                // lock ownership. It survives a tool server's death while external
                // writers may still be using a path that server exposed.
                match fs::symlink_metadata(self.repo.protection_path(slot)) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => {
                        failed.insert(slot.clone(), e.to_string());
                        continue;
                    }
                    Ok(_) => {
                        match state::read::<state::ReuseProtection>(
                            &self.repo.protection_path(slot),
                        ) {
                            Ok(record)
                                if record.schema_version == state::SCHEMA_VERSION
                                    && &record.lease.slot_id == slot =>
                            {
                                busy += 1;
                            }
                            Ok(_) => {
                                failed
                                    .insert(slot.clone(), "Invalid reuse protection record".into());
                            }
                            Err(e) => {
                                failed.insert(slot.clone(), e.summary());
                            }
                        }
                        tracing::debug!(event = "slot.protected", slot = %slot, "Slot protected; skipping");
                        continue;
                    }
                }
                if let Ok(record) = state::read::<serde_json::Value>(&self.repo.record_path(slot)) {
                    if record["schema_version"]
                        .as_u64()
                        .is_some_and(|v| v != u64::from(state::SCHEMA_VERSION))
                    {
                        return Err(Error::new(
                            crate::ErrorCode::UnsupportedConfiguration,
                            "Unsupported slot metadata version",
                        ));
                    }
                }
                if let Err(error) = current.validate_ready_state(slot) {
                    // Unknown versions remain a compatibility barrier. Corrupt
                    // advisory state quarantines only this candidate; never let
                    // it hide independent healthy capacity or authorize cleanup.
                    if error.code == crate::ErrorCode::UnsupportedConfiguration {
                        return Err(error);
                    }
                    failed.insert(slot.clone(), error.summary());
                    continue;
                }
                let validation = recovery::validate_slot_path(&self.repo, &current.config, slot);
                let path = current.config.worktree_root.join(slot.as_str());
                if validation.is_ok() && path.try_exists()? {
                    let processes =
                        crate::quiescence::all_processes_using(&fs::canonicalize(&path)?)?;
                    if !processes.is_empty() {
                        busy += 1;
                        observed_processes
                            .extend(processes.into_iter().take(16 - observed_processes.len()));
                        continue;
                    }
                }
                match validation.and_then(|()| {
                    recovery::prepare(
                        &self.repo,
                        &current.config,
                        slot,
                        &guard,
                        &oid,
                        options.git_timeout,
                    )
                }) {
                    Ok(recovery) => {
                        let info = crate::LeaseInfo {
                            slot_id: slot.clone(),
                            lease_id: crate::LeaseId::generate(),
                            worktree_path: current.config.worktree_root.join(slot.as_str()),
                            base_oid: oid.clone(),
                            head_mode: crate::HeadMode::Detached,
                            recovery,
                            cleanup_policy: current.config.cleanup_policy,
                            ownership: crate::Ownership::InProcess,
                            task: None,
                            request_key: None,
                            requested_base: None,
                            acquired_at_unix_ms: Some(crate::reservation::now_ms()),
                        };
                        let record = SlotRecord {
                            released_head: None,
                            idle_since_unix_ms: None,
                            schema_version: state::SCHEMA_VERSION,
                            phase: crate::state::SlotPhase::Leased,
                            lease: Some(info.clone()),
                            failure: None,
                        };
                        state::write(&self.repo.record_path(slot), &record)?;
                        state::checkpoint("lease-recorded");
                        return Ok(Lease::new(current.clone(), guard, info));
                    }
                    Err(mut e) => {
                        let record = SlotRecord {
                            released_head: None,
                            idle_since_unix_ms: None,
                            schema_version: state::SCHEMA_VERSION,
                            phase: crate::state::SlotPhase::Quarantined,
                            lease: None,
                            failure: Some(e.summary()),
                        };
                        let _ = state::write(&self.repo.record_path(slot), &record);
                        failed.insert(slot.clone(), e.summary());
                        if e.is_git_timeout() {
                            e.code = crate::ErrorCode::PoolUnhealthy;
                            e.details.capacity = Some(crate::CapacityFailure {
                                capacity: current.config.capacity,
                                busy_slots: busy,
                                failed_slots: failed,
                            });
                            return Err(e);
                        }
                    }
                }
            }
            // A spare being materialized is preparation, not a reservation using
            // capacity. Permit its bounded preparation to finish even with wait=0.
            // Once its record becomes leased, ordinary capacity semantics resume.
            if preparing && start.elapsed() < options.git_timeout {
                std::thread::sleep(
                    retry_delay.min(options.git_timeout.saturating_sub(start.elapsed())),
                );
                retry_delay = (retry_delay * 2).min(Duration::from_millis(100));
                continue;
            }
            if busy == 0 || options.wait.is_zero() || start.elapsed() >= options.wait {
                let code = if busy == 0 {
                    crate::ErrorCode::PoolUnhealthy
                } else if options.wait.is_zero() {
                    crate::ErrorCode::PoolExhausted
                } else {
                    crate::ErrorCode::AcquireTimeout
                };
                return Err(Error::new(code, "No verified worktree slot is available")
                    .with_details(|d| {
                        d.processes = observed_processes;
                        d.capacity = Some(crate::CapacityFailure {
                            capacity: current.config.capacity,
                            busy_slots: busy,
                            failed_slots: failed,
                        })
                    })
                    .retryable(busy > 0));
            }
            std::thread::sleep(retry_delay.min(options.wait.saturating_sub(start.elapsed())));
            retry_delay = (retry_delay * 2).min(Duration::from_millis(100));
        }
    }
    /// List durable reuse protections (all CLI reservations).
    /// This is diagnostic metadata; it does not establish current lock ownership.
    ///
    /// # Errors
    /// Reports malformed/unsupported protection records and filesystem errors.
    pub fn protected_leases(&self) -> Result<Vec<crate::LeaseInfo>> {
        let pool = self.refreshed()?;
        let mut leases = Vec::new();
        for slot in pool.config.slots.keys() {
            let path = self.repo.protection_path(slot);
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
                Ok(_) => (),
            }
            match state::read::<state::ReuseProtection>(&path) {
                Ok(record) => {
                    if record.schema_version != state::SCHEMA_VERSION
                        || &record.lease.slot_id != slot
                    {
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

    /// Release a durable reservation or abandoned protected lease after stopping ALL writers.
    ///
    /// This intentionally does not return a usable checkout. The next acquisition
    /// normalizes and verifies it. A live lock owner must release its own lease.
    ///
    /// # Errors
    /// Rejects busy locks, missing active protections, stale generations and I/O failures.
    /// Repeating a completed release succeeds while its generation remains current.
    pub fn release_protected(
        &self,
        slot: &crate::SlotId,
        lease_id: crate::LeaseId,
    ) -> Result<crate::ReleaseResult> {
        let _operation = self.operation()?;
        let pool = self.refreshed()?;
        let identity = pool
            .config
            .slots
            .get(slot)
            .ok_or_else(|| Error::new(crate::ErrorCode::InvalidLease, "Unknown slot"))?;
        let guard = match SlotGuard::try_lock(&self.repo.lock_path(slot), Some(*identity))? {
            Some(guard) => guard,
            None => {
                // A background preparer may own an already released generation.
                // Acknowledge its completed release without unlocking or mutating
                // anything. A successor's published generation never matches.
                let record: SlotRecord = state::read(&self.repo.record_path(slot))?;
                if record.schema_version == state::SCHEMA_VERSION
                    && record.phase == crate::state::SlotPhase::Released
                    && record
                        .lease
                        .as_ref()
                        .is_some_and(|l| l.lease_id == lease_id && &l.slot_id == slot)
                    && matches!(fs::symlink_metadata(self.repo.protection_path(slot)), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
                {
                    return Ok(crate::ReleaseResult {
                        slot_id: slot.clone(),
                        lease_id,
                        released: true,
                        cleanup: crate::CleanupTiming::Deferred,
                    });
                }
                return Err(Error::new(
                    crate::ErrorCode::InvalidLease,
                    "Protected lease still has a live lock owner",
                ));
            }
        };
        match fs::symlink_metadata(self.repo.protection_path(slot)) {
            Ok(_) => {
                let record: state::ReuseProtection = state::read(&self.repo.protection_path(slot))?;
                if record.schema_version != state::SCHEMA_VERSION
                    || &record.lease.slot_id != slot
                    || record.lease.lease_id != lease_id
                {
                    return Err(Error::new(
                        crate::ErrorCode::LeaseMismatch,
                        "Protected lease generation mismatch",
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // A lost release response may leave only the same generation's
                // released receipt. Never release an ordinary unprotected lease.
                let record: state::SlotRecord = state::read(&self.repo.record_path(slot))?;
                if record.schema_version != state::SCHEMA_VERSION
                    || record.phase != state::SlotPhase::Released
                    || !record
                        .lease
                        .as_ref()
                        .is_some_and(|l| &l.slot_id == slot && l.lease_id == lease_id)
                {
                    return Err(Error::new(
                        crate::ErrorCode::LeaseMismatch,
                        "No matching released generation or active protection",
                    ));
                }
            }
            Err(e) => return Err(e.into()),
        }
        use std::os::fd::AsFd;
        crate::lease::release(&pool, slot, lease_id, guard.file.as_fd())
    }
}

/// Settings persisted before initialization creates storage, so an interrupted
/// `init` can only be resumed with identical settings.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InitIntent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<crate::ReadySettings>,
    pub schema_version: u32,
    pub capacity: usize,
    pub cleanup_policy: crate::CleanupPolicy,
    pub worktree_root: std::path::PathBuf,
}

/// Whether a checkout root would place worktrees inside, or around, Git or Groove
/// administration. The legacy `<state>/worktrees` root remains accepted.
pub(crate) fn overlaps_administration(repo: &Repository, root: &Path) -> bool {
    root == repo.root
        || repo.common.starts_with(root)
        || (root.starts_with(&repo.common) && root != repo.legacy_worktree_root())
}

/// The registered working tree (main or linked) that would contain `root`, if any.
/// Legacy roots inside Git administration are never scanned by Git.
pub(crate) fn enclosing_checkout(
    repo: &Repository,
    root: &Path,
    timeout: Duration,
) -> Result<Option<std::path::PathBuf>> {
    if root.starts_with(&repo.common) {
        return Ok(None);
    }
    let listing = Git::new(timeout).run(
        &repo.common,
        &["worktree", "list", "--porcelain", "-z"],
        &[],
    )?;
    use std::os::unix::ffi::OsStrExt;
    Ok(listing
        .split(|b| *b == 0)
        .filter_map(|field| field.strip_prefix(b"worktree "))
        .map(|path| std::path::PathBuf::from(std::ffi::OsStr::from_bytes(path)))
        .find(|checkout| root.starts_with(checkout)))
}

/// Create (or adopt) a permanent lock file and durably record its identity.
fn create_lock(path: &Path) -> Result<locking::Identity> {
    let file = locking::open(path, true)?;
    file.sync_all()?;
    locking::identity(&file)
}
