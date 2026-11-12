mod support;

use serde_json::Value;
use std::{
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::PathBuf,
    process::{Command, Output},
    time::{Duration, Instant},
};
use support::{Repo, TestChild};

fn cli(repo: &Repo, args: &[&str]) -> Output {
    TestChild::spawn(support::groove_in(&repo.path).args(args))
        .wait_with_output()
        .unwrap()
}

struct ResumePeer {
    writer: File,
    _reader: File,
    done: PathBuf,
    active: bool,
}

impl Drop for ResumePeer {
    fn drop(&mut self) {
        if self.active {
            // TestChild kills/reaps the supervisor before this guard unwinds.
            // Release a surviving shim too; no descendant Git writer is spawned
            // by any injected failure mode.
            let _ = self.writer.write_all(b"abort\n");
            let deadline = Instant::now() + support::PROCESS_TIMEOUT;
            while !self.done.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

fn rejected_transaction(mode: &str) {
    let repo = Repo::new();
    fs::write(repo.path.join("tracked.txt"), "saved transaction bytes\n").unwrap();
    let saved = cli(&repo, &["snapshot", "save"]);
    let id = support::assert_success(&saved)["result"]["snapshot"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let snapshot = format!("refs/groove/snapshots/{id}");
    let pin = format!("refs/groove/snapshot-pins/{id}");
    let oid = repo.git(&["rev-parse", &snapshot]);
    repo.git(&["update-ref", &pin, &oid]);
    let refs = repo.git(&[
        "for-each-ref",
        "--format=%(refname)%09%(objectname)",
        "refs/groove/",
    ]);
    let lock = repo.path.join(".git/groove-snapshots/lock");
    let identity = fs::metadata(&lock).unwrap();
    let bin = repo.temp.path().join("protocol-peer");
    fs::create_dir(&bin).unwrap();
    let fifo = bin.join("resume");
    let name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: name is NUL-terminated and 0600 is a valid permission mask.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    let reader = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let writer = OpenOptions::new()
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    let mut peer = ResumePeer {
        writer,
        _reader: reader,
        done: bin.join("done"),
        active: true,
    };
    let real_git = TestChild::spawn(Command::new("sh").args(["-c", "command -v git"]))
        .wait_with_output()
        .unwrap();
    assert!(real_git.status.success());
    let script = bin.join("git");
    fs::write(
        &script,
        r#"#!/bin/bash
set -eu
update=false
stdin=false
for arg in "$@"; do
    case "$arg" in update-ref) update=true ;; --stdin) stdin=true ;; esac
done
if "$update" && "$stdin"; then
    : > "$GROOVE_PROTOCOL_DIR/prefix"
    deletion=false
    while IFS= read -r line; do
        printf '%s\n' "$line" >> "$GROOVE_PROTOCOL_DIR/prefix"
        case "$line" in delete\ *) deletion=true ;; esac
        [ "$line" != prepare ] || break
    done
    if "$deletion"; then
        trap ': > "$GROOVE_PROTOCOL_DIR/done"' EXIT
        : > "$GROOVE_PROTOCOL_DIR/entered"
        case "$GROOVE_PROTOCOL_MODE" in
            early-exit) exit 0 ;;
            malformed)
                printf 'start: ok\nprepare: invalid\n'
                while IFS= read -r line; do
                    printf '%s\n' "$line" >> "$GROOVE_PROTOCOL_DIR/suffix"
                done
                exit 0 ;;
            timeout)
                read -r -t 10 go < "$GROOVE_PROTOCOL_DIR/resume" || true
                exit 0 ;;
        esac
        exit 24
    fi
    # Absence verification has static prepare/abort input; relay it unchanged.
    cat >> "$GROOVE_PROTOCOL_DIR/prefix"
    exec "$GROOVE_REAL_GIT" "$@" < "$GROOVE_PROTOCOL_DIR/prefix"
fi
exec "$GROOVE_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![bin.clone()];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let mut command = support::groove_in(&repo.path);
    command
        .args([
            "--git-timeout-ms",
            "1500",
            "snapshot",
            "delete",
            &id,
            "--force",
        ])
        .env("PATH", std::env::join_paths(paths).unwrap())
        .env(
            "GROOVE_REAL_GIT",
            String::from_utf8(real_git.stdout).unwrap().trim(),
        )
        .env("GROOVE_PROTOCOL_DIR", &bin)
        .env("GROOVE_PROTOCOL_MODE", mode);
    let start = Instant::now();
    let output = TestChild::spawn(&mut command).wait_with_output().unwrap();
    peer.active = false;
    assert!(start.elapsed() < support::PROCESS_TIMEOUT);
    assert!(
        bin.join("entered").exists(),
        "injected peer was not reached: {output:?}"
    );
    assert_eq!(output.status.code(), Some(7), "{mode}: {output:?}");
    // from_slice rejects extra responses and trailing human output.
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&error)
        .unwrap();
    assert_eq!(error["ok"], false);
    assert_eq!(error["error"]["code"], "GIT_ERROR");
    assert!(!fs::read_to_string(bin.join("suffix"))
        .unwrap_or_default()
        .contains("commit"));
    assert_eq!(
        repo.git(&[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/groove/"
        ]),
        refs
    );
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"saved transaction bytes\n"
    );
    assert_eq!(
        repo.git(&["show", &format!("{oid}:work/tracked.txt")]),
        "saved transaction bytes"
    );
    let after = fs::metadata(&lock).unwrap();
    assert_eq!((identity.dev(), identity.ino()), (after.dev(), after.ino()));
    let healthy = cli(&repo, &["snapshot", "delete", &id, "--force"]);
    assert_eq!(support::assert_success(&healthy)["result"]["deleted"], id);
    assert!(repo
        .git(&["for-each-ref", "--format=%(refname)", &snapshot, &pin])
        .is_empty());
    let after = fs::metadata(lock).unwrap();
    assert_eq!((identity.dev(), identity.ino()), (after.dev(), after.ino()));
}

#[test]
fn successful_exit_without_preparation_acknowledgment_preserves_the_snapshot() {
    rejected_transaction("early-exit");
}

#[test]
fn malformed_preparation_acknowledgment_never_authorizes_commit() {
    rejected_transaction("malformed");
}

#[test]
fn unresponsive_transaction_has_a_deadline_and_allows_a_healthy_retry() {
    rejected_transaction("timeout");
}
