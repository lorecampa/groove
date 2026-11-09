//! Repository-scoped stdio MCP adapter. Lease protections survive server shutdown.
mod server;

use clap::Parser;
use rmcp::{transport::stdio, ServiceExt};
use std::{path::PathBuf, time::Duration};

#[derive(Parser)]
#[command(version, about = "Local MCP server for Groove worktree pools")]
struct Args {
    /// Repository accessible to both this server and the agent's editing tools.
    #[arg(long)]
    repo: PathBuf,
    /// Timeout for each Git maintenance operation.
    #[arg(long, default_value_t = 30000, value_parser = clap::value_parser!(u64).range(1..=120000))]
    git_timeout_ms: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let repo = std::fs::canonicalize(args.repo)?;
    let server = server::GrooveServer::new(repo, Duration::from_millis(args.git_timeout_ms));
    let service = server.serve(stdio()).await?;
    // EOF closes the transport. Dropping handles preserves durable reuse protection.
    // SIGTERM/SIGKILL are safe for the same reason; no cleanup runs from Drop.
    service.waiting().await?;
    Ok(())
}
