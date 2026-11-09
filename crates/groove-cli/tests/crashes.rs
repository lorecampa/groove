#![cfg(feature = "test-hooks")]
mod support;
use groove_core::{transport, Pool, PoolOptions};
use std::{
    io::Read,
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::MetadataExt, net::UnixListener},
    },
    process::{Command, Stdio},
};
use support::Repo;

fn checkpoint_command(r: &Repo, name: &str, args: &[&str]) -> (Command, UnixListener) {
    let socket = r.temp.path().join("checkpoint.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_groove"));
    command
        .arg("--repo")
        .arg(&r.path)
        .args(args)
        .env("GROOVE_TEST_CHECKPOINT", name)
        .env("GROOVE_TEST_SOCKET", socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    (command, listener)
}
fn kill_at_checkpoint(command: &mut Command, listener: UnixListener, name: &str) {
    let mut child = support::TestChild::spawn(command);
    let (mut stream, _) = listener.accept().unwrap();
    let mut bytes = vec![0; name.len()];
    stream.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, name.as_bytes());
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
    drop(stream);
}
#[test]
fn initialization_crash_boundaries_resume_without_replacing_locks() {
    for name in [
        "metadata-synced",
        "metadata-renamed",
        "before-ready-manifest",
    ] {
        let r = Repo::new();
        let (mut command, listener) = checkpoint_command(&r, name, &["init", "--capacity", "1"]);
        kill_at_checkpoint(&mut command, listener, name);
        let lock = r.state().join("locks/wt-0001.lock");
        let old = std::fs::metadata(&lock).ok().map(|m| m.ino());
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        if let Some(old) = old {
            assert_eq!(std::fs::metadata(&lock).unwrap().ino(), old);
        }
        assert!(Pool::open(&r.path)
            .unwrap()
            .acquire(Default::default())
            .is_ok());
    }
}
#[test]
fn acquisition_crash_boundaries_never_hand_out_an_unverified_worktree() {
    for name in [
        "before-normalization",
        "old-worktree-removed",
        "worktree-created",
        "verified",
        "metadata-synced",
        "metadata-renamed",
        "lease-recorded",
    ] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        if name == "before-normalization" {
            drop(pool.acquire(Default::default()).unwrap());
        }
        let ino = std::fs::metadata(r.state().join("locks/wt-0001.lock"))
            .unwrap()
            .ino();
        let (receiver, sender) = transport::channel().unwrap();
        let (mut command, listener) = checkpoint_command(
            &r,
            name,
            &["acquire", "--base", "HEAD", "--lease-channel-fd"],
        );
        command.arg(sender.as_raw_fd().to_string());
        // SAFETY: sender remains live through spawning; command is used once.
        unsafe {
            transport::inherit_fd(&mut command, sender.as_fd());
        }
        kill_at_checkpoint(&mut command, listener, name);
        drop(sender);
        drop(receiver);
        let lease = pool.acquire(Default::default()).unwrap();
        assert_eq!(
            std::fs::read_to_string(lease.info().worktree_path.join("tracked.txt")).unwrap(),
            "baseline\n"
        );
        assert_eq!(
            std::fs::metadata(r.state().join("locks/wt-0001.lock"))
                .unwrap()
                .ino(),
            ino
        );
    }
}
#[test]
fn release_response_loss_never_unlocks_successor() {
    for name in ["release-recorded", "release-unlocked"] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let (info, fd) = pool
            .acquire(Default::default())
            .unwrap()
            .into_descriptor()
            .unwrap();
        let (mut command, listener) = checkpoint_command(&r, name, &["release", "--slot"]);
        command
            .arg(info.slot_id.as_str())
            .arg("--lease-id")
            .arg(info.lease_id.to_string())
            .arg("--lease-fd")
            .arg(fd.as_raw_fd().to_string());
        // SAFETY: fd remains live through spawning; command is used once.
        unsafe {
            transport::inherit_fd(&mut command, fd.as_fd());
        }
        kill_at_checkpoint(&mut command, listener, name);
        if name == "release-recorded" {
            assert!(pool.acquire(Default::default()).is_err());
        }
        drop(fd);
        let _successor = pool.acquire(Default::default()).unwrap();
        let independently_opened =
            std::fs::File::open(r.state().join("locks/wt-0001.lock")).unwrap();
        assert!(pool
            .release_fd(&info.slot_id, info.lease_id, independently_opened.as_fd())
            .is_err());
        assert!(pool.acquire(Default::default()).is_err());
    }
}

#[test]
fn release_retry_finishes_same_generation_after_crash() {
    for name in ["metadata-renamed", "release-recorded", "release-unlocked"] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.acquire(Default::default()).unwrap();
        lease.protect_from_reuse().unwrap();
        let (info, fd) = lease.into_descriptor().unwrap();
        let (mut command, listener) = checkpoint_command(&r, name, &["release", "--slot"]);
        command
            .arg(info.slot_id.as_str())
            .arg("--lease-id")
            .arg(info.lease_id.to_string())
            .arg("--lease-fd")
            .arg(fd.as_raw_fd().to_string());
        // SAFETY: fd remains open through spawning; command is used once.
        unsafe {
            transport::inherit_fd(&mut command, fd.as_fd());
        }
        kill_at_checkpoint(&mut command, listener, name);
        if name != "release-unlocked" {
            assert_eq!(
                pool.acquire(Default::default()).err().unwrap().code,
                groove_core::ErrorCode::PoolExhausted
            );
            let independent = std::fs::File::open(r.state().join("locks/wt-0001.lock")).unwrap();
            assert_eq!(
                pool.release_fd(&info.slot_id, info.lease_id, independent.as_fd())
                    .unwrap_err()
                    .code,
                groove_core::ErrorCode::InvalidLease
            );
        }
        assert!(
            pool.release_fd(&info.slot_id, info.lease_id, fd.as_fd())
                .unwrap()
                .released
        );
        let successor = pool.acquire(Default::default()).unwrap();
        assert_ne!(successor.info().lease_id, info.lease_id);
        assert_eq!(
            pool.release_fd(&info.slot_id, info.lease_id, fd.as_fd())
                .unwrap_err()
                .code,
            groove_core::ErrorCode::LeaseMismatch
        );
        assert_eq!(
            pool.acquire(Default::default()).err().unwrap().code,
            groove_core::ErrorCode::PoolExhausted
        );
    }
}

#[test]
fn durable_shell_reservations_survive_sigkill_and_release_retries_are_generation_safe() {
    for checkpoint in ["reuse-protected", "release-recorded", "release-unlocked"] {
        let r = Repo::new();
        Pool::init(
            &r.path,
            PoolOptions {
                capacity: 1,
                ..Default::default()
            },
        )
        .unwrap();
        if checkpoint == "reuse-protected" {
            let (mut command, listener) = checkpoint_command(&r, checkpoint, &["acquire"]);
            kill_at_checkpoint(&mut command, listener, checkpoint);
        } else {
            let output = Command::new(env!("CARGO_BIN_EXE_groove"))
                .arg("--repo")
                .arg(&r.path)
                .arg("acquire")
                .output()
                .unwrap();
            support::assert_success(&output);
        }
        let pool = Pool::open(&r.path).unwrap();
        let lease = pool.protected_leases().unwrap().remove(0);
        if checkpoint != "reuse-protected" {
            let (mut command, listener) = checkpoint_command(
                &r,
                checkpoint,
                &[
                    "release",
                    "--slot",
                    lease.slot_id.as_str(),
                    "--lease-id",
                    &lease.lease_id.to_string(),
                    "--writers-stopped",
                ],
            );
            kill_at_checkpoint(&mut command, listener, checkpoint);
        } else {
            assert!(pool.acquire(Default::default()).is_err());
        }
        // Use independent CLI processes throughout. Taking locks in this
        // multithreaded harness can transiently lend them to another test's fork
        // before CLOEXEC runs; that is not the ordinary one-shot CLI lifecycle.
        let release = |slot: &groove_core::SlotId, id: groove_core::LeaseId| {
            Command::new(env!("CARGO_BIN_EXE_groove"))
                .arg("--repo")
                .arg(&r.path)
                .args([
                    "release",
                    "--slot",
                    slot.as_str(),
                    "--lease-id",
                    &id.to_string(),
                    "--writers-stopped",
                ])
                .output()
                .unwrap()
        };
        support::assert_success(&release(&lease.slot_id, lease.lease_id));
        support::assert_success(&release(&lease.slot_id, lease.lease_id));
        let acquired = Command::new(env!("CARGO_BIN_EXE_groove"))
            .arg("--repo")
            .arg(&r.path)
            .arg("acquire")
            .output()
            .unwrap();
        support::assert_success(&acquired);
        let next = pool.protected_leases().unwrap().remove(0);
        assert!(!release(&lease.slot_id, lease.lease_id).status.success());
        support::assert_success(&release(&next.slot_id, next.lease_id));
    }
}
