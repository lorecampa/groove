use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "groove",
    version,
    about = "Descriptor-owned Git worktree pools"
)]
pub struct Args {
    #[arg(long, global = true, default_value = ".")]
    pub repo: PathBuf,
    #[arg(long, global = true)]
    pub request_id: Option<String>,
    #[arg(long, global = true)]
    pub diagnostics: bool,
    #[arg(long, global=true, default_value_t=120000, value_parser=clap::value_parser!(u64).range(1..))]
    pub git_timeout_ms: u64,
    #[command(subcommand)]
    pub command: Action,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Cleanup {
    Pristine,
    Warm,
}
#[derive(Subcommand)]
pub enum Action {
    Init {
        #[arg(long, value_parser=clap::value_parser!(u16).range(1..=4096))]
        capacity: u16,
        #[arg(long, value_enum, default_value = "pristine")]
        cleanup: Cleanup,
        #[arg(long)]
        worktree_root: Option<PathBuf>,
    },
    Acquire {
        #[arg(long)]
        base: String,
        #[arg(long, value_parser=clap::value_parser!(i32).range(0..))]
        lease_channel_fd: i32,
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
    },
    Release {
        #[arg(long)]
        slot: groove_core::SlotId,
        #[arg(long)]
        lease_id: groove_core::LeaseId,
        #[arg(long, value_parser=clap::value_parser!(i32).range(0..))]
        lease_fd: i32,
    },
}
