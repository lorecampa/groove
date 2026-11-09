use crate::help;
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "groove",
    version,
    about = "Reusable Git worktree pools and local checkpoints for coding agents",
    after_help = help::OVERVIEW
)]
pub struct Args {
    /// Repository path; Git discovery also works from subdirectories and linked worktrees.
    #[arg(long, global = true, default_value = ".")]
    pub repo: PathBuf,
    /// Echo this ID in responses/logs for correlation; does not make retries idempotent.
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    /// Mirror debug events to stderr; repeat (-vv) for trace detail.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Timeout per Git operation and pool admin-lock wait, in milliseconds (positive).
    #[arg(long, global=true, default_value_t=120000, value_parser=clap::value_parser!(u64).range(1..))]
    pub git_timeout_ms: u64,
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Cleanup {
    /// Discard tracked edits and all untracked/ignored files before reuse.
    Pristine,
    /// Discard tracked/non-ignored edits; retain unverified ignored files when healthy.
    Warm,
}
#[derive(Subcommand)]
pub enum Action {
    /// Save and recover local Git checkpoints, independently of a pool.
    #[command(after_help = help::SNAPSHOT)]
    Snapshot {
        #[command(subcommand)]
        command: SnapshotAction,
    },
    /// Inspect or change user-wide preferences (no repository required).
    #[command(after_help = help::CONFIG)]
    Config {
        #[command(subcommand)]
        command: ConfigAction,
    },
    /// Initialize a fixed-capacity pool; identical repeated settings are safe.
    #[command(after_help = help::INIT)]
    Init {
        /// Maximum simultaneous reservations (1..4096); immutable after initialization.
        #[arg(long, value_parser=clap::value_parser!(u16).range(1..=4096))]
        capacity: u16,
        /// Cleanup policy applied on acquisition; may delete workspace contents.
        #[arg(long, value_enum, default_value = "pristine")]
        cleanup: Cleanup,
        /// Dedicated empty local checkout root (default: under the common Git directory).
        #[arg(long)]
        worktree_root: Option<PathBuf>,
    },
    /// Reserve a cleaned, verified checkout until explicitly released.
    #[command(after_help = help::ACQUIRE)]
    Acquire {
        /// Git revision to resolve to a commit for the detached checkout.
        #[arg(long, default_value = "HEAD")]
        base: String,
        /// Advanced: inherited Unix SOCK_SEQPACKET FD for SCM_RIGHTS lease handoff.
        #[arg(long, value_parser=clap::value_parser!(i32).range(0..))]
        lease_channel_fd: Option<i32>,
        /// Capacity wait in milliseconds; 0 scans once (separate from Git timeouts).
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
    },
    /// List durable reservations, including ones left by interrupted commands.
    #[command(after_help = help::LEASES)]
    Leases,
    /// Release only after preserving results and stopping every writer.
    #[command(after_help = help::RELEASE)]
    Release {
        /// Acquired slot identifier, e.g. wt-0001 (through wt-4096).
        #[arg(long)]
        slot: groove_core::SlotId,
        /// Original acquisition UUID; stale generations are rejected.
        #[arg(long)]
        lease_id: groove_core::LeaseId,
        /// Advanced: inherited lease FD for descriptor-owner release.
        #[arg(long, value_parser=clap::value_parser!(i32).range(0..), conflicts_with = "writers_stopped")]
        lease_fd: Option<i32>,
        /// Confirm results are saved and ALL writers stopped; removes durable protection.
        #[arg(long, required_unless_present = "lease_fd")]
        writers_stopped: bool,
    },
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Config { .. } => "config",
            Self::Snapshot { .. } => "snapshot",
            Self::Init { .. } => "init",
            Self::Acquire { .. } => "acquire",
            Self::Leases => "leases",
            Self::Release { .. } => "release",
        }
    }
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show effective settings, file path and environment override indicators.
    Show,
    /// Show the user configuration file path, even if settings are broken.
    Path,
    /// Persist one preference; see `groove config --help` for keys and limits.
    #[command(after_help = help::CONFIG)]
    Set {
        /// Dotted setting name, e.g. logging.enabled or logging.max_files.
        key: String,
        /// TOML value; quote strings, e.g. '"/tmp/groove-logs"'.
        value: String,
    },
    /// Remove a file preference; defaults or environment overrides then apply.
    Unset {
        /// Dotted setting name, e.g. logging.directory.
        key: String,
    },
    /// Edit a temporary copy with VISUAL/EDITOR/vi; validate before atomic save.
    #[command(after_help = help::CONFIG)]
    Edit,
}

#[derive(Subcommand)]
pub enum SnapshotAction {
    /// Save tracked and non-ignored untracked files without changing HEAD/index.
    #[command(
        after_help = "Stop writers first. Ignored files and empty directories are excluded. Returns\nsnapshot_result.snapshot with the snapshot UUID. See groove snapshot --help\nfor supported configurations, storage and consistency limits."
    )]
    Save {
        /// Optional description of the checkpoint (quote multiword descriptions).
        #[arg(default_value = "")]
        description: String,
        /// Task/session identifier recorded with the snapshot.
        #[arg(long)]
        session: Option<String>,
        /// Maximum changed-file size in bytes (default 100 MiB); 0 removes the limit.
        #[arg(long, default_value_t = 104857600)]
        max_file_bytes: u64,
    },
    /// List repository-wide snapshots and their IDs, metadata and pin state.
    List,
    /// Inspect one snapshot's metadata and saved tree identifiers.
    Show {
        /// Snapshot UUID returned by save/list.
        id: String,
    },
    /// Show a patch from a snapshot to current files, or to another snapshot.
    Diff {
        /// Source snapshot UUID.
        id: String,
        /// Target snapshot UUID; omit to compare against current files, including untracked.
        other: Option<String>,
    },
    /// Restore staging/files with a safety snapshot, or recover into a new directory.
    #[command(after_help = help::RESTORE)]
    Restore {
        /// Snapshot UUID to recover.
        id: String,
        /// Recover into this nonexistent directory instead of overwriting current files.
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Delete one local snapshot; object space may not be reclaimed immediately.
    Delete {
        /// Snapshot UUID to delete.
        id: String,
        /// Allow deletion even when this snapshot is pinned.
        #[arg(long)]
        force: bool,
    },
    /// Protect a snapshot from automatic retention and pruning.
    Pin {
        /// Snapshot UUID to pin; explicit delete --force can still remove it.
        id: String,
    },
    /// Remove pin protection; future pruning may delete the snapshot.
    Unpin {
        /// Snapshot UUID to unpin.
        id: String,
    },
    /// Delete eligible unpinned snapshots, or preview with --dry-run.
    #[command(after_help = help::RETENTION)]
    Prune {
        /// Select snapshots older than this age in days, e.g. 30d.
        #[arg(long, value_parser = parse_age)]
        older_than: Option<u64>,
        /// Keep the newest N snapshots per workspace (pins count toward N).
        #[arg(long)]
        keep_last: Option<usize>,
        /// List selected snapshot IDs without deleting them.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show or replace automatic retention settings; disabled by default.
    #[command(after_help = help::RETENTION)]
    Retention {
        /// Set an age threshold in days, e.g. 30d; combines with count using AND.
        #[arg(long, value_parser = parse_age, conflicts_with = "disable")]
        older_than: Option<u64>,
        /// Keep the newest N snapshots per workspace; pinned snapshots always survive.
        #[arg(long, conflicts_with = "disable")]
        keep_last: Option<usize>,
        /// Clear automatic retention settings; does not delete snapshots.
        #[arg(long)]
        disable: bool,
    },
    /// Run normal shared Git GC; expiration/reflogs may delay space reclamation.
    Compact,
}
fn parse_age(value: &str) -> Result<u64, String> {
    value
        .strip_suffix('d')
        .ok_or("Use a number of days, e.g. 30d")?
        .parse::<u64>()
        .map_err(|_| "Invalid day count".to_string())?
        .checked_mul(86400)
        .ok_or("Age is too large".into())
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
        let mut command = Args::command();
        command.build();
        command.debug_assert();
        let mut command = Args::command();
        command.build();
        check(&command);
    }
}
