mod support;

use groove_core::{ErrorCode, Pool, PoolOptions};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use support::{Repo, TestChild};

// These helper processes deliberately inherit the Git runner's pipes. TestChild
// captures into files, so it is used for Groove, not for these pipe-owning peers.
#[test]
fn git_pipe_peer() {
    let Some(root) = std::env::var_os("GROOVE_PIPE_PEER_ROOT").map(PathBuf::from) else {
        return;
    };
    let mut command = Command::new("bash");
    command
        .args([
            "-c",
            "\"$GROOVE_PIPE_EXE\" --exact git_pipe_grandchild --nocapture & wait",
        ])
        .stdin(Stdio::null());
    if std::env::var("GROOVE_PIPE_STREAM").unwrap() == "stdout" {
        command.stdout(Stdio::inherit()).stderr(Stdio::null());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::inherit());
    }
    let mut intermediate = command.spawn().unwrap();
    // Connect after spawning, so descendants cannot retain this control stream.
    let mut leader = UnixStream::connect(root.join("leader.sock")).unwrap();
    leader
        .set_read_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    leader
        .set_write_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    leader.write_all(b"leader-ready").unwrap();
    leader.write_all(&std::process::id().to_be_bytes()).unwrap();
    leader.write_all(&intermediate.id().to_be_bytes()).unwrap();
    let mut decision = [0];
    match leader.read(&mut decision) {
        Ok(1) => {
            assert_eq!(decision, *b"E");
            // Exit with live descendants: Groove must stop them before joining
            // its readers. Dropping std::process::Child does not kill the child.
            drop(intermediate);
        }
        // On parent failure, its grandchild socket also closes. The grandchild
        // exits without writing; the intermediate shell then reaps it and exits.
        _ => {
            let _ = intermediate.wait();
        }
    }
}

#[test]
fn git_pipe_grandchild() {
    let Some(root) = std::env::var_os("GROOVE_PIPE_PEER_ROOT").map(PathBuf::from) else {
        return;
    };
    let fd = if std::env::var("GROOVE_PIPE_STREAM").unwrap() == "stdout" {
        libc::STDOUT_FILENO
    } else {
        libc::STDERR_FILENO
    };
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fd is an inherited standard descriptor; metadata is writable stat storage.
    assert_eq!(unsafe { libc::fstat(fd, metadata.as_mut_ptr()) }, 0);
    // SAFETY: successful fstat initialized metadata above.
    let metadata = unsafe { metadata.assume_init() };
    assert_eq!(metadata.st_mode & libc::S_IFMT, libc::S_IFIFO);
    let mut holder = UnixStream::connect(root.join("holder.sock")).unwrap();
    holder
        .set_read_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    holder
        .set_write_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    holder.write_all(b"grandchild-ready").unwrap();
    // SAFETY: getppid/getpgrp only observe this process's parent and group identities.
    let (parent, group) = unsafe { (libc::getppid(), libc::getpgrp()) };
    holder.write_all(&(parent as u32).to_be_bytes()).unwrap();
    holder.write_all(&(group as u32).to_be_bytes()).unwrap();
    let mut decision = [0];
    if holder.read(&mut decision).unwrap() == 1 {
        assert_eq!(decision, *b"W");
        fs::write("late-from-git-grandchild", "stale writer").unwrap();
        holder.write_all(b"written").unwrap();
    }
}

fn number(stream: &mut UnixStream) -> u32 {
    let mut bytes = [0; 4];
    stream.read_exact(&mut bytes).unwrap();
    u32::from_be_bytes(bytes)
}

fn await_group_exit(group: u32) {
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    loop {
        let mut status = 0;
        // SAFETY: only children adopted into this isolated test process from the
        // fixture's group are reaped; WNOHANG prevents an unbounded wait.
        let reaped = unsafe { libc::waitpid(-(group as i32), &mut status, libc::WNOHANG) };
        if reaped > 0 {
            assert!(libc::WIFSIGNALED(status));
            assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
            continue;
        }
        if reaped < 0 {
            assert_eq!(
                io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
        // SAFETY: signal zero only observes the fixture's process group; no PID
        // observation here authorizes sending a signal or releasing a reservation.
        if unsafe { libc::kill(-(group as i32), 0) } < 0
            && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return;
        }
        assert!(Instant::now() < deadline, "Git peer group did not stop");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn check_pipe_descendants(stream: &str) {
    // Linux adopts orphaned fixture descendants here so the test can reap them;
    // macOS delegates adoption to its system reaper. This runs in an isolated process.
    #[cfg(target_os = "linux")]
    {
        // SAFETY: this isolated fixture process adopts its own orphaned children.
        assert_eq!(
            // SAFETY: this isolated fixture process adopts its own orphaned children.
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
    }
    for leader_exits in [true, false] {
        let repo = Repo::new();
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(support::worktree_root(&repo.path, "default")),
                ..Default::default()
            },
        )
        .unwrap();
        let pool = Pool::open(&repo.path).unwrap();
        let root = repo.temp.path().join("peer");
        fs::create_dir(&root).unwrap();
        let leader_listener = UnixListener::bind(root.join("leader.sock")).unwrap();
        let holder_listener = UnixListener::bind(root.join("holder.sock")).unwrap();
        let real_git = TestChild::spawn(Command::new("sh").args(["-c", "command -v git"]))
            .wait_with_output()
            .unwrap();
        assert!(real_git.status.success());
        let real_git = String::from_utf8(real_git.stdout).unwrap();
        let script = root.join("git");
        fs::write(
            &script,
            r#"#!/bin/bash
case " $* " in
    *" checkout "*)
        "$GROOVE_REAL_GIT" "$@" || exit "$?"
        exec "$GROOVE_PIPE_EXE" --exact git_pipe_peer --nocapture ;;
esac
exec "$GROOVE_REAL_GIT" "$@"
"#,
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        let mut paths = vec![root.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        let mut child = TestChild::spawn(
            support::groove_in(&repo.path)
                .args(["--git-timeout", "1500ms", "acquire"])
                .env("PATH", std::env::join_paths(paths).unwrap())
                .env("GROOVE_REAL_GIT", real_git.trim())
                .env("GROOVE_PIPE_EXE", std::env::current_exe().unwrap())
                .env("GROOVE_PIPE_PEER_ROOT", &root)
                .env("GROOVE_PIPE_STREAM", stream),
        );
        let mut leader = child
            .checkpoint(&leader_listener, "leader-ready", support::PROCESS_TIMEOUT)
            .unwrap();
        let group = number(&mut leader);
        let intermediate = number(&mut leader);
        let mut holder = child
            .checkpoint(
                &holder_listener,
                "grandchild-ready",
                support::PROCESS_TIMEOUT,
            )
            .unwrap();
        assert_eq!(
            number(&mut holder),
            intermediate,
            "must be a real grandchild"
        );
        assert_eq!(
            number(&mut holder),
            group,
            "grandchild must retain the Git group"
        );
        assert_eq!(
            pool.acquire(Default::default()).unwrap_err().code,
            ErrorCode::PoolExhausted
        );
        if leader_exits {
            leader.write_all(b"E").unwrap();
        }
        // This bounds completion INCLUDING stdout/stderr readers, even when the
        // leader has already exited. A surviving pipe writer would stall the CLI.
        child.wait_timeout(Duration::from_secs(5)).unwrap();
        let output = child.wait_with_output().unwrap();
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
        jsonschema::validator_for(&schema)
            .unwrap()
            .validate(&response)
            .unwrap();
        assert_eq!(leader.read(&mut [0]).unwrap(), 0);
        assert_eq!(
            holder.read(&mut [0]).unwrap(),
            0,
            "grandchild must stop before reuse"
        );
        await_group_exit(group);
        if leader_exits {
            assert!(output.status.success(), "{output:?}");
            assert_eq!(response["ok"], true);
            let id = response["result"]["reservation_id"]
                .as_str()
                .unwrap()
                .parse()
                .unwrap();
            assert_eq!(pool.protected_leases().unwrap()[0].lease_id, id);
            pool.release_lease(id).unwrap();
        } else {
            assert_eq!(output.status.code(), Some(6), "{output:?}");
            assert_eq!(response["ok"], false);
            assert_eq!(response["error"]["code"], "POOL_UNHEALTHY");
            assert!(pool.protected_leases().unwrap().is_empty());
        }
        let next = pool.reserve(Default::default()).unwrap();
        // Exercise the late-write trigger after capacity has been handed out again.
        // The stopped grandchild's socket must reject it; no time-based guess is needed.
        assert!(holder.write_all(b"W").is_err());
        assert!(!next.worktree_path.join("late-from-git-grandchild").exists());
        assert_eq!(
            fs::read(next.worktree_path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        pool.release_lease(next.lease_id).unwrap();
    }
}

#[test]
fn exited_and_timed_out_git_leaders_stop_grandchildren_retaining_stdout_before_reuse() {
    if support::in_subprocess(
        "exited_and_timed_out_git_leaders_stop_grandchildren_retaining_stdout_before_reuse",
    ) {
        check_pipe_descendants("stdout");
    }
}

#[test]
fn exited_and_timed_out_git_leaders_stop_grandchildren_retaining_stderr_before_reuse() {
    if support::in_subprocess(
        "exited_and_timed_out_git_leaders_stop_grandchildren_retaining_stderr_before_reuse",
    ) {
        check_pipe_descendants("stderr");
    }
}
