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
    /// Print one versioned JSON object on stdout instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Echo this ID in JSON responses and logs for correlation; not an idempotency key.
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    /// Show informational diagnostics; -vv adds debug, -vvv adds trace.
    #[arg(short, long, global = true, action = clap::ArgAction::Count, conflicts_with = "quiet")]
    pub verbose: u8,
    /// Hide warnings and progress; errors and results remain.
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
    /// List repository pools, including interrupted pools; hide deleted pools by default.
    #[command(after_help = help!("pools"))]
    Pools {
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
        /// Desired ready, unreserved checkouts (default: pool_defaults.ready_target, 1).
        #[arg(long)]
        ready_target: Option<usize>,
        /// Evict excess idle checkouts after this duration, e.g. 7d, or never.
        #[arg(long)]
        idle_timeout: Option<groove_core::IdleTimeout>,
        /// Local revision for spare preparation; defaults to the initializing checkout's HEAD branch or commit.
        #[arg(long)]
        ready_base: Option<String>,
        /// Maximum simultaneous reservations (1..=4096); grow later with `config set`.
        #[arg(long, value_parser = parse_capacity)]
        capacity: usize,
        /// Cleanup applied when a slot is reused; may delete workspace contents.
        #[arg(long, value_enum, default_value = "pristine")]
        cleanup: Cleanup,
        /// Exact, initially empty checkout root (overrides worktrees.root and ~/.worktrees).
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
    },
    /// Return a workspace to the pool; optionally save a pinned snapshot first.
    #[command(after_help = help!("release"))]
    Release {
        #[command(flatten)]
        lease: LeaseSelector,
        /// Save and pin a snapshot before release; a save failure keeps the reservation.
        #[arg(long)]
        save: bool,
        /// Description recorded with the pinned snapshot.
        #[arg(long, requires = "save", default_value = "workspace saved")]
        description: String,
        /// Largest changed file to capture, in bytes; 0 removes the limit.
        #[arg(long, requires = "save", default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
        /// Skip unsaved-change and process checks (with --save, still save changes).
        /// Without --save, unsaved
        /// contents are discarded on the next acquisition.
        #[arg(long)]
        force: bool,
    },
    /// List active reservations, or every slot.
    #[command(after_help = help!("list"))]
    List {
        /// Sort by slot ID or task label, with slot ID breaking ties.
        #[arg(long, value_enum, default_value = "slot")]
        sort: SlotSort,
        #[command(flatten)]
        page: PageArgs,
        /// List every slot with its status and suggested next action.
        #[arg(long)]
        slots: bool,
        /// Include bounded local Git status for each slot (implies --slots).
        #[arg(long)]
        details: bool,
    },
    /// Show pool settings and slots, or one reservation.
    #[command(after_help = help!("show"), group(clap::ArgGroup::new("collection_options")
        .args(["limit", "offset", "unlimited", "sort", "order"]).multiple(true).conflicts_with("OptionalSelector")))]
    Show {
        /// Sort the slot collection by slot ID or task label, then slot ID.
        #[arg(long, value_enum, default_value = "slot")]
        sort: SlotSort,
        #[command(flatten)]
        page: PageArgs,
        #[command(flatten)]
        selector: OptionalSelector,
        /// Include bounded local Git status.
        #[arg(long)]
        details: bool,
        /// Scan this pool's logical storage sizes (can be slow; pool view only).
        #[arg(long, conflicts_with_all = ["lease_id", "task", "slot"])]
        disk_usage: bool,
    },
    /// Print a shell completion script for commands, options and fixed values.
    #[command(after_help = help!("completions"))]
    Completions {
        /// Shell to generate completions for; no shell is selected by default.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// Save, inspect, restore and delete local checkpoints.
    #[command(subcommand, after_help = help!("snapshot"))]
    Snapshot(SnapshotCommand),
    /// Pool settings and user preferences.
    #[command(subcommand, after_help = help!("config"))]
    Config(ConfigCommand),
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

/// Output window shared by collection commands.
#[derive(ClapArgs, Debug, Clone, Default)]
pub struct PageArgs {
    /// Sort direction, including tie-breakers; default: desc for created, asc otherwise.
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
        #[arg(default_value = "")]
        description: String,
        /// Task or session identifier recorded with the snapshot.
        #[arg(long)]
        session: Option<String>,
        /// Largest changed file to capture, in bytes; 0 removes the limit.
        #[arg(long, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// List snapshots across all linked worktrees (default: newest first).
    #[command(after_help = help!("snapshot-list"))]
    List {
        /// Sort snapshots by creation time or snapshot ID; ties use snapshot ID.
        #[arg(long, value_enum, default_value = "created")]
        sort: TimeSort,
        #[command(flatten)]
        page: PageArgs,
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
        #[arg(long, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// Restore in place or into a new directory.
    #[command(after_help = help!("restore"))]
    Restore {
        /// Snapshot ID.
        id: String,
        /// Recover into this new directory instead of overwriting current files.
        #[arg(long)]
        to: Option<PathBuf>,
        /// Largest changed file in the safety snapshot, in bytes; 0 removes the limit.
        /// Unused with --to.
        #[arg(long, default_value = DEFAULT_MAX_FILE_BYTES)]
        max_file_bytes: u64,
    },
    /// Delete one snapshot.
    Delete {
        /// Snapshot ID.
        id: String,
        /// Also delete a snapshot pinned by `release --save`.
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
        /// Desired ready, unreserved checkouts; cannot exceed resulting capacity. Zero disables replenishment.
        #[arg(long)]
        ready_target: Option<usize>,
        /// Idle duration for excess checkouts (positive s/m/h/d), or never to disable eviction.
        #[arg(long)]
        idle_timeout: Option<groove_core::IdleTimeout>,
        /// Local revision for spare preparation; no implicit fetch or project commands.
        #[arg(long)]
        ready_base: Option<String>,
        /// New capacity (1..=4096); shrinking is rejected.
        #[arg(long, value_parser = parse_capacity, required_unless_present_any = ["cleanup", "ready_target", "idle_timeout", "ready_base"])]
        capacity: Option<usize>,
        /// Cleanup policy for future acquisitions.
        #[arg(long, value_enum)]
        cleanup: Option<Cleanup>,
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
            Self::Pools { .. } => "pools",
            Self::Destroy { .. } => "destroy",
            Self::Init { .. } => "init",
            Self::Acquire { .. } => "acquire",
            Self::Release { .. } => "release",
            Self::List { .. } => "list",
            Self::Show { .. } => "show",
            Self::Completions { .. } => "completions",
            Self::Snapshot(_) => "snapshot",
            Self::Config(_) => "config",
        }
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
