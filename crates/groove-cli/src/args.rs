//! Command-line grammar. Every command and option carries help; long help lives in `help/`.

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use groove_core::{LeaseId, PoolId, PoolName, SlotId};
use std::path::PathBuf;

macro_rules! help {
    ($name:literal) => {
        include_str!(concat!("../help/", $name, ".txt"))
    };
}

/// Changed files larger than this abort a snapshot unless explicitly raised.
const DEFAULT_MAX_FILE_BYTES: &str = "100M";

#[derive(Parser)]
#[command(
    name = "groove",
    propagate_version = true,
    disable_help_subcommand = true,
    version = concat!(env!("CARGO_PKG_VERSION"), " (revision ", env!("GROOVE_BUILD_REVISION"), ")"),
    about = "Reusable Git worktree pools and local checkpoints for parallel coding tasks",
    after_long_help = help!("overview")
)]
pub struct Args {
    /// Repository path.
    #[arg(
        long,
        short = 'C',
        global = true,
        help_heading = "Global options",
        default_value = ".",
        long_help = "Repository path; discovery also works from subdirectories and linked worktrees."
    )]
    pub repo: PathBuf,
    /// Pool namespace (owning workspace's pool, otherwise default).
    #[arg(
        long,
        hide = true,
        long_help = "Pool namespace: explicit --pool wins, otherwise use the pool owning the current workspace (or --repo), then default. Ownership is validated from Groove state and Git registration. Use an explicit name for destroy from the main checkout when several pools exist."
    )]
    pub pool: Option<PoolName>,
    /// Color output automatically, always, or never (auto honors NO_COLOR).
    #[arg(
        long,
        global = true,
        value_enum,
        default_value = "auto",
        help_heading = "Global options"
    )]
    pub color: ColorMode,
    /// Print one versioned JSON response.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        long_help = "Print one versioned JSON object on stdout instead of text."
    )]
    pub json: bool,
    /// Correlation ID for JSON responses and logs.
    #[arg(
        long,
        global = true,
        help_heading = "Global options",
        long_help = "Echo this ID in JSON responses and logs for correlation; not an idempotency key."
    )]
    pub request_id: Option<String>,
    /// Increase diagnostics (-vv debug, -vvv trace).
    #[arg(short, long, global = true, help_heading = "Global options", long_help = "Show informational diagnostics; -vv adds debug, -vvv adds trace.", action = clap::ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,
    /// Hide warnings and progress; errors and results remain.
    #[arg(short, long, global = true, help_heading = "Global options")]
    pub quiet: bool,
    /// Positive Git and lock timeout, e.g. 2m or 30s.
    #[arg(long = "git-timeout", value_name = "DURATION", hide = true, default_value = "2m", value_parser = parse_positive_duration)]
    pub git_timeout_ms: u64,
    #[arg(skip)]
    pub git_timeout_explicit: bool,
    #[command(subcommand)]
    pub command: Command,
    #[arg(skip)]
    pub deprecations: Vec<&'static str>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

#[derive(ClapArgs)]
pub struct PoolSelection {
    /// Pool to use; otherwise select the owning workspace's pool or the only pool.
    #[arg(short = 'p', long = "pool", value_name = "NAME")]
    pub command_pool: Option<PoolName>,
}

#[derive(ClapArgs)]
pub struct GitTimeoutArgs {
    /// Override this command's Git budget; defaults to the selected pool's git-timeout.
    #[arg(long = "timeout", visible_alias = "git-timeout", value_name = "DURATION", value_parser = parse_positive_duration)]
    pub command_timeout: Option<u64>,
}

impl Args {
    /// Normalize the transitional aliases once, before routing across pools.
    pub fn normalize(mut self) -> Result<Self, String> {
        if self.pool.is_some() {
            self.deprecations.push("Global --pool is deprecated; select a pool on acquire or name the pool positionally");
        }
        if self.git_timeout_explicit {
            self.deprecations
                .push("Global --git-timeout is deprecated; use the command's --timeout");
        }
        let command_pool = match &self.command {
            Command::Acquire { pool, .. } | Command::List { pool, .. } => pool.command_pool.clone(),
            _ => None,
        };
        if let Some(pool) = command_pool {
            if self.pool.as_ref().is_some_and(|legacy| legacy != &pool) {
                return Err("Conflicting pool selections".into());
            }
            self.pool = Some(pool);
        }
        let command_timeout = match &self.command {
            Command::Acquire { timeout, .. } | Command::Release { timeout, .. } => {
                timeout.command_timeout
            }
            Command::Pool(PoolCommand::Init { saved_timeout, .. }) => *saved_timeout,
            _ => None,
        };
        if let Some(timeout) = command_timeout {
            if self.git_timeout_explicit && self.git_timeout_ms != timeout {
                return Err("Conflicting Git timeout overrides".into());
            }
            self.git_timeout_ms = timeout;
            self.git_timeout_explicit = true;
        }
        match &mut self.command {
            Command::Acquire {
                positional_task,
                task,
                ..
            } => {
                if task.is_some() {
                    self.deprecations
                        .push("--task is deprecated; use acquire TASK");
                }
                *task = positional_task.take().or(task.take());
                if let Some(task) = task {
                    validate_task(task)?;
                }
            }
            Command::Release {
                lease,
                save,
                message,
                description,
                ..
            } => {
                if let Some(message) = message.take() {
                    *save = true;
                    *description = message;
                }
                normalize_selector(
                    &mut lease.reservation,
                    &mut lease.task,
                    &mut lease.lease_id,
                    &mut self.deprecations,
                )?;
            }
            Command::Path { selector: lease } => {
                normalize_selector(
                    &mut lease.reservation,
                    &mut lease.task,
                    &mut lease.lease_id,
                    &mut self.deprecations,
                )?;
            }
            Command::Show { selector, .. } => {
                if selector.slot.is_some() {
                    self.deprecations
                        .push("show --slot is deprecated; use pool show NAME --slot SLOT");
                }
                normalize_selector(
                    &mut selector.reservation,
                    &mut selector.task,
                    &mut selector.lease_id,
                    &mut self.deprecations,
                )?;
            }
            Command::Snapshot(SnapshotCommand::Save {
                message,
                description,
                ..
            }) => {
                if !description.is_empty() {
                    self.deprecations.push("Positional snapshot descriptions are deprecated; use snapshot save -m MESSAGE");
                }
                if let Some(message) = message.take() {
                    *description = message;
                }
            }
            Command::List { slots, details, .. } if *slots || *details => {
                self.deprecations
                    .push("list --slots/--details are deprecated; use pool show NAME");
            }
            Command::Pool(PoolCommand::Set {
                capacity,
                cleanup,
                ready_target,
                idle_timeout,
                ready_base,
                ready_git_timeout,
                ..
            }) if capacity.is_some()
                || cleanup.is_some()
                || ready_target.is_some()
                || idle_timeout.is_some()
                || ready_base.is_some()
                || ready_git_timeout.is_some() =>
            {
                self.deprecations
                    .push("pool set setting flags are deprecated; use KEY=VALUE");
            }
            _ => {}
        }
        Ok(self)
    }
}

fn validate_task(task: &str) -> Result<(), String> {
    if task.is_empty()
        || task.len() > 128
        || !task
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        || uuid::Uuid::parse_str(task).is_ok()
    {
        return Err("TASK must contain 1..128 ASCII letters, digits, dots, underscores or hyphens and must not look like a UUID".into());
    }
    Ok(())
}

fn normalize_selector(
    reservation: &mut Option<String>,
    task: &mut Option<String>,
    id: &mut Option<LeaseId>,
    warnings: &mut Vec<&'static str>,
) -> Result<(), String> {
    if task.is_some() || id.is_some() {
        warnings.push("--task and --reservation-id are deprecated; use a positional RESERVATION");
    }
    if let Some(value) = reservation.take() {
        match value.parse::<LeaseId>() {
            Ok(value) => *id = Some(value),
            Err(_) => {
                validate_task(&value)?;
                *task = Some(value);
            }
        }
    }
    Ok(())
}

/// Selects a reservation explicitly, or infers it for release inside its workspace.
#[derive(ClapArgs, Debug, Clone)]
#[group(multiple = false)]
pub struct LeaseSelector {
    /// Task label or reservation UUID; omit inside the reservation's workspace.
    #[arg(value_name = "RESERVATION")]
    pub reservation: Option<String>,
    /// Reservation UUID returned by acquire.
    #[arg(long = "reservation-id", value_name = "ID", hide = true)]
    pub lease_id: Option<LeaseId>,
    /// Task label of exactly one active reservation.
    #[arg(long, hide = true)]
    pub task: Option<String>,
}

/// Select exactly one reservation or permanent slot.
#[derive(ClapArgs, Debug, Clone, Default)]
#[group(multiple = false)]
pub struct OptionalSelector {
    /// Task label or reservation UUID; omit inside the reservation's workspace.
    #[arg(value_name = "RESERVATION")]
    pub reservation: Option<String>,
    /// Reservation UUID returned by acquire.
    #[arg(long = "reservation-id", value_name = "ID", hide = true)]
    pub lease_id: Option<LeaseId>,
    /// Task label of exactly one active reservation.
    #[arg(long, hide = true)]
    pub task: Option<String>,
    /// Permanent slot, such as wt-0001.
    #[arg(long, hide = true)]
    pub slot: Option<SlotId>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Cleanup {
    /// Discard tracked edits and all untracked and ignored files before reuse.
    Pristine,
    /// Keep ignored files (build caches) when the checkout can be normalized in place.
    Warm,
}
impl From<Cleanup> for groove_core::CleanupPolicy {
    fn from(value: Cleanup) -> Self {
        match value {
            Cleanup::Pristine => Self::Pristine,
            Cleanup::Warm => Self::Warm,
        }
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// Show command help or the exit-codes, durations, recovery and environment topics.
    Help {
        /// Command path or topic, for example pool create or recovery.
        #[arg(value_name = "COMMAND|TOPIC")]
        topic: Vec<String>,
    },
    /// Upgrade the installed CLI, optionally including existing agent skill plugins.
    #[command(after_long_help = help!("upgrade"))]
    Upgrade(UpgradeArgs),
    /// Create, inspect, configure and delete repository pools.
    #[command(subcommand, after_long_help = help!("pool"))]
    Pool(PoolCommand),
    /// Reserve a clean checkout of a commit until it is released.
    #[command(after_long_help = help!("acquire"))]
    Acquire {
        #[command(flatten)]
        pool: PoolSelection,
        #[command(flatten)]
        timeout: GitTimeoutArgs,
        /// Task label (1..128 ASCII letters, digits, dots, underscores or hyphens).
        #[arg(value_name = "TASK", conflicts_with = "task")]
        positional_task: Option<String>,
        /// Starting commit, branch or tag; checkout HEAD is detached.
        #[arg(
            short = 'b',
            long,
            default_value = "HEAD",
            long_help = "Commit, branch or tag whose files the checkout starts from (HEAD detached there)."
        )]
        base: String,
        /// Unique reservation label, selectable with --task.
        #[arg(
            long,
            hide = true,
            long_help = "Deprecated task selector; use positional TASK (1..128 ASCII letters, digits, dots, underscores or hyphens)."
        )]
        task: Option<String>,
        /// Idempotency key for safe acquisition retries.
        #[arg(
            long = "idempotency-key",
            visible_alias = "key",
            value_name = "KEY",
            long_help = "Idempotency key: retrying the same key/base/task returns the same reservation."
        )]
        request_key: Option<String>,
        /// Wait for a free slot, e.g. 30s; 0s scans once.
        #[arg(short = 'w', long = "wait", value_name = "DURATION", default_value = "0s", value_parser = parse_duration)]
        wait_ms: u64,
        /// Print exactly the workspace path in human output.
        #[arg(long, visible_alias = "print-path")]
        path_only: bool,
    },
    /// Return a workspace to the pool; optionally save a pinned snapshot first.
    #[command(after_long_help = help!("release"), group(clap::ArgGroup::new("saving").multiple(true).args(["save", "message"])))]
    Release {
        #[command(flatten)]
        timeout: GitTimeoutArgs,
        #[command(flatten)]
        lease: LeaseSelector,
        /// Save and pin a snapshot before release; a save failure keeps the reservation.
        #[arg(short = 's', long)]
        save: bool,
        /// Snapshot description; implies --save.
        #[arg(short = 'm', long, conflicts_with = "description")]
        message: Option<String>,
        /// Description recorded with the pinned snapshot.
        #[arg(
            long,
            hide = true,
            requires = "save",
            default_value = "workspace saved"
        )]
        description: String,
        /// File size limit, e.g. 100M; 0 means unlimited.
        #[arg(long = "max-file-size", value_name = "SIZE", value_parser = parse_size, requires = "saving", default_value = DEFAULT_MAX_FILE_BYTES, long_help = "Largest changed file to capture, e.g. 100M; 0 removes the limit.")]
        max_file_bytes: u64,
        /// Discard unsaved work; never bypasses active writers.
        #[arg(
            short = 'f',
            long,
            long_help = "Discard unsaved changes (with --save, still save changes). Active writers always block release. Without --save, unsaved contents are discarded on the next acquisition."
        )]
        force: bool,
    },
    /// List active reservations across repository pools.
    #[command(after_long_help = help!("list"))]
    List {
        #[command(flatten)]
        pool: PoolSelection,
        /// Sort by task, pool or acquisition time; reservation UUID breaks ties.
        #[arg(long, value_enum, default_value = "task")]
        sort: ReservationSort,
        #[command(flatten)]
        page: PageArgs,
        /// List every slot with its status and suggested next action.
        #[arg(long, hide = true)]
        slots: bool,
        /// Include bounded local Git status for each slot (implies --slots).
        #[arg(long, hide = true)]
        details: bool,
    },
    /// Show exactly one reservation or slot.
    #[command(after_long_help = help!("show"))]
    Show {
        #[command(flatten)]
        selector: OptionalSelector,
        /// Include bounded local Git status.
        #[arg(long)]
        details: bool,
    },
    /// Print only the selected reservation's workspace path.
    Path {
        #[command(flatten)]
        selector: LeaseSelector,
    },
    /// Print a shell completion script for commands, options and fixed values.
    #[command(after_long_help = help!("completions"))]
    Completions {
        /// Shell to generate completions for; no shell is selected by default.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Save, inspect, restore and delete local checkpoints.
    #[command(subcommand, after_long_help = help!("snapshot"))]
    Snapshot(SnapshotCommand),
    /// User preferences shared by every repository.
    #[command(subcommand, after_long_help = help!("config"))]
    Config(ConfigCommand),
}

#[derive(ClapArgs)]
pub struct UpgradeArgs {
    /// Check CLI availability and discover plugins without installing or refreshing catalogs.
    #[arg(long, conflicts_with = "dry_run")]
    pub check: bool,
    /// Show source, destination and literal installer commands without executing them.
    #[arg(short = 'n', long)]
    pub dry_run: bool,
    /// Also update installed, enabled user Groove plugins in Codex and Claude Code.
    #[arg(long)]
    pub with_plugins: bool,
    /// Select an exact stable CLI release; downgrades and prereleases are rejected.
    #[arg(long = "to", visible_alias = "target-version", value_name = "VERSION")]
    pub target_version: Option<String>,
    /// Deadline per installer process (positive duration, e.g. 30m); HTTP checks have separate short limits.
    #[arg(long, default_value = "30m", value_parser = parse_duration)]
    pub timeout: u64,
}

/// Ordering for timestamped collections.
#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum TimeSort {
    /// Creation time, then ID (default order: descending).
    Created,
    /// ID, lexicographically (default order: ascending).
    Id,
}

/// Direction for both a collection's sort key and its tie-breakers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SortOrder {
    /// Smallest/oldest first; unlabelled tasks first.
    Asc,
    /// Largest/newest first; unlabelled tasks last.
    Desc,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum PoolSort {
    /// Pool name (default order: ascending).
    Name,
    /// Lifecycle state name, then pool name (default order: ascending).
    State,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SlotSort {
    /// Permanent slot ID (default order: ascending).
    Slot,
    /// Task label, then slot ID (default order: ascending, unlabelled first).
    Task,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum ReservationSort {
    /// Task label (ascending).
    Task,
    /// Pool name, then task label (ascending).
    Pool,
    /// Acquisition time (newest first).
    Acquired,
    /// Deprecated slot ordering.
    #[value(hide = true)]
    Slot,
}

/// Output window shared by collection commands.
#[derive(ClapArgs, Debug, Clone, Default)]
pub struct PageArgs {
    /// Sort direction, including tie-breakers; default: desc for created/acquired, asc otherwise.
    #[arg(long, value_enum)]
    pub order: Option<SortOrder>,
    /// Return at most this many entries (default: 50); ordering is command-specific.
    #[arg(long, conflicts_with = "unlimited", value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: Option<u64>,
    /// Return every matching entry explicitly (conflicts with --limit).
    #[arg(long)]
    pub unlimited: bool,
    /// Skip this many entries after filtering; pages can shift with concurrent changes.
    #[arg(long, default_value_t = 0)]
    pub offset: u64,
}
impl PageArgs {
    /// Apply direction and pagination to a collection in ascending order.
    pub fn apply<T>(&self, entries: Vec<T>) -> (Vec<T>, crate::output::Pagination) {
        self.apply_ordered(entries, SortOrder::Asc)
    }

    /// Apply direction and pagination to a collection in its command's default order.
    pub fn apply_ordered<T>(
        &self,
        mut entries: Vec<T>,
        default_order: SortOrder,
    ) -> (Vec<T>, crate::output::Pagination) {
        if self.order.is_some_and(|order| order != default_order) {
            entries.reverse();
        }
        let total = entries.len();
        let limit = if self.unlimited {
            None
        } else {
            Some(self.limit.unwrap_or(50))
        };
        let entries: Vec<T> = entries
            .into_iter()
            .skip(usize::try_from(self.offset).unwrap_or(usize::MAX))
            .take(
                limit
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(usize::MAX),
            )
            .collect();
        let end = self.offset.saturating_add(entries.len() as u64);
        let pagination = crate::output::Pagination {
            total,
            offset: self.offset,
            limit,
            next_offset: (end < total as u64).then_some(end),
        };
        (entries, pagination)
    }
}

#[derive(Subcommand)]
pub enum SnapshotCommand {
    /// Save staged and working files without changing HEAD or the index.
    Save {
        /// Description of the checkpoint.
        #[arg(short = 'm', long, conflicts_with = "description")]
        message: Option<String>,
        /// Description of the checkpoint.
        #[arg(default_value = "")]
        description: String,
        /// Task or session identifier recorded with the snapshot.
        #[arg(long)]
        session: Option<String>,
        /// Largest changed file to capture, e.g. 100M; 0 removes the limit.
        #[arg(long = "max-file-size", value_name = "SIZE", value_parser = parse_size, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// List snapshots across all linked worktrees (default: newest first).
    #[command(after_long_help = help!("snapshot-list"))]
    List {
        /// Only snapshots pinned for saved release or recovery.
        #[arg(long)]
        pinned: bool,
        /// Sort snapshots by creation time or snapshot ID; ties use snapshot ID.
        #[arg(long, value_enum, default_value = "created")]
        sort: TimeSort,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Report snapshot counts and repository-wide Git object storage.
    #[command(after_long_help = help!("snapshot-storage"))]
    Storage {
        /// Scan saved trees for up to 1000 largest unique blobs; 0 skips scanning.
        #[arg(long, value_name = "N", default_value_t = 0, value_parser = clap::value_parser!(u16).range(0..=1000))]
        largest: u16,
    },
    /// Preview or apply explicit retention, preserving pinned snapshots.
    #[command(after_long_help = help!("snapshot-prune"))]
    Prune {
        /// Required age threshold, e.g. 30d; accepts ms, s, m, h or d (0s allows any age).
        #[arg(long = "older-than", value_name = "DURATION", value_parser = parse_retention_age)]
        older_than_ms: u64,
        /// Keep this many newest snapshots per originating workspace, including pins.
        #[arg(long, value_name = "N", default_value_t = 20)]
        keep_last: usize,
        /// Delete eligible snapshot refs; never overrides pins or runs Git GC.
        #[arg(long, conflicts_with = "dry_run", hide = true)]
        apply: bool,
        /// Confirm deletion without an interactive prompt.
        #[arg(short = 'y', long, conflicts_with = "dry_run")]
        yes: bool,
        /// Explicitly preview without deleting anything (the default).
        #[arg(short = 'n', long, conflicts_with = "apply")]
        dry_run: bool,
    },
    /// Show one snapshot's metadata.
    Show {
        /// Snapshot ID.
        id: String,
    },
    /// Print a patch from a snapshot to current files, or to another snapshot.
    /// Human output preserves arbitrary bytes. JSON uses patch_encoding=base64
    /// when patch bytes are not UTF-8; otherwise patch contains ordinary text.
    Diff {
        /// Source snapshot ID.
        id: String,
        /// Target snapshot ID; omit to compare with current files.
        other: Option<String>,
        /// Largest changed current file to capture, in bytes; 0 removes the limit.
        /// Unused when comparing two saved snapshots.
        #[arg(long = "max-file-size", value_name = "SIZE", value_parser = parse_size, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// Restore in place or into a new directory.
    #[command(after_long_help = help!("restore"))]
    Restore {
        /// Snapshot ID.
        id: String,
        /// Recover into this new directory instead of overwriting current files.
        #[arg(long)]
        to: Option<PathBuf>,
        /// Largest changed file in the safety snapshot, in bytes; 0 removes the limit.
        /// Unused with --to.
        #[arg(long = "max-file-size", value_name = "SIZE", value_parser = parse_size, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// Delete one snapshot.
    Delete {
        /// Snapshot UUIDs or unique prefixes of at least eight characters.
        #[arg(required = true, value_name = "SNAPSHOT")]
        ids: Vec<String>,
        /// Also delete a snapshot pinned for saved release or interrupted recovery.
        #[arg(long = "pinned", visible_alias = "force")]
        force: bool,
        /// Preview the selected snapshots without deleting them.
        #[arg(short = 'n', long, conflicts_with = "yes")]
        dry_run: bool,
        /// Confirm deletion of multiple snapshots without prompting.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum PoolCommand {
    /// Print one pool setting (capacity, cleanup, ready-target, idle-timeout, ready-base, ready-git-timeout, workspace-root).
    #[command(after_long_help = help!("pool-get"), allow_missing_positional = true)]
    Get {
        /// Pool to inspect; also accepts the legacy --pool selection.
        name: Option<PoolName>,
        /// Setting to read; unknown keys are rejected.
        key: String,
    },
    /// List repository pools, including interrupted pools; hide deleted pools by default.
    #[command(after_long_help = help!("pool-list"))]
    List {
        /// Sort pools by name or lifecycle state, then name.
        #[arg(long, value_enum, default_value = "name")]
        sort: PoolSort,
        #[command(flatten)]
        page: PageArgs,
        /// Include deleted pools with retained history and recovery information.
        #[arg(long)]
        all: bool,
        /// Scan logical workspace, cache and log sizes (can be slow on large pools).
        #[arg(long)]
        disk_usage: bool,
    },
    /// Delete an idle pool's checkouts and legacy caches, retaining snapshots.
    #[command(name = "delete", visible_alias = "destroy", after_long_help = help!("pool-destroy"))]
    Destroy {
        /// Pool to delete; also accepts the legacy --pool selection.
        name: Option<PoolName>,
        /// Validate and preview deletion without removing data or changing lifecycle state.
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Confirm deletion without an interactive prompt.
        #[arg(short = 'y', long, conflicts_with = "dry_run")]
        yes: bool,
        /// Discard residual edits, ignored files and broken checkouts; never bypasses owners or reservations.
        #[arg(short = 'f', long = "force")]
        discard_changes: bool,
        /// Require the generation returned by a previous preview; protects a replacement pool.
        #[arg(long)]
        pool_id: Option<PoolId>,
    },
    /// Create a pool of reusable worktree slots for this repository.
    #[command(name = "create", visible_alias = "init", after_long_help = help!("pool-init"))]
    Init {
        /// Save the pool's default foreground Git budget (default: pool-defaults.git-timeout, 2m).
        #[arg(long = "git-timeout", value_name = "DURATION", value_parser = parse_positive_duration)]
        saved_timeout: Option<u64>,
        /// Name of the pool to create (default: default).
        name: Option<PoolName>,
        /// Desired ready, unreserved checkouts (default: pool_defaults.ready_target, 0).
        #[arg(long)]
        ready_target: Option<usize>,
        /// Evict excess idle checkouts after this duration, e.g. 7d, or never.
        #[arg(long)]
        idle_timeout: Option<groove_core::IdleTimeout>,
        /// Local revision for spare preparation; defaults to the initializing checkout's HEAD branch or commit.
        #[arg(long)]
        ready_base: Option<String>,
        /// Background Git timeout (positive ms/s/m/h); unset inherits --git-timeout.
        #[arg(long, value_name = "DURATION")]
        ready_git_timeout: Option<groove_core::ReadyGitTimeout>,
        /// Maximum simultaneous reservations (1..=4096); grow later with `pool set`.
        #[arg(long, value_parser = parse_capacity)]
        capacity: usize,
        /// Cleanup applied when a slot is reused; may delete workspace contents.
        #[arg(long, value_enum, default_value = "pristine")]
        cleanup: Cleanup,
        /// Exact, initially empty checkout root (overrides worktrees.root and ~/.worktrees).
        #[arg(long = "root", visible_alias = "workspace-root", value_name = "DIR")]
        worktree_root: Option<PathBuf>,
        /// Resume an interrupted init with its original settings.
        #[arg(long)]
        resume: bool,
    },
    /// Show pool settings, slots and background preparation.
    #[command(after_long_help = help!("pool-show"))]
    Show {
        /// Pool to inspect; also accepts the legacy --pool selection.
        name: Option<PoolName>,
        /// Inspect one permanent slot, such as wt-0001.
        #[arg(long)]
        slot: Option<SlotId>,
        /// Include bounded Git status for every slot.
        #[arg(long)]
        details: bool,
        /// Scan logical storage sizes (can be slow).
        #[arg(long)]
        disk_usage: bool,
        /// Sort slots, with slot ID breaking ties.
        #[arg(long, value_enum, default_value = "slot")]
        sort: SlotSort,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Grow capacity and/or change cleanup for future acquisitions.
    #[command(after_long_help = help!("pool-set"), group(clap::ArgGroup::new("settings").required(true).multiple(true)
        .args(["values", "capacity", "cleanup", "ready_target", "idle_timeout", "ready_base", "ready_git_timeout"])))]
    Set {
        /// Optional pool name followed by KEY=VALUE settings (for example capacity=4).
        #[arg(value_name = "NAME|KEY=VALUE")]
        values: Vec<PoolSettingArgument>,
        /// Desired ready, unreserved checkouts; cannot exceed resulting capacity. Zero disables replenishment.
        #[arg(long, hide = true)]
        ready_target: Option<usize>,
        /// Idle duration for excess checkouts (positive s/m/h/d), or never to disable eviction.
        #[arg(long, hide = true)]
        idle_timeout: Option<groove_core::IdleTimeout>,
        /// Local revision for spare preparation; no implicit fetch or project commands.
        #[arg(long, hide = true)]
        ready_base: Option<String>,
        /// Background Git timeout (positive ms/s/m/h); unset inherits --git-timeout.
        #[arg(long, hide = true, value_name = "DURATION")]
        ready_git_timeout: Option<groove_core::ReadyGitTimeout>,
        /// New capacity (1..=4096); shrinking is rejected.
        #[arg(long, hide = true, value_parser = parse_capacity)]
        capacity: Option<usize>,
        /// Cleanup policy for future acquisitions.
        #[arg(long, hide = true, value_enum)]
        cleanup: Option<Cleanup>,
    },
}

impl PoolCommand {
    pub fn name(&self) -> Option<&PoolName> {
        match self {
            Self::Get { name, .. }
            | Self::Destroy { name, .. }
            | Self::Init { name, .. }
            | Self::Show { name, .. } => name.as_ref(),
            Self::Set { values, .. } => values.iter().find_map(|value| match value {
                PoolSettingArgument::Name(name) => Some(name),
                PoolSettingArgument::Setting(_, _) => None,
            }),
            Self::List { .. } => None,
        }
    }
}

/// Pool settings retain their typed namespace separately from assignment strings.
#[derive(Clone, Debug)]
pub enum PoolSettingArgument {
    Name(PoolName),
    Setting(String, String),
}

impl std::str::FromStr for PoolSettingArgument {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some((key, value)) = value.split_once('=') {
            Ok(Self::Setting(key.into(), value.into()))
        } else {
            value
                .parse()
                .map(Self::Name)
                .map_err(|error: groove_core::Error| error.to_string())
        }
    }
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Print one effective preference; unset optional values print an empty line.
    Get {
        /// Dotted preference key, e.g. logging.level or worktrees.root.
        key: String,
    },
    /// Show effective preferences and which environment variables override them.
    Show,
    /// Print the preferences file path, even if its contents are invalid.
    Path,
    /// Edit preferences with VISUAL, EDITOR or vi; validated before saving.
    Edit,
    /// Set preferences, e.g. logging.max_files=50 logging.enabled=false.
    Set {
        /// KEY=VALUE pairs; values use TOML syntax (strings may be bare).
        #[arg(required = true, value_parser = parse_setting)]
        settings: Vec<(String, String)>,
    },
    /// Remove preferences, restoring their defaults.
    Unset {
        /// Dotted keys such as logging.directory.
        #[arg(required = true)]
        keys: Vec<String>,
    },
}

impl Command {
    /// Stable command name for logs.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Help { .. } => "help",
            Self::Upgrade(_) => "upgrade",
            Self::Pool(command) => match command {
                PoolCommand::Get { .. } => "pool get",
                PoolCommand::Init { .. } => "pool create",
                PoolCommand::List { .. } => "pool list",
                PoolCommand::Show { .. } => "pool show",
                PoolCommand::Set { .. } => "pool set",
                PoolCommand::Destroy { .. } => "pool delete",
            },
            Self::Acquire { .. } => "acquire",
            Self::Release { .. } => "release",
            Self::List { .. } => "list",
            Self::Show { .. } => "show",
            Self::Path { .. } => "path",
            Self::Completions { .. } => "completions",
            Self::Snapshot(_) => "snapshot",
            Self::Config(_) => "config",
        }
    }
}

fn parse_duration(value: &str) -> Result<u64, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else if let Some(number) = value.strip_suffix('h') {
        (number, 3_600_000)
    } else {
        return Err("Duration requires a unit: ms, s, m or h (for example 30s)".into());
    };
    if number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("Duration must be a non-negative integer with a unit, for example 30s".into());
    }
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| "Duration is too large".into())
}

fn parse_retention_age(value: &str) -> Result<u64, String> {
    if let Some(days) = value.strip_suffix('d') {
        if days.is_empty() || !days.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("Retention age requires a non-negative integer and unit, e.g. 30d".into());
        }
        return days
            .parse::<u64>()
            .ok()
            .and_then(|days| days.checked_mul(86_400_000))
            .ok_or_else(|| "Retention age is too large".into());
    }
    parse_duration(value).map_err(|_| {
        "Retention age requires ms, s, m, h or d within the supported range, e.g. 30d".into()
    })
}

fn parse_positive_duration(value: &str) -> Result<u64, String> {
    let duration = parse_duration(value)?;
    if duration == 0 {
        return Err("Git timeout must be positive, for example 2m".into());
    }
    Ok(duration)
}

fn parse_size(value: &str) -> Result<u64, String> {
    let boundary = value
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, suffix) = value.split_at(boundary);
    let multiplier = match suffix.to_ascii_lowercase().as_str() {
        "" => 1,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        _ => {
            return Err(
                "Size requires bytes or K, M or G, for example 100M (0 is unlimited)".into(),
            )
        }
    };
    number
        .parse::<u64>()
        .ok()
        .and_then(|number| number.checked_mul(multiplier))
        .ok_or_else(|| "Size must be a non-negative integer within the supported byte range".into())
}

fn parse_capacity(value: &str) -> Result<usize, String> {
    let capacity: usize = value.parse().map_err(|_| "Capacity must be a number")?;
    if (1..=groove_core::MAX_CAPACITY).contains(&capacity) {
        Ok(capacity)
    } else {
        Err(format!(
            "Capacity must be 1..={}",
            groove_core::MAX_CAPACITY
        ))
    }
}

fn parse_setting(value: &str) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or("Use KEY=VALUE, e.g. logging.max_files=50")?;
    Ok((key.trim().to_owned(), value.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn requested_direction_applies_to_the_collection_before_the_page_window() {
        for (default_order, entries, order, expected) in [
            (
                SortOrder::Asc,
                vec![1, 2, 3, 4],
                SortOrder::Desc,
                vec![3, 2],
            ),
            (
                SortOrder::Desc,
                vec![4, 3, 2, 1],
                SortOrder::Asc,
                vec![2, 3],
            ),
            (
                SortOrder::Desc,
                vec![4, 3, 2, 1],
                SortOrder::Desc,
                vec![3, 2],
            ),
        ] {
            let (actual, pagination) = PageArgs {
                order: Some(order),
                limit: Some(2),
                offset: 1,
                ..PageArgs::default()
            }
            .apply_ordered(entries, default_order);
            assert_eq!(actual, expected);
            assert_eq!(pagination.total, 4);
            assert_eq!(pagination.next_offset, Some(3));
        }
    }

    #[test]
    fn large_collections_default_to_a_bounded_page_and_can_be_exhausted() {
        let (entries, page) = PageArgs::default().apply((0..10_000).collect::<Vec<_>>());
        assert_eq!(entries, (0..50).collect::<Vec<_>>());
        assert_eq!(page.total, 10_000);
        assert_eq!(page.limit, Some(50));
        assert_eq!(page.next_offset, Some(50));
        let (entries, page) = PageArgs {
            offset: 9_999,
            ..PageArgs::default()
        }
        .apply((0..10_000).collect::<Vec<_>>());
        assert_eq!(entries, vec![9_999]);
        assert_eq!(page.next_offset, None);
        let (entries, page) = PageArgs {
            unlimited: true,
            ..PageArgs::default()
        }
        .apply((0..10_000).collect::<Vec<_>>());
        assert_eq!(entries.len(), 10_000);
        assert_eq!(page.limit, None);
        assert_eq!(page.next_offset, None);
        for offset in [10_000, u64::MAX] {
            let (entries, page) = PageArgs {
                offset,
                ..PageArgs::default()
            }
            .apply((0..10_000).collect::<Vec<_>>());
            assert!(entries.is_empty());
            assert_eq!(page.total, 10_000);
            assert_eq!(page.next_offset, None);
        }
    }

    #[test]
    fn every_command_and_argument_has_help() {
        fn check(command: &clap::Command) {
            assert!(
                command.get_about().is_some(),
                "{} needs a summary",
                command.get_name()
            );
            for arg in command.get_arguments() {
                assert!(
                    arg.get_help().is_some(),
                    "{}: {} needs help",
                    command.get_name(),
                    arg.get_id()
                );
            }
            for subcommand in command.get_subcommands() {
                check(subcommand);
            }
        }
        Args::command().debug_assert();
        let mut command = Args::command();
        command.build();
        check(&command);
    }

    #[test]
    fn release_selector_allows_inference_or_one_explicit_reservation() {
        assert!(Args::try_parse_from(["groove", "release"]).is_ok());
        assert!(Args::try_parse_from(["groove", "release", "--save"]).is_ok());
        assert!(Args::try_parse_from(["groove", "release", "--task", "a"]).is_ok());
        let both = [
            "groove",
            "release",
            "--task",
            "a",
            "--reservation-id",
            "00000000-0000-4000-8000-000000000000",
        ];
        assert!(Args::try_parse_from(both).is_err());
    }

    #[test]
    fn pool_configuration_requires_any_setting_rather_than_capacity() {
        let error = Args::try_parse_from(["groove", "pool", "set"])
            .err()
            .unwrap()
            .to_string();
        for (flag, value) in [
            ("--capacity", "2"),
            ("--cleanup", "warm"),
            ("--ready-target", "0"),
            ("--idle-timeout", "never"),
            ("--ready-base", "main"),
            ("--ready-git-timeout", "10m"),
        ] {
            assert!(error.contains(flag), "{error}");
            assert!(Args::try_parse_from(["groove", "pool", "set", flag, value]).is_ok());
        }
        assert!(Args::try_parse_from([
            "groove",
            "pool",
            "set",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never"
        ])
        .is_ok());
    }
}

#[cfg(test)]
mod scalar_tests {
    use super::*;

    #[test]
    fn durations_require_units_and_reject_signs_fractions_and_overflow() {
        for (input, expected) in [
            ("0s", 0),
            ("7ms", 7),
            ("30s", 30_000),
            ("2m", 120_000),
            ("3h", 10_800_000),
            ("18446744073709551615ms", u64::MAX),
        ] {
            assert_eq!(parse_duration(input).unwrap(), expected, "{input}");
        }
        for input in [
            "30",
            "",
            "s",
            "-1s",
            "+1s",
            "1.5s",
            "1d",
            "1S",
            "18446744073709551616ms",
            "18446744073709551615s",
        ] {
            assert!(parse_duration(input).is_err(), "{input}");
        }
        assert!(parse_duration("30").unwrap_err().contains("30s"));
        assert!(parse_positive_duration("0ms").is_err());
        assert_eq!(parse_positive_duration("1ms").unwrap(), 1);
    }

    #[test]
    fn file_sizes_accept_binary_units_and_reject_overflow() {
        for (input, expected) in [
            ("0", 0),
            ("123", 123),
            ("1K", 1024),
            ("2kb", 2048),
            ("3KiB", 3072),
            ("100M", 104_857_600),
            ("1gIb", 1_073_741_824),
            ("18446744073709551615", u64::MAX),
        ] {
            assert_eq!(parse_size(input).unwrap(), expected, "{input}");
        }
        for input in [
            "",
            "M",
            "-1",
            "+1K",
            "1.5M",
            "1T",
            "1 B",
            "18446744073709551616",
            "18446744073709551615G",
        ] {
            assert!(parse_size(input).is_err(), "{input}");
        }
    }

    #[test]
    fn show_allows_inference_or_one_selector_and_pool_show_has_collection_options() {
        assert!(Args::try_parse_from(["groove", "show"]).is_ok());
        for (flag, value) in [
            ("--task", "label"),
            ("--slot", "wt-0001"),
            ("--reservation-id", "00000000-0000-4000-8000-000000000000"),
        ] {
            assert!(Args::try_parse_from(["groove", "show", flag, value]).is_ok());
            assert!(Args::try_parse_from(["groove", "show", flag, value, "--limit", "1"]).is_err());
        }
        assert!(
            Args::try_parse_from(["groove", "show", "--task", "label", "--slot", "wt-0001"])
                .is_err()
        );
        assert!(Args::try_parse_from([
            "groove",
            "pool",
            "show",
            "--details",
            "--disk-usage",
            "--limit",
            "1"
        ])
        .is_ok());
    }
}
