mod support;
use groove_core::{AcquireOptions, Pool, PoolOptions};
use std::{
    ffi::CString,
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::PermissionsExt},
    },
    process::{Command, Stdio},
    time::Duration,
};
use support::Repo;

fn real_git() -> String {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn fifo(path: &std::path::Path) {
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: path is NUL-terminated and mode is a valid permission mask.
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
}
#[test]
fn oversized_git_failure_keeps_the_json_error_envelope_small() {
    let repo = Repo::new();
    let bin = repo.temp.path().join("failing-git");
    fs::create_dir(&bin).unwrap();
    let shim = bin.join("git");
    fs::write(&shim, "#!/bin/bash\nprintf 'stdout failure %1000000s' x\nprintf 'stderr failure %1000000s' y >&2\nexit 33\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
    let output = support::groove_in(&repo.path)
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .arg("list")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.len() < 100 * 1024, "{}", output.stdout.len());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    assert_eq!(value["error"]["code"], "GIT_ERROR");
    let git = &value["error"]["details"]["git"];
    assert_eq!(git["exit_code"], 33);
    assert_eq!(git["output_truncated"], true);
    for stream in ["stdout", "stderr"] {
        let text = git[stream].as_str().unwrap();
        assert!(text.starts_with(&format!("{stream} failure")));
        assert!(text.len() <= 16 * 1024);
    }
}

#[test]
fn killed_groove_leaves_mutating_git_child_holding_both_locks() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let sibling_name = "sibling".parse().unwrap();
    Pool::init_named(
        &r.path,
        &sibling_name,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&r.path, sibling_name.as_str())),
            ..Default::default()
        },
    )
    .unwrap();
    let sibling = Pool::open_named(&r.path, &sibling_name).unwrap();
    let bin = r.temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let ready = r.temp.path().join("ready.fifo");
    let resume = r.temp.path().join("resume.fifo");
    fifo(&ready);
    fifo(&resume);
    let script = bin.join("git");
    // The shim is a deterministic barrier immediately before a real mutating Git
    // exec. It inherits the same descriptors as Git and stays alive after SIGKILL
    // of Groove, then execs the installed Git to finish the registration mutation.
    fs::write(&script, "#!/bin/sh\ncase \" $* \" in\n*\" worktree add \"*) printf ready > \"$GROOVE_READY_FIFO\"; read go < \"$GROOVE_RESUME_FIFO\" ;;\nesac\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = support::groove_in(&r.path);
    command
        .arg("--repo")
        .arg(&r.path)
        .args(["acquire", "--base", "HEAD"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("GROOVE_REAL_GIT", real_git())
        .env("GROOVE_READY_FIFO", &ready)
        .env("GROOVE_RESUME_FIFO", &resume)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut child = support::TestChild::spawn(&mut command);
    let mut ready_file = fs::File::open(&ready).unwrap();
    let mut marker = [0; 5];
    ready_file.read_exact(&mut marker).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(
        pool.acquire(Default::default()).err().unwrap().code,
        groove_core::ErrorCode::PoolExhausted
    );
    let admin = fs::File::open(r.state().join("locks/admin.lock")).unwrap();
    // SAFETY: admin is an open descriptor; the operation is nonblocking.
    let locked = unsafe { libc::flock(admin.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(locked, -1);
    let deletion = Pool::destroy(
        &r.path,
        groove_core::DestroyOptions {
            discard_changes: true,
            git_timeout: Duration::from_millis(100),
            ..Default::default()
        },
    );
    let sibling_blocked = sibling.acquire(AcquireOptions {
        git_timeout: Duration::from_millis(100),
        ..Default::default()
    });
    fs::OpenOptions::new()
        .write(true)
        .open(&resume)
        .unwrap()
        .write_all(b"go\n")
        .unwrap();
    let _next = pool
        .acquire(AcquireOptions {
            wait: Duration::from_secs(10),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(deletion.unwrap_err().code, groove_core::ErrorCode::IoError);
    assert_eq!(
        sibling_blocked.unwrap_err().code,
        groove_core::ErrorCode::PoolUnhealthy
    );
    let other = sibling.acquire(Default::default()).unwrap();
    assert_ne!(other.info().worktree_path, _next.info().worktree_path);
    assert_eq!(
        fs::read_to_string(r.path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn git_timeout_terminates_the_mutating_process_group_before_recovery() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let bin = r.temp.path().join("timeout-bin");
    fs::create_dir(&bin).unwrap();
    let ready = r.temp.path().join("timeout-ready");
    let resume = r.temp.path().join("never-resume");
    fifo(&ready);
    fifo(&resume);
    let script = bin.join("git");
    fs::write(&script,"#!/bin/sh\ncase \" $* \" in\n*\" worktree add \"*) printf ready > \"$GROOVE_READY_FIFO\"; read go < \"$GROOVE_RESUME_FIFO\" ;;\nesac\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = support::groove_in(&r.path);
    command
        .arg("--repo")
        .arg(&r.path)
        .args(["--git-timeout-ms", "500", "acquire", "--base", "HEAD"])
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("GROOVE_REAL_GIT", real_git())
        .env("GROOVE_READY_FIFO", &ready)
        .env("GROOVE_RESUME_FIFO", &resume)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = support::TestChild::spawn(&mut command);
    let mut bytes = [0; 5];
    fs::File::open(&ready)
        .unwrap()
        .read_exact(&mut bytes)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(6));
    assert!(pool.acquire(Default::default()).is_ok());
}

#[test]
fn checkout_child_retains_slot_without_serializing_another_registration() {
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 2,
            worktree_root: Some(support::worktree_root(&r.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let bin = r.temp.path().join("checkout-bin");
    fs::create_dir(&bin).unwrap();
    let ready = r.temp.path().join("checkout-ready");
    let resume = r.temp.path().join("checkout-resume");
    fifo(&ready);
    fifo(&resume);
    let script = bin.join("git");
    fs::write(&script, "#!/bin/sh\ncase \" $* \" in\n*\" checkout \"*) printf ready > \"$GROOVE_READY_FIFO\"; read go < \"$GROOVE_RESUME_FIFO\" ;;\nesac\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    let mut command = support::groove_in(&r.path);
    command
        .arg("--repo")
        .arg(&r.path)
        .arg("acquire")
        .env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        )
        .env("GROOVE_REAL_GIT", real_git())
        .env("GROOVE_READY_FIFO", &ready)
        .env("GROOVE_RESUME_FIFO", &resume)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = support::TestChild::spawn(&mut command);
    let mut marker = [0; 5];
    fs::File::open(&ready)
        .unwrap()
        .read_exact(&mut marker)
        .unwrap();
    let second = pool.acquire(AcquireOptions {
        git_timeout: Duration::from_secs(2),
        ..Default::default()
    });
    child.kill().unwrap();
    child.wait().unwrap();
    // Resume the inherited-descriptor writer before assertions, including on a
    // regression where the other registration could not obtain the admin lock.
    fs::OpenOptions::new()
        .write(true)
        .open(&resume)
        .unwrap()
        .write_all(b"go\n")
        .unwrap();
    let _second = second.unwrap();
    let _recovered = pool
        .acquire(AcquireOptions {
            wait: Duration::from_secs(10),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(fs::read(r.path.join("tracked.txt")).unwrap(), b"baseline\n");
}
