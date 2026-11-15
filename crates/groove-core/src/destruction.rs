//! Explicit, resumable removal of idle pool workspaces and disposable artifacts.
use crate::{
    lifecycle::{self, Lifecycle, Phase},
    locking::{self, SlotGuard},
    pool::InitIntent,
    repository::{self, Repository},
    state::{self, Config, SlotRecord},
    Error, ErrorCode, Pool, PoolId, Result, SlotId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Options for [`Pool::destroy`]. No option bypasses ownership or durable protection.
#[derive(Debug, Clone)]
pub struct DestroyOptions {
    /// Validate and describe removal without deleting files or changing lifecycle state.
    pub dry_run: bool,
    /// Explicitly discard residual edits, untracked/ignored files and broken checkouts.
    /// All reservations must still be released and all writers stopped.
    pub discard_changes: bool,
    /// Optional generation precondition; use the ID from a preview for safe automation.
    pub pool_id: Option<PoolId>,
    /// Positive bound for each Git operation and administrative-lock wait.
    pub git_timeout: Duration,
}
impl Default for DestroyOptions {
    fn default() -> Self {
        Self {
            dry_run: false,
            discard_changes: false,
            pool_id: None,
            git_timeout: crate::DEFAULT_GIT_TIMEOUT,
        }
    }
}

/// Removal plan or completed deletion. Permanent coordination is retained.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestroyResult {
    /// Namespace selected for removal.
    #[serde(default)]
    pub pool_name: crate::PoolName,
    /// Pool generation, absent in previews before a generation was durably assigned.
    pub pool_id: Option<PoolId>,
    /// Whether this invocation was a preview.
    pub dry_run: bool,
    /// True only when deletion has durably completed.
    pub deleted: bool,
    /// Whether a previous invocation already completed this deletion.
    pub already_deleted: bool,
    /// Whether this invocation continued an interrupted removal.
    pub resumed: bool,
    /// Retained directory containing locks, journals, receipts and execution history.
    pub state_dir: PathBuf,
    /// Checkout root; the root itself is retained, even when external.
    pub worktree_root: PathBuf,
    /// Slots whose paths and exact native registrations are removed (including lazy slots).
    pub slots: Vec<SlotId>,
    /// Disposable artifact/staging directories removed, excluding permanent cache locks.
    pub artifacts: Vec<PathBuf>,
}

fn conflict(message: &str, path: &Path) -> Error {
    Error::new(ErrorCode::ConfigConflict, message).with_details(|d| d.path = Some(path.into()))
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn stopped(path: &Path) -> Result<()> {
    if !exists(path)? {
        return Ok(());
    }
    let processes = crate::quiescence::all_processes_using(&fs::canonicalize(path)?)?;
    if !processes.is_empty() {
        return Err(Error::new(ErrorCode::WorkspaceInUse, "Processes still use data selected for deletion; stop them and run destroy from outside the pool")
            .with_details(|d| { d.path = Some(path.into()); d.processes = processes; }));
    }
    Ok(())
}

// Refuse filesystem boundaries inside a tree: a mounted build directory must not
// redirect recursive removal onto another filesystem. Interior symlinks are unlinked,
// never traversed, by remove_dir_all.
pub(crate) fn validate_tree(path: &Path, device: u64) -> Result<()> {
    #[cfg(target_os = "linux")]
    crate::platform::mounts::reject_below(path)?;
    validate_device_tree(path, device)
}

fn validate_device_tree(path: &Path, device: u64) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        if metadata.dev() != device {
            return Err(conflict("Mounted directory in removal tree", path));
        }
        for item in fs::read_dir(path)? {
            validate_device_tree(&item?.path(), device)?;
        }
    }
    Ok(())
}

fn artifacts(repo: &Repository) -> Result<Vec<PathBuf>> {
    let root = repo.root.join("artifacts");
    if !exists(&root)? {
        return Ok(Vec::new());
    }
    repository::owned_dir(&root)?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .ok_or_else(|| conflict("Unknown artifact entry", &path))?;
        let hex = |s: &str| {
            s.len() == 64
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        if name.strip_suffix(".lock").is_some_and(hex) {
            let _ = locking::open(&path, false)?;
        } else if hex(name) || (name.starts_with(".cache-stage-") && name.len() > 13) {
            repository::owned_dir(&path)?;
            validate_tree(&path, fs::metadata(&root)?.dev())?;
            stopped(&path)?;
            paths.push(path);
        } else {
            return Err(conflict(
                "Unknown artifact entry; preserve or move it before deletion",
                &path,
            ));
        }
    }
    paths.sort();
    Ok(paths)
}

fn require_saved(repo: &Repository, path: &Path, timeout: Duration) -> Result<()> {
    crate::recovery::registration(repo, path)?;
    if crate::recovery::interrupted(repo, path)? {
        return Err(Error::new(ErrorCode::UnsavedChanges, "Interrupted Git operation remains; recover it or acknowledge loss with --discard-changes")
            .with_details(|d| d.path = Some(path.into())));
    }
    let git = crate::git::Git::new(timeout);
    let audit = git.audit_index(path, crate::git::Invocation::default())?;
    if audit.assume_unchanged || audit.skip_worktree {
        return Err(Error::new(ErrorCode::UnsupportedConfiguration, "Index flags can hide edits; clear assume-unchanged/skip-worktree flags or use --discard-changes")
            .with_details(|d| d.path = Some(path.into())));
    }
    let status = git.run_with(
        path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignored",
            "--no-renames",
            "--no-ahead-behind",
        ],
        crate::git::Invocation {
            reject_stderr: true,
            ..Default::default()
        },
    )?;
    if !status.is_empty() {
        return Err(Error::new(ErrorCode::UnsavedChanges, "Workspace contains residual files or edits; save them or acknowledge loss with --discard-changes")
            .with_details(|d| d.path = Some(path.into())));
    }
    if let Some(special) = crate::workspace_files::first_special(path, timeout)? {
        return Err(Error::new(ErrorCode::UnsavedChanges,
            "Workspace contains a special file; export it or acknowledge loss with --discard-changes")
            .with_details(|details| details.path = Some(special)));
    }
    crate::quiescence::require_saved_head(path, timeout)
}
impl Pool {
    /// Remove an idle pool's checkouts, exact Git registrations and disposable caches.
    ///
    /// All reservations must first be finished/released and every writer stopped.
    /// Permanent locks, request-key tombstones, recoverable finish receipts, logs/history,
    /// branches and snapshots survive. The checkout root itself is retained.
    /// Completed receipts for missing snapshots may be pruned; pending pin cleanup is kept.
    /// A durable deletion record blocks allocation after interruption; retry with
    /// the same `discard_changes` policy. A new `init` is allowed only after completion.
    ///
    /// Use `pool_id` from a preview when retries must never target a replacement pool.
    /// An interrupted init can also be cancelled, without first materializing slots.
    /// This implicit `default` selection refuses when several non-deleted pools
    /// exist; use [`Self::destroy_named`] to select one explicitly.
    ///
    /// # Errors
    /// Refuses live descriptors, durable reservations/executions, observed processes,
    /// unsaved residual files (unless acknowledged), unsafe paths, unknown metadata,
    /// mismatched generations and Git/I/O failures. No force option overrides locks.
    /// Failure after removal starts retains the deletion state for explicit retry.
    ///
    /// ```no_run
    /// use groove_core::{DestroyOptions, Pool};
    /// # fn main() -> groove_core::Result<()> {
    /// let preview = Pool::destroy("/path/to/repo", DestroyOptions { dry_run: true, ..Default::default() })?;
    /// Pool::destroy("/path/to/repo", DestroyOptions { pool_id: preview.pool_id, ..Default::default() })?;
    /// # Ok(()) }
    /// ```
    pub fn destroy(repo: impl AsRef<Path>, options: DestroyOptions) -> Result<DestroyResult> {
        Self::destroy_selected(repo, &crate::PoolName::default(), false, options)
    }

    /// Delete exactly the selected namespace, leaving every other pool intact.
    /// # Errors
    /// See [`Self::destroy`]. A mismatched `pool_id` never selects another namespace.
    pub fn destroy_named(
        repo: impl AsRef<Path>,
        name: &crate::PoolName,
        options: DestroyOptions,
    ) -> Result<DestroyResult> {
        Self::destroy_selected(repo, name, true, options)
    }

    fn destroy_selected(
        repo: impl AsRef<Path>,
        name: &crate::PoolName,
        explicit: bool,
        options: DestroyOptions,
    ) -> Result<DestroyResult> {
        if options.git_timeout.is_zero() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Git timeout must be positive",
            ));
        }
        let repo = Repository::discover_named(repo.as_ref(), name, options.git_timeout)?;
        if !exists(&repo.root)? {
            return Err(Error::new(
                ErrorCode::NotInitialized,
                "Pool is not initialized",
            ));
        }
        repository::owned_dir(&repo.root)?;
        repository::owned_dir(&repo.root.join("locks"))?;
        let gate = if options.dry_run {
            lifecycle::preview_gate(&repo, options.git_timeout)?
        } else {
            lifecycle::destroy_gate(&repo, options.git_timeout)?
        };
        let previous = lifecycle::read(&repo)?;
        let intent_path = repo.root.join("initializing.json");
        // A new init can die after publishing intent but before assigning its new
        // generation. Do not mistake that intent for an already-deleted old pool.
        let pending_init =
            previous.as_ref().is_some_and(|r| r.phase == Phase::Deleted) && exists(&intent_path)?;
        let pool_id = previous
            .as_ref()
            .filter(|_| !pending_init)
            .map(|r| r.pool_id);
        if options.pool_id.is_some() && pool_id != options.pool_id {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "Pool ID does not match; refusing to delete a replacement pool",
            ));
        }
        let admin_file = locking::open(&repo.admin_lock_path(), false)?;
        let _admin = SlotGuard::wait(
            &repo.admin_lock_path(),
            Some(locking::identity(&admin_file)?),
            options.git_timeout,
        )?;
        if !explicit {
            let count = crate::namespace::repositories(&repo)?
                .iter()
                .map(crate::namespace::summary)
                .collect::<Result<Vec<_>>>()?
                .iter()
                .filter(|p| p.phase != crate::PoolPhase::Deleted)
                .count();
            if count > 1 {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    "Several pools exist; select one explicitly with --pool NAME before deletion",
                ));
            }
        }
        let initializing = previous
            .as_ref()
            .is_some_and(|r| r.phase == Phase::Initializing)
            || ((!exists(&repo.config_path())? || pending_init) && exists(&intent_path)?);
        if initializing && exists(&repo.config_path())? {
            let published: Config = state::read(&repo.config_path())?;
            published.validate()?;
            lifecycle::validate_config_locks(&repo, &published)?;
        }
        let config: Config = if initializing {
            let intent: InitIntent = state::read(&intent_path)?;
            if intent.schema_version != state::SCHEMA_VERSION
                || !(1..=crate::MAX_CAPACITY).contains(&intent.capacity)
            {
                return Err(Error::new(
                    ErrorCode::UnsupportedConfiguration,
                    "Invalid initialization intent",
                ));
            }
            Config {
                ready_lock: if exists(&repo.root.join("locks/ready.lock"))? {
                    Some(locking::identity(&locking::open(
                        &repo.root.join("locks/ready.lock"),
                        false,
                    )?)?)
                } else {
                    None
                },
                // An interrupted init has no published worker infrastructure.
                ready: None,
                schema_version: state::SCHEMA_VERSION,
                capacity: intent.capacity,
                cleanup_policy: intent.cleanup_policy,
                worktree_root: intent.worktree_root,
                admin_identity: locking::identity(&admin_file)?,
                slots: BTreeMap::new(),
                execution_locks: BTreeMap::new(),
            }
        } else {
            if !exists(&repo.config_path())? {
                return Err(Error::new(
                    ErrorCode::NotInitialized,
                    "Pool has no manifest or initialization intent",
                ));
            }
            let config: Config = state::read(&repo.config_path())?;
            config.validate()?;
            config
        };
        if config.admin_identity != locking::identity(&admin_file)? {
            return Err(conflict(
                "Administrative lock identity changed",
                &repo.admin_lock_path(),
            ));
        }
        let mut result = DestroyResult {
            pool_name: repo.name.clone(),
            pool_id,
            dry_run: options.dry_run,
            deleted: false,
            already_deleted: false,
            resumed: previous
                .as_ref()
                .is_some_and(|r| r.phase == Phase::Deleting),
            state_dir: repo.root.clone(),
            worktree_root: config.worktree_root.clone(),
            slots: (1..=config.capacity)
                .map(SlotId::new)
                .collect::<Result<_>>()?,
            artifacts: Vec::new(),
        };
        if !initializing && previous.as_ref().is_some_and(|r| r.phase == Phase::Deleted) {
            post_deletion_cleanup(&repo, &config.worktree_root, options.git_timeout);
            result.deleted = true;
            result.already_deleted = true;
            return Ok(result);
        }
        lifecycle::validate_retired_locks(&repo)?;
        if result.resumed
            && previous
                .as_ref()
                .is_some_and(|r| r.discard_changes != options.discard_changes)
        {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "Retry destroy with the original --discard-changes policy",
            ));
        }
        let root = &config.worktree_root;
        repository::no_symlinks(root)?;
        if !root.is_absolute() || crate::pool::overlaps_administration(&repo, root) {
            return Err(conflict("Unsafe checkout root", root));
        }
        // A recorded default root from another repository location cannot be adopted.
        if repo
            .recorded_default_common(root)
            .is_some_and(|common| common != repo.common)
        {
            return Err(conflict(
                "Moved pool; restore the repository at its original location before deletion",
                root,
            ));
        }
        if crate::pool::enclosing_checkout(&repo, root, options.git_timeout)?.is_some() {
            return Err(conflict("Checkout root is inside a Git checkout", root));
        }
        if exists(root)? {
            repository::owned_dir(root)?;
        }
        // Exclusive lifecycle admission prevents every new cooperating owner.
        // Probe and close individually: successful probes remain valid while this
        // gate is held, without requiring 8192 open FDs at maximum capacity.
        // Include unpublished growth and retired slots, and surviving children.
        for item in fs::read_dir(repo.root.join("locks"))? {
            let item = item?;
            let name = item.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| conflict("Unknown lock entry", &item.path()))?;
            if matches!(name, "admin.lock" | "lifecycle.lock") {
                continue;
            }
            if name == "ready.lock" {
                let _guard =
                    SlotGuard::try_lock(&item.path(), config.ready_lock)?.ok_or_else(|| {
                        conflict("Ready worker still owns its descriptor", &item.path())
                            .retryable(true)
                    })?;
                continue;
            }
            let stem = name
                .strip_suffix(".execution.lock")
                .or_else(|| name.strip_suffix(".lock"));
            let slot = stem
                .and_then(|s| s.parse::<SlotId>().ok())
                .ok_or_else(|| conflict("Unknown lock entry", &item.path()))?;
            let expected = if name.ends_with(".execution.lock") {
                config.execution_locks.get(&slot)
            } else {
                config.slots.get(&slot)
            };
            let _guard =
                SlotGuard::try_lock(&item.path(), expected.copied())?.ok_or_else(|| {
                    conflict(
                        "Slot or execution still has a live descriptor owner",
                        &item.path(),
                    )
                    .retryable(true)
                })?;
        }
        // Missing configured locks must fail too; directory enumeration is not proof.
        for (slot, identity) in &config.slots {
            if locking::identity(&locking::open(&repo.lock_path(slot), false)?)? != *identity {
                return Err(conflict(
                    "Permanent slot lock identity changed",
                    &repo.lock_path(slot),
                ));
            }
            if locking::identity(&locking::open(&repo.execution_lock_path(slot), false)?)?
                != config.execution_locks[slot]
            {
                return Err(conflict(
                    "Permanent execution lock identity changed",
                    &repo.execution_lock_path(slot),
                ));
            }
        }
        if exists(&repo.root.join("slots"))? {
            repository::owned_dir(&repo.root.join("slots"))?;
            for item in fs::read_dir(repo.root.join("slots"))? {
                let item = item?;
                let name = item.file_name();
                let name = name.to_string_lossy();
                if name.ends_with(".protected.json") || name.ends_with(".execution.json") {
                    return Err(conflict("Release reservations before deleting the pool; legacy executions must be resolved with the previous Groove version", &item.path()));
                }
            }
        }
        for slot in &result.slots {
            let path = root.join(slot.as_str());
            repository::no_symlinks(&path)?;
            crate::recovery::validate_disposal(&repo, &path)?;
            let record_path = repo.record_path(slot);
            if exists(&record_path)? {
                let record: SlotRecord = state::read(&record_path)?;
                if record.schema_version != state::SCHEMA_VERSION
                    || record.lease.as_ref().is_some_and(|l| l.slot_id != *slot)
                {
                    return Err(conflict("Invalid slot metadata", &record_path));
                }
            } else if !initializing {
                return Err(conflict("Missing slot metadata", &record_path));
            }
            if exists(&path)? {
                if initializing {
                    return Err(conflict(
                        "Interrupted initialization has an unexpected checkout",
                        &path,
                    ));
                }
                // Git creates checkouts using the caller's umask, so they can be
                // group-writable inside the private, owner-only managed root.
                let metadata = fs::metadata(&path)?;
                if !metadata.is_dir() || metadata.uid() != fs::metadata(root)?.uid() {
                    return Err(conflict(
                        "Checkout must be a directory owned by the pool user",
                        &path,
                    ));
                }
                validate_tree(&path, fs::metadata(root)?.dev())?;
                stopped(&path)?;
                if !options.discard_changes && !result.resumed {
                    require_saved(&repo, &path, options.git_timeout)?;
                }
            }
        }
        result.artifacts = artifacts(&repo)?;
        if options.dry_run {
            return Ok(result);
        }
        let mut record = Lifecycle {
            schema_version: state::SCHEMA_VERSION,
            pool_id: result.pool_id.unwrap_or_else(PoolId::generate),
            phase: Phase::Deleting,
            lock_identity: locking::identity(&gate)?,
            discard_changes: options.discard_changes,
        };
        // Cancelling init first persists a manifest, allowing retries after its intent
        // is removed. No new slot locks are invented or removed in this path.
        if initializing {
            let mut manifest = config.clone();
            repository::mkdir(&repo.root.join("slots"))?;
            for slot in &result.slots {
                for (path, identities) in [
                    (repo.lock_path(slot), &mut manifest.slots),
                    (
                        repo.execution_lock_path(slot),
                        &mut manifest.execution_locks,
                    ),
                ] {
                    let file = locking::open(&path, true)?;
                    file.sync_all()?;
                    identities.insert(slot.clone(), locking::identity(&file)?);
                }
                if !exists(&repo.record_path(slot))? {
                    state::write(&repo.record_path(slot), &SlotRecord::empty())?;
                }
            }
            state::sync_dir(&repo.root.join("locks"))?;
            state::write(&repo.config_path(), &manifest)?;
        }
        lifecycle::write(&repo, &record)?;
        result.pool_id = Some(record.pool_id);
        state::checkpoint("pool-deleting");
        for slot in &result.slots {
            crate::recovery::remove_owned(&repo, &root.join(slot.as_str()))?;
            state::checkpoint("pool-slot-removed");
        }
        for path in &result.artifacts {
            repository::owned_dir(path)?;
            crate::workspace_files::make_writable(path)?;
            fs::remove_dir_all(path)?;
            state::sync_dir(path.parent().expect("artifact entries have a parent"))?;
            state::checkpoint("pool-artifact-removed");
        }
        if exists(&intent_path)? {
            fs::remove_file(&intent_path)?;
            state::sync_dir(&repo.root)?;
        }
        repository::mkdir(&repo.root.join("retired"))?;
        let archived: Config = state::read(&repo.config_path())?;
        state::write(
            &repo
                .root
                .join("retired")
                .join(format!("{}.json", record.pool_id)),
            &archived,
        )?;
        record.phase = Phase::Deleted;
        lifecycle::write(&repo, &record)?;
        state::checkpoint("pool-deleted");
        post_deletion_cleanup(&repo, root, options.git_timeout);
        result.deleted = true;
        Ok(result)
    }
}

fn post_deletion_cleanup(repo: &Repository, root: &Path, timeout: Duration) {
    if let Err(error) = crate::namespace::release_root_claim(repo, root) {
        tracing::warn!(%error, "Pool deleted, but checkout root claim could not be released; rerun destroy to retry");
    }
    if let Err(error) = crate::finish::prune_missing_receipts(repo, timeout) {
        tracing::warn!(%error, "Pool deleted, but missing snapshot receipts could not be pruned; rerun destroy to retry");
    }
}
