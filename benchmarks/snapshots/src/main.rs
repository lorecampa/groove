//! Development-only reproducible snapshot lab; never shipped with Groove.
mod container;
mod fixture;
mod harness;
mod report;
mod util;
use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;
use util::{Result, Runner};

#[derive(Parser)]
#[command(
    about = "Reproducible snapshot benchmarks and deterministic scale checks",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Build/cache the pinned container and print its image ID.
    Build,
    /// Run deterministic correctness tests in the pinned container.
    Check(ContainerArgs),
    /// Measure the release Rust CLI in the pinned container.
    Benchmark(ContainerArgs),
    /// Run directly on this host (non-canonical unless inside the pinned container).
    Run(RunArgs),
    /// Verify tool versions against environment.lock.json.
    VerifyEnvironment,
    /// Compare two successful canonical reports with matching workloads/environment.
    Compare {
        /// Baseline JSON report.
        before: PathBuf,
        /// Candidate JSON report.
        after: PathBuf,
    },
}
#[derive(Args)]
struct ContainerArgs {
    /// Repeat for each deterministic fixture size.
    #[arg(long, required=true, value_parser=["smoke","large","million"])]
    profile: Vec<String>,
    /// Disk-backed scratch parent; disposable data is removed on completion.
    #[arg(long)]
    temp_root: PathBuf,
    /// Destination JSON report, atomically replaced.
    #[arg(long)]
    output: PathBuf,
    /// Number of warm changed-save measurements.
    #[arg(long, default_value_t = 5)]
    runs: usize,
    /// Source repository, mounted read-only and cloned without hardlinks.
    #[arg(long)]
    real_repo: Option<PathBuf>,
}
#[derive(Args)]
pub(crate) struct RunArgs {
    /// Correctness checks or performance measurements.
    #[arg(long, value_parser=["check","benchmark"])]
    mode: String,
    /// Repeat for each deterministic fixture size.
    #[arg(long, required=true, value_parser=["smoke","large","million"])]
    profile: Vec<String>,
    /// Number of warm changed-save measurements.
    #[arg(long, default_value_t = 5)]
    runs: usize,
    /// Groove release binary (defaults to target/release/groove).
    #[arg(long)]
    binary: Option<PathBuf>,
    /// Explicit release snapshot_scale test executable; otherwise discover it.
    #[arg(long)]
    scale_binary: Option<PathBuf>,
    /// Scratch parent with enough disk space and inodes.
    #[arg(long)]
    temp_root: PathBuf,
    /// Destination JSON report, atomically replaced.
    #[arg(long)]
    output: PathBuf,
    /// Enforce pinned tool versions, storage and resource limits.
    #[arg(long)]
    canonical: bool,
    /// Source repository to clone; never benchmark in the source checkout.
    #[arg(long)]
    real_repo: Option<PathBuf>,
}
fn execute() -> Result<()> {
    match Cli::parse().command {
        Action::Build => {
            println!("{}", container::build()?.1);
            Ok(())
        }
        Action::Check(args) => container::run("check", args),
        Action::Benchmark(args) => container::run("benchmark", args),
        Action::Run(args) => harness::run(args),
        Action::VerifyEnvironment => {
            println!("{}", report::verify_environment(&Runner::new())?);
            Ok(())
        }
        Action::Compare { before, after } => {
            let a = serde_json::from_slice(&std::fs::read(before)?)?;
            let b = serde_json::from_slice(&std::fs::read(after)?)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&report::compare(&a, &b)?)?
            );
            Ok(())
        }
    }
}
fn main() {
    if let Err(error) = execute() {
        eprintln!("groove-bench: {error}");
        std::process::exit(1);
    }
}
