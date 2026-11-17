mod support;

use serde_json::Value;
use std::{fs, path::Path, process::Output};
use support::{Repo, TestChild};

const CAPTURE_PHASES: &[&str] = &[
    "validation",
    "special_file_scan",
    "scratch_index_tree",
    "staging",
    "staged_sizes",
    "work_tree",
    "stability",
    "manifest",
    "capture",
];

fn invoke(path: &Path, args: &[&str], verbose: bool) -> (Output, Value, Vec<Value>) {
    invoke_command(support::groove_in(path), args, verbose)
}

fn invoke_command(
    mut command: std::process::Command,
    args: &[&str],
    verbose: bool,
) -> (Output, Value, Vec<Value>) {
    let logs = tempfile::tempdir().unwrap();
    // Let the logger create its private directory with mode 0700, independently
    // of the test runner's umask.
    let directory = logs.path().join("logs");
    command
        .env("GROOVE_LOG_ENABLED", "true")
        .env("GROOVE_LOG_DIR", &directory);
    if verbose {
        command.arg("-vv");
    }
    let output = TestChild::spawn(command.args(args))
        .wait_with_output()
        .unwrap();
    // from_slice rejects a second response or diagnostic text on stdout.
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["api_version"], 2);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    let mut timings = Vec::new();
    for entry in fs::read_dir(&directory).unwrap() {
        let path = entry.unwrap().path();
        if path
            .extension()
            .is_some_and(|extension| extension == "jsonl")
        {
            for line in fs::read_to_string(path).unwrap().lines() {
                let event: Value = serde_json::from_str(line).unwrap();
                if event["event"] == "snapshot_timing" {
                    assert_eq!(event["level"], "DEBUG");
                    assert!(event["fields"]["elapsed_us"].as_u64().is_some());
                    timings.push(event["fields"].clone());
                }
            }
        }
    }
    (output, response, timings)
}

fn phase(events: &[Value], operation: &str, name: &str, outcome: &str) {
    let matches: Vec<_> = events
        .iter()
        .filter(|event| event["operation"] == operation && event["phase"] == name)
        .collect();
    assert_eq!(matches.len(), 1, "{operation}/{name}: {events:?}");
    assert_eq!(matches[0]["outcome"], outcome, "{matches:?}");
}

fn succeeded(output: &Output, response: &Value) {
    assert!(output.status.success(), "{response}");
    assert_eq!(response["ok"], true);
}

fn capture(events: &[Value], operation: &str, publish: bool) {
    for name in CAPTURE_PHASES {
        phase(events, operation, name, "success");
    }
    if publish {
        phase(events, operation, "publish", "success");
    } else {
        assert!(!events
            .iter()
            .any(|event| { event["operation"] == operation && event["phase"] == "publish" }));
    }
}

#[test]
fn save_reports_named_phases_without_changing_json_or_workspace() {
    let repo = Repo::new();
    fs::write(repo.path.join("tracked.txt"), "saved edit\n").unwrap();
    let index = fs::read(repo.path.join(".git/index")).unwrap();
    let (output, response, events) = invoke(&repo.path, &["snapshot", "save"], true);
    succeeded(&output, &response);
    capture(&events, "save", true);
    phase(&events, "save", "lock_wait", "success");
    phase(&events, "save", "overall", "success");
    assert!(String::from_utf8_lossy(&output.stderr).contains("operation=\"save\""));
    assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"saved edit\n"
    );
}

#[test]
fn capture_failure_reports_completed_and_failed_phases_without_publication() {
    let repo = Repo::new();
    fs::write(repo.path.join("tracked.txt"), "oversized edit\n").unwrap();
    let (output, response, events) = invoke(
        &repo.path,
        &["snapshot", "save", "--max-file-size", "1"],
        true,
    );
    assert_eq!(output.status.code(), Some(2), "{response}");
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    for name in [
        "lock_wait",
        "validation",
        "special_file_scan",
        "scratch_index_tree",
    ] {
        phase(&events, "save", name, "success");
    }
    for name in ["staging", "capture", "overall"] {
        phase(&events, "save", name, "error");
    }
    assert!(!events.iter().any(|event| event["phase"] == "publish"));
    let (output, listed, _) = invoke(&repo.path, &["snapshot", "list"], false);
    succeeded(&output, &listed);
    assert_eq!(listed["result"]["snapshots"].as_array().unwrap().len(), 0);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"oversized edit\n"
    );
}

#[test]
fn restore_distinguishes_safety_capture_from_restore_and_diff_capture() {
    let repo = Repo::new();
    let (output, saved, _) = invoke(&repo.path, &["snapshot", "save"], false);
    succeeded(&output, &saved);
    let id = saved["result"]["snapshot"]["id"].as_str().unwrap();
    fs::write(repo.path.join("tracked.txt"), "current edit\n").unwrap();
    let (output, diff, events) = invoke(&repo.path, &["snapshot", "diff", id], true);
    succeeded(&output, &diff);
    capture(&events, "diff_capture", false);
    phase(&events, "diff", "lock_wait", "success");
    phase(&events, "diff", "overall", "success");
    assert!(diff["result"]["patch"]
        .as_str()
        .unwrap()
        .contains("+current edit"));
    let (output, restored, events) = invoke(&repo.path, &["snapshot", "restore", id], true);
    succeeded(&output, &restored);
    capture(&events, "restore_safety", true);
    for name in [
        "lock_wait",
        "preflight",
        "safety_pin",
        "apply",
        "safety_unpin",
        "overall",
    ] {
        phase(&events, "restore", name, "success");
    }
    assert!(restored["result"]["safety_snapshot_id"].as_str().is_some());
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
}

#[test]
fn failed_restore_safety_capture_reports_restore_failure_and_preserves_files() {
    let repo = Repo::new();
    let (output, saved, _) = invoke(&repo.path, &["snapshot", "save"], false);
    succeeded(&output, &saved);
    let id = saved["result"]["snapshot"]["id"].as_str().unwrap();
    fs::write(repo.path.join("tracked.txt"), "current edit\n").unwrap();
    let (output, response, events) = invoke(
        &repo.path,
        &["snapshot", "restore", id, "--max-file-size", "1"],
        true,
    );
    assert_eq!(output.status.code(), Some(2), "{response}");
    assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    phase(&events, "restore", "preflight", "success");
    phase(&events, "restore_safety", "staging", "error");
    phase(&events, "restore_safety", "capture", "error");
    phase(&events, "restore", "overall", "error");
    assert!(!events.iter().any(|event| event["phase"] == "apply"));
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"current edit\n"
    );
}

#[test]
fn recovery_reports_creation_and_apply_without_a_safety_capture() {
    let repo = Repo::new();
    let (output, saved, _) = invoke(&repo.path, &["snapshot", "save"], false);
    succeeded(&output, &saved);
    let id = saved["result"]["snapshot"]["id"].as_str().unwrap();
    let destination = repo.temp.path().join("recovered");
    let (output, response, events) = invoke(
        &repo.path,
        &[
            "snapshot",
            "restore",
            id,
            "--to",
            destination.to_str().unwrap(),
        ],
        true,
    );
    succeeded(&output, &response);
    for name in [
        "lock_wait",
        "preflight",
        "recovery_creation",
        "apply",
        "overall",
    ] {
        phase(&events, "restore", name, "success");
    }
    assert!(!events
        .iter()
        .any(|event| event["operation"] == "restore_safety"));
    assert_eq!(
        fs::read(destination.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
}

#[test]
fn ordinary_json_commands_keep_debug_timings_off_stderr() {
    let repo = Repo::new();
    let (output, response, events) = invoke(&repo.path, &["snapshot", "save"], false);
    succeeded(&output, &response);
    phase(&events, "save", "overall", "success");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("operation=\"save\""));
}

#[test]
fn saved_release_identifies_its_capture_separately_from_standalone_save() {
    let repo = Repo::new();
    let (output, response, _) = invoke(&repo.path, &["pool", "init", "--capacity", "1"], false);
    succeeded(&output, &response);
    let (output, acquired, _) = invoke(
        &repo.path,
        &[
            "acquire",
            "--task",
            "timed-release",
            "--request-id",
            "timed-release",
        ],
        false,
    );
    succeeded(&output, &acquired);
    let workspace = acquired["result"]["workspace_path"].as_str().unwrap();
    fs::write(Path::new(workspace).join("tracked.txt"), "release edit\n").unwrap();
    let (output, response, events) = invoke(
        &repo.path,
        &["release", "--save", "--task", "timed-release"],
        true,
    );
    succeeded(&output, &response);
    capture(&events, "release_save", true);
    phase(&events, "release_save", "lock_wait", "success");
    phase(&events, "release_save", "overall", "success");
    assert!(!events.iter().any(|event| event["operation"] == "save"));
    assert_eq!(response["result"]["snapshot"]["pinned"], true);
}

#[test]
fn lock_timeout_reports_failed_wait_and_overall_without_starting_capture() {
    use std::os::fd::AsRawFd;
    let repo = Repo::new();
    let (output, saved, _) = invoke(&repo.path, &["snapshot", "save"], false);
    succeeded(&output, &saved);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(repo.path.join(".git/groove-snapshots/lock"))
        .unwrap();
    // SAFETY: flock borrows a valid descriptor; nonblocking acquisition cannot hang.
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(locked, 0);
    // Make discovery slower than the lock wait on every host. A short lock
    // budget must not become a short Git command budget before reaching flock.
    use std::os::unix::fs::PermissionsExt;
    let wrapper = repo.temp.path().join("slow-git");
    fs::create_dir(&wrapper).unwrap();
    fs::write(
        wrapper.join("git"),
        "#!/bin/sh\nsleep 0.1\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(wrapper.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .env(
            "PATH",
            format!("{}:{}", wrapper.display(), std::env::var("PATH").unwrap()),
        )
        .env(
            "GROOVE_REAL_GIT",
            std::env::var_os("GROOVE_TEST_GIT").unwrap(),
        );
    let (output, response, events) = invoke_command(
        command,
        &["snapshot", "--lock-timeout", "25ms", "save"],
        true,
    );
    assert_eq!(output.status.code(), Some(2), "{response}");
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["retryable"], true);
    phase(&events, "save", "lock_wait", "error");
    phase(&events, "save", "overall", "error");
    assert!(!events.iter().any(|event| event["phase"] == "capture"));
    drop(lock);
    let id = saved["result"]["snapshot"]["id"].as_str().unwrap();
    let (output, shown, _) = invoke(&repo.path, &["snapshot", "show", id], false);
    succeeded(&output, &shown);
    assert_eq!(shown["result"]["snapshot"]["id"], id);
}
