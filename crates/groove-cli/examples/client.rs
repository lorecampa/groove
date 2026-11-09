//! Reference supervisor for the CLI protocol. This is not an additional CLI command.
//! cargo run -p groove-cli --example client -- target/debug/groove /path/to/repo main
use groove_core::{transport, LeaseInfo};
use std::{
    os::fd::{AsFd, AsRawFd},
    process::{Command, Stdio},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let binary = args
        .next()
        .ok_or("Usage: client <groove-binary> <repo> [base]")?;
    let repo = args.next().ok_or("Missing repo")?;
    let base = args.next().unwrap_or_else(|| "HEAD".into());
    let (receiver, sender) = transport::channel()?;
    let mut acquire = Command::new(&binary);
    acquire
        .arg("--repo")
        .arg(&repo)
        .args(["--request-id", "example-acquire", "acquire", "--base"])
        .arg(base)
        .arg("--lease-channel-fd")
        .arg(sender.as_raw_fd().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    // SAFETY: sender remains open through spawn and this Command is not reused.
    unsafe {
        transport::inherit_fd(&mut acquire, sender.as_fd());
    }
    let child = acquire.spawn()?;
    drop(sender);
    let received = transport::receive_record(receiver.as_fd(), Some("example-acquire"));
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(format!(
            "acquire failed: {}",
            String::from_utf8_lossy(&output.stdout)
        )
        .into());
    }
    let (record, descriptor) = received?;
    let info: LeaseInfo = serde_json::from_value(record["result"].clone())?;
    eprintln!("Lease {}: {}", info.lease_id, info.worktree_path.display());
    // Run and supervise task writers here, preserving results before release.
    // The reference client deliberately performs no worktree mutations.
    let mut release = Command::new(binary);
    release
        .arg("--repo")
        .arg(repo)
        .args(["--request-id", "example-release", "release", "--slot"])
        .arg(info.slot_id.as_str())
        .arg("--lease-id")
        .arg(info.lease_id.to_string())
        .arg("--lease-fd")
        .arg(descriptor.as_raw_fd().to_string());
    // SAFETY: descriptor remains open through output and this Command is not reused.
    unsafe {
        transport::inherit_fd(&mut release, descriptor.as_fd());
    }
    let output = release.output()?;
    if !output.status.success() {
        return Err(format!(
            "release failed: {}",
            String::from_utf8_lossy(&output.stdout)
        )
        .into());
    }
    // No worktree use is allowed after release, even if its response was lost.
    drop(descriptor);
    println!("{}", String::from_utf8_lossy(&output.stdout).trim_end());
    Ok(())
}
