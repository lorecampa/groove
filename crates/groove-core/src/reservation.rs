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
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Longest accepted task label, in bytes.
const MAX_TASK_BYTES: usize = 256;
/// Longest accepted request key, in bytes.
const MAX_REQUEST_KEY_BYTES: usize = 128;

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Durable reservation settings. Keys are scoped to one pool namespace and never expire.
///
/// ```no_run
/// use groove_core::{Pool, ReservationOptions};
/// let pool = Pool::open("/path/to/repo")?;
/// let lease = pool.reserve(ReservationOptions {
///     task: Some("fix parser".into()),
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
    /// Task label, at most 256 bytes, usable with [`Pool::lease_by_task`]; never an
    /// ownership credential.
    pub task: Option<String>,
    /// Unique logical operation key, 1..128 bytes. Reuse only to retry the same task/base.
    pub request_key: Option<String>,
}
impl ReservationOptions {
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
                "Task must be 1..256 bytes and request key 1..128 bytes",
            ));
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

/// Journal entry for a request key; completed keys stay tombstoned forever.
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

impl Pool {
    fn request_path(&self, key: &str) -> PathBuf {
        let digest = crate::project::hex(Sha256::digest(key.as_bytes()));
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
        self.lock_permanent(&self.request_path(key).with_extension("lock"), options)
    }

    /// Permanent lock serializing reservations that share one task label.
    fn label_lock_path(&self, task: &str) -> PathBuf {
        let digest = crate::project::hex(Sha256::digest(task.as_bytes()));
        self.repo
            .root
            .join("requests")
            .join(format!("task-{digest}.lock"))
    }

    /// Open (creating if needed) and wait for a permanent lock under `requests/`.
    fn lock_permanent(
        &self,
        lock: &std::path::Path,
        options: &AcquireOptions,
    ) -> Result<SlotGuard> {
        let file = locking::open(lock, true)?;
        file.sync_all()?;
        state::sync_dir(&self.repo.root.join("requests"))?;
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

    /// Reserve a verified checkout, with crash-safe task-key retries across restarts.
    ///
    /// The checkout is clean and its HEAD is detached at the requested base.
    ///
    /// # Errors
    /// Rejects reused/completed keys and changed base/task inputs.
    /// A lost response can be recovered by retrying the same key. No timeout or process
    /// exit releases a reservation.
    pub fn reserve(&self, options: ReservationOptions) -> Result<LeaseInfo> {
        let _operation = self.operation()?;
        options.validate()?;
        repository::mkdir(&self.repo.root.join("requests"))?;
        repository::owned_dir(&self.repo.root.join("requests"))?;
        let _key_guard = match &options.request_key {
            Some(key) => {
                let guard = self.lock_request_key(key, &options.acquire)?;
                if let Some(info) = self.resume_request(key, &options)? {
                    return Ok(info);
                }
                Some(guard)
            }
            None => None,
        };
        // Task labels select reservations, so at most one active reservation may
        // carry each label. A permanent per-label lock serializes the check with
        // protection publication, so concurrent acquisitions cannot both pass.
        let _label_guard = match &options.task {
            Some(task) => {
                let guard = self.lock_permanent(&self.label_lock_path(task), &options.acquire)?;
                if let Some(existing) = self
                    .protected_leases()?
                    .into_iter()
                    .find(|lease| lease.task.as_deref() == Some(task.as_str()))
                {
                    return Err(Error::new(
                        ErrorCode::InvalidArgument,
                        "An active reservation already has this task label; reuse it, release it, or choose another label",
                    )
                    .with_details(|d| {
                        d.lease_id = Some(existing.lease_id);
                        d.worktree_path = Some(existing.worktree_path.clone());
                    }));
                }
                Some(guard)
            }
            None => None,
        };
        let base = options.acquire.base.clone();
        let mut lease = self.acquire(options.acquire)?;
        lease.info.task = options.task;
        lease.info.request_key = options.request_key;
        lease.info.requested_base = Some(base);
        lease.info.ownership = crate::Ownership::Durable;
        state::write(
            &self.repo.record_path(&lease.info.slot_id),
            &SlotRecord {
                schema_version: state::SCHEMA_VERSION,
                phase: SlotPhase::Leased,
                lease: Some(lease.info.clone()),
                failure: None,
            },
        )?;
        lease.protect_from_reuse()?;
        state::checkpoint("reservation-protected");
        self.record_request(&lease.info, false)?;
        state::checkpoint("reservation-journaled");
        Ok(lease.info.clone())
    }

    /// Resolve a reservation or current-generation release receipt by lease ID.
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
                "Lease is unknown or superseded by another generation",
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
                "Several active reservations share this task label; select one by lease ID",
            )),
        }
    }

    /// Release by lease ID; the slot is resolved and rechecked while its lock is held.
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
        let _operation = self.operation()?;
        let pool = self.refreshed()?;
        let info = pool.lease_info(id)?;
        let expected = pool
            .config
            .slots
            .get(&info.slot_id)
            .ok_or_else(|| Error::new(ErrorCode::InvalidLease, "Unknown slot"))?;
        let guard = SlotGuard::try_lock(&pool.repo.lock_path(&info.slot_id), Some(*expected))?
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
            || record.phase != SlotPhase::Leased
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
