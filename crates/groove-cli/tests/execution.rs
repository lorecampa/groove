mod support;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use support::{assert_success, Repo, TestChild};

fn command(repo: &Repo, args: &[&str]) -> Command {
    let mut command = support::groove_in(&repo.path);
    command.args(args).env("GROOVE_LOG_ENABLED", "false");
    command
}
fn cli(repo: &Repo, args: &[&str]) -> Output {
    command(repo, args).output().unwrap()
}
fn checked(out: &Output) -> Value {
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&value)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {value}");
    value
}
fn reserve(repo: &Repo) -> Value {
    assert_success(&cli(repo, &["init", "--capacity", "1"]));
    let out = cli(
        repo,
        &[
            "acquire",
            "--request-key",
            "task-1",
            "--task",
            "test execution",
        ],
    );
    assert_success(&out);
    checked(&out)["result"].clone()
}
fn wait_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing {}", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn start(repo: &Repo, id: &str, script: &str) -> TestChild {
    TestChild::spawn(
        command(
            repo,
            &[
                "exec",
                "--lease-id",
                id,
                "--grace-ms",
                "20",
                "--",
                "bash",
                "-c",
                script,
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()),
    )
}
/// These tests leave worker output in the workspace; discarding it is intended.
fn release(repo: &Repo, id: &str) {
    assert_success(&cli(repo, &["release", "--force", "--lease-id", id]));
}

#[test]
fn captures_output_preserves_worker_exit_and_keeps_reservation() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let out = cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--grace-ms",
            "0",
            "--",
            "bash",
            "-c",
            "printf hello; printf problem >&2; exit 17",
        ],
    );
    assert_eq!(out.status.code(), Some(17));
    let value = checked(&out);
    assert_eq!(value["ok"], true);
    let execution = &value["result"]["execution"];
    assert_eq!(execution["exit_code"], 17);
    assert_eq!(
        fs::read_to_string(execution["run"]["stdout_path"].as_str().unwrap()).unwrap(),
        "hello"
    );
    assert_eq!(
        fs::read_to_string(execution["run"]["stderr_path"].as_str().unwrap()).unwrap(),
        "problem"
    );
    assert!(
        !Path::new(execution["run"]["stdout_path"].as_str().unwrap())
            .starts_with(lease["worktree_path"].as_str().unwrap())
    );
    assert!(!cli(&repo, &["acquire"]).status.success());
    checked(&cli(&repo, &["show"]));
    assert!(!cli(
        &repo,
        &[
            "release",
            "--lease-id",
            "00000000-0000-4000-8000-000000000000"
        ]
    )
    .status
    .success());
    release(&repo, id);
}

#[test]
fn live_execution_blocks_release_with_or_without_save_and_recovery_until_stop() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let worker = start(&repo, id, "echo ready > ready; while :; do sleep 1; done");
    wait_file(&repo.slot(1).join("ready"));
    // The running worker is seen as a process using the workspace ...
    for mut args in [vec!["release"], vec!["release", "--save"]] {
        args.extend(["--lease-id", id]);
        let out = cli(&repo, &args);
        assert!(!out.status.success(), "{args:?}");
        let error = checked(&out)["error"].clone();
        assert_eq!(error["code"], "WORKSPACE_IN_USE", "{args:?}");
        assert!(error["details"]["processes"]
            .as_array()
            .is_some_and(|p| !p.is_empty()));
    }
    // ... and --force never bypasses the managed-execution lock.
    for args in [
        vec!["release", "--force", "--lease-id", id],
        vec!["release", "--save", "--force", "--lease-id", id],
        vec!["recover", "--lease-id", id],
    ] {
        let out = cli(&repo, &args);
        assert!(!out.status.success(), "{args:?}");
        assert_eq!(checked(&out)["error"]["code"], "INVALID_LEASE", "{args:?}");
    }
    assert_success(&cli(&repo, &["stop", "--lease-id", id]));
    let out = worker.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(143),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(checked(&out)["result"]["execution"]["cancel_signal"], 15);
    assert!(!repo.state().join("slots/wt-0001.execution.json").exists());
    release(&repo, id);
}

#[test]
fn timeout_kills_descendants_and_prevents_late_writes_after_reuse() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let out = cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--timeout-ms",
            "150",
            "--grace-ms",
            "20",
            "--",
            "bash",
            "-c",
            "(sleep 0.6; echo stale > late) & wait",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(124),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        checked(&out)["result"]["execution"]["termination"],
        "timed_out"
    );
    release(&repo, id);
    assert_success(&cli(&repo, &["acquire"]));
    std::thread::sleep(Duration::from_millis(700));
    assert!(!repo.slot(1).join("late").exists());
}

#[test]
fn supervisor_cancellation_signals_clean_workers_and_return_json() {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        let repo = Repo::new();
        let lease = reserve(&repo);
        let id = lease["lease_id"].as_str().unwrap();
        let worker = start(&repo, id, "echo ready > ready; exec sleep 60");
        wait_file(&repo.slot(1).join("ready"));
        // SAFETY: this is our unreaped direct test child, whose PID cannot have been reused.
        assert_eq!(unsafe { libc::kill(worker.id() as i32, signal) }, 0);
        let out = worker.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(128 + signal));
        assert_eq!(
            checked(&out)["result"]["execution"]["cancel_signal"],
            signal
        );
        release(&repo, id);
    }
}

#[test]
fn leader_exit_stops_term_ignoring_descendants_before_reuse() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let worker = start(&repo, id,
        "(trap '' TERM; echo ready > ready; sleep 0.8; echo stale > late) & while ! test -f ready; do sleep 0.01; done; exit 17");
    let out = worker.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(17),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(
        checked(&out)["result"]["execution"]["termination"],
        "exited"
    );
    assert!(!repo.state().join("slots/wt-0001.execution.json").exists());
    release(&repo, id);
    assert_success(&cli(&repo, &["acquire"]));
    // This observes absence of a delayed write, after teardown already succeeded.
    std::thread::sleep(Duration::from_millis(900));
    assert!(!repo.slot(1).join("late").exists());
}

#[test]
fn failed_group_teardown_preserves_results_and_requires_explicit_recovery() {
    use std::os::unix::process::CommandExt;
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let worker = start(
        &repo,
        id,
        "echo keep > result; echo ready > ready; exec sleep 60",
    );
    wait_file(&repo.slot(1).join("ready"));
    let shown = checked(&cli(&repo, &["show", "--lease-id", id]));
    let pid = shown["result"]["slot"]["execution"]["leader_pid"]
        .as_u64()
        .unwrap();
    // The live, unreaped leader still reserves this group identity. This member
    // is our direct child, so Groove cannot reap it, even as a Linux subreaper.
    // Withhold our wait until after Groove's bounded teardown check: the zombie
    // deterministically keeps the group present on both supported platforms.
    let mut member = TestChild::spawn(
        support::command(&repo.path, "sleep")
            .arg("60")
            .process_group(pid as i32),
    );
    assert_success(&cli(&repo, &["stop", "--lease-id", id]));
    let out = worker.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert_eq!(
        checked(&out)["error"]["code"],
        "INVALID_LEASE",
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    member.wait().unwrap();
    for mut args in [vec!["release"], vec!["release", "--save"]] {
        args.extend(["--force", "--lease-id", id]);
        let out = cli(&repo, &args);
        assert_eq!(checked(&out)["error"]["code"], "INVALID_LEASE");
    }
    assert!(!cli(&repo, &["acquire"]).status.success());
    assert_eq!(fs::read(repo.slot(1).join("result")).unwrap(), b"keep\n");
    let shown = checked(&cli(&repo, &["show", "--lease-id", id]));
    assert_eq!(shown["result"]["lease"]["lease_id"], id);
    assert!(!shown["result"]["slot"]["execution"].is_null());
    assert_success(&cli(&repo, &["recover", "--lease-id", id]));
    assert_eq!(fs::read(repo.slot(1).join("result")).unwrap(), b"keep\n");
    release(&repo, id);
}

#[test]
fn supervisor_sigkill_leaves_execution_and_reservation_protected() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let mut worker = start(&repo, id, "echo ready > ready; exec sleep 1");
    wait_file(&repo.slot(1).join("ready"));
    worker.kill().unwrap();
    worker.wait().unwrap();
    let deletion = cli(&repo, &["destroy", "--discard-changes"]);
    assert_eq!(checked(&deletion)["error"]["code"], "CONFIG_CONFLICT");
    assert!(repo.slot(1).join("ready").exists());
    for operation in ["release", "recover", "stop"] {
        assert!(!cli(&repo, &[operation, "--lease-id", id]).status.success());
    }
    assert!(!cli(&repo, &["acquire"]).status.success());
    // The explicitly launched sleep has a bounded lifetime. Its exit does not clear state.
    std::thread::sleep(Duration::from_millis(1200));
    let deletion = cli(&repo, &["destroy", "--discard-changes"]);
    assert_eq!(checked(&deletion)["error"]["code"], "CONFIG_CONFLICT");
    assert!(!cli(&repo, &["release", "--lease-id", id]).status.success());
    assert_success(&cli(&repo, &["recover", "--lease-id", id]));
    release(&repo, id);
}

#[test]
fn output_limits_and_spawn_errors_preserve_reservation() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let out = cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--max-log-bytes",
            "128",
            "--grace-ms",
            "0",
            "--",
            "bash",
            "-c",
            "printf '%1024s' x",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    let value = checked(&out);
    assert_eq!(
        value["result"]["execution"]["termination"],
        "output_limited"
    );
    assert_eq!(
        fs::metadata(
            value["result"]["execution"]["run"]["stdout_path"]
                .as_str()
                .unwrap()
        )
        .unwrap()
        .len(),
        128
    );
    let missing = cli(
        &repo,
        &["exec", "--lease-id", id, "--", "/no/such/groove-program"],
    );
    assert_eq!(missing.status.code(), Some(7));
    let failure = checked(&missing);
    let details = &failure["error"]["details"];
    assert_eq!(failure["error"]["code"], "IO_ERROR");
    assert_eq!(details["program"], "/no/such/groove-program");
    assert_eq!(details["lease_id"], id);
    assert_eq!(details["worktree_path"], lease["worktree_path"]);
    let history = checked(&cli(
        &repo,
        &["history", "show", details["run_id"].as_str().unwrap()],
    ));
    assert_eq!(history["result"]["run"]["outcome"], "spawn_failed");
    assert_eq!(history["result"]["run"]["program"], details["program"]);
    assert!(!repo.state().join("slots/wt-0001.execution.json").exists());
    release(&repo, id);
}

#[test]
fn request_keys_retry_across_processes_and_finish_receipts_survive_reuse() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    for (file, schema) in [
        (
            "config.json",
            include_str!("../schemas/config-v1.schema.json"),
        ),
        (
            "slots/wt-0001.json",
            include_str!("../schemas/slot-v1.schema.json"),
        ),
    ] {
        let schema: Value = serde_json::from_str(schema).unwrap();
        let record: Value =
            serde_json::from_slice(&fs::read(repo.state().join(file)).unwrap()).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&record), "{file}: {record}");
        if file == "config.json" {
            assert_eq!(record["schema_version"], 1);
            let mut missing_locks = record;
            missing_locks
                .as_object_mut()
                .unwrap()
                .remove("execution_locks");
            assert!(!validator.is_valid(&missing_locks));
        }
    }
    fs::write(repo.slot(1).join("tracked.txt"), "preserved results").unwrap();
    let again = cli(
        &repo,
        &[
            "acquire",
            "--request-key",
            "task-1",
            "--task",
            "test execution",
        ],
    );
    assert_eq!(assert_success(&again)["result"]["lease_id"], id);
    let failed = cli(
        &repo,
        &[
            "release",
            "--save",
            "--lease-id",
            id,
            "--max-file-bytes",
            "1",
        ],
    );
    assert!(!failed.status.success());
    let out = cli(&repo, &["release", "--save", "--lease-id", id]);
    assert_success(&out);
    let result = checked(&out)["result"].clone();
    assert_eq!(result["snapshot"]["pinned"], true);
    let next = assert_success(&cli(&repo, &["acquire"]))["result"].clone();
    assert_eq!(
        assert_success(&cli(&repo, &["release", "--save", "--lease-id", id]))["result"],
        result
    );
    assert!(!cli(
        &repo,
        &[
            "acquire",
            "--request-key",
            "task-1",
            "--task",
            "test execution"
        ]
    )
    .status
    .success());
    release(&repo, next["lease_id"].as_str().unwrap());
}

#[test]
fn worker_output_is_mirrored_in_both_formats_and_quiet_keeps_captured_bytes() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    for json in [false, true] {
        for quiet in [false, true] {
            let mut cmd = command(&repo, &["exec", "--lease-id", id]);
            if !json {
                cmd.env_remove("GROOVE_OUTPUT");
            }
            if quiet {
                cmd.arg("--quiet");
            }
            let out = cmd
                .args([
                    "--",
                    "bash",
                    "-c",
                    "printf 'out\\000bytes'; printf 'err\\n' >&2; exit 17",
                ])
                .output()
                .unwrap();
            assert_eq!(out.status.code(), Some(17));
            if quiet {
                assert!(out.stderr.is_empty(), "{out:?}");
            } else {
                assert!(out.stderr.windows(9).any(|bytes| bytes == b"out\0bytes"));
                assert!(out.stderr.windows(4).any(|bytes| bytes == b"err\n"));
            }
            if json {
                let result = checked(&out);
                assert_eq!(out.stdout.iter().filter(|b| **b == b'\n').count(), 1);
                let run = &result["result"]["execution"]["run"];
                assert_eq!(
                    fs::read(run["stdout_path"].as_str().unwrap()).unwrap(),
                    b"out\0bytes"
                );
                assert_eq!(
                    fs::read(run["stderr_path"].as_str().unwrap()).unwrap(),
                    b"err\n"
                );
            } else {
                assert!(String::from_utf8_lossy(&out.stdout).contains("exited with status 17"));
                assert!(!out.stdout.contains(&0));
            }
        }
    }
}

// Redirect inside the wrapper because TestChild owns its outer capture streams.
fn redirected(repo: &Repo, stderr: &Path) -> Command {
    let mut command = support::command(&repo.path, "bash");
    command
        .args(["-c", "exec \"$@\" 2>\"$GROOVE_TEST_STDERR\"", "redirect"])
        .arg(env!("CARGO_BIN_EXE_groove"))
        .env("GROOVE_TEST_STDERR", stderr)
        .env("GROOVE_OUTPUT", "json")
        .env("GROOVE_LOG_ENABLED", "false");
    command
}

#[test]
fn output_arrives_before_worker_completion() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let terminal = repo.temp.path().join("terminal");
    let resume = repo.temp.path().join("resume");
    let mut child = TestChild::spawn(
        redirected(&repo, &terminal)
            .args([
                "exec",
                "--lease-id",
                lease["lease_id"].as_str().unwrap(),
                "--timeout-ms",
                "10000",
                "--",
                "bash",
                "-c",
                "printf ready; while ! test -f \"$1\"; do sleep 0.01; done; printf done >&2",
                "fixture",
            ])
            .arg(&resume),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if fs::read(&terminal).unwrap_or_default() == b"ready" {
            break;
        }
        assert!(
            child.try_wait().unwrap().is_none(),
            "worker exited before live output"
        );
        assert!(Instant::now() < deadline, "no live output");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(child.try_wait().unwrap().is_none());
    fs::write(resume, "continue").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_success(&out);
    checked(&out);
    assert_eq!(fs::read(terminal).unwrap(), b"readydone");
}

#[test]
fn blocked_or_failed_stderr_does_not_block_timeout_or_json_result() {
    use std::os::unix::fs::OpenOptionsExt;
    let repo = Repo::new();
    let lease = reserve(&repo);
    let fifo = repo.temp.path().join("stderr.fifo");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let _reader = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&fifo)
        .unwrap();
    for blocked in [true, false] {
        let mut command = redirected(&repo, &fifo);
        if !blocked {
            // Open an existing file read-only inside the wrapper. TestChild
            // owns its outer capture streams; this makes the CLI's fd 2 fail
            // writes on Linux and macOS without a platform-specific device.
            command = support::command(&repo.path, "bash");
            command
                .args([
                    "-c",
                    "exec 2<\"$GROOVE_TEST_STDERR\"; exec \"$@\"",
                    "redirect",
                ])
                .arg(env!("CARGO_BIN_EXE_groove"))
                .env("GROOVE_TEST_STDERR", repo.path.join("tracked.txt"))
                .env("GROOVE_OUTPUT", "json")
                .env("GROOVE_LOG_ENABLED", "false");
        }
        let mut child = TestChild::spawn(command.args([
            "exec",
            "--lease-id",
            lease["lease_id"].as_str().unwrap(),
            "--timeout-ms",
            "500",
            "--grace-ms",
            "0",
            "--",
            "bash",
            "-c",
            "head -c 2097152 /dev/zero; sleep 30",
        ]));
        assert_eq!(
            child.wait_timeout(Duration::from_secs(5)).unwrap().code(),
            Some(124)
        );
        let out = child.wait_with_output().unwrap();
        let result = checked(&out);
        assert_eq!(result["result"]["execution"]["termination"], "timed_out");
        let path = result["result"]["execution"]["run"]["stdout_path"]
            .as_str()
            .unwrap();
        assert_eq!(fs::metadata(path).unwrap().len(), 2097152);
        let shown = checked(&cli(
            &repo,
            &["show", "--lease-id", lease["lease_id"].as_str().unwrap()],
        ));
        assert!(shown["result"]["slot"]["execution"].is_null());
    }
}

#[test]
fn managed_environment_clears_git_overrides_and_allows_explicit_command_settings() {
    let repo = Repo::new();
    let lease = reserve(&repo);
    let id = lease["lease_id"].as_str().unwrap();
    let out = command(&repo, &["exec", "--lease-id", id, "--", "bash", "-c",
        "printf '%s:%s:%s' \"${GIT_AUTHOR_NAME-unset}\" \"${GIT_SSH_COMMAND-unset}\" \"$AUDIT_CUSTOM\""])
        .env("GIT_AUTHOR_NAME", "inherited author")
        .env("GIT_SSH_COMMAND", "custom-ssh")
        .env("AUDIT_CUSTOM", "kept").output().unwrap();
    let result = assert_success(&out);
    let stdout = result["result"]["execution"]["run"]["stdout_path"]
        .as_str()
        .unwrap();
    assert_eq!(fs::read_to_string(stdout).unwrap(), "unset:unset:kept");
    let out = cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--",
            "env",
            "GIT_AUTHOR_NAME=explicit",
            "bash",
            "-c",
            "printf '%s' \"$GIT_AUTHOR_NAME\"",
        ],
    );
    let result = assert_success(&out);
    let stdout = result["result"]["execution"]["run"]["stdout_path"]
        .as_str()
        .unwrap();
    assert_eq!(fs::read_to_string(stdout).unwrap(), "explicit");
    release(&repo, id);
}
