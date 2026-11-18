//! Durable reservations: idempotent request keys, task-label lookup and release.

use crate::{
    locking::{self, SlotGuard},
    repository,
    state::{self, SlotPhase, SlotRecord},
    AcquireOptions, Error, ErrorCode, Lease, LeaseId, LeaseInfo, Pool, ReleaseResult, Result,
    SlotId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Longest accepted task label, in bytes.
const MAX_TASK_BYTES: usize = 128;
/// Longest accepted request key, in bytes.
const MAX_REQUEST_KEY_BYTES: usize = 128;

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Durable reservation settings. Keys are scoped to one pool namespace and retained
/// for at least seven days after completion; active reservations never expire.
///
/// ```no_run
/// use groove_core::{Pool, ReservationOptions};
/// let pool = Pool::open("/path/to/repo")?;
/// let lease = pool.reserve(ReservationOptions {
///     task: Some("fix-parser".into()),
///     request_key: Some("fix-parser-1".into()),
///     ..Default::default()
/// })?;
/// println!("work in {}", lease.worktree_path.display());
/// # Ok::<(), groove_core::Error>(())
/// ```
#[derive(Debug, Clone, Default)]
pub struct ReservationOptions {
    /// Baseline, wait budget and Git timeout.
    pub acquire: AcquireOptions,
    /// Repository-unique task label, 1..128 ASCII letters, digits, dots, underscores
    /// or hyphens, excluding UUID-shaped strings. Usable with [`Pool::lease_by_task`]; never an
    /// ownership credential.
    pub task: Option<String>,
    /// Unique logical operation key, 1..128 bytes. Reuse only to retry the same task/base.
    pub request_key: Option<String>,
}
impl ReservationOptions {
    /// Supply a generated label when the caller omitted one.
    ///
    /// A request key deterministically selects its label so retries across process
    /// exits keep the same inputs. Without a key, each call chooses a fresh label.
    /// Explicit labels are unchanged; [`Pool::reserve`] validates and arbitrates
    /// them. This builder performs no I/O and cannot fail.
    ///
    /// ```
    /// use groove_core::ReservationOptions;
    /// let options = ReservationOptions::default().with_generated_task();
    /// let task = options.task.unwrap();
    /// assert!(task.starts_with("task-") && task.len() == 17);
    /// ```
    pub fn with_generated_task(mut self) -> Self {
        if self.task.is_none() {
            let suffix = self.request_key.as_ref().map_or_else(
                || uuid::Uuid::new_v4().simple().to_string(),
                |key| state::hex(Sha256::digest(key.as_bytes())),
            );
            // 48 bits keeps labels short enough to type while collisions among
            // a repository's active labels stay negligible; reserve rejects any.
            self.task = Some(format!("task-{}", &suffix[..12]));
        }
        self
    }

    fn validate(&self) -> Result<()> {
        let too_long = |value: &Option<String>, max: usize| {
            value
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > max)
        };
        if too_long(&self.task, MAX_TASK_BYTES)
            || too_long(&self.request_key, MAX_REQUEST_KEY_BYTES)
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Task and request key must be 1..128 bytes",
            ));
        }
        if self.task.as_ref().is_some_and(|task| {
            !task
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
                || uuid::Uuid::parse_str(task).is_ok()
        }) {
            return Err(Error::new(ErrorCode::InvalidArgument,
                "Task must use ASCII letters, digits, dots, underscores or hyphens and must not resemble a UUID"));
        }
        // Labels are shown in terminals and tables; control characters could
        // forge output or run terminal escape sequences.
        if [&self.task, &self.request_key]
            .into_iter()
            .flatten()
            .any(|value| value.chars().any(char::is_control))
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Task and request key must not contain control characters",
            ));
        }
        Ok(())
    }
    /// Whether a recorded reservation was created from these same inputs.
    fn matches(&self, base: Option<&str>, task: Option<&String>) -> bool {
        base == Some(self.acquire.base.as_str()) && task == self.task.as_ref()
    }
}

/// Journal entry for a request key; completed keys have a seven-day retention period.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestRecord {
    schema_version: u32,
    key: String,
    base: String,
    task: Option<String>,
    lease_id: LeaseId,
    slot_id: SlotId,
    released: bool,
}

fn key_reused() -> Error {
    Error::new(
        ErrorCode::InvalidArgument,
        "Request key was already used with different base/task inputs",
    )
}

/// Retain existing permanent per-key locks; new keys and labels share bounded,
/// separate sets of 256 permanent locks. Never unlink a lock: a waiter may already
/// hold its inode. As elsewhere, mixed Groove versions in one pool are unsupported.
fn request_lock_path(repo: &repository::Repository, key: &str, label: bool) -> Result<PathBuf> {
    let digest = crate::state::hex(Sha256::digest(key.as_bytes()));
    let prefix = if label { "task-" } else { "" };
    let legacy = repo
        .root
        .join("requests")
        .join(format!("{prefix}{digest}.lock"));
    if legacy.try_exists()? {
        return Ok(legacy);
    }
    let directory = repo.root.join("request-locks");
    repository::mkdir(&directory)?;
    repository::owned_dir(&directory)?;
    Ok(directory.join(format!("{prefix}{}.lock", &digest[..2])))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PruneCursor {
    schema_version: u32,
    offset: usize,
}

/// Called with lifecycle admission by the worker, or exclusive admission by
/// destroy. Only journals are disposable; permanent synchronization inodes stay.
pub(crate) fn prune_requests(repo: &repository::Repository) -> Result<()> {
    use std::{fs, os::fd::AsFd, time::Duration};
    const RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);
    const LIMIT: usize = 1000;
    let directory = repo.root.join("requests");
    if !directory.try_exists()? {
        return Ok(());
    }
    repository::owned_dir(&directory)?;
    // Rotate the inspected window so young entries and historical lock files do
    // not permanently hide a backlog. Directory enumeration is cheap; bound the
    // metadata reads, locks and journal parsing to LIMIT entries per pass.
    let cursor_path = repo.root.join("request-prune-cursor.json");
    let offset = if cursor_path.try_exists()? {
        let cursor: PruneCursor = state::read(&cursor_path)?;
        if cursor.schema_version != state::SCHEMA_VERSION {
            return Err(Error::new(
                ErrorCode::UnsupportedConfiguration,
                "Unsupported request pruning cursor",
            ));
        }
        cursor.offset
    } else {
        0
    };
    let mut visited = 0;
    let mut removed = 0;
    for entry in fs::read_dir(&directory)?.skip(offset).take(LIMIT) {
        let entry = entry?;
        visited += 1;
        let path = entry.path();
        let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().is_none_or(|extension| extension != "json")
            || name.len() != 64
            || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        repository::no_symlinks(&path)?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let old = |path: &std::path::Path| -> Result<bool> {
            Ok(fs::metadata(path)?
                .modified()?
                .elapsed()
                .is_ok_and(|age| age > RETENTION))
        };
        if !old(&path)? {
            continue;
        }
        let record: RequestRecord = state::read(&path)?;
        if record.schema_version != state::SCHEMA_VERSION
            || state::hex(Sha256::digest(record.key.as_bytes())) != name
        {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Invalid request journal",
            ));
        }
        let lock_path = request_lock_path(repo, &record.key, false)?;
        let file = locking::open(&lock_path, true)?;
        file.sync_all()?;
        state::sync_dir(lock_path.parent().expect("request lock has a parent"))?;
        if !locking::flock(file.as_fd(), libc::LOCK_EX | libc::LOCK_NB)? {
            continue;
        }
        // Re-read under the same lock as acquire. A concurrent replay may have
        // refreshed this record after the age check above.
        let mut record: RequestRecord = state::read(&path)?;
        if !old(&path)? {
            continue;
        }
        let protection = repo.protection_path(&record.slot_id);
        if protection.try_exists()? {
            let active: state::ReuseProtection = state::read(&protection)?;
            if active.schema_version != state::SCHEMA_VERSION {
                return Err(Error::new(
                    ErrorCode::ProtocolError,
                    "Invalid reuse protection record",
                ));
            }
            if active.lease.request_key.as_ref() == Some(&record.key) {
                continue;
            }
        }
        if record.released {
            fs::remove_file(&path)?;
            removed += 1;
        } else {
            // An interrupted completion without its tombstone has no reliable
            // completion timestamp. Start retention now rather than use acquire age.
            record.released = true;
            state::write(&path, &record)?;
        }
    }
    if removed > 0 {
        state::sync_dir(&directory)?;
    }
    state::write(
        &cursor_path,
        &PruneCursor {
            schema_version: state::SCHEMA_VERSION,
            offset: if visited < LIMIT {
                0
            } else {
                offset + visited - removed
            },
        },
    )
}

impl Pool {
    fn request_path(&self, key: &str) -> PathBuf {
        let digest = crate::state::hex(Sha256::digest(key.as_bytes()));
        self.repo
            .root
            .join("requests")
            .join(format!("{digest}.json"))
    }

    pub(crate) fn record_request(&self, info: &LeaseInfo, released: bool) -> Result<()> {
        let Some(key) = &info.request_key else {
            return Ok(());
        };
        state::write(
            &self.request_path(key),
            &RequestRecord {
                schema_version: state::SCHEMA_VERSION,
                key: key.clone(),
                base: info.requested_base.clone().unwrap_or_default(),
                task: info.task.clone(),
                lease_id: info.lease_id,
                slot_id: info.slot_id.clone(),
                released,
            },
        )
    }

    /// Serialize all retries of one request key on its permanent lock file.
    fn lock_request_key(&self, key: &str, options: &AcquireOptions) -> Result<SlotGuard> {
        self.lock_permanent(&request_lock_path(&self.repo, key, false)?, options)
    }

    /// Permanent lock serializing reservations that share one task label.
    fn label_lock_path(&self, task: &str) -> Result<PathBuf> {
        let directory = self.repo.namespace_root().join("task-labels");
        repository::mkdir(&directory)?;
        repository::owned_dir(&directory)?;
        let digest = state::hex(Sha256::digest(task.as_bytes()));
        Ok(directory.join(format!("{}.lock", &digest[..2])))
    }

    // Registry records accelerate diagnosis, but never confer ownership. Always
    // reconcile against protections under the repository label lock, including
    // pools created by older versions and crashes before registry publication.
    fn label_reservations(&self, task: &str) -> Result<Vec<(crate::PoolName, LeaseInfo)>> {
        let mut matches = Vec::new();
        for repo in crate::namespace::repositories(&self.repo)? {
            if crate::namespace::summary(&repo)?.phase != crate::PoolPhase::Ready {
                continue;
            }
            let pool = Pool::load(repo, self.git_timeout)?;
            for lease in pool.protected_leases()? {
                if lease.task.as_deref() == Some(task) {
                    matches.push((pool.repo.name.clone(), lease));
                }
            }
        }
        Ok(matches)
    }

    fn record_label(&self, lease: &LeaseInfo) -> Result<()> {
        if let Some(task) = &lease.task {
            let digest = state::hex(Sha256::digest(task.as_bytes()));
            state::write(
                &self
                    .repo
                    .namespace_root()
                    .join("task-labels")
                    .join(format!("{digest}.json")),
                &serde_json::json!({"schema_version": state::SCHEMA_VERSION, "task": task,
                    "pool": self.repo.name, "reservation_id": lease.lease_id}),
            )?;
        }
        Ok(())
    }

    /// Open (creating if needed) and wait for a permanent request/label lock.
    fn lock_permanent(
        &self,
        lock: &std::path::Path,
        options: &AcquireOptions,
    ) -> Result<SlotGuard> {
        let file = locking::open(lock, true)?;
        file.sync_all()?;
        state::sync_dir(lock.parent().expect("request lock has a parent"))?;
        SlotGuard::wait(lock, Some(locking::identity(&file)?), options.git_timeout)
    }

    /// Return the existing reservation for a retried key, or `None` for a fresh key.
    fn resume_request(&self, key: &str, options: &ReservationOptions) -> Result<Option<LeaseInfo>> {
        let path = self.request_path(key);
        let journal = match state::read::<RequestRecord>(&path) {
            Ok(record) => Some(record),
            Err(e) if !path.exists() && e.code == ErrorCode::IoError => None,
            Err(e) => return Err(e),
        };
        if let Some(record) = &journal {
            if record.schema_version != state::SCHEMA_VERSION
                || record.key != key
                || !options.matches(Some(&record.base), record.task.as_ref())
            {
                return Err(key_reused());
            }
            if record.released {
                return Err(Error::new(
                    ErrorCode::InvalidLease,
                    "Request key was completed; use a new key for a new task",
                ));
            }
        }
        // Protection itself contains the key, recovering a crash before journal publication.
        if let Some(info) = self
            .protected_leases()?
            .into_iter()
            .find(|info| info.request_key.as_deref() == Some(key))
        {
            if !options.matches(info.requested_base.as_deref(), info.task.as_ref()) {
                return Err(key_reused());
            }
            let record: SlotRecord = state::read(&self.repo.record_path(&info.slot_id))?;
            if record.phase != SlotPhase::Leased
                || record.lease.as_ref().map(|i| i.lease_id) != Some(info.lease_id)
            {
                self.record_request(&info, true)?;
                return Err(Error::new(
                    ErrorCode::InvalidLease,
                    "Request key was completed; use a new key",
                ));
            }
            self.record_request(&info, false)?;
            return Ok(Some(info));
        }
        if journal.is_some() {
            return Err(Error::new(
                ErrorCode::InvalidLease,
                "Original reservation is no longer active; use a new request key",
            ));
        }
        Ok(None)
    }

    /// Reserve a verified checkout, or recover a reservation by its request key.
    ///
    /// A new reservation starts clean with HEAD detached at the requested base.
    /// Retrying an active request key with the same base expression and task label
    /// returns its original metadata without cleaning the checkout or resetting
    /// edits. This also applies if the base branch has moved since acquisition.
    /// Reservations survive process exit and require explicit release after all
    /// writers have stopped.
    ///
    /// # Errors
    /// Rejects invalid labels or keys, labels held by another reservation, completed
    /// keys and changed base/task inputs. Also reports acquisition, lock and metadata
    /// failures. A lost response can be recovered by retrying the same inputs and key.
    pub fn reserve(&self, options: ReservationOptions) -> Result<LeaseInfo> {
        let _operation = self.operation()?;
        options.validate()?;
        repository::mkdir(&self.repo.root.join("requests"))?;
        repository::owned_dir(&self.repo.root.join("requests"))?;
        let _key_guard = match &options.request_key {
            Some(key) => {
                let guard = self.lock_request_key(key, &options.acquire)?;
                Some(guard)
            }
            None => None,
        };
        // Task labels select reservations, so at most one active reservation may
        // carry each label. A permanent per-label lock serializes the check with
        // protection publication, so concurrent acquisitions cannot both pass.
        let _label_guard = options
            .task
            .as_ref()
            .map(|task| self.lock_permanent(&self.label_lock_path(task)?, &options.acquire))
            .transpose()?;
        let resumed = options
            .request_key
            .as_ref()
            .map(|key| self.resume_request(key, &options))
            .transpose()?
            .flatten();
        if let Some(task) = &options.task {
            let matches = self.label_reservations(task)?;
            if matches.len() > 1 {
                return Err(Error::new(ErrorCode::InvalidArgument, format!(
                        "Task '{task}' has legacy duplicate reservations in pools {}. Release each by reservation UUID and reacquire with distinct labels; no reservation was changed",
                        matches.iter().map(|(name, _)| name.to_string()).collect::<Vec<_>>().join(", "))));
            }
            if let Some((pool, existing)) = matches.first().filter(|(_, existing)| {
                resumed
                    .as_ref()
                    .is_none_or(|info| info.lease_id != existing.lease_id)
            }) {
                return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        format!("Task '{task}' is already active in pool '{pool}'; use its reservation, release it, or choose another label"),
                    )
                    .with_details(|d| {
                        d.lease_id = Some(existing.lease_id);
                        d.worktree_path = Some(existing.worktree_path.clone());
                    }));
            }
        }
        if let Some(info) = resumed {
            self.record_label(&info)?;
            return Ok(info);
        }
        let base = options.acquire.base.clone();
        let mut lease = self.acquire(options.acquire)?;
        lease.info.task = options.task;
        lease.info.request_key = options.request_key;
        lease.info.requested_base = Some(base);
        lease.info.ownership = crate::Ownership::Durable;
        state::write(
            &self.repo.record_path(&lease.info.slot_id),
            &SlotRecord {
                released_head: None,
                idle_since_unix_ms: None,
                schema_version: state::SCHEMA_VERSION,
                phase: SlotPhase::Leased,
                lease: Some(lease.info.clone()),
                failure: None,
            },
        )?;
        lease.protect_from_reuse()?;
        state::checkpoint("reservation-protected");
        self.record_label(&lease.info)?;
        self.record_request(&lease.info, false)?;
        state::checkpoint("reservation-journaled");
        Ok(lease.info)
    }

    /// Resolve a reservation or current-generation release receipt by reservation ID.
    ///
    /// # Errors
    /// Rejects unknown/stale IDs and malformed metadata. Resolving never grants lock ownership.
    pub fn lease_info(&self, id: LeaseId) -> Result<LeaseInfo> {
        let current = self.refreshed()?;
        let mut failure = None;
        for slot in current.config.slots.keys() {
            let record: SlotRecord = match state::read(&self.repo.record_path(slot)) {
                Ok(record) => record,
                Err(error) => {
                    failure.get_or_insert(error);
                    continue;
                }
            };
            if let Some(info) = record.lease {
                if info.lease_id == id && &info.slot_id == slot {
                    return Ok(info);
                }
            }
        }
        Err(failure.unwrap_or_else(|| {
            Error::new(
                ErrorCode::LeaseMismatch,
                "Reservation is unknown or superseded by another generation",
            )
        }))
    }

    /// Resolve the single active reservation whose task label is `task`.
    ///
    /// # Errors
    /// Rejects labels that match no reservation, or more than one.
    pub fn lease_by_task(&self, task: &str) -> Result<LeaseInfo> {
        let mut matches = self
            .protected_leases()?
            .into_iter()
            .filter(|lease| lease.task.as_deref() == Some(task));
        match (matches.next(), matches.next()) {
            (Some(lease), None) => Ok(lease),
            (None, _) => Err(Error::new(
                ErrorCode::LeaseMismatch,
                "No active reservation has this task label",
            )),
            (Some(_), Some(_)) => Err(Error::new(
                ErrorCode::InvalidArgument,
                "Several active reservations share this task label; select one by reservation ID",
            )),
        }
    }

    /// Resolve the active durable reservation for the registered workspace containing `path`.
    ///
    /// Subdirectories and filesystem aliases resolve through Git to the workspace
    /// root. The native registration, selected pool, protection and slot generation
    /// must agree; directory names, task labels and historical receipts alone are
    /// insufficient. This observation grants no descriptor ownership. Callers
    /// must retain the returned ID and use ordinary release/finish operations,
    /// which revalidate that generation under its kernel lock.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidArgument`] outside an acquired workspace in
    /// this pool, [`ErrorCode::LeaseMismatch`] for inconsistent generations, and
    /// propagates discovery, registration, lifecycle and metadata errors.
    ///
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// let pool = groove_core::Pool::open(".")?;
    /// let reservation = pool.lease_for_workspace(".")?;
    /// // Stop all writers, then release using reservation.lease_id.
    /// # let _ = reservation;
    /// # Ok(()) }
    /// ```
    pub fn lease_for_workspace(&self, path: impl AsRef<Path>) -> Result<LeaseInfo> {
        use std::os::unix::ffi::OsStrExt;
        let _operation = self.operation()?;
        let pool = self.refreshed()?;
        let invalid = || {
            Error::new(ErrorCode::InvalidArgument,
            "No active reservation can be inferred in the selected pool; supply --task or --reservation-id")
        };
        let repo = repository::Repository::discover(path.as_ref(), self.git_timeout)?;
        if repo.common != pool.repo.common {
            return Err(invalid());
        }
        let top = crate::git::Git::new(self.git_timeout).run(
            &repo.origin,
            &["rev-parse", "--show-toplevel"],
            &[],
        )?;
        let root = std::fs::canonicalize(Path::new(std::ffi::OsStr::from_bytes(
            top.strip_suffix(b"\n").unwrap_or(&top),
        )))?;
        let slot = root
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.parse::<SlotId>().ok())
            .filter(|slot| {
                root.parent() == Some(pool.config.worktree_root.as_path())
                    && pool.config.slots.contains_key(slot)
            })
            .ok_or_else(invalid)?;
        crate::recovery::registration(&pool.repo, &root)?;
        let path = pool.repo.protection_path(&slot);
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(invalid()),
            Err(error) => return Err(error.into()),
            Ok(_) => (),
        }
        let protection: state::ReuseProtection = state::read(&path)?;
        let record: SlotRecord = state::read(&pool.repo.record_path(&slot))?;
        if protection.schema_version != state::SCHEMA_VERSION
            || record.schema_version != state::SCHEMA_VERSION
        {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Unsupported workspace reservation metadata",
            ));
        }
        if protection.lease.slot_id != slot
            || protection.lease.worktree_path != root
            || record.phase != SlotPhase::Leased
            || record
                .lease
                .as_ref()
                .map(|lease| (lease.lease_id, &lease.slot_id, &lease.worktree_path))
                != Some((protection.lease.lease_id, &slot, &root))
        {
            return Err(Error::new(ErrorCode::LeaseMismatch,
                "Workspace reservation changed or release is already in progress; retry with the original reservation ID"));
        }
        // Read-only selection intentionally retains no slot lock. The release
        // path must reject this ID if another generation wins the slot meanwhile.
        state::checkpoint("workspace-reservation-selected");
        Ok(protection.lease)
    }

    /// Release by reservation ID; the slot is resolved and rechecked while its lock is held.
    /// All external writers must already have stopped.
    ///
    /// # Errors
    /// Rejects stale IDs, active descriptor/managed owners and metadata failures.
    pub fn release_lease(&self, id: LeaseId) -> Result<ReleaseResult> {
        self.release_protected(&self.lease_info(id)?.slot_id, id)
    }

    /// Hold a durable reservation's kernel lock, for a server or checkpoint operation.
    ///
    /// # Errors
    /// Rejects active owners and released/stale generations; protection is never removed.
    pub fn hold_reserved(&self, id: LeaseId) -> Result<Lease> {
        self.hold_protected(id, false)
    }

    /// Finish may recapture a partially released generation while its durable
    /// protection still prevents reuse. Ordinary reservation holders cannot.
    pub(crate) fn hold_for_finish(&self, id: LeaseId) -> Result<Lease> {
        self.hold_protected(id, true)
    }

    fn hold_protected(&self, id: LeaseId, resume_release: bool) -> Result<Lease> {
        let _operation = self.operation()?;
        let pool = self.refreshed()?;
        let info = pool.lease_info(id)?;
        let expected = pool
            .config
            .slots
            .get(&info.slot_id)
            .ok_or_else(|| Error::new(ErrorCode::InvalidLease, "Unknown slot"))?;
        let guard = SlotGuard::claim_reserved(
            &pool.repo.lock_path(&info.slot_id),
            *expected,
            self.git_timeout,
        )?
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidLease,
                "Reservation has a live descriptor owner",
            )
        })?;
        let record: SlotRecord = state::read(&pool.repo.record_path(&info.slot_id))?;
        let protection: state::ReuseProtection =
            state::read(&pool.repo.protection_path(&info.slot_id))?;
        if record.schema_version != state::SCHEMA_VERSION
            || !(record.phase == SlotPhase::Leased
                || (resume_release && record.phase == SlotPhase::Released))
            || record.lease.as_ref().map(|i| i.lease_id) != Some(id)
            || protection.schema_version != state::SCHEMA_VERSION
            || protection.lease.lease_id != id
        {
            return Err(Error::new(
                ErrorCode::LeaseMismatch,
                "Reservation is no longer active",
            ));
        }
        Ok(Lease::new(pool, guard, info))
    }
}
