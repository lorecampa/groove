//! Durable run diagnostics. History never authorizes workspace reuse.
use crate::{
    error::PathContext, repository, state, Error, ErrorCode, ExecResult, ExecutionInfo, LeaseId,
    LeaseInfo, Pool, Result, SlotId, Termination,
};
use serde::{Deserialize, Serialize};
use std::{fmt, fs, path::PathBuf, str::FromStr};

/// A UUID v4 identifying one managed run, independent of its reservation.
///
/// ```
/// let id: groove_core::RunId = "00000000-0000-4000-8000-000000000001".parse()?;
/// assert!("../outside".parse::<groove_core::RunId>().is_err());
/// # Ok::<(), groove_core::Error>(())
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RunId(uuid::Uuid);
impl FromStr for RunId {
    type Err = Error;
    fn from_str(value: &str) -> Result<Self> {
        let id = uuid::Uuid::parse_str(value).map_err(|e| {
            Error::new(ErrorCode::InvalidArgument, "Run ID must be a UUID v4").with_source(e)
        })?;
        if id.get_version_num() != 4 || id.get_variant() != uuid::Variant::RFC4122 {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Run ID must be a UUID v4",
            ));
        }
        Ok(Self(id))
    }
}
impl TryFrom<String> for RunId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self> {
        value.parse()
    }
}
impl From<RunId> for String {
    fn from(value: RunId) -> String {
        value.to_string()
    }
}
impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Diagnostic origin of a managed command; never contains argv or environment values.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RunOrigin {
    /// Recovered execution whose original diagnostic record was never published.
    Unknown,
    /// A raw command.
    #[default]
    Command,
    /// One actually executed setup step (cache-skipped steps create no run).
    Setup {
        /// Project profile name.
        profile: String,
        /// Step name within that profile.
        step: String,
    },
    /// A named project recipe.
    Recipe {
        /// Recipe name.
        name: String,
    },
    /// A project or built-in agent command.
    Agent {
        /// Agent name.
        name: String,
    },
}

/// Recorded outcome. An unresolved record may describe an active or interrupted run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// No terminal outcome was durably published; never infer completion from age.
    Unresolved,
    /// Supervision and teardown completed; inspect `termination`, `exit_code` and `signal`.
    Completed,
    /// The operating system rejected spawning the program.
    SpawnFailed,
    /// An operator recovered the execution after stopping writers; exit outcome unknown.
    Recovered,
}

/// Log retention state; removing is a recoverable cleanup transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogState {
    /// Captured files are retained.
    Present,
    /// Explicit cleanup started; retry cleanup to finish removing any remaining files.
    Removing,
    /// Explicit cleanup removed both stream files.
    Removed,
}

/// Durable diagnostic history for one execution. Contains no command arguments or environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunHistory {
    /// Persisted record format.
    pub schema_version: u32,
    /// Unique run identity.
    pub run_id: RunId,
    /// Permanent slot used by this run.
    pub slot_id: SlotId,
    /// Reservation generation, retained after release and slot reuse.
    pub lease_id: LeaseId,
    /// Task label at execution time; labels may be reused.
    pub task: Option<String>,
    /// Program only, without arguments.
    pub program: String,
    /// Raw command, setup step, recipe or agent.
    pub origin: RunOrigin,
    /// Diagnostic wall-clock start time.
    pub started_at_unix_ms: u64,
    /// Time of terminal record publication or operator recovery.
    pub ended_at_unix_ms: Option<u64>,
    /// Measured elapsed time; unknown for operator-recovered runs.
    pub duration_ms: Option<u64>,
    /// Recorded outcome, independent of ownership.
    pub outcome: RunOutcome,
    /// How a supervised run ended.
    pub termination: Option<Termination>,
    /// Worker exit code, when available.
    pub exit_code: Option<i32>,
    /// Worker termination signal, when available.
    pub signal: Option<i32>,
    /// Requested cancellation signal, when available.
    pub cancel_signal: Option<i32>,
    /// Spawn failure diagnostic, if spawning failed.
    pub failure: Option<String>,
    /// Retention state of stdout/stderr files.
    pub logs: LogState,
    /// Original stdout location; may no longer exist after cleanup.
    pub stdout_path: PathBuf,
    /// Original stderr location; may no longer exist after cleanup.
    pub stderr_path: PathBuf,
}

/// Filters over historical labels and generations, not just active reservations.
#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    /// Match this historical task label exactly.
    pub task: Option<String>,
    /// Match this reservation generation.
    pub lease_id: Option<LeaseId>,
}
impl Pool {
    pub(crate) fn run_dir(&self, id: RunId) -> PathBuf {
        self.repo.root.join("task-logs").join(id.to_string())
    }
    pub(crate) fn read_run(&self, id: RunId) -> Result<RunHistory> {
        let dir = self.run_dir(id);
        repository::owned_dir(&dir)?;
        let path = dir.join("run.json");
        let record: RunHistory = state::read(&path)?;
        if record.schema_version != state::SCHEMA_VERSION
            || record.run_id != id
            || record.stdout_path != dir.join("stdout.log")
            || record.stderr_path != dir.join("stderr.log")
            || (record.outcome == RunOutcome::Unresolved) != record.ended_at_unix_ms.is_none()
            || (record.outcome == RunOutcome::Completed) != record.termination.is_some()
            || (record.outcome == RunOutcome::Unresolved && record.logs != LogState::Present)
        {
            return Err(
                Error::new(ErrorCode::ProtocolError, "Invalid run history record")
                    .with_details(|d| d.path = Some(path)),
            );
        }
        if !self.config.slots.contains_key(&record.slot_id) {
            self.archived_execution_identity(&record.slot_id)
                .map_err(|e| {
                    Error::new(
                        ErrorCode::ProtocolError,
                        "Run history names an unknown slot",
                    )
                    .with_source(e)
                })?;
        }
        Ok(record)
    }
    pub(crate) fn write_run(&self, record: &RunHistory) -> Result<()> {
        let dir = self.run_dir(record.run_id);
        repository::owned_dir(&dir)?;
        state::write(&dir.join("run.json"), record)
    }
    pub(crate) fn begin_run(
        &self,
        lease: &LeaseInfo,
        info: &ExecutionInfo,
        origin: RunOrigin,
    ) -> Result<RunHistory> {
        let run_id = info.run_id.parse()?;
        let dir = self.run_dir(run_id);
        if info.stdout_path != dir.join("stdout.log") || info.stderr_path != dir.join("stderr.log")
        {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Execution log paths do not match its run ID",
            ));
        }
        let record = RunHistory {
            schema_version: state::SCHEMA_VERSION,
            run_id,
            slot_id: lease.slot_id.clone(),
            lease_id: lease.lease_id,
            task: lease.task.clone(),
            program: info.program.clone(),
            origin,
            started_at_unix_ms: info.started_at_unix_ms,
            ended_at_unix_ms: None,
            duration_ms: None,
            outcome: RunOutcome::Unresolved,
            termination: None,
            exit_code: None,
            signal: None,
            cancel_signal: None,
            failure: None,
            logs: LogState::Present,
            stdout_path: info.stdout_path.clone(),
            stderr_path: info.stderr_path.clone(),
        };
        self.write_run(&record)?;
        Ok(record)
    }
    pub(crate) fn complete_run(
        &self,
        record: &mut RunHistory,
        result: &ExecResult,
        elapsed: std::time::Duration,
    ) -> Result<()> {
        record.outcome = RunOutcome::Completed;
        record.ended_at_unix_ms = Some(crate::reservation::now_ms());
        record.duration_ms = Some(elapsed.as_millis().min(u128::from(u64::MAX)) as u64);
        record.termination = Some(result.termination);
        record.exit_code = result.exit_code;
        record.signal = result.signal;
        record.cancel_signal = result.cancel_signal;
        self.write_run(record)
    }
    pub(crate) fn recover_run(&self, lease: &LeaseInfo, info: &ExecutionInfo) -> Result<()> {
        let id = info.run_id.parse()?;
        let mut record = match fs::symlink_metadata(self.run_dir(id).join("run.json")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.begin_run(lease, info, RunOrigin::Unknown)?
            }
            Err(e) => return Err(e.into()),
            Ok(_) => self.read_run(id)?,
        };
        if record.lease_id != lease.lease_id || record.slot_id != lease.slot_id {
            return Err(Error::new(
                ErrorCode::ProtocolError,
                "Run history does not match recovery reservation",
            ));
        }
        if record.outcome == RunOutcome::Unresolved {
            record.outcome = RunOutcome::Recovered;
            record.ended_at_unix_ms = Some(crate::reservation::now_ms());
            self.write_run(&record)?;
        }
        Ok(())
    }

    /// Read a run even after its reservation is released or its logs are removed.
    ///
    /// # Errors
    /// Rejects missing, malformed or unsafe records. This observation grants no ownership.
    pub fn history_show(&self, id: RunId) -> Result<RunHistory> {
        self.refreshed()?.read_run(id)
    }

    /// List historical runs newest first, optionally matching a task and/or lease.
    ///
    /// Observations are nontransactional. Directories without history (including
    /// older runs) are omitted, never adopted as safe cleanup candidates.
    ///
    /// # Errors
    /// Reports unsafe paths, corrupt records and I/O errors; concurrent deletions are skipped.
    ///
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// let pool = groove_core::Pool::open(".")?;
    /// for run in pool.history(&groove_core::HistoryFilter::default())? {
    ///     println!("{} {:?}", run.run_id, run.outcome);
    /// }
    /// # Ok(()) }
    /// ```
    pub fn history(&self, filter: &HistoryFilter) -> Result<Vec<RunHistory>> {
        let pool = self.refreshed()?;
        let root = pool.repo.root.join("task-logs");
        repository::no_symlinks(&root)?;
        let entries = match fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).at(&root),
        };
        repository::owned_dir(&root)?;
        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.at(&root)?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Ok(id) = name.parse::<RunId>() else {
                continue;
            };
            if name != id.to_string() {
                continue;
            }
            repository::no_symlinks(&entry.path())?;
            let path = entry.path().join("run.json");
            match fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e).at(&path),
                Ok(_) => {}
            }
            let record = match pool.read_run(id) {
                Ok(record) => record,
                Err(e) if e.code == ErrorCode::IoError && !path.try_exists()? => continue,
                Err(e) => return Err(e),
            };
            if filter
                .task
                .as_ref()
                .is_none_or(|task| record.task.as_ref() == Some(task))
                && filter.lease_id.is_none_or(|lease| record.lease_id == lease)
            {
                records.push(record);
            }
        }
        records.sort_by(|a, b| {
            b.started_at_unix_ms
                .cmp(&a.started_at_unix_ms)
                .then_with(|| b.run_id.cmp(&a.run_id))
        });
        Ok(records)
    }
}
