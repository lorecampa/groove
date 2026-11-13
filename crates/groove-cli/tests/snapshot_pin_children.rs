mod support;

use groove_core::{Pool, PoolOptions};
use serde_json::Value;
use std::{
    ffi::{CString, OsStr},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
            process::ExitStatusExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use support::{Repo, TestChild};

const READY: &str = "pin-verification-ready";

fn cli(root: &Path, args: &[&str]) -> Output {
    TestChild::spawn(support::groove_in(root).args(args))
        .wait_with_output()
        .unwrap()
}

fn response(output: &Output) -> Value {
    // from_slice also rejects a second response or trailing non-JSON output.
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    value
}

fn saved(root: &Path) -> String {
    support::assert_success(&cli(root, &["snapshot", "save"]))["result"]["snapshot"]["id"]
        .as_str()
        .unwrap()
        .into()
}

fn refs(repo: &Repo) -> String {
    repo.git(&[
        "for-each-ref",
        "--format=%(refname)%09%(objectname)",
        "refs/groove/",
    ])
}

struct GitShim {
    path: PathBuf,
    real_git: PathBuf,
    resume: File,
    _reader: File,
    active: bool,
}

impl GitShim {
    fn new(repo: &Repo) -> Self {
        let path = repo.temp.path().join("git-peer");
        fs::create_dir(&path).unwrap();
        let executable = TestChild::spawn(Command::new("sh").args(["-c", "command -v git"]))
            .wait_with_output()
            .unwrap();
        assert!(executable.status.success());
        let real_git = PathBuf::from(String::from_utf8(executable.stdout).unwrap().trim());
        let fifo = path.join("resume");
        let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is NUL-terminated and the mode is a valid permission mask.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        // Hold both endpoints so neither opening nor signalling the FIFO can
        // block, including when readiness fails before the peer opens its end.
        let reader = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap();
        let resume = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
            .unwrap();
        let script = path.join("git");
        fs::write(
            &script,
            r#"#!/bin/bash
set -euo pipefail
update_ref=false
stdin=false
for_each_ref=false
pin_inventory=false
for arg in "$@"; do
    case "$arg" in
        update-ref) update_ref=true ;;
        --stdin) stdin=true ;;
        for-each-ref) for_each_ref=true ;;
        refs/groove/snapshot-pins/*) pin_inventory=true ;;
    esac
done
if "$for_each_ref" && "$pin_inventory"; then
    if [ "${GROOVE_PIN_AFTER_PREPARE:-}" = timeout ] && [ -f "$GROOVE_PIN_PEER_DIR/prepared" ]; then
        printf 'injected post-prepare inventory timeout\n' >&2
        : > "$GROOVE_PIN_PEER_DIR/inner-entered"
        read -r -t 10 go < "$GROOVE_PIN_PEER_DIR/resume" || true
        exit 23
    fi
    case "${GROOVE_PIN_INVENTORY_FAILURE:-}" in
        exit) printf 'injected pin inventory failure\n' >&2; exit 23 ;;
        warning)
            "$GROOVE_REAL_GIT" "$@"
            printf 'injected pin inventory warning\n' >&2
            exit 0 ;;
    esac
fi
if "$update_ref" && "$stdin" && [ -n "${GROOVE_PIN_CHILD_EXE:-}" ]; then
    : > "$GROOVE_PIN_PEER_DIR/input"
    deletion=false
    while IFS= read -r line; do
        printf '%s\n' "$line" >> "$GROOVE_PIN_PEER_DIR/input"
        case "$line" in delete\ *) deletion=true ;; esac
        [ "$line" != prepare ] || break
    done
    if ! "$deletion"; then
        cat >> "$GROOVE_PIN_PEER_DIR/input"
        if [ "${GROOVE_PIN_TRANSACTION:-absence}" = deletion ]; then
            exec "$GROOVE_REAL_GIT" "$@" < "$GROOVE_PIN_PEER_DIR/input"
        fi
    fi
    printf '%s\0' "$@" > "$GROOVE_PIN_PEER_DIR/args"
    "$GROOVE_PIN_CHILD_EXE" --exact snapshot_pin_git_peer --nocapture \
        2> "$GROOVE_PIN_PEER_DIR/peer.stderr" |
        tee "$GROOVE_PIN_PEER_DIR/peer.stdout" |
        while IFS= read -r line; do
            case "$line" in
                GROOVE_PIN_PROTOCOL:*) printf '%s\n' "${line#GROOVE_PIN_PROTOCOL:}" ;;
            esac
        done
    exit
fi
exec "$GROOVE_REAL_GIT" "$@"
"#,
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            path,
            real_git,
            resume,
            _reader: reader,
            active: false,
        }
    }

    fn command(&self, root: &Path, args: &[&str]) -> Command {
        let mut command = support::groove_in(root);
        let mut paths = vec![self.path.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .args(args)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("GROOVE_REAL_GIT", &self.real_git)
            .env("GROOVE_PIN_PEER_DIR", &self.path);
        command
    }

    fn finish(&mut self, decision: &[u8]) -> io::Result<String> {
        self.resume.write_all(decision)?;
        let deadline = Instant::now() + support::PROCESS_TIMEOUT;
        loop {
            match fs::read_to_string(self.path.join("done")) {
                Ok(result) => {
                    self.active = false;
                    return Ok(if result == "success" || result == "aborted" {
                        result
                    } else {
                        // These streams outlive the killed supervisor's capture
                        // pipes, so an unexpected peer panic remains diagnosable.
                        format!(
                            "{result}; peer stdout={} peer stderr={}",
                            fs::read_to_string(self.path.join("peer.stdout")).unwrap_or_default(),
                            fs::read_to_string(self.path.join("peer.stderr")).unwrap_or_default(),
                        )
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => (),
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "waiting for pin verification peer cleanup",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for GitShim {
    fn drop(&mut self) {
        if self.active {
            // Abort rather than execute the transaction when an assertion or
            // readiness wait unwinds. The peer bounds its own waits and Git child.
            if let Err(error) = self.finish(b"abort\n") {
                eprintln!("pin verification peer cleanup: {error}");
            }
        }
    }
}

struct PeerDone {
    path: PathBuf,
    result: String,
}

impl Drop for PeerDone {
    fn drop(&mut self) {
        // Publish only after the peer's owned Git child has completed or been
        // killed/reaped on unwind. Rename keeps polling from reading half a result.
        let temporary = self.path.join("done.tmp");
        if fs::write(&temporary, &self.result).is_ok() {
            let _ = fs::rename(temporary, self.path.join("done"));
        }
    }
}

#[test]
fn snapshot_pin_git_peer() {
    let Some(path) = std::env::var_os("GROOVE_PIN_PEER_DIR").map(PathBuf::from) else {
        return;
    };
    let mut done = PeerDone {
        path: path.clone(),
        result: "peer failed before Git completed".into(),
    };
    let input = fs::read(path.join("input")).unwrap();
    if input
        .split(|byte| *byte == b'\n')
        .any(|line| line.starts_with(b"delete "))
    {
        prepared_pin_peer(&path, &input, &mut done);
        return;
    }
    let decision = peer_decision(&path);
    if decision == b"abort\n" {
        done.result = "aborted".into();
        return;
    }
    assert_eq!(decision, b"go\n");
    let mut command = real_git_command(&path);
    command.stdin(File::open(path.join("input")).unwrap());
    let output = TestChild::spawn(&mut command).wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    done.result = "success".into();
}

fn peer_decision(path: &Path) -> Vec<u8> {
    let mut resume = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path.join("resume"))
        .unwrap();
    let mut ready = UnixStream::connect(path.join("ready.sock")).unwrap();
    ready
        .set_write_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    ready.write_all(READY.as_bytes()).unwrap();
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    let mut decision = Vec::new();
    while !decision.ends_with(b"\n") {
        let mut byte = [0];
        match resume.read(&mut byte) {
            Ok(0) => panic!("resume FIFO closed before a decision"),
            Ok(_) => decision.push(byte[0]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("reading resume FIFO: {error}"),
        }
        assert!(Instant::now() < deadline, "pin peer resume deadline");
    }
    decision
}

fn real_git_command(path: &Path) -> Command {
    let arguments = fs::read(path.join("args")).unwrap();
    assert_eq!(arguments.last(), Some(&0));
    let mut command = Command::new(std::env::var_os("GROOVE_REAL_GIT").unwrap());
    command.args(
        arguments[..arguments.len() - 1]
            .split(|byte| *byte == 0)
            .map(OsStr::from_bytes),
    );
    command
}

fn prepared_pin_peer(path: &Path, prefix: &[u8], done: &mut PeerDone) {
    // The shell redirects only the real Git's streams, so TestChild still owns
    // and reaps that direct child while we observe its flushed protocol replies.
    let arguments = fs::read(path.join("args")).unwrap();
    let mut command = Command::new("bash");
    command
        .args(["-c", "exec \"$GROOVE_REAL_GIT\" \"$@\" > \"$GROOVE_PIN_PEER_DIR/git.stdout\" 2> \"$GROOVE_PIN_PEER_DIR/git.stderr\"", "git-peer"])
        .args(arguments[..arguments.len() - 1].split(|byte| *byte == 0).map(OsStr::from_bytes))
        .stdin(Stdio::piped());
    let mut git = TestChild::spawn(&mut command);
    let mut stdin = git.stdin.take().unwrap();
    stdin.write_all(prefix).unwrap();
    const PREPARED: &[u8] = b"start: ok\nprepare: ok\n";
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    loop {
        let output = fs::read(path.join("git.stdout")).unwrap_or_default();
        if output == PREPARED {
            fs::write(path.join("prepared"), b"ready").unwrap();
            break;
        }
        assert!(
            git.try_wait().unwrap().is_none(),
            "Git exited before preparing: {output:?}"
        );
        assert!(
            Instant::now() < deadline,
            "Git prepare acknowledgment deadline: {output:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let decision = peer_decision(path);
    if decision == b"abort\n" {
        drop(stdin);
        assert!(git.wait().unwrap().success());
        done.result = "aborted".into();
        return;
    }
    assert_eq!(decision, b"go\n");
    forward_protocol(PREPARED);
    // Only this isolated peer changes its stdin flags. The supervisor's pipe
    // writer is a different descriptor; EOF after its death aborts real Git.
    // SAFETY: fd 0 is the live stdin inherited from the shim's original Git child.
    let flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
    assert!(flags >= 0);
    // SAFETY: the descriptor is live and O_NONBLOCK is a valid status flag.
    let changed = unsafe { libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    assert_eq!(changed, 0);
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    let mut suffix = Vec::new();
    loop {
        let mut byte = [0];
        match io::stdin().read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                suffix.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("reading transaction decision: {error}"),
        }
        assert!(Instant::now() < deadline, "transaction commit/EOF deadline");
    }
    fs::write(path.join("suffix"), &suffix).unwrap();
    assert!(suffix.is_empty() || suffix == b"commit\n", "{suffix:?}");
    stdin.write_all(&suffix).unwrap();
    drop(stdin);
    assert!(
        git.wait().unwrap().success(),
        "{}",
        fs::read_to_string(path.join("git.stderr")).unwrap()
    );
    let output = fs::read(path.join("git.stdout")).unwrap();
    forward_protocol(&output[PREPARED.len()..]);
    done.result = "success".into();
}

fn forward_protocol(bytes: &[u8]) {
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        // A killed supervisor may close its pipe; teardown still must close the
        // real transaction's stdin and reap Git even when forwarding fails.
        let _ = io::stdout().write_all(b"GROOVE_PIN_PROTOCOL:");
        let _ = io::stdout().write_all(line);
        let _ = io::stdout().flush();
    }
}

fn can_lock_exclusively(path: &Path) -> bool {
    let file = File::open(path).unwrap();
    // SAFETY: file owns this live descriptor and LOCK_NB prevents blocking.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return true;
    }
    assert_eq!(io::Error::last_os_error().kind(), io::ErrorKind::WouldBlock);
    false
}

#[test]
fn pin_absence_child_retains_snapshot_and_managed_admission_after_supervisor_sigkill() {
    for managed in [false, true] {
        surviving_pin_child(managed, false);
    }
}

#[test]
fn prepared_deletion_child_retains_snapshot_and_managed_admission_after_supervisor_sigkill() {
    for managed in [false, true] {
        surviving_pin_child(managed, true);
    }
}

fn surviving_pin_child(managed: bool, deletion: bool) {
    let repo = Repo::new();
    let root = if managed {
        Pool::init(
            &repo.path,
            PoolOptions {
                capacity: 1,
                worktree_root: Some(support::worktree_root(&repo.path, "default")),
                ..Default::default()
            },
        )
        .unwrap();
        let lease = Pool::open(&repo.path)
            .unwrap()
            .acquire(Default::default())
            .unwrap();
        lease.info().worktree_path.clone()
    } else {
        repo.path.clone()
    };
    fs::write(root.join("tracked.txt"), "saved result\n").unwrap();
    let id = saved(&root);
    let original_refs = refs(&repo);
    let lock = repo.path.join(".git/groove-snapshots/lock");
    let identity = fs::metadata(&lock).unwrap();
    let admission = repo.state().join("locks/lifecycle.lock");
    let mut shim = GitShim::new(&repo);
    let listener = UnixListener::bind(shim.path.join("ready.sock")).unwrap();
    let mut command = shim.command(
        &root,
        &["snapshot", if deletion { "delete" } else { "show" }, &id],
    );
    command.env("GROOVE_PIN_CHILD_EXE", std::env::current_exe().unwrap());
    if deletion {
        command.env("GROOVE_PIN_TRANSACTION", "deletion");
    }
    let mut supervisor = TestChild::spawn(&mut command);
    shim.active = true;
    let _ready = supervisor
        .checkpoint(&listener, READY, support::PROCESS_TIMEOUT)
        .unwrap();
    let transaction = fs::read_to_string(shim.path.join("input")).unwrap();
    assert!(transaction.contains(&format!("verify refs/groove/snapshot-pins/{id} ")));
    if deletion {
        assert!(transaction.ends_with("prepare\n"));
        assert_eq!(
            fs::read(shim.path.join("git.stdout")).unwrap(),
            b"start: ok\nprepare: ok\n"
        );
    } else {
        assert!(transaction.ends_with("abort\n"));
    }
    supervisor.kill().unwrap();
    assert_eq!(supervisor.wait().unwrap().signal(), Some(libc::SIGKILL));
    assert!(!can_lock_exclusively(&lock));
    if managed {
        assert!(!can_lock_exclusively(&admission));
    }
    let blocked = cli(
        &repo.path,
        &["--git-timeout-ms", "250", "snapshot", "show", &id],
    );
    assert_eq!(blocked.status.code(), Some(2), "{blocked:?}");
    let error = response(&blocked);
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(error["error"]["retryable"], true);
    assert_eq!(refs(&repo), original_refs);
    assert_eq!(shim.finish(b"go\n").unwrap(), "success");
    if deletion {
        assert!(fs::read(shim.path.join("suffix")).unwrap().is_empty());
    }
    let healthy = cli(
        &root,
        &["--git-timeout-ms", "3000", "snapshot", "show", &id],
    );
    let snapshot = support::assert_success(&healthy);
    assert_eq!(snapshot["result"]["snapshot"]["id"], id);
    assert_eq!(snapshot["result"]["snapshot"]["pinned"], false);
    assert!(can_lock_exclusively(&lock));
    if managed {
        assert!(can_lock_exclusively(&admission));
    }
    let after = fs::metadata(lock).unwrap();
    assert_eq!((after.dev(), after.ino()), (identity.dev(), identity.ino()));
    assert_eq!(refs(&repo), original_refs);
    assert_eq!(
        fs::read(root.join("tracked.txt")).unwrap(),
        b"saved result\n"
    );
}

#[test]
fn prepared_deletion_forwards_real_acknowledgments_and_commits_only_after_validation() {
    let repo = Repo::new();
    let id = saved(&repo.path);
    let original_refs = refs(&repo);
    let mut shim = GitShim::new(&repo);
    let listener = UnixListener::bind(shim.path.join("ready.sock")).unwrap();
    let mut supervisor = TestChild::spawn(
        shim.command(&repo.path, &["snapshot", "delete", &id])
            .env("GROOVE_PIN_TRANSACTION", "deletion")
            .env("GROOVE_PIN_CHILD_EXE", std::env::current_exe().unwrap()),
    );
    shim.active = true;
    let _ready = supervisor
        .checkpoint(&listener, READY, support::PROCESS_TIMEOUT)
        .unwrap();
    assert_eq!(refs(&repo), original_refs);
    assert_eq!(shim.finish(b"go\n").unwrap(), "success");
    let output = supervisor.wait_with_output().unwrap();
    assert_eq!(support::assert_success(&output)["result"]["deleted"], id);
    assert_eq!(fs::read(shim.path.join("suffix")).unwrap(), b"commit\n");
    assert_eq!(
        fs::read(shim.path.join("git.stdout")).unwrap(),
        b"start: ok\nprepare: ok\ncommit: ok\n"
    );
    assert!(refs(&repo).is_empty());
}

#[test]
fn post_prepare_inventory_timeout_preserves_refs_and_bounds_supervisor_teardown() {
    let repo = Repo::new();
    let id = saved(&repo.path);
    let original_refs = refs(&repo);
    let lock = repo.path.join(".git/groove-snapshots/lock");
    let identity = fs::metadata(&lock).unwrap();
    let native_lock = repo
        .path
        .join(format!(".git/refs/groove/snapshots/{id}.lock"));
    let mut shim = GitShim::new(&repo);
    let listener = UnixListener::bind(shim.path.join("ready.sock")).unwrap();
    let mut supervisor = TestChild::spawn(
        shim.command(
            &repo.path,
            &["--git-timeout-ms", "1500", "snapshot", "delete", &id],
        )
        .env("GROOVE_PIN_TRANSACTION", "deletion")
        .env("GROOVE_PIN_AFTER_PREPARE", "timeout")
        .env("GROOVE_PIN_CHILD_EXE", std::env::current_exe().unwrap()),
    );
    shim.active = true;
    let _ready = supervisor
        .checkpoint(&listener, READY, support::PROCESS_TIMEOUT)
        .unwrap();
    assert!(
        native_lock.exists(),
        "real Git did not retain its prepared ref lock"
    );
    shim.resume.write_all(b"go\n").unwrap();
    supervisor.wait_timeout(Duration::from_secs(5)).unwrap();
    let output = supervisor.wait_with_output().unwrap();
    // The runner has now completed and reaped its transaction process group.
    // A spent hard deadline can kill Git before EOF abort removes its ref lock;
    // do not wait for a completion marker from a killed peer or sweep its locks.
    shim.active = false;
    assert_eq!(output.status.code(), Some(7), "{output:?}");
    let error = response(&output);
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "GIT_ERROR");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("injected post-prepare inventory timeout")
    );
    assert!(shim.path.join("inner-entered").exists());
    assert!(fs::read(shim.path.join("suffix"))
        .unwrap_or_default()
        .is_empty());
    assert!(!fs::read_to_string(shim.path.join("git.stdout"))
        .unwrap()
        .contains("commit: ok"));
    assert_eq!(refs(&repo), original_refs);
    assert!(can_lock_exclusively(&lock));
    let after = fs::metadata(lock).unwrap();
    assert_eq!((identity.dev(), identity.ino()), (after.dev(), after.ino()));
}

#[test]
fn peer_failure_and_parent_unwind_preserve_refs_and_allow_a_healthy_retry() {
    for unwind in [false, true] {
        let repo = Repo::new();
        let id = saved(&repo.path);
        let original_refs = refs(&repo);
        let mut shim = GitShim::new(&repo);
        let done = shim.path.join("done");
        let listener = UnixListener::bind(shim.path.join("ready.sock")).unwrap();
        let mut supervisor = TestChild::spawn(
            shim.command(&repo.path, &["snapshot", "show", &id])
                .env("GROOVE_PIN_CHILD_EXE", std::env::current_exe().unwrap()),
        );
        shim.active = true;
        let _ready = supervisor
            .checkpoint(&listener, READY, support::PROCESS_TIMEOUT)
            .unwrap();
        supervisor.kill().unwrap();
        assert_eq!(supervisor.wait().unwrap().signal(), Some(libc::SIGKILL));
        if unwind {
            let panic = std::panic::catch_unwind(move || {
                let _peer = shim;
                panic!("simulate failed assertion with a surviving pin peer");
            });
            assert_eq!(
                panic.unwrap_err().downcast_ref::<&str>(),
                Some(&"simulate failed assertion with a surviving pin peer")
            );
            assert_eq!(fs::read_to_string(done).unwrap(), "aborted");
        } else {
            let diagnostic = shim.finish(b"invalid\n").unwrap();
            assert!(diagnostic.contains("peer failed before Git completed"));
            assert!(diagnostic.contains("snapshot_pin_git_peer"));
            assert!(diagnostic.contains("assertion `left == right` failed"));
        }
        let healthy = cli(
            &repo.path,
            &["--git-timeout-ms", "3000", "snapshot", "show", &id],
        );
        assert_eq!(
            support::assert_success(&healthy)["result"]["snapshot"]["pinned"],
            false
        );
        assert_eq!(refs(&repo), original_refs);
        assert!(can_lock_exclusively(
            &repo.path.join(".git/groove-snapshots/lock")
        ));
    }
}

#[test]
fn failed_or_warning_pin_inventory_returns_one_git_error_and_preserves_refs() {
    let repo = Repo::new();
    let id = saved(&repo.path);
    let reference = format!("refs/groove/snapshots/{id}");
    let pin = format!("refs/groove/snapshot-pins/{id}");
    let oid = repo.git(&["rev-parse", &reference]);
    repo.git(&["update-ref", &pin, &oid]);
    let original_refs = refs(&repo);
    let shim = GitShim::new(&repo);
    for failure in ["exit", "warning"] {
        for args in [
            vec!["snapshot", "show", &id],
            vec!["snapshot", "list"],
            vec!["snapshot", "delete", &id, "--force"],
        ] {
            let output = TestChild::spawn(
                shim.command(&repo.path, &args)
                    .env("GROOVE_PIN_INVENTORY_FAILURE", failure),
            )
            .wait_with_output()
            .unwrap();
            assert_eq!(
                output.status.code(),
                Some(7),
                "{failure}/{args:?}: {output:?}"
            );
            let error = response(&output);
            assert_eq!(error["ok"], false);
            assert_eq!(error["error"]["code"], "GIT_ERROR");
            assert!(String::from_utf8_lossy(&output.stdout).contains("injected pin inventory"));
            assert_eq!(refs(&repo), original_refs);
        }
    }
    let healthy = cli(&repo.path, &["snapshot", "show", &id]);
    assert_eq!(
        support::assert_success(&healthy)["result"]["snapshot"]["pinned"],
        true
    );
}
