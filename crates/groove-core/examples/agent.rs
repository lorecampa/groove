//! cargo run -p groove-core --example agent -- /path/to/repo main
use groove_core::{AcquireOptions, Pool};
use std::process::Command;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let repo = args.next().ok_or("Usage: agent <repo> [base]")?;
    let base = args
        .next()
        .map(|s| s.into_string().map_err(|_| "Base must be UTF-8"))
        .transpose()?
        .unwrap_or_else(|| "HEAD".into());
    let pool = Pool::open(repo)?;
    let mut lease = pool.acquire(AcquireOptions {
        base,
        ..Default::default()
    })?;
    eprintln!(
        "Acquired {} at {}",
        lease.info().slot_id,
        lease.info().worktree_path.display()
    );
    let mut command = Command::new("git");
    command.args(["status", "--short"]);
    let status = lease.run_worker(command)?;
    if !status.success() {
        return Err("Worker failed".into());
    }
    // Preserve task results (for example commit to a task-owned branch) here.
    // This example only inspects the checkout and has nothing to preserve.
    lease.release()?;
    Ok(())
}
