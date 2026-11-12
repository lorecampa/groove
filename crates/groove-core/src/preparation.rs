use crate::{
    cache::{self, Artifacts},
    locking::SlotGuard,
    platform::control::{send_stop, StopListener},
    project::{invalid, valid_name},
    recovery,
    state::{self, SlotPhase, SlotRecord},
    Error, ErrorCode, ExecOptions, ExecResult, ExecutionSignals, LeaseId, LeaseInfo, Pool, Project,
    Result, SetupRecipe, StepWhen,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) const RECORD_LIMIT: u64 = 16 * 1024 * 1024;

/// Durable project readiness for one reservation generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreparationStatus {
    /// Explicit setup is in progress or was interrupted; never inferred ready from a path.
    Preparing,
    /// Every required step and approved artifact publication completed.
    Ready,
    /// A setup command, artifact operation or cancellation prevented readiness.
    Failed,
}

/// Acknowledges an asynchronous stop request; the reservation remains protected.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum StopRequest {
    /// The live preparation supervisor accepted cancellation.
    Preparation {
        /// Current preparation observation, not proof that its writers have stopped.
        preparation_stop_requested: Box<Preparation>,
    },
    /// The current managed execution accepted cancellation.
    Execution {
        /// Current execution observation; inspect until teardown completes.
        stop_requested: Box<crate::ExecutionInfo>,
    },
}

/// Result of one named setup step, including skipped compatible-cache work.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupStepResult {
    /// Step name from the selected project recipe.
    pub name: String,
    /// True only for an explicitly declared cache-miss step on a cache hit.
    pub skipped: bool,
    /// Managed execution and log paths, absent for skipped steps.
    pub execution: Option<ExecResult>,
}

/// Durable readiness record; it is diagnostic state, never ownership authority.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preparation {
    /// Metadata format version.
    pub schema_version: u32,
    /// Exact reservation generation this preparation belongs to.
    pub lease_id: LeaseId,
    /// Named setup recipe.
    pub profile: String,
    /// Hash of project definition, selected recipe and compatibility inputs.
    pub fingerprint: String,
    /// Persisted readiness outcome, not inferred from process IDs or timestamps.
    pub status: PreparationStatus,
    /// Current setup phase or command name.
    pub step: Option<String>,
    /// Completed/skipped command results in order.
    pub steps: Vec<SetupStepResult>,
    /// Selected immutable artifact compatibility key.
    pub cache_key: Option<String>,
    /// Whether a compatible cache was actually seeded for this attempt.
    pub cache_hit: bool,
    /// Regular artifact bytes seeded into this workspace.
    pub seeded_bytes: u64,
    /// Files/symlinks seeded, excluding directories.
    pub seeded_files: u64,
    /// Diagnostic failure reason, if any.
    pub error: Option<String>,
    /// Signal or explicit stop that cancelled preparation.
    pub cancelled: Option<i32>,
    /// Diagnostic start time, never an expiry.
    pub started_at_unix_ms: u64,
    /// Diagnostic completion time, absent while preparing/interrupted.
    pub completed_at_unix_ms: Option<u64>,
    /// Random platform-local control endpoint for the live setup supervisor; no PID signalling.
    pub control_name: String,
}

/// Controls for explicitly requested project setup.
#[derive(Debug, Clone)]
pub struct PreparationOptions {
    /// Re-run from the beginning after failure/interruption or changed configuration.
    /// Previously completed steps can have side effects; retry is always explicit.
    pub retry: bool,
    /// Optional quiescent prepared checkout to import approved artifacts from.
    pub cache_source: Option<PathBuf>,
    /// Positive per-artifact operation and Git timeout.
    pub timeout: Duration,
    /// Managed execution limits and cancellation for every step.
    /// Each step supplies its own argv and configured runtime deadline.
    pub execution: ExecOptions,
}
impl Default for PreparationOptions {
    fn default() -> Self {
        Self {
            retry: false,
            cache_source: None,
            timeout: crate::DEFAULT_GIT_TIMEOUT,
            execution: ExecOptions::default(),
        }
    }
}

/// A named project command selected for [`Pool::exec_named`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamedCommand<'a> {
    /// A `[recipes.NAME]` entry from `.groove.toml`.
    Recipe(&'a str),
    /// An `[agents.NAME]` entry, or a built-in agent executable name.
    Agent(&'a str),
}
impl NamedCommand<'_> {
    fn name(&self) -> &str {
        match self {
            Self::Recipe(name) | Self::Agent(name) => name,
        }
    }
}

/// Built-in agent names that map to their executable when `.groove.toml` has no entry.
const BUILTIN_AGENTS: [&str; 5] = ["claude", "codex", "opencode", "gemini", "aider"];

/// Background thread that turns signals and control-socket requests into cancellation.
struct Control {
    cancelled: Arc<AtomicBool>,
    signal: Arc<AtomicI32>,
    shutdown: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Control {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Control {
    fn start(name: &str, cancelled: Option<Arc<AtomicBool>>) -> Result<Self> {
        let listener = StopListener::bind(name)?;
        let cancelled = cancelled.unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let signal = Arc::new(AtomicI32::new(0));
        let shutdown = Arc::new(AtomicBool::new(false));
        let (cancel, reason, stop) = (cancelled.clone(), signal.clone(), shutdown.clone());
        let worker = thread::Builder::new()
            .name("groove-setup-control".into())
            .spawn(move || {
                let Ok(signals) = ExecutionSignals::block() else {
                    cancel.store(true, Ordering::Release);
                    return;
                };
                let request = |number| {
                    reason.store(number, Ordering::Release);
                    cancel.store(true, Ordering::Release);
                };
                while !stop.load(Ordering::Acquire) {
                    match signals.poll() {
                        Ok(Some(number)) => request(number),
                        Err(_) => cancel.store(true, Ordering::Release),
                        Ok(None) => (),
                    }
                    if listener.stop_requested().unwrap_or(false) {
                        request(libc::SIGTERM);
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            })?;
        Ok(Self {
            cancelled,
            signal,
            shutdown,
            worker: Some(worker),
        })
    }
    fn reason(&self) -> Option<i32> {
        self.cancelled
            .load(Ordering::Acquire)
            .then(|| match self.signal.load(Ordering::Acquire) {
                0 => libc::SIGTERM,
                signal => signal,
            })
    }
    fn check(&self) -> Result<()> {
        match self.reason() {
            Some(_) => Err(invalid("Setup cancelled")),
            None => Ok(()),
        }
    }
}

/// One in-progress setup attempt and its durable readiness record.
struct SetupRun<'a> {
    pool: &'a Pool,
    lease: &'a LeaseInfo,
    project: &'a Project,
    recipe: &'a SetupRecipe,
    options: &'a PreparationOptions,
    control: &'a Control,
    path: PathBuf,
    preparation: Preparation,
}
impl<'a> SetupRun<'a> {
    fn save(&self) -> Result<()> {
        state::write(&self.path, &self.preparation)
    }

    /// Validate, import and seed approved artifacts; returns the cache to publish later.
    fn seed_cache(&mut self) -> Result<Option<Artifacts<'a>>> {
        let (recipe, project): (&'a SetupRecipe, &'a Project) = (self.recipe, self.project);
        let (Some(name), Some(key)) = (&recipe.cache, &self.preparation.cache_key) else {
            if self.options.cache_source.is_some() {
                return Err(invalid(
                    "--cache-source needs a cache in the selected setup recipe",
                ));
            }
            return Ok(None);
        };
        let checkout = &self.lease.worktree_path;
        let mut cache = Artifacts::open(
            self.pool,
            name,
            &project.caches[name],
            key.clone(),
            self.options.timeout,
            self.control.cancelled.clone(),
        )?;
        cache.validate_ignored(checkout)?;
        if let Some(source) = &self.options.cache_source {
            cache.import(source, checkout)?;
        }
        if let Some((bytes, files)) = cache.seed(checkout)? {
            self.preparation.cache_hit = true;
            self.preparation.seeded_bytes = bytes;
            self.preparation.seeded_files = files;
        }
        self.save()?;
        Ok(Some(cache))
    }

    /// Run every step in order. Returns `false` after recording the first failed step.
    fn run_steps(&mut self, execution: &SlotGuard) -> Result<bool> {
        for step in &self.recipe.steps {
            self.control.check()?;
            self.preparation.step = Some(step.name.clone());
            self.save()?;
            state::checkpoint("before-setup-step");
            let skipped = step.when == StepWhen::CacheMiss && self.preparation.cache_hit;
            let result = if skipped {
                None
            } else {
                let mut command = self.options.execution.clone();
                command.command = step.command.iter().map(Into::into).collect();
                command.origin = crate::RunOrigin::Setup {
                    profile: self.preparation.profile.clone(),
                    step: step.name.clone(),
                };
                if command.timeout.is_zero() {
                    command.timeout = Duration::from_millis(step.timeout_ms);
                }
                command.cancellation = Some(self.control.cancelled.clone());
                Some(
                    self.pool
                        .exec_guarded(self.lease.lease_id, command, execution)?,
                )
            };
            let failed = result.as_ref().is_some_and(|run| !run.succeeded());
            self.preparation.steps.push(SetupStepResult {
                name: step.name.clone(),
                skipped,
                execution: result,
            });
            self.save()?;
            state::checkpoint("setup-step-completed");
            if failed {
                self.preparation.status = PreparationStatus::Failed;
                self.preparation.error =
                    Some(format!("Setup step '{}' did not succeed", step.name));
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Refuse readiness if `.groove.toml` or compatibility inputs changed mid-setup.
    fn verify_definition(&self) -> Result<()> {
        let current = Project::read(&self.lease.worktree_path)?;
        if definition(&current, self.lease, &self.preparation.profile)?.0
            != self.preparation.fingerprint
        {
            return Err(invalid(
                "Project setup definition or compatibility inputs changed during setup",
            ));
        }
        Ok(())
    }

    fn execute(&mut self, execution: &SlotGuard) -> Result<()> {
        let cache = self.seed_cache()?;
        if !self.run_steps(execution)? {
            return Ok(());
        }
        self.control.check()?;
        self.verify_definition()?;
        if let Some(mut cache) = cache {
            self.preparation.step = Some("cache-publish".into());
            self.save()?;
            cache.validate_ignored(&self.lease.worktree_path)?;
            cache.publish(&self.lease.worktree_path)?;
        }
        self.preparation.status = PreparationStatus::Ready;
        self.preparation.step = None;
        state::checkpoint("before-setup-ready");
        Ok(())
    }

    /// Persist the outcome. Cancellation is a failed preparation, not an error.
    fn finish(mut self, outcome: Result<()>) -> Result<Preparation> {
        self.preparation.cancelled = self.control.reason();
        self.preparation.completed_at_unix_ms = Some(crate::reservation::now_ms());
        if let Err(error) = outcome {
            self.preparation.status = PreparationStatus::Failed;
            self.preparation.error = Some(error.to_string());
            self.save()?;
            if self.preparation.cancelled.is_some() {
                return Ok(self.preparation);
            }
            let (lease, preparation) = (self.lease, self.preparation);
            return Err(error.with_details(|d| {
                d.lease_id = Some(lease.lease_id);
                d.worktree_path = Some(lease.worktree_path.clone());
                d.preparation = Some(Box::new(preparation));
            }));
        }
        if self.preparation.cancelled.is_some() {
            self.preparation.status = PreparationStatus::Failed;
            self.preparation.error = Some("Setup cancelled".into());
        }
        self.save()?;
        state::checkpoint("setup-completed");
        Ok(self.preparation)
    }
}

impl Pool {
    /// Request cancellation of live preparation or the current managed execution.
    /// Historical interrupted preparation never prevents stopping a new raw
    /// execution launched after writer recovery. Both routes authenticate their
    /// endpoint; this never clears protection or infers ownership from metadata.
    ///
    /// ```no_run
    /// let pool = groove_core::Pool::open("/path/to/project")?;
    /// let lease = pool.reserve(Default::default())?;
    /// // From another controller while a task is running:
    /// let requested = pool.request_stop(lease.lease_id)?;
    /// println!("{requested:?}"); // Inspect until teardown completes before release.
    /// # Ok::<(), groove_core::Error>(())
    /// ```
    ///
    /// # Errors
    /// Reports stale leases, unavailable supervisors and malformed records.
    /// Retains the preparation failure if both stop routes fail.
    pub fn request_stop(&self, id: LeaseId) -> Result<StopRequest> {
        let preparation_error = match self.preparation(id) {
            Ok(Some(preparation)) if preparation.status == PreparationStatus::Preparing => {
                match self.stop_preparation(id) {
                    Ok(preparation) => {
                        return Ok(StopRequest::Preparation {
                            preparation_stop_requested: Box::new(preparation),
                        })
                    }
                    Err(error) => Some(error),
                }
            }
            Ok(_) => None,
            Err(error) => Some(error),
        };
        match (self.stop_execution(id), preparation_error) {
            (Ok(execution), _) => Ok(StopRequest::Execution {
                stop_requested: Box::new(execution),
            }),
            (Err(error), Some(primary)) => {
                Err(primary.with_details(|d| d.secondary_error = Some(error.to_string())))
            }
            (Err(error), None) => Err(error),
        }
    }

    pub(crate) fn preparation_path(&self, slot: &crate::SlotId) -> PathBuf {
        self.repo
            .root
            .join("slots")
            .join(format!("{slot}.setup.json"))
    }

    pub(crate) fn protected_context(&self, id: LeaseId) -> Result<LeaseInfo> {
        let current = self.refreshed()?;
        let info = current.lease_info(id)?;
        let expected = current.config.worktree_root.join(info.slot_id.as_str());
        let record: SlotRecord = state::read(&self.repo.record_path(&info.slot_id))?;
        let protection: state::ReuseProtection =
            state::read(&self.repo.protection_path(&info.slot_id))?;
        if record.schema_version != state::SCHEMA_VERSION
            || record.phase != SlotPhase::Leased
            || record.lease.as_ref().map(|lease| lease.lease_id) != Some(id)
            || protection.schema_version != state::SCHEMA_VERSION
            || protection.lease.lease_id != id
            || protection.lease.slot_id != info.slot_id
            || info.worktree_path != expected
            || protection.lease.worktree_path != expected
        {
            return Err(Error::new(
                ErrorCode::LeaseMismatch,
                "Operation requires the current protected reservation",
            ));
        }
        recovery::validate_slot_path(&self.repo, &current.config, &info.slot_id)?;
        recovery::registration(&self.repo, &expected)?;
        Ok(info)
    }

    /// Read readiness for this exact lease generation; absence means setup was never requested.
    /// This observation grants neither ownership nor permission to reclaim a workspace.
    ///
    /// # Errors
    /// Rejects stale IDs, malformed metadata and I/O failures.
    pub fn preparation(&self, id: LeaseId) -> Result<Option<Preparation>> {
        let info = self.lease_info(id)?;
        let path = self.preparation_path(&info.slot_id);
        match state::read_bounded::<Preparation>(&path, RECORD_LIMIT) {
            Ok(preparation) if preparation.schema_version != state::SCHEMA_VERSION => {
                Err(Error::new(
                    ErrorCode::ProtocolError,
                    "Unsupported setup metadata version",
                ))
            }
            Ok(preparation) if preparation.lease_id == id => Ok(Some(preparation)),
            Ok(_) => Ok(None),
            Err(error) if !path.try_exists()? && error.code == ErrorCode::IoError => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Execute a named setup recipe and publish explicit durable readiness.
    /// Keeps the reservation protected after success, failure, interruption and cancellation.
    /// A compatible ready retry returns its receipt without repeating commands. Explicit
    /// retry restarts the recipe; callers must design side effects accordingly.
    ///
    /// ```no_run
    /// use groove_core::{NamedCommand, Pool, PreparationOptions, PreparationStatus};
    /// let pool = Pool::open("/path/to/project")?;
    /// let lease = pool.reserve(Default::default())?;
    /// let setup = pool.prepare(lease.lease_id, "dev", PreparationOptions::default())?;
    /// if setup.status == PreparationStatus::Ready {
    ///     pool.exec_named(lease.lease_id, NamedCommand::Recipe("test"), &[], Default::default())?;
    /// }
    /// // Preserve results and stop writers before explicitly finishing/releasing.
    /// # Ok::<(), groove_core::Error>(())
    /// ```
    ///
    /// # Errors
    /// Rejects stale/live/orphan executions, unsafe artifacts, configuration changes
    /// without retry and I/O failures. Errors include lease/path/readiness context.
    /// Failed worker commands return a failed preparation with their captured results.
    pub fn prepare(
        &self,
        id: LeaseId,
        profile: &str,
        options: PreparationOptions,
    ) -> Result<Preparation> {
        let _operation = self.operation()?;
        if !valid_name(profile) || options.timeout.is_zero() {
            return Err(invalid(
                "Setup needs a valid profile and positive artifact timeout",
            ));
        }
        let initial = self.lease_info(id)?;
        let execution = self.execution_guard(&initial.slot_id)?;
        self.require_execution_clear(&initial.slot_id)?;
        let lease = self.protected_context(id)?;
        let project = Project::read(&lease.worktree_path)?;
        let recipe = project
            .setup
            .get(profile)
            .ok_or_else(|| invalid("Unknown setup profile in .groove.toml"))?;
        let (fingerprint, cache_key) = definition(&project, &lease, profile)?;
        if let Some(previous) = self.preparation(id)? {
            if previous.status == PreparationStatus::Ready
                && previous.fingerprint == fingerprint
                && !options.retry
            {
                return Ok(previous);
            }
            if !options.retry {
                return Err(invalid(
                    "Setup is failed, interrupted or changed; inspect it, stop/recover writers and explicitly retry",
                )
                .with_details(|d| {
                    d.lease_id = Some(id);
                    d.worktree_path = Some(lease.worktree_path.clone());
                    d.preparation = Some(Box::new(previous));
                }));
            }
            let prefix = format!("groove-setup-{id}-");
            if !previous
                .control_name
                .strip_prefix(&prefix)
                .is_some_and(|run| {
                    uuid::Uuid::parse_str(run).is_ok_and(|uuid| uuid.to_string() == run)
                })
            {
                return Err(Error::new(
                    ErrorCode::ProtocolError,
                    "Invalid setup control endpoint",
                ));
            }
            crate::platform::control::cleanup_endpoint(&previous.control_name)?;
        }
        let _signals = ExecutionSignals::block()?;
        let control_name = format!("groove-setup-{id}-{}", uuid::Uuid::new_v4());
        let control = Control::start(&control_name, options.execution.cancellation.clone())?;
        let mut run = SetupRun {
            pool: self,
            lease: &lease,
            project: &project,
            recipe,
            options: &options,
            control: &control,
            path: self.preparation_path(&lease.slot_id),
            preparation: Preparation {
                schema_version: state::SCHEMA_VERSION,
                lease_id: id,
                profile: profile.into(),
                fingerprint,
                status: PreparationStatus::Preparing,
                step: Some("cache".into()),
                steps: Vec::new(),
                cache_key,
                cache_hit: false,
                seeded_bytes: 0,
                seeded_files: 0,
                error: None,
                cancelled: None,
                started_at_unix_ms: crate::reservation::now_ms(),
                completed_at_unix_ms: None,
                control_name,
            },
        };
        run.save()?;
        state::checkpoint("setup-recorded");
        let outcome = run.execute(&execution);
        run.finish(outcome)
    }

    /// Request cancellation of a live setup, including artifact-copy phases.
    /// Does not clear readiness, execution records or reservation protection.
    ///
    /// # Errors
    /// Rejects stale IDs, missing/non-running setup and unavailable supervisors.
    pub fn stop_preparation(&self, id: LeaseId) -> Result<Preparation> {
        let preparation = self
            .preparation(id)?
            .ok_or_else(|| invalid("No project setup was requested for this lease"))?;
        if preparation.status != PreparationStatus::Preparing {
            return Err(invalid("Project setup is not running"));
        }
        send_stop(
            &preparation.control_name,
            "Setup supervisor unavailable; stop/recover writers before an explicit setup retry",
        )?;
        Ok(preparation)
    }

    /// Run a named project recipe or agent, appending `args` to its argv prefix.
    /// Never runs required setup implicitly; successful current readiness is checked.
    /// Built-in agent names (`claude`, `codex`, `opencode`, `gemini`, `aider`)
    /// map only to their executable; pass the tool's noninteractive arguments explicitly.
    ///
    /// # Errors
    /// Rejects unknown names, stale readiness, ownership/execution conflicts and worker failures.
    /// A worker's nonzero exit remains an execution result, and never releases the reservation.
    pub fn exec_named(
        &self,
        id: LeaseId,
        target: NamedCommand<'_>,
        args: &[std::ffi::OsString],
        mut options: ExecOptions,
    ) -> Result<ExecResult> {
        let _operation = self.operation()?;
        let name = target.name();
        if !valid_name(name) {
            return Err(invalid("Recipe/agent name must be a simple identifier"));
        }
        let initial = self.lease_info(id)?;
        let execution = self.execution_guard(&initial.slot_id)?;
        let info = self.protected_context(id)?;
        let project = Project::read(&info.worktree_path)?;
        let configured = match target {
            NamedCommand::Recipe(_) => project.recipes.get(name),
            NamedCommand::Agent(_) => project.agents.get(name),
        };
        let builtin;
        let recipe = match (configured, target) {
            (Some(recipe), _) => recipe,
            (None, NamedCommand::Agent(_)) if BUILTIN_AGENTS.contains(&name) => {
                builtin = crate::CommandRecipe {
                    command: vec![name.into()],
                    timeout_ms: 0,
                    requires_setup: None,
                };
                &builtin
            }
            (None, _) => return Err(invalid("Unknown recipe or agent in .groove.toml")),
        };
        if let Some(profile) = &recipe.requires_setup {
            let ready = self.preparation(id)?.is_some_and(|ready| {
                ready.status == PreparationStatus::Ready
                    && ready.profile == *profile
                    && definition(&project, &info, profile)
                        .is_ok_and(|(fingerprint, _)| fingerprint == ready.fingerprint)
            });
            if !ready {
                return Err(invalid(
                    "This command needs successful current setup; run `groove exec --setup` first",
                ));
            }
        }
        options.command = recipe
            .command
            .iter()
            .map(Into::into)
            .chain(args.iter().cloned())
            .collect();
        options.origin = match target {
            NamedCommand::Recipe(_) => crate::RunOrigin::Recipe { name: name.into() },
            NamedCommand::Agent(_) => crate::RunOrigin::Agent { name: name.into() },
        };
        if options.timeout.is_zero() {
            options.timeout = Duration::from_millis(recipe.timeout_ms);
        }
        self.exec_guarded(id, options, &execution)
    }
}

fn definition(
    project: &Project,
    info: &LeaseInfo,
    profile: &str,
) -> Result<(String, Option<String>)> {
    let recipe = project
        .setup
        .get(profile)
        .ok_or_else(|| invalid("Unknown setup profile"))?;
    let mut hash = Sha256::new();
    hash.update(project.fingerprint()?);
    hash.update(profile);
    let key = if let Some(name) = &recipe.cache {
        let mut key = Sha256::new();
        key.update(name);
        key.update(cache::fingerprint(
            &info.worktree_path,
            &project.caches[name],
        )?);
        key.update(serde_json::to_vec(recipe)?);
        key.update(serde_json::to_vec(&project.workspace)?);
        let key = crate::project::hex(key.finalize());
        hash.update(&key);
        Some(key)
    } else {
        None
    };
    Ok((crate::project::hex(hash.finalize()), key))
}
