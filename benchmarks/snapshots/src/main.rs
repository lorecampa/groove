//! Development-only benchmark lab; never shipped with Groove.
mod container;
mod evidence;
mod fixture;
mod harness;
mod lifecycle;
mod profiles;
mod report;
mod statistics;
mod util;
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use util::{require, Result, Runner};

#[derive(Parser)]
#[command(
    about = "Run complete, versioned Groove benchmark scenarios",
    version,
    after_help = "Examples:\n  cargo run -p groove-bench --locked -- profiles\n  cargo run -p groove-bench --locked -- benchmark snapshot-incremental\n  cargo run -p groove-bench --locked -- benchmark all\n  cargo run -p groove-bench --locked -- check snapshot-scale\n\nProfiles own all fixtures, operations and sampling. Docker builds release binaries offline.\nOne report is atomically replaced per invocation; generated repositories are disposable."
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// List complete scenarios, resources and sampling; optionally describe one profile.
    Profiles { profile: Option<String> },
    /// Measure selected profiles in pinned Docker; use 'all' to run every scenario.
    Benchmark(ExecuteArgs),
    /// Verify each selected profile case once, without repeated performance sampling.
    Check(ExecuteArgs),
    /// Validate a report and recompute its summary; never creates another report.
    Inspect {
        #[arg(default_value = "benchmarks/snapshots/results/report.json")]
        report: PathBuf,
    },
    #[command(hide = true)]
    Build,
    #[command(hide = true)]
    VerifyEnvironment,
    #[command(name = "__run", hide = true)]
    Run(RunArgs),
}
#[derive(Clone, Args)]
pub(crate) struct Plan {
    /// Complete scenario names, or 'all'. Run 'profiles' to see definitions and cost.
    #[arg(value_name="PROFILE",required=true,num_args=1..)]
    profiles: Vec<String>,
    /// Disk-backed scratch parent; interrupted native fixtures may be retained for recovery.
    #[arg(long, default_value = "target/benchmark-scratch")]
    temp_root: PathBuf,
    /// Single report artifact, atomically replacing this path, including operational failures.
    #[arg(long, default_value = "benchmarks/snapshots/results/report.json")]
    output: PathBuf,
}
#[derive(Args)]
pub(crate) struct ExecuteArgs {
    #[command(flatten)]
    plan: Plan,
    /// Baseline Groove checkout. Both CLIs compile independently in the same pinned image.
    #[arg(long, conflicts_with = "host")]
    baseline_source: Option<PathBuf>,
    /// Explore on this host instead of Docker; results do not claim canonical isolation.
    #[arg(long)]
    host: bool,
    /// Native candidate release binary; requires --host. Defaults to target/release/groove.
    #[arg(long, requires = "host")]
    binary: Option<PathBuf>,
    /// Native baseline release binary; requires --host.
    #[arg(long, requires = "host")]
    baseline_binary: Option<PathBuf>,
    /// Bind the canonical container to four available CPUs, e.g. 0-3; no exclusive reservation.
    #[arg(long, conflicts_with = "host")]
    cpu_set: Option<String>,
}
#[derive(Clone, Copy, Debug, ValueEnum, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Mode {
    Benchmark,
    Check,
}
impl Mode {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Benchmark => "benchmark",
            Self::Check => "check",
        }
    }
}
#[derive(Args)]
pub(crate) struct RunArgs {
    #[command(flatten)]
    plan: Plan,
    #[arg(long, value_enum)]
    mode: Mode,
    #[arg(long)]
    binary: Option<PathBuf>,
    #[arg(long)]
    baseline: Option<PathBuf>,
    #[arg(long)]
    runtime_contract: Option<PathBuf>,
}
fn execute_profiles(mode: Mode, args: ExecuteArgs) -> Result<()> {
    profiles::select(&args.plan.profiles)?;
    require(
        mode != Mode::Check || (args.baseline_source.is_none() && args.baseline_binary.is_none()),
        "check verifies one candidate; baselines are for benchmark comparisons",
    )?;
    if args.host {
        harness::run(RunArgs {
            plan: args.plan,
            mode,
            binary: args.binary,
            baseline: args.baseline_binary,
            runtime_contract: None,
        })
    } else {
        container::run(mode, args)
    }
}
fn execute() -> Result<()> {
    // SAFETY: this sets only this process's creation mask, before worker threads
    // are started; no pointers or borrowed resources are involved.
    unsafe {
        libc::umask(0o022);
    }
    util::install_signals()?;
    match Cli::parse().command {
        Action::Profiles { profile } => profiles::display(profile.as_deref()),
        Action::Benchmark(args) => execute_profiles(Mode::Benchmark, args),
        Action::Check(args) => execute_profiles(Mode::Check, args),
        Action::Build => {
            println!("{}", container::build(None)?.1);
            Ok(())
        }
        Action::VerifyEnvironment => {
            println!("{}", report::verify_environment(&Runner::new())?);
            Ok(())
        }
        Action::Run(args) => harness::run(args),
        Action::Inspect { report } => {
            let mut value = serde_json::from_slice(&std::fs::read(report)?)?;
            report::summarize(&mut value)?;
            println!("{}", serde_json::to_string_pretty(&value["summary"])?);
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
