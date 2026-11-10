use serde_json::Value;
use std::{
    fs,
    process::{Command, Output},
};
fn run(home: &std::path::Path, args: &[&str]) -> Output {
    command(home).args(args).output().unwrap()
}
fn command(home: &std::path::Path) -> Command {
    let mut cmd = support::groove();
    cmd.current_dir(home)
        .env("XDG_CONFIG_HOME", home)
        .env("XDG_STATE_HOME", home.join("state"))
        .env_remove("GROOVE_LOG_ENABLED")
        .env_remove("GROOVE_LOG_DIR");
    cmd
}
fn ok(out: Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema).unwrap().is_valid(&value));
    value["result"].clone()
}
#[test]
fn defaults_updates_precedence_and_validation() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let show = |h| ok(run(h, &["config", "global", "show"]));
    assert_eq!(show(h)["config"]["logging"]["max_files"], 100);
    assert!(!h.join("groove/config.toml").exists());
    ok(run(h, &["config", "global", "set", "logging.max_files=12"]));
    ok(run(
        h,
        &["config", "global", "set", "logging.enabled=false"],
    ));
    assert_eq!(show(h)["config"]["logging"]["enabled"], false);
    let effective = ok(command(h)
        .env("GROOVE_LOG_ENABLED", "true")
        .args(["config", "global", "show"])
        .output()
        .unwrap());
    assert_eq!(effective["config"]["logging"]["enabled"], true);
    assert_eq!(effective["environment_overrides"]["logging.enabled"], true);
    let before = fs::read(h.join("groove/config.toml")).unwrap();
    for args in [
        vec!["config", "global", "set", "logging.max_files=0"],
        vec!["config", "global", "set", "logging.unknown=true"],
        vec!["config", "global", "set", "pool.backend=git"],
        vec!["config", "global", "set", "logging.enabled=123"],
        vec!["config", "global", "set", "no-equals-sign"],
    ] {
        assert!(!run(h, &args).status.success(), "{args:?}");
        assert_eq!(fs::read(h.join("groove/config.toml")).unwrap(), before);
    }
    ok(run(h, &["config", "global", "unset", "logging.enabled"]));
    assert_eq!(show(h)["config"]["logging"]["enabled"], true);
    // Bare strings need no TOML quoting.
    let logs = h.join("custom logs");
    ok(run(
        h,
        &[
            "config",
            "global",
            "set",
            &format!("logging.directory={}", logs.display()),
        ],
    ));
    assert_eq!(
        show(h)["config"]["logging"]["directory"],
        logs.to_str().unwrap()
    );
    // Edits preserve unrelated comments.
    let file = h.join("groove/config.toml");
    fs::write(
        &file,
        format!(
            "# Keep this comment\n{}",
            fs::read_to_string(&file).unwrap()
        ),
    )
    .unwrap();
    ok(run(h, &["config", "global", "set", "logging.max_files=15"]));
    assert!(fs::read_to_string(file)
        .unwrap()
        .contains("# Keep this comment"));
}

#[test]
fn editor_validates_and_keeps_stdout_json_only() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    ok(run(h, &["config", "global", "set", "logging.max_files=12"]));
    let editor = h.join("editor.sh");
    fs::write(
        &editor,
        "printf '%s\\n' 'editor UI'\nprintf '[logging]\\nmax_files = 22\\n' > \"$1\"\n",
    )
    .unwrap();
    let out = command(h)
        .env("VISUAL", format!("bash '{}'", editor.display()))
        .args(["config", "global", "edit"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("editor UI"));
    ok(out);
    let max_files =
        |h| ok(run(h, &["config", "global", "show"]))["config"]["logging"]["max_files"].clone();
    assert_eq!(max_files(h), 22);
    fs::write(&editor, "printf '%s' 'invalid TOML!' > \"$1\"\n").unwrap();
    let out = command(h)
        .env("VISUAL", format!("bash '{}'", editor.display()))
        .args(["config", "global", "edit"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(max_files(h), 22);
    // Broken configuration is still reachable through path/edit.
    fs::write(h.join("groove/config.toml"), "broken!").unwrap();
    ok(run(h, &["config", "global", "path"]));
}

#[test]
fn global_updates_validate_and_publish_all_preferences_together() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    ok(run(
        h,
        &[
            "config",
            "global",
            "set",
            "logging.max_file_mib=200",
            "logging.max_total_mib=300",
        ],
    ));
    let config = h.join("groove/config.toml");
    let before = fs::read(&config).unwrap();
    // One invalid value rejects the whole update.
    assert_eq!(
        run(
            h,
            &[
                "config",
                "global",
                "set",
                "logging.max_files=5",
                "logging.max_total_mib=100"
            ]
        )
        .status
        .code(),
        Some(2)
    );
    assert_eq!(fs::read(&config).unwrap(), before);
}
#[test]
fn log_fields_are_typed_and_correlated() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let out = run(h, &["-vv", "--request-id", "req-123", "list"]);
    assert!(!out.status.success()); // No pool; failure still has structured events.
    let logs = h.join("state/groove/logs");
    let file = fs::read_dir(logs)
        .unwrap()
        .flatten()
        .find(|f| f.path().extension().is_some_and(|e| e == "jsonl"))
        .unwrap();
    let events: Vec<Value> = fs::read_to_string(file.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for event in &events {
        let timestamp = event["timestamp"].as_str().unwrap();
        let parsed = chrono::DateTime::parse_from_rfc3339(timestamp).unwrap();
        assert_eq!(parsed.offset().local_minus_utc(), 0);
        assert_eq!(timestamp.len(), 24); // Milliseconds and UTC Z suffix.
        assert!(timestamp.ends_with('Z'));
        assert!(event.get("timestamp_ms").is_none());
        let stderr = String::from_utf8_lossy(&out.stderr);
        let expected = format!(
            "{} {} {}",
            timestamp,
            event["level"].as_str().unwrap(),
            event["message"].as_str().unwrap()
        );
        if event["event"] == "command.failed" {
            assert!(!stderr.contains("Command failed"));
            assert_eq!(stderr.matches("(GIT_ERROR)").count(), 1);
        } else {
            assert!(
                stderr.lines().any(|line| line.starts_with(&expected)),
                "{stderr}"
            );
        }
        assert_eq!(event["log_version"], 1);
        assert_eq!(event["request_id"], "req-123");
        assert_eq!(event["command"], "list");
        assert!(event["pid"].is_number());
        assert!(event["message"].is_string());
    }
    let failed = events
        .iter()
        .find(|e| e["event"] == "command.failed")
        .unwrap();
    assert!(failed["fields"]["elapsed_ms"].is_number());
    assert!(failed["fields"]["code"]
        .as_str()
        .unwrap()
        .chars()
        .all(|c| c.is_ascii_uppercase() || c == '_'));
}

#[test]
fn configured_retention_is_used_and_removed_flags_are_rejected() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    ok(run(h, &["config", "global", "set", "logging.max_files=2"]));
    for _ in 0..5 {
        run(h, &["list"]);
    }
    assert_eq!(
        fs::read_dir(h.join("state/groove/logs"))
            .unwrap()
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "jsonl"))
            .count(),
        2
    );
    for args in [
        vec!["--log-dir", "/tmp", "list"],
        vec!["--no-log-file", "list"],
    ] {
        let out = run(h, &args);
        assert_eq!(out.status.code(), Some(2));
    }
}

mod support;

fn timestamp_in_window(text: &str, before: i64, after: i64) {
    let time = chrono::DateTime::parse_from_rfc3339(text).expect("RFC 3339 timestamp");
    assert_eq!(text.len(), 24);
    assert!(text.ends_with('Z'));
    assert_eq!(time.offset().local_minus_utc(), 0);
    assert!(
        (before..=after).contains(&time.timestamp_millis()),
        "stale/future timestamp: {text}"
    );
}

#[test]
fn successful_lifecycle_has_fresh_matching_timestamps_and_startup_notices() {
    let home = tempfile::tempdir().unwrap();
    let repo = support::Repo::new();
    let mut lease = Value::Null;
    let mut invocations = std::collections::HashSet::new();
    for action in ["init", "acquire", "list", "release"] {
        let logs = home.path().join(action);
        let before = chrono::Utc::now().timestamp_millis();
        let mut cmd = command(home.path());
        cmd.env("GROOVE_LOG_DIR", &logs)
            .arg("--repo")
            .arg(&repo.path)
            .args(["-vv", "--request-id", "lifecycle", action]);
        match action {
            "init" => {
                cmd.args(["--capacity", "1"]);
            }
            "release" => {
                cmd.args(["--lease-id", lease["lease_id"].as_str().unwrap()]);
            }
            _ => {}
        }
        let out = cmd.output().unwrap();
        let after = chrono::Utc::now().timestamp_millis();
        let stderr = String::from_utf8(out.stderr.clone()).unwrap();
        // Includes the startup announcement, not just tracing events.
        assert!(stderr.contains("DEBUG Log file:"));
        for line in stderr.lines() {
            timestamp_in_window(line.split_once(' ').unwrap().0, before, after);
        }
        let files: Vec<_> = fs::read_dir(logs)
            .unwrap()
            .flatten()
            .filter(|f| f.path().extension().is_some_and(|e| e == "jsonl"))
            .collect();
        assert_eq!(files.len(), 1);
        let events: Vec<Value> = fs::read_to_string(files[0].path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(!events.is_empty());
        let id = events[0]["invocation_id"].as_str().unwrap();
        assert!(invocations.insert(id.to_owned()));
        for event in &events {
            let ts = event["timestamp"].as_str().unwrap();
            timestamp_in_window(ts, before, after);
            assert!(event.get("timestamp_ms").is_none());
            assert_eq!(event["invocation_id"], id);
            assert_eq!(event["request_id"], "lifecycle");
            assert_eq!(event["command"], action);
            let prefix = format!(
                "{ts} {} {}",
                event["level"].as_str().unwrap(),
                event["message"].as_str().unwrap()
            );
            assert!(
                stderr.lines().any(|line| line.starts_with(&prefix)),
                "missing: {prefix}"
            );
        }
        assert!(events.iter().any(|e| e["event"] == "command.completed"));
        assert!(!events.iter().any(|e| e["event"] == "command.failed"));
        let result = ok(out);
        if action == "acquire" {
            lease = result;
        }
    }
}

#[test]
fn unavailable_log_warning_is_timestamped_and_does_not_break_stdout() {
    let home = tempfile::tempdir().unwrap();
    let blocked = home.path().join("not-a-directory");
    fs::write(&blocked, "blocked").unwrap();
    let before = chrono::Utc::now().timestamp_millis();
    let out = command(home.path())
        .env("GROOVE_LOG_DIR", blocked)
        .args(["-v", "list"])
        .output()
        .unwrap();
    let after = chrono::Utc::now().timestamp_millis();
    assert!(!out.status.success());
    let response: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(response["ok"], false);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("WARN File logging unavailable"));
    for line in stderr
        .lines()
        .filter(|line| line.contains("WARN File logging unavailable"))
    {
        timestamp_in_window(line.split_once(' ').unwrap().0, before, after);
    }
}

#[test]
fn logging_lock_owner_process() {
    use std::{
        fs::{File, FileTimes},
        io::{Read, Write},
        os::{fd::AsRawFd, unix::net::UnixStream},
        time::{Duration, SystemTime},
    };
    let Some(path) = std::env::var_os("GROOVE_TEST_LOG_OWNER_FILE") else {
        return;
    };
    let mut file = File::create(path).unwrap();
    // SAFETY: file owns a valid descriptor for the complete lifetime of this lock.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
    writeln!(file, "{{}}").unwrap();
    file.flush().unwrap();
    file.set_times(FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)))
        .unwrap();
    let mut socket =
        UnixStream::connect(std::env::var_os("GROOVE_TEST_LOG_OWNER_SOCKET").unwrap()).unwrap();
    socket.write_all(b"R").unwrap();
    // The parent kills this child to test kernel lock release after a crash.
    let mut stop = [0];
    socket.read_exact(&mut stop).unwrap();
}

#[test]
fn concurrent_cleanup_protects_live_logs_and_reclaims_crashed_owner() {
    use std::{
        io::Read,
        os::unix::{fs::symlink, net::UnixListener},
        process::Stdio,
        time::{Duration, Instant},
    };
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let repo = support::Repo::new();
    ok(run(h, &["config", "global", "set", "logging.max_files=2"]));
    ok(command(h)
        .arg("--repo")
        .arg(&repo.path)
        .args(["init", "--capacity", "1"])
        .output()
        .unwrap());
    let logs = h.join("state/groove/logs");
    let active = logs.join("groove-active.jsonl");
    let unrelated = logs.join("unrelated.txt");
    fs::write(&unrelated, "keep").unwrap();
    symlink(&unrelated, logs.join("groove-link.jsonl")).unwrap();
    let socket = h.join("ready.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    // A real process holds the same OS lock as a logging invocation. Its log is
    // deliberately expired, making pruning eligible as soon as the owner dies.
    let mut owner_cmd = Command::new(std::env::current_exe().unwrap());
    owner_cmd
        .args(["--exact", "logging_lock_owner_process", "--nocapture"])
        .env("GROOVE_TEST_LOG_OWNER_FILE", &active)
        .env("GROOVE_TEST_LOG_OWNER_SOCKET", &socket)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let mut owner = support::TestChild::spawn(&mut owner_cmd);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ready = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    owner.try_wait().unwrap().is_none(),
                    "log owner exited before readiness"
                );
                assert!(Instant::now() < deadline, "log owner readiness timed out");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    };
    ready
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut byte = [0];
    ready.read_exact(&mut byte).unwrap();
    assert_eq!(byte, [b'R']);
    let mut children = Vec::new();
    for _ in 0..8 {
        let mut cmd = command(h);
        cmd.arg("--repo")
            .arg(&repo.path)
            .arg("list")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        children.push(support::TestChild::spawn(&mut cmd));
    }
    for child in children {
        ok(child.wait_with_output().unwrap());
    }
    assert_eq!(fs::read_to_string(&active).unwrap(), "{}\n");
    assert_eq!(fs::read_to_string(&unrelated).unwrap(), "keep");
    assert!(fs::symlink_metadata(logs.join("groove-link.jsonl"))
        .unwrap()
        .file_type()
        .is_symlink());
    // SIGKILL tests OS lock release, not graceful cleanup.
    owner.kill().unwrap();
    owner.wait().unwrap();
    ok(command(h)
        .arg("--repo")
        .arg(&repo.path)
        .arg("list")
        .output()
        .unwrap());
    assert!(!active.exists());
    let files: Vec<_> = fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .filter(|f| {
            f.file_type().unwrap().is_file() && f.path().extension().is_some_and(|e| e == "jsonl")
        })
        .collect();
    assert!(files.len() <= 2);
    for file in files {
        for line in fs::read_to_string(file.path()).unwrap().lines() {
            serde_json::from_str::<Value>(line).unwrap();
        }
    }
}
