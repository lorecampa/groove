pub use crate::platform::signals::ExecutionSignals;
use crate::platform::{
    control::{send_stop, StopListener},
    signals::cancellation_mask,
};
use crate::{
    repository,
    state::{self, SlotPhase, SlotRecord},
    Error, ErrorCode, LeaseId, LeaseInfo, Pool, Result, SlotId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{FileExt, OpenOptionsExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::PathBuf,
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

/// Longest accepted SIGTERM-to-SIGKILL grace period.
const MAX_GRACE: Duration = Duration::from_secs(60);
/// How long teardown waits for a signalled process group to disappear.
const GROUP_EXIT_DEADLINE: Duration = Duration::from_secs(2);
/// Supervisor polling interval while a worker runs.
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// The original stream of a captured worker output chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    /// Worker standard output.
    Stdout,
    /// Worker standard error; this does not imply error severity.
    Stderr,
}

/// Optional observer of captured worker output. Core never prints this output.
///
/// Callbacks run on the supervisor thread and **must return promptly**, without
/// blocking I/O or panicking. Use a bounded queue and `try_send` for presentation.
/// Chunks are arbitrary bytes, may split UTF-8 or lines, and preserve order within
/// each stream only. Observation is best effort; saved files are authoritative.
///
/// ```
/// use groove_core::{ExecutionObserver, OutputStream};
/// #[derive(Debug)]
/// struct Viewer(std::sync::mpsc::SyncSender<Vec<u8>>);
/// impl ExecutionObserver for Viewer {
///     fn output(&self, _stream: OutputStream, bytes: &[u8]) {
///         let _ = self.0.try_send(bytes.to_vec());
///     }
/// }
/// ```
pub trait ExecutionObserver: std::fmt::Debug + Send + Sync {
    /// Observe bytes already captured in the run's log file.
    fn output(&self, stream: OutputStream, bytes: &[u8]);
}

/// Options for one explicitly launched command. The reservation is never automatically released.
#[derive(Debug, Clone)]
pub struct ExecOptions {
    /// Program and arguments, executed directly without shell interpolation.
    pub command: Vec<OsString>,
    /// Maximum runtime; zero means no runtime limit.
    pub timeout: Duration,
    /// Grace period between SIGTERM and SIGKILL, at most 60 seconds.
    pub grace: Duration,
    /// Maximum bytes in each captured stream; exceeding it stops the command.
    pub max_log_bytes: u64,
    /// Optional cooperative cancellation flag, checked throughout managed execution.
    pub cancellation: Option<Arc<AtomicBool>>,
    /// Explicit ordinary environment overrides. GIT_/GROOVE_ names are reserved.
    pub environment: BTreeMap<String, String>,
    /// Optional nonblocking observer; capture and execution limits are independent.
    pub observer: Option<Arc<dyn ExecutionObserver>>,
    /// Diagnostic origin stored with durable run history.
    pub origin: crate::RunOrigin,
}
impl Default for ExecOptions {
    fn default() -> Self {
        Self {
            command: Vec::new(),
            timeout: Duration::ZERO,
            grace: Duration::from_millis(500),
            max_log_bytes: 8 * 1024 * 1024,
            cancellation: None,
            environment: BTreeMap::new(),
            observer: None,
            origin: crate::RunOrigin::Command,
        }
    }
}
impl ExecOptions {
    fn validate(&self) -> Result<()> {
        if self.command.is_empty()
            || self.max_log_bytes == 0
            || self.grace > MAX_GRACE
            || self
                .environment
                .iter()
                .any(|(key, value)| !crate::project::env_valid(key, value))
        {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Exec needs a program, positive output limit and grace at most 60 seconds",
            ));
        }
        Ok(())
    }
}

/// Durable diagnostic record and reuse prohibition for a managed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionInfo {
    /// Format version.
    pub schema_version: u32,
    /// Reservation generation.
    pub lease_id: LeaseId,
    /// Unique run identifier, distinct from the reservation and operating-system PID.
    pub run_id: String,
    /// Program only; potentially sensitive argument values are not persisted.
    pub program: String,
    /// Start time for diagnostics only.
    pub started_at_unix_ms: u64,
    /// Leader PID, diagnostic only; never sufficient authority for recovery or signalling.
    pub leader_pid: Option<u32>,
    /// Platform-local control endpoint name for asking the live supervisor to stop.
    pub control_name: String,
    /// Standard-output file outside the disposable checkout.
    pub stdout_path: PathBuf,
    /// Standard-error file outside the disposable checkout.
    pub stderr_path: PathBuf,
}

/// Why a managed worker stopped running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Termination {
    /// The worker exited (or was killed by a signal) on its own.
    Exited,
    /// The runtime deadline elapsed and Groove stopped the worker.
    TimedOut,
    /// A signal or explicit stop request cancelled the worker.
    Cancelled,
    /// A captured stream exceeded its byte limit and Groove stopped the worker.
    OutputLimited,
}

/// Result of managed execution. A successful execution operation may contain a failed worker exit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecResult {
    /// Reservation remains protected after execution.
    pub lease_id: LeaseId,
    /// Run metadata and captured output locations.
    pub run: ExecutionInfo,
    /// Why the worker stopped.
    pub termination: Termination,
    /// Program's exit code, absent for termination by signal.
    pub exit_code: Option<i32>,
    /// Signal that terminated the worker, if any.
    pub signal: Option<i32>,
    /// Signal (or SIGTERM for an explicit stop request) that caused cancellation.
    pub cancel_signal: Option<i32>,
}
impl ExecResult {
    /// Whether the worker exited on its own with status zero.
    pub fn succeeded(&self) -> bool {
        self.termination == Termination::Exited && self.exit_code == Some(0)
    }
    /// Shell-style status for this run: the worker's exit code, `124` for a timeout,
    /// `128 + signal` for cancellation or signal death, and `1` when output was limited.
    ///
    /// ```
    /// # use groove_core::{ExecResult, Termination};
    /// # fn check(result: &ExecResult) {
    /// if result.termination == Termination::TimedOut {
    ///     assert_eq!(result.shell_status(), 124);
    /// }
    /// # }
    /// ```
    pub fn shell_status(&self) -> u8 {
        let signalled = |signal: i32| (128 + signal).clamp(0, 255) as u8;
        match self.termination {
            Termination::TimedOut => 124,
            Termination::OutputLimited => 1,
            Termination::Cancelled => signalled(self.cancel_signal.unwrap_or(libc::SIGTERM)),
            Termination::Exited => match (self.exit_code, self.signal) {
                (Some(code), _) => code.clamp(0, 255) as u8,
                (None, Some(signal)) => signalled(signal),
                (None, None) => 1,
            },
        }
    }
}

/// An unreaped worker. Dropping it kills and reaps the whole process group.
struct Worker(Option<Child>);
impl Worker {
    fn child(&mut self) -> &mut Child {
        self.0
            .as_mut()
            .expect("a worker is only reaped by Worker::reap, which consumes it")
    }
    /// Signal the group, wait out the grace period, kill it, then reap the leader.
    fn terminate(mut self, grace: Duration) -> Result<(u32, ExitStatus)> {
        let pid = self.child().id();
        // SAFETY: the leader is unreaped, reserving this dedicated process-group identity.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
        std::thread::sleep(grace);
        // SAFETY: WNOWAIT has kept the leader unreaped through all group signalling.
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
        let mut child = self.0.take().expect("worker not yet reaped");
        Ok((pid, child.wait()?))
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            // SAFETY: this child has not been reaped and owns the group created by process_group(0).
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn reap_group(pid: u32) {
    loop {
        // SAFETY: WNOHANG reaps only our children in this execution's process group.
        if unsafe { libc::waitpid(-(pid as i32), std::ptr::null_mut(), libc::WNOHANG) } <= 0 {
            break;
        }
    }
}

/// Reap adopted group members until the process group no longer exists.
///
/// Only members of this execution's group are reaped (when the executable opted
/// into subreaper behavior); unrelated process groups are never touched.
fn await_group_exit(pid: u32) -> Result<()> {
    let deadline = Instant::now() + GROUP_EXIT_DEADLINE;
    loop {
        reap_group(pid);
        if !crate::platform::process::group_exists(pid)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::new(
                ErrorCode::InvalidLease,
                "Process group remains present; execution and reservation stay protected",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

impl ExecutionInfo {
    fn validate(&self, id: LeaseId) -> Result<()> {
        if self.schema_version != state::SCHEMA_VERSION
            || self.lease_id != id
            || uuid::Uuid::parse_str(&self.run_id).is_err()
            || self.control_name != control_name(id, &self.run_id)
        {
            return Err(Error::new(
                ErrorCode::LeaseMismatch,
                "Execution generation mismatch",
            ));
        }
        Ok(())
    }
}

fn control_name(id: LeaseId, run: &str) -> String {
    format!("groove-{id}-{run}")
}

fn clear(pool: &Pool, slot: &SlotId, info: &ExecutionInfo) -> Result<()> {
    let current: ExecutionInfo = state::read(&pool.repo.execution_path(slot))?;
    if current.run_id != info.run_id || current.lease_id != info.lease_id {
        return Err(Error::new(
            ErrorCode::LeaseMismatch,
            "Execution generation changed",
        ));
    }
    std::fs::remove_file(pool.repo.execution_path(slot))?;
    state::sync_dir(&pool.repo.root.join("slots"))
}

/// Captured stdout/stderr files for one run, outside the disposable checkout.
struct RunLogs {
    stdout: File,
    stderr: File,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}
impl RunLogs {
    fn create(pool: &Pool, run: &str) -> Result<Self> {
        let dir = pool.repo.root.join("task-logs").join(run);
        repository::mkdir(&dir)?;
        repository::owned_dir(&dir)?;
        let open = |name: &str| -> Result<(File, PathBuf)> {
            let path = dir.join(name);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            Ok((file, path))
        };
        let (stdout, stdout_path) = open("stdout.log")?;
        let (stderr, stderr_path) = open("stderr.log")?;
        Ok(Self {
            stdout,
            stderr,
            stdout_path,
            stderr_path,
        })
    }
    fn exceeds(&self, limit: u64) -> Result<bool> {
        Ok(self.stdout.metadata()?.len() > limit || self.stderr.metadata()?.len() > limit)
    }
    /// Truncate any stream that grew past the limit; report whether one did.
    fn truncate(&self, limit: u64) -> Result<bool> {
        let mut limited = false;
        for file in [&self.stdout, &self.stderr] {
            if file.metadata()?.len() > limit {
                limited = true;
                file.set_len(limit)?;
            }
        }
        Ok(limited)
    }
}

/// Read with independent offsets so observation cannot change worker write positions.
struct Forwarder<'a> {
    observer: Option<&'a dyn ExecutionObserver>,
    offsets: [u64; 2],
}
impl Forwarder<'_> {
    fn poll(&mut self, logs: &RunLogs, limit: u64, budget: u64) {
        let Some(observer) = self.observer else {
            return;
        };
        let mut bytes = [0; 8192];
        for (index, (file, stream)) in [
            (&logs.stdout, OutputStream::Stdout),
            (&logs.stderr, OutputStream::Stderr),
        ]
        .into_iter()
        .enumerate()
        {
            let end = limit.min(self.offsets[index].saturating_add(budget));
            while self.offsets[index] < end {
                let count = (bytes.len() as u64).min(end - self.offsets[index]) as usize;
                match file.read_at(&mut bytes[..count], self.offsets[index]) {
                    Ok(0) => break,
                    Ok(count) => {
                        self.offsets[index] += count as u64;
                        observer.output(stream, &bytes[..count]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        self.observer = None;
                        tracing::warn!(event = "execution.mirror_failed", %error, "Live output unavailable; captured logs remain available");
                        return;
                    }
                }
            }
        }
    }
}

/// Everything the supervision loop watches for a stop condition.
struct Supervision<'a> {
    options: &'a ExecOptions,
    logs: &'a RunLogs,
    signals: &'a ExecutionSignals,
    control: &'a StopListener,
    started: Instant,
}
impl Supervision<'_> {
    /// Poll until the worker exits or Groove must stop it.
    fn run(
        &self,
        child: &Child,
        forwarder: &mut Forwarder<'_>,
    ) -> Result<(Termination, Option<i32>)> {
        loop {
            forwarder.poll(self.logs, self.options.max_log_bytes, 65536);
            if self.logs.exceeds(self.options.max_log_bytes)? {
                return Ok((Termination::OutputLimited, None));
            }
            if crate::platform::process::exited(child)? {
                return Ok((Termination::Exited, None));
            }
            if let Some(signal) = self.cancellation()? {
                return Ok((Termination::Cancelled, Some(signal)));
            }
            if !self.options.timeout.is_zero() && self.started.elapsed() >= self.options.timeout {
                return Ok((Termination::TimedOut, None));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
    fn cancellation(&self) -> Result<Option<i32>> {
        if self
            .options
            .cancellation
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Ok(Some(libc::SIGTERM));
        }
        if let Some(signal) = self.signals.poll()? {
            return Ok(Some(signal));
        }
        Ok(self.control.stop_requested()?.then_some(libc::SIGTERM))
    }
}

/// Build the worker command: direct argv, controlled environment, its own process
/// group, and an inherited execution-lock descriptor so a killed supervisor cannot
/// free the execution lock while the worker survives.
fn worker_command(
    lease: &LeaseInfo,
    pool_name: &crate::PoolName,
    options: &ExecOptions,
    environment: BTreeMap<String, String>,
    logs: &RunLogs,
    execution_fd: RawFd,
) -> Result<Command> {
    let mut command = Command::new(&options.command[0]);
    command
        .args(&options.command[1..])
        .current_dir(&lease.worktree_path)
        .stdin(Stdio::null())
        .stdout(logs.stdout.try_clone()?)
        .stderr(logs.stderr.try_clone()?)
        .process_group(0);
    for (key, _) in std::env::vars_os().filter(|(k, _)| {
        let key = k.to_string_lossy();
        key.starts_with("GIT_") || key.starts_with("GROOVE_")
    }) {
        command.env_remove(key);
    }
    command
        .envs(environment)
        .envs(&options.environment)
        .env("GROOVE_LEASE_ID", lease.lease_id.to_string())
        .env("GROOVE_POOL", pool_name.as_str())
        .env("GROOVE_WORKTREE", &lease.worktree_path)
        .env("GROOVE_SLOT_ID", lease.slot_id.as_str())
        .env("GROOVE_SLOT_INDEX", lease.slot_id.number().to_string())
        .env("GROOVE_TASK", lease.task.as_deref().unwrap_or(""));
    let mask = cancellation_mask();
    // SAFETY: pre_exec performs only async-signal-safe fcntl and sigprocmask calls on
    // values captured before fork; the descriptor stays owned until spawning completes.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(execution_fd, libc::F_GETFD);
            if flags < 0 || libc::fcntl(execution_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::sigprocmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}

impl Pool {
    /// Take the slot's execution lock without waiting. Workers and maintenance
    /// share this exclusion; contention alone never identifies the lock owner.
    pub(crate) fn execution_guard(&self, slot: &SlotId) -> Result<crate::locking::SlotGuard> {
        // Re-read the manifest: growth may have added this slot after `self` was opened.
        let config: state::Config = state::read(&self.repo.config_path())?;
        config.validate()?;
        let identity = match config.execution_locks.get(slot) {
            Some(identity) => *identity,
            None => self.archived_execution_identity(slot)?,
        };
        crate::locking::SlotGuard::try_lock(&self.repo.execution_lock_path(slot), Some(identity))?
            .ok_or_else(|| {
                Error::new(
                    ErrorCode::InvalidLease,
                    "Slot execution lock is busy; another worker or maintenance operation holds it; retry this operation",
                )
                .retryable(true)
            })
    }

    /// Fail while a durable execution record (live or orphaned) still protects the slot.
    pub(crate) fn require_execution_clear(&self, slot: &SlotId) -> Result<()> {
        match std::fs::symlink_metadata(self.repo.execution_path(slot)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
            Ok(_) => Err(Error::new(
                ErrorCode::InvalidLease,
                "Managed execution remains protected; stop or recover it before retrying this operation",
            )),
        }
    }

    /// Run a command inside an existing durable reservation and retain an independent execution lock.
    /// Captures output outside the checkout. Stops the owned process group before returning; never
    /// releases the reservation. Process groups cover cooperative descendants only.
    ///
    /// # Errors
    /// Rejects stale/released leases, concurrent or orphaned execution, invalid commands and I/O failures.
    /// Failed teardown leaves a durable execution prohibition requiring explicit recovery.
    pub fn exec(&self, id: LeaseId, options: ExecOptions) -> Result<ExecResult> {
        let _operation = self.operation()?;
        let initial = self.lease_info(id)?;
        let execution = self.execution_guard(&initial.slot_id)?;
        self.exec_guarded(id, options, &execution)
    }

    /// Verify that `id` is the current, protected, leased generation of its slot.
    fn current_reservation(&self, id: LeaseId) -> Result<LeaseInfo> {
        let lease = self.lease_info(id)?;
        self.require_execution_clear(&lease.slot_id)?;
        let record: SlotRecord = state::read(&self.repo.record_path(&lease.slot_id))?;
        let protection: state::ReuseProtection =
            state::read(&self.repo.protection_path(&lease.slot_id))?;
        if record.schema_version != state::SCHEMA_VERSION
            || record.phase != SlotPhase::Leased
            || record.lease.as_ref().map(|i| i.lease_id) != Some(id)
            || protection.schema_version != state::SCHEMA_VERSION
            || protection.lease.lease_id != id
        {
            return Err(Error::new(
                ErrorCode::LeaseMismatch,
                "Exec requires the current protected reservation",
            ));
        }
        Ok(lease)
    }

    pub(crate) fn exec_guarded(
        &self,
        id: LeaseId,
        options: ExecOptions,
        execution: &crate::locking::SlotGuard,
    ) -> Result<ExecResult> {
        options.validate()?;
        let lease = self.current_reservation(id)?;
        let environment = crate::Project::read(&lease.worktree_path)?.environment(&lease)?;
        let signals = ExecutionSignals::block()?;
        let run = uuid::Uuid::new_v4().to_string();
        let control_name = control_name(id, &run);
        let control = StopListener::bind(&control_name)?;
        let logs = RunLogs::create(self, &run)?;
        // Publish the durable prohibition before spawning anything.
        let mut info = ExecutionInfo {
            schema_version: state::SCHEMA_VERSION,
            lease_id: id,
            run_id: run,
            program: options.command[0].to_string_lossy().into_owned(),
            started_at_unix_ms: crate::reservation::now_ms(),
            leader_pid: None,
            control_name,
            stdout_path: logs.stdout_path.clone(),
            stderr_path: logs.stderr_path.clone(),
        };
        let record_path = self.repo.execution_path(&lease.slot_id);
        state::write(&record_path, &info)?;
        state::checkpoint("execution-protected");
        let started = Instant::now();
        let mut history = self.begin_run(&lease, &info, options.origin.clone())?;
        state::checkpoint("execution-recorded");
        let mut supervision = Supervision {
            options: &options,
            logs: &logs,
            signals: &signals,
            control: &control,
            started: Instant::now(),
        };
        if let Some(signal) = supervision.cancellation()? {
            // Observed cancellation needs a terminal history record even when no
            // worker was launched. Keep protection until logs and history are durable.
            logs.stdout.sync_all()?;
            logs.stderr.sync_all()?;
            let result = ExecResult {
                lease_id: id,
                run: info,
                termination: Termination::Cancelled,
                exit_code: None,
                signal: None,
                cancel_signal: Some(signal),
            };
            self.complete_run(&mut history, &result, started.elapsed())?;
            state::checkpoint("run-completed");
            clear(self, &lease.slot_id, &result.run)?;
            return Ok(result);
        }
        let mut command = worker_command(
            &lease,
            &self.repo.name,
            &options,
            environment,
            &logs,
            execution.file.as_raw_fd(),
        )?;
        let mut worker = match command.spawn() {
            Ok(child) => Worker(Some(child)),
            Err(e) => {
                history.outcome = crate::RunOutcome::SpawnFailed;
                history.ended_at_unix_ms = Some(crate::reservation::now_ms());
                history.duration_ms =
                    Some(started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64);
                history.failure = Some(e.to_string());
                self.write_run(&history)?;
                clear(self, &lease.slot_id, &info)?;
                return Err(Error::new(
                    ErrorCode::IoError,
                    format!(
                        "Could not start {:?} in {}: {e}",
                        options.command[0],
                        lease.worktree_path.display()
                    ),
                )
                .with_source(e)
                .with_details(|details| {
                    details.lease_id = Some(id);
                    details.worktree_path = Some(lease.worktree_path.clone());
                    details.program = Some(info.program.clone());
                    details.run_id = Some(history.run_id);
                }));
            }
        };
        info.leader_pid = Some(worker.child().id());
        state::write(&record_path, &info)?;
        supervision.started = Instant::now();
        let mut forwarder = Forwarder {
            observer: options.observer.as_deref(),
            offsets: [0; 2],
        };
        let (mut termination, cancel_signal) = supervision.run(worker.child(), &mut forwarder)?;
        let (pid, status) = worker.terminate(options.grace)?;
        await_group_exit(pid)?;
        if logs.truncate(options.max_log_bytes)? {
            termination = Termination::OutputLimited;
        }
        forwarder.poll(&logs, options.max_log_bytes, options.max_log_bytes);
        logs.stdout.sync_all()?;
        logs.stderr.sync_all()?;
        let result = ExecResult {
            lease_id: id,
            run: info,
            termination,
            exit_code: status.code(),
            signal: status.signal(),
            cancel_signal,
        };
        self.complete_run(&mut history, &result, started.elapsed())?;
        state::checkpoint("run-completed");
        clear(self, &lease.slot_id, &result.run)?;
        Ok(result)
    }

    /// Ask the live per-command supervisor to terminate its owned workers.
    /// The reservation remains protected.
    pub(crate) fn stop_execution(&self, id: LeaseId) -> Result<ExecutionInfo> {
        let lease = self.lease_info(id)?;
        let info: ExecutionInfo = state::read(&self.repo.execution_path(&lease.slot_id))?;
        info.validate(id)?;
        send_stop(
            &info.control_name,
            "Supervisor unavailable; stop external writers then explicitly recover the execution",
        )?;
        Ok(info)
    }

    /// Clear an orphaned execution prohibition after the operator has stopped all external writers.
    /// Never infers safety from age or a missing PID. Live execution locks always reject recovery.
    ///
    /// # Errors
    /// Rejects live owners and mismatched records. This does not release the lease.
    pub fn recover_execution(&self, id: LeaseId) -> Result<ExecutionInfo> {
        let _operation = self.operation()?;
        let lease = self.lease_info(id)?;
        let _guard = self.execution_guard(&lease.slot_id)?;
        let info: ExecutionInfo = state::read(&self.repo.execution_path(&lease.slot_id))?;
        info.validate(id)?;
        self.recover_run(&lease, &info)?;
        crate::platform::control::cleanup_endpoint(&info.control_name)?;
        clear(self, &lease.slot_id, &info)?;
        Ok(info)
    }
}
