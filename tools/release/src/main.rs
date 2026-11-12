//! Command entry point for the unpublished Rust release tool.
use clap::{Parser, Subcommand};
use groove_release::{Release, Result};
use std::{fs, path::PathBuf};

#[derive(Parser)]
#[command(
    about = "Prepare and verify Groove releases; uploading requires publish --execute",
    version
)]
struct Cli {
    /// Source checkout, normally selected by scripts/release.sh.
    #[arg(long, global = true, default_value = concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))]
    repo_root: PathBuf,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Check versions; with --tag also check release notes, Git state and tag identity.
    Validate {
        #[arg(long)]
        tag: Option<String>,
        /// Require the tagged commit to be an ancestor of this Git ref.
        #[arg(long)]
        branch: Option<String>,
    },
    /// Prepare version, lockfile and changelog changes; never commit, tag or upload.
    Prepare { version: String },
    /// Compile extracted .crate contents and smoke-test production binaries.
    Packages {
        /// Allow uncommitted changes for local verification only.
        #[arg(long)]
        allow_dirty: bool,
        #[arg(long)]
        target: Option<String>,
        /// Use Cargo's release profile.
        #[arg(long)]
        release: bool,
        /// Embed dependency information using separately installed cargo-auditable.
        #[arg(long)]
        auditable: bool,
    },
    /// Write the current version's changelog section.
    Notes {
        #[arg(long)]
        output: PathBuf,
    },
    /// Publish core and CLI, with checksum-verified retries.
    Publish {
        #[arg(long)]
        tag: String,
        /// Explicitly authorize registry uploads.
        #[arg(long)]
        execute: bool,
    },
    /// Bundle verified binaries, documentation, checksums and build metadata.
    Bundle {
        #[arg(long)]
        bin_dir: PathBuf,
        #[arg(long)]
        target: String,
        #[arg(long)]
        output: PathBuf,
    },
}

fn execute() -> Result<()> {
    let args = Cli::parse();
    let release = Release::new(args.repo_root)?;
    match args.command {
        Action::Validate { tag, branch } => {
            println!("{}", release.validate(tag.as_deref(), branch.as_deref())?)
        }
        Action::Prepare { version } => release.prepare(&version)?,
        Action::Packages {
            allow_dirty,
            target,
            release: optimized,
            auditable,
        } => release.packages(allow_dirty, target.as_deref(), optimized, auditable)?,
        Action::Notes { output } => fs::write(output, release.notes()?)?,
        Action::Publish { tag, execute } => release.publish(&tag, execute)?,
        Action::Bundle {
            bin_dir,
            target,
            output,
        } => release.bundle(&bin_dir, &target, &output)?,
    }
    Ok(())
}

fn main() {
    if let Err(error) = execute() {
        eprintln!("Release failed: {error}");
        std::process::exit(1);
    }
}
