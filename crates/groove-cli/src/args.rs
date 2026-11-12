//! Command-line grammar. Every command and option carries help; long help lives in `help/`.

use clap::{Args as ClapArgs, Parser, Subcommand, ValueEnum};
use groove_core::{LeaseId, PoolId, PoolName, RunId, SlotId};
use std::{ffi::OsString, path::PathBuf};

macro_rules! help {
    ($name:literal) => {
        include_str!(concat!("../help/", $name, ".txt"))
    };
}

/// Changed files larger than this abort a snapshot unless explicitly raised.
const DEFAULT_MAX_FILE_BYTES: &str = "104857600";

#[derive(Parser)]
#[command(
    name = "groove",
    version = concat!(env!("CARGO_PKG_VERSION"), " (revision ", env!("GROOVE_BUILD_REVISION"), ")"),
    about = "Reusable Git worktree pools and local checkpoints for parallel coding tasks",
    after_help = help!("overview")
)]
pub struct Args {
    /// Repository path; discovery also works from subdirectories and linked worktrees.
    #[arg(long, global = true, default_value = ".")]
    pub repo: PathBuf,
    /// Pool namespace (default: default). Use an explicit name for destroy when several pools exist.
    #[arg(long, global = true)]
    pub pool: Option<PoolName>,
    /// Print one versioned JSON object on stdout instead of text (or set GROOVE_OUTPUT=json).
    #[arg(long, global = true)]
    pub json: bool,
    /// Echo this ID in JSON responses and logs for correlation; not an idempotency key.
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    /// Show informational diagnostics; -vv adds debug, -vvv adds trace.
    #[arg(short, long, global = true, action = clap::ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,
    /// Hide warnings, progress and live worker output; errors and results remain.
    #[arg(short, long, global = true)]
    pub quiet: bool,
    /// Timeout per Git operation and pool admin/snapshot lock wait, in milliseconds.
    #[arg(long, global = true, default_value_t = 120_000, value_parser = clap::value_parser!(u64).range(1..))]
    pub git_timeout_ms: u64,
    #[command(subcommand)]
    pub command: Command,
}

/// Selects one reservation by its lease ID or its unique task label.
#[derive(ClapArgs, Debug, Clone)]
#[group(required = true, multiple = false)]
pub struct LeaseSelector {
    /// Lease UUID returned by acquire.
    #[arg(long)]
    pub lease_id: Option<LeaseId>,
    /// Task label of exactly one active reservation.
    #[arg(long)]
    pub task: Option<String>,
}

/// Like [`LeaseSelector`], but selecting nothing is allowed.
#[derive(ClapArgs, Debug, Clone, Default)]
#[group(multiple = false)]
pub struct OptionalSelector {
    /// Lease UUID returned by acquire.
    #[arg(long)]
    pub lease_id: Option<LeaseId>,
    /// Task label of exactly one active reservation.
    #[arg(long)]
    pub task: Option<String>,
    /// Permanent slot, such as wt-0001.
    #[arg(long)]
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
    /// List all repository pools, including interrupted and retained deleted pools.
    #[command(after_help = help!("pools"))]
    Pools,
    /// Delete an idle pool's checkouts and caches, retaining snapshots and history.
    #[command(after_help = help!("destroy"))]
    Destroy {
        /// Validate and preview deletion without removing data or changing lifecycle state.
        #[arg(long)]
        dry_run: bool,
        /// Discard residual edits, ignored files and broken checkouts; never bypasses owners or reservations.
        #[arg(long)]
        discard_changes: bool,
        /// Require the generation returned by a previous preview; protects a replacement pool.
        #[arg(long)]
        pool_id: Option<PoolId>,
    },
    /// Create a pool of reusable worktree slots for this repository.
    #[command(after_help = help!("init"))]
    Init {
        /// Maximum simultaneous reservations (1..=4096); grow later with `config set`.
        #[arg(long, value_parser = parse_capacity)]
        capacity: usize,
        /// Cleanup applied when a slot is reused; may delete workspace contents.
        #[arg(long, value_enum, default_value = "pristine")]
        cleanup: Cleanup,
        /// Dedicated, initially empty checkout root (default: under the Git directory).
        #[arg(long)]
        worktree_root: Option<PathBuf>,
        /// Resume an interrupted init with its original settings.
        #[arg(long)]
        resume: bool,
    },
    /// Reserve a clean checkout of a commit until it is released.
    #[command(after_help = help!("acquire"))]
    Acquire {
        /// Commit, branch or tag whose files the checkout starts from (HEAD detached there).
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Unique name for this reservation (1..256 bytes, no control characters); later commands select it with --task.
        #[arg(long)]
        task: Option<String>,
        /// Idempotency key: retrying the same key/base/task returns the same reservation.
        #[arg(long)]
        request_key: Option<String>,
        /// Milliseconds to wait for a free slot; 0 scans once.
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
        #[command(flatten)]
        setup: SetupArgs,
    },
    /// Run a command, setup profile, recipe or agent inside a reservation.
    #[command(after_help = help!("exec"))]
    Exec {
        #[command(flatten)]
        lease: LeaseSelector,
        #[command(flatten)]
        limits: ExecLimits,
        #[command(flatten)]
        setup: SetupArgs,
        /// Named project command; its required setup must already be ready.
        #[arg(long, conflicts_with_all = ["agent", "profile"])]
        recipe: Option<String>,
        /// Named project agent, or claude/codex/opencode/gemini/aider.
        #[arg(long, conflicts_with = "profile")]
        agent: Option<String>,
        /// Program and arguments, or extra recipe/agent arguments (after `--`).
        #[arg(last = true, required_unless_present_any = ["profile", "recipe", "agent"], num_args = 1..)]
        command: Vec<OsString>,
    },
    /// Ask a running exec or setup to stop; the reservation is kept.
    #[command(
        after_help = "EXAMPLE:\n  groove stop --task tasks/fix-parser\n\nStopping is asynchronous: run `groove show` until the execution clears.\nIf the supervisor died, stop external writers yourself, then run `groove recover`."
    )]
    Stop {
        #[command(flatten)]
        lease: LeaseSelector,
    },
    /// Clear an orphaned execution record after stopping ALL external writers yourself.
    #[command(
        after_help = "EXAMPLE:\n  groove recover --task tasks/fix-parser\n\nOperator action: stop every writer first. A live execution lock rejects recovery;\nmissing PIDs and age never authorize it. The reservation itself is kept."
    )]
    Recover {
        #[command(flatten)]
        lease: LeaseSelector,
    },
    /// Save and pin a snapshot of the workspace, then release it.
    #[command(after_help = help!("finish"))]
    Finish {
        #[command(flatten)]
        lease: LeaseSelector,
        /// Description recorded with the pinned snapshot.
        #[arg(long, default_value = "task complete")]
        description: String,
        /// Largest changed file to capture, in bytes; 0 removes the limit.
        #[arg(long, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
        /// Finish even if other processes are still using the workspace.
        #[arg(long)]
        force: bool,
    },
    /// Return a workspace to the pool; commit or finish and stop writers first.
    #[command(after_help = help!("release"))]
    Release {
        #[command(flatten)]
        lease: LeaseSelector,
        /// Release even with unsaved changes or processes still using the workspace;
        /// the next acquisition discards its contents.
        #[arg(long)]
        force: bool,
    },
    /// List active reservations, or every slot.
    #[command(after_help = help!("list"))]
    List {
        /// List every slot with its status and suggested next action.
        #[arg(long)]
        slots: bool,
        /// Include bounded local Git status for each slot (implies --slots).
        #[arg(long)]
        details: bool,
    },
    /// Show pool settings and slots, or one reservation.
    #[command(after_help = help!("show"))]
    Show {
        #[command(flatten)]
        selector: OptionalSelector,
        /// Include bounded local Git status.
        #[arg(long)]
        details: bool,
    },
    /// Print a shell completion script for commands, options and fixed values.
    #[command(after_help = help!("completions"))]
    Completions {
        /// Shell to generate completions for; no shell is selected by default.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// List and inspect executions, including runs from released reservations.
    #[command(after_help = help!("history"), args_conflicts_with_subcommands = true)]
    History {
        /// Match a historical task label (including reused labels).
        #[arg(long)]
        task: Option<String>,
        /// Match a historical reservation generation.
        #[arg(long)]
        lease_id: Option<LeaseId>,
        #[command(flatten)]
        page: PageArgs,
        #[command(subcommand)]
        command: Option<HistoryCommand>,
    },
    /// Inspect storage usage and explicitly remove retained execution logs.
    #[command(subcommand, after_help = help!("storage"))]
    Storage(StorageCommand),
    /// Save, inspect, restore and delete local checkpoints.
    #[command(subcommand, after_help = help!("snapshot"))]
    Snapshot(SnapshotCommand),
    /// Pool settings, project definition and user preferences.
    #[command(subcommand, after_help = help!("config"))]
    Config(ConfigCommand),
}

#[derive(Subcommand)]
pub enum HistoryCommand {
    /// Show a run's outcome, origin, timing and log retention state.
    Show {
        /// Run UUID returned by exec or history.
        run_id: RunId,
    },
    /// Permanently forget a terminal run after its logs have been cleaned.
    #[command(
        after_help = "First run storage clean --logs. Active or unresolved runs cannot be deleted.\nThis never releases a reservation or deletes finish receipts or snapshots."
    )]
    Delete {
        /// Run UUID to forget; deletion cannot be undone.
        run_id: RunId,
    },
}

/// Optional output window; omitted limits preserve the original complete listing.
#[derive(ClapArgs, Debug, Clone, Default)]
pub struct PageArgs {
    /// Return at most this many entries, newest first; default: all entries.
    #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
    pub limit: Option<u64>,
    /// Skip this many newest entries; default: 0. Listings are not a transactional cursor.
    #[arg(long, default_value_t = 0)]
    pub offset: u64,
}
impl PageArgs {
    pub fn apply<T>(&self, entries: Vec<T>) -> Vec<T> {
        entries
            .into_iter()
            .skip(usize::try_from(self.offset).unwrap_or(usize::MAX))
            .take(
                self.limit
                    .and_then(|n| usize::try_from(n).ok())
                    .unwrap_or(usize::MAX),
            )
            .collect()
    }
}

#[derive(Subcommand)]
pub enum StorageCommand {
    /// Show logical bytes in logs, history, artifact caches and managed workspaces.
    #[command(
        after_help = "Counts logical file lengths, not reclaimable disk blocks; symlinks are not followed.\nShared Git objects and snapshots are not estimated. Legacy logs are reported as untracked."
    )]
    Show,
    /// Permanently remove eligible logs, keeping their execution history.
    #[command(after_help = help!("storage"))]
    Clean {
        /// Explicitly select captured stdout/stderr; required, no cleanup target is implicit.
        #[arg(long, required = true)]
        logs: bool,
        /// Minimum time since run completion/recovery: integer plus s, m, h or d; 0s selects all.
        #[arg(long, default_value = "30d", value_parser = parse_age)]
        older_than: std::time::Duration,
        /// Preview eligibility and logical bytes without deleting files or changing history.
        #[arg(long)]
        dry_run: bool,
    },
}

fn parse_age(value: &str) -> Result<std::time::Duration, String> {
    let split = value
        .len()
        .checked_sub(1)
        .ok_or("Use an integer and s, m, h or d (e.g. 30d)")?;
    let (number, unit) = value
        .split_at_checked(split)
        .ok_or("Use an integer and s, m, h or d")?;
    let multiplier = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err("Use s, m, h or d (e.g. 30d)".into()),
    };
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err("Age must be a nonnegative integer with a unit".into());
    }
    number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
        .map(std::time::Duration::from_secs)
        .ok_or("Age is too large".into())
}

/// Explicit project setup options shared by `acquire` and `exec`.
#[derive(ClapArgs, Debug, Clone, Default)]
pub struct SetupArgs {
    /// Run this .groove.toml setup profile; failure keeps the reservation protected.
    #[arg(long = "setup", id = "profile", value_name = "PROFILE")]
    pub profile: Option<String>,
    /// Re-run a failed, interrupted or changed setup from its first step.
    #[arg(long, requires = "profile")]
    pub retry: bool,
    /// Import approved, compatible artifacts from this stopped, prepared checkout.
    #[arg(long, requires = "profile")]
    pub cache_source: Option<PathBuf>,
}

/// Runtime and output bounds for managed execution.
#[derive(ClapArgs, Debug, Clone)]
pub struct ExecLimits {
    /// Maximum runtime in milliseconds; 0 means unlimited, or the recipe/step default.
    #[arg(long, default_value_t = 0)]
    pub timeout_ms: u64,
    /// Grace period between SIGTERM and SIGKILL, at most 60000 ms.
    #[arg(long, default_value_t = 500, value_parser = clap::value_parser!(u64).range(0..=60_000))]
    pub grace_ms: u64,
    /// Bytes kept per output stream; exceeding it stops the worker.
    #[arg(long, default_value_t = 8 * 1024 * 1024, value_parser = clap::value_parser!(u64).range(1..))]
    pub max_log_bytes: u64,
}

#[derive(Subcommand)]
pub enum SnapshotCommand {
    /// Save staged and working files without changing HEAD or the index.
    Save {
        /// Description of the checkpoint.
        #[arg(default_value = "")]
        description: String,
        /// Task or session identifier recorded with the snapshot.
        #[arg(long)]
        session: Option<String>,
        /// Largest changed file to capture, in bytes; 0 removes the limit.
        #[arg(long, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// List snapshots across all linked worktrees, newest first.
    List {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Show one snapshot's metadata.
    Show {
        /// Snapshot ID.
        id: String,
    },
    /// Print a patch from a snapshot to current files, or to another snapshot.
    Diff {
        /// Source snapshot ID.
        id: String,
        /// Target snapshot ID; omit to compare with current files.
        other: Option<String>,
    },
    /// Restore in place or into a new directory.
    #[command(after_help = help!("restore"))]
    Restore {
        /// Snapshot ID.
        id: String,
        /// Recover into this new directory instead of overwriting current files.
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Delete one snapshot.
    Delete {
        /// Snapshot ID.
        id: String,
        /// Also delete a snapshot pinned by `finish`.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum ConfigCommand {
    /// Show this repository's pool settings.
    Show,
    /// Grow capacity and/or change cleanup for future acquisitions.
    Set {
        /// New capacity (1..=4096); shrinking is rejected.
        #[arg(long, value_parser = parse_capacity, required_unless_present = "cleanup")]
        capacity: Option<usize>,
        /// Cleanup policy for future acquisitions.
        #[arg(long, value_enum)]
        cleanup: Option<Cleanup>,
    },
    /// Validate and show this checkout's .groove.toml; never runs project commands.
    Project {
        /// Print a starter .groove.toml instead; nothing is written.
        #[arg(long)]
        example: bool,
    },
    /// User preferences shared by every repository.
    #[command(subcommand)]
    Global(GlobalCommand),
}

#[derive(Subcommand)]
pub enum GlobalCommand {
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
            Self::Pools => "pools",
            Self::Destroy { .. } => "destroy",
            Self::Init { .. } => "init",
            Self::Acquire { .. } => "acquire",
            Self::Exec { .. } => "exec",
            Self::Stop { .. } => "stop",
            Self::Recover { .. } => "recover",
            Self::Finish { .. } => "finish",
            Self::Release { .. } => "release",
            Self::List { .. } => "list",
            Self::Show { .. } => "show",
            Self::Completions { .. } => "completions",
            Self::Snapshot(_) => "snapshot",
            Self::Config(_) => "config",
            Self::History { .. } => "history",
            Self::Storage(_) => "storage",
        }
    }
    /// Whether this invocation may supervise child processes (exec or setup).
    pub fn supervises_workers(&self) -> bool {
        matches!(self, Self::Exec { .. })
            || matches!(self, Self::Acquire { setup, .. } if setup.profile.is_some())
    }
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
    fn lease_selector_requires_exactly_one() {
        assert!(Args::try_parse_from(["groove", "release"]).is_err());
        assert!(Args::try_parse_from(["groove", "release", "--task", "a"]).is_ok());
        let both = [
            "groove",
            "release",
            "--task",
            "a",
            "--lease-id",
            "00000000-0000-4000-8000-000000000000",
        ];
        assert!(Args::try_parse_from(both).is_err());
    }
}
