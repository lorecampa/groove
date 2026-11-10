mod support;
use serde_json::Value;
use support::{assert_success, Repo};

fn cli(r: &Repo, args: &[&str]) -> std::process::Output {
    support::groove()
        .env("GROOVE_LOG_DIR", r.path.join("test-logs"))
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env_remove("GROOVE_LOG_ENABLED")
        .arg("--repo")
        .arg(&r.path)
        .args(args)
        .output()
        .unwrap()
}
fn check(output: &std::process::Output) -> Value {
    let v: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}: {v}");
    v
}
#[test]
fn every_output_path_is_one_json_response() {
    let r = Repo::new();
    for (args, exit, text) in [
        (vec!["--help"], 0, Some("Usage:")),
        (vec!["--version"], 0, Some(env!("CARGO_PKG_VERSION"))),
        (vec![], 2, None),
        (vec!["unknown"], 2, None),
        (vec!["init", "--capacity", "0"], 2, None),
        (vec!["acquire", "--lease-channel-fd", "3"], 2, None),
        (vec!["release", "--lease-id", "no"], 2, None),
        (vec!["release"], 2, None),
    ] {
        let output = cli(&r, &args);
        let response = check(&output);
        assert_eq!(output.status.code(), Some(exit), "{args:?}: {response}");
        assert_eq!(response["ok"], exit == 0, "{args:?}: {response}");
        if let Some(text) = text {
            assert!(response["result"]["text"].as_str().unwrap().contains(text));
        } else {
            assert_eq!(response["error"]["code"], "INVALID_ARGUMENT", "{args:?}");
        }
        if exit == 0 {
            assert!(output.stderr.is_empty());
        } else {
            assert!(String::from_utf8_lossy(&output.stderr).contains("INVALID_ARGUMENT"));
        }
        assert!(
            !r.state().exists(),
            "{args:?} unexpectedly initialized a pool"
        );
        assert_eq!(
            std::fs::read(r.path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
    }
    let output = cli(&r, &["init", "--capacity", "2"]);
    assert_success(&output);
    let v = check(&output);
    assert_eq!(v["request_id"], Value::Null);
    assert_eq!(v["result"]["created"], true);
    let output = cli(&r, &["--request-id", "again", "init", "--capacity", "2"]);
    assert_eq!(check(&output)["error"]["code"], "CONFIG_CONFLICT");
    assert_eq!(output.status.code(), Some(4));
    let output = cli(&r, &["-v", "init", "--capacity", "3"]);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(check(&output)["error"]["code"], "CONFIG_CONFLICT");
    assert!(String::from_utf8_lossy(&output.stderr).contains("CONFIG_CONFLICT"));
}
#[test]
fn concurrent_init_creates_exactly_one_pool() {
    let r = Repo::new();
    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(
            support::groove()
                .arg("--repo")
                .arg(&r.path)
                .args(["init", "--capacity", "2"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap(),
        );
    }
    let mut created = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        let response = check(&out);
        if response["ok"] == true {
            assert_success(&out);
            assert_eq!(response["result"]["created"], true);
            created += 1;
        } else {
            assert_eq!(out.status.code(), Some(4));
            assert_eq!(response["error"]["code"], "CONFIG_CONFLICT");
        }
    }
    assert_eq!(created, 1);
}

#[test]
fn persisted_metadata_matches_published_schemas() {
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
    let pool = groove_core::Pool::open(&r.path).unwrap();
    let mut lease = pool.acquire(Default::default()).unwrap();
    for (file, text) in [
        (
            "config.json",
            include_str!("../schemas/config-v1.schema.json"),
        ),
        (
            "slots/wt-0001.json",
            include_str!("../schemas/slot-v1.schema.json"),
        ),
    ] {
        let schema: Value = serde_json::from_str(text).unwrap();
        let record: Value =
            serde_json::from_slice(&std::fs::read(r.state().join(file)).unwrap()).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let errors: Vec<_> = validator
            .iter_errors(&record)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{file}: {errors:?}");
    }
    lease.release().unwrap();
}

#[test]
fn settings_and_read_commands_match_the_protocol() {
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
    let lease = check(&cli(&r, &["acquire", "--task", "live"]));
    let id = lease["result"]["lease_id"].as_str().unwrap();
    let configured = cli(
        &r,
        &["config", "set", "--capacity", "3", "--cleanup", "warm"],
    );
    assert_success(&configured);
    let response = check(&configured);
    assert_eq!(response["result"]["pool"]["capacity"], 3);
    assert_eq!(response["result"]["updated"], true);
    assert_eq!(
        check(&cli(&r, &["config", "show"]))["result"]["pool"]["cleanup_policy"],
        "warm"
    );
    assert_eq!(
        check(&cli(&r, &["list", "--slots"]))["result"]["inspection"]["slots"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        check(&cli(&r, &["show", "--lease-id", id]))["result"]["lease"]["task"],
        "live"
    );
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/config-v1.schema.json")).unwrap();
    let record: Value =
        serde_json::from_slice(&std::fs::read(r.state().join("config.json")).unwrap()).unwrap();
    assert!(jsonschema::validator_for(&schema)
        .unwrap()
        .is_valid(&record));
    // Removed spellings fail as argument errors rather than doing something else.
    for old in [
        vec!["save"],
        vec!["config", "--global"],
        vec!["show", "--snapshot", id],
        vec!["list", "--snapshots"],
        vec!["init", "--capacity", "1", "--backend", "git"],
        vec![
            "exec",
            "--lease-id",
            id,
            "--cgroup-parent",
            "/",
            "--",
            "true",
        ],
        vec!["inspect"],
        vec!["leases"],
    ] {
        let out = cli(&r, &old);
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(check(&out)["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_success(&cli(&r, &["release", "--lease-id", id]));
}

#[test]
fn malformed_identifiers_are_argument_errors_that_keep_the_request_id() {
    let r = Repo::new();
    for args in [
        ["show", "--slot", "../outside"],
        ["show", "--slot", "wt-0000"],
        ["show", "--slot", "wt-4097"],
        ["show", "--slot", "wt-1"],
        ["release", "--lease-id", "not-a-uuid"],
    ] {
        let output = cli(
            &r,
            &[&["--request-id", "invalid-id"][..], &args[..]].concat(),
        );
        assert_eq!(output.status.code(), Some(2));
        let response = check(&output);
        assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(response["request_id"], "invalid-id");
    }
}

#[test]
fn published_slot_schema_agrees_with_validated_rust_ids() {
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema["$defs"]["slot"]).unwrap();
    for number in 0..=9999 {
        let text = format!("wt-{number:04}");
        assert_eq!(
            validator.is_valid(&serde_json::json!(text)),
            text.parse::<groove_core::SlotId>().is_ok(),
            "{text}"
        );
    }
}

#[test]
fn logging_flags_preserve_protocol_and_failures_are_best_effort() {
    let r = Repo::new();
    let logs = r.path.join("test-logs");
    let output = cli(&r, &["init", "--capacity", "1"]);
    assert_success(&output);
    assert!(output.stderr.is_empty());
    let entries: Vec<_> = std::fs::read_dir(&logs)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .collect();
    assert_eq!(entries.len(), 1);
    let contents = std::fs::read_to_string(entries[0].path()).unwrap();
    assert!(contents.contains("Command completed"));
    for line in contents.lines() {
        serde_json::from_str::<Value>(line).unwrap();
    }
    let output = support::groove()
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env("GROOVE_LOG_ENABLED", "false")
        .arg("--repo")
        .arg(&r.path)
        .args(["-vvv", "acquire"])
        .output()
        .unwrap();
    let lease = assert_success(&output)["result"].clone();
    assert!(String::from_utf8_lossy(&output.stderr).contains("TRACE"));
    // A regular file cannot be a log directory. Release still succeeds.
    let blocked = r.path.join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    let output = support::groove()
        .arg("--repo")
        .arg(&r.path)
        .env("GROOVE_LOG_DIR", blocked)
        .env("XDG_CONFIG_HOME", r.path.join("config-home"))
        .env_remove("GROOVE_LOG_ENABLED")
        .args(["release", "--lease-id", lease["lease_id"].as_str().unwrap()])
        .output()
        .unwrap();
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("WARN File logging unavailable"));
    assert_eq!(
        std::fs::read_dir(&logs)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .count(),
        1
    );
    let removed = cli(&r, &["--diagnostics", "list"]);
    assert_eq!(check(&removed)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn logging_controls_apply_to_help_and_invalid_arguments() {
    let r = Repo::new();
    let logs = r.path.join("test-logs");
    for args in [vec!["--help"], vec!["unknown"]] {
        let out = support::groove()
            .env("XDG_CONFIG_HOME", r.path.join("config-home"))
            .env("GROOVE_LOG_DIR", &logs)
            .env("GROOVE_LOG_ENABLED", "false")
            .args(args)
            .output()
            .unwrap();
        check(&out);
        assert!(!logs.exists());
    }
    let out = cli(&r, &["-v", "unknown"]);
    assert_eq!(check(&out)["error"]["code"], "INVALID_ARGUMENT");
    assert!(String::from_utf8_lossy(&out.stderr).contains("INVALID_ARGUMENT"));
    assert!(std::fs::read_dir(logs)
        .unwrap()
        .flatten()
        .any(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl")));
}

#[test]
fn command_help_is_available_without_a_repository_or_valid_config() {
    let temp = tempfile::tempdir().unwrap();
    let config_home = temp.path().join("config");
    std::fs::create_dir_all(config_home.join("groove")).unwrap();
    std::fs::write(config_home.join("groove/config.toml"), "invalid toml [").unwrap();
    let commands: &[&[&str]] = &[
        &[],
        &["init"],
        &["acquire"],
        &["release"],
        &["show"],
        &["list"],
        &["completions"],
        &["history"],
        &["history", "show"],
        &["history", "delete"],
        &["storage"],
        &["storage", "show"],
        &["storage", "clean"],
        &["exec"],
        &["stop"],
        &["recover"],
        &["finish"],
        &["config"],
        &["config", "show"],
        &["config", "set"],
        &["config", "project"],
        &["config", "global"],
        &["config", "global", "path"],
        &["config", "global", "edit"],
        &["config", "global", "set"],
        &["snapshot"],
        &["snapshot", "save"],
        &["snapshot", "list"],
        &["snapshot", "diff"],
        &["snapshot", "restore"],
        &["snapshot", "delete"],
    ];
    for command in commands {
        for flag in ["-h", "--help"] {
            let output = support::groove()
                .current_dir(temp.path())
                .env("XDG_CONFIG_HOME", &config_home)
                .env("GROOVE_LOG_ENABLED", "false")
                .args(*command)
                .arg(flag)
                .output()
                .unwrap();
            assert!(output.status.success(), "{command:?}: {output:?}");
            assert!(output.stderr.is_empty());
            let response = check(&output);
            assert_eq!(response["ok"], true);
            let help = response["result"]["text"].as_str().unwrap();
            assert!(help.contains("Usage:"), "{command:?}: {help}");
            if *command == ["release"] {
                assert!(help.contains("groove release --lease-id UUID"));
                assert!(!help.contains("--writers-stopped"));
                assert!(help.contains("next acquisition"));
            }
        }
    }
    let output = support::groove()
        .current_dir(temp.path())
        .env("XDG_CONFIG_HOME", &config_home)
        .env("GROOVE_LOG_ENABLED", "false")
        .args(["snapshot", "help", "restore"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let response = check(&output);
    let help = response["result"]["text"].as_str().unwrap();
    assert!(help.contains("safety snapshot"));
    assert!(help.contains("must not exist"));
    assert!(help.contains("HEAD must equal the"));
}

#[test]
fn human_output_is_the_default_and_json_is_opt_in() {
    let r = Repo::new();
    let human = |args: &[&str]| {
        support::groove()
            .env_remove("GROOVE_OUTPUT")
            .env("GROOVE_LOG_ENABLED", "false")
            .arg("--repo")
            .arg(&r.path)
            .args(args)
            .output()
            .unwrap()
    };
    let init = human(&["init", "--capacity", "1"]);
    assert!(init.status.success());
    let text = String::from_utf8(init.stdout).unwrap();
    assert!(text.starts_with("Initialized a pool of 1 slots."), "{text}");
    assert!(serde_json::from_str::<Value>(&text).is_err());
    let acquired = human(&["acquire", "--task", "human"]);
    assert!(String::from_utf8_lossy(&acquired.stdout).contains("reserved for 'human'"));
    let shown = String::from_utf8(human(&["show", "--task", "human"]).stdout).unwrap();
    let path = shown
        .lines()
        .find_map(|line| line.trim().strip_prefix("path:"))
        .unwrap()
        .trim();
    assert!(std::path::Path::new(path).is_dir(), "{shown}");
    // Errors go to stderr with the exit category; stdout stays empty.
    let missing = human(&["release", "--task", "absent"]);
    assert_eq!(missing.status.code(), Some(5));
    assert!(missing.stdout.is_empty());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("LEASE_MISMATCH"));
    // --json overrides the human default.
    let listed = human(&["--json", "list"]);
    assert_eq!(check(&listed)["result"]["leases"][0]["task"], "human");
    // Help is plain text unless JSON is requested.
    let help = human(&["--help"]);
    assert!(String::from_utf8_lossy(&help.stdout).starts_with("Reusable Git worktree"));
    assert_eq!(check(&human(&["--json", "--version"]))["ok"], true);
}

#[test]
fn completions_preserve_output_modes_without_a_repository_or_valid_config() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("config");
    std::fs::create_dir_all(config.join("groove")).unwrap();
    std::fs::write(config.join("groove/config.toml"), "invalid toml [").unwrap();
    for shell in ["bash", "zsh", "fish", "powershell", "elvish"] {
        let run = |json_flag: bool, json_env: bool| {
            let mut command = support::groove();
            command
                .current_dir(temp.path())
                .env("XDG_CONFIG_HOME", &config)
                .env("GROOVE_OUTPUT", if json_env { "json" } else { "human" })
                .args(["completions", shell]);
            if json_flag {
                command.arg("--json");
            }
            command.output().unwrap()
        };
        let human = run(false, false);
        assert!(human.status.success(), "{human:?}");
        assert!(human.stderr.is_empty());
        let script = String::from_utf8(human.stdout).unwrap();
        assert!(script.contains("details"), "{shell}");
        assert!(script.contains("completions"));
        for (flag, env) in [(true, false), (false, true)] {
            let json = run(flag, env);
            assert_success(&json);
            assert!(json.stderr.is_empty());
            assert_eq!(check(&json)["result"]["text"], script);
        }
        if shell == "bash" {
            let path = temp.path().join("groove.bash");
            std::fs::write(&path, script).unwrap();
            let output = std::process::Command::new("bash")
                .args([
                    "--noprofile",
                    "--norc",
                    "-c",
                    "source \"$1\"; COMP_WORDS=(groove sh); COMP_CWORD=1; _groove groove sh groove; printf '%s\\n' \"${COMPREPLY[@]}\"",
                    "completion-test",
                ])
                .arg(path)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "show");
        }
    }
    let invalid = support::groove()
        .current_dir(temp.path())
        .args(["completions", "unknown-shell"])
        .env("GROOVE_LOG_ENABLED", "false")
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
}

#[test]
fn common_failures_have_the_same_advice_in_human_and_json_output() {
    let r = Repo::new();
    let expect_hint = |args: &[&str], code: &str, status: i32, action: &str| {
        let json = cli(&r, args);
        let response = check(&json);
        assert_eq!(json.status.code(), Some(status));
        assert!(String::from_utf8_lossy(&json.stderr).contains(code));
        assert_eq!(response["error"]["code"], code);
        let hint = response["error"]["hint"].as_str().unwrap();
        assert!(hint.contains(action), "{hint}");
        assert!(!hint.contains("--force"));
        let human = support::groove()
            .env_remove("GROOVE_OUTPUT")
            .env("GROOVE_LOG_ENABLED", "false")
            .env("XDG_CONFIG_HOME", r.path.join("config-home"))
            .arg("--repo")
            .arg(&r.path)
            .args(args)
            .output()
            .unwrap();
        assert_eq!(human.status.code(), Some(status));
        assert!(human.stdout.is_empty());
        assert!(String::from_utf8_lossy(&human.stderr).contains(&format!("Hint: {hint}")));
    };
    expect_hint(&["list"], "NOT_INITIALIZED", 4, "groove init");
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
    assert_success(&cli(&r, &["acquire", "--task", "busy"]));
    expect_hint(&["acquire"], "POOL_EXHAUSTED", 3, "--wait-ms");
    expect_hint(
        &["acquire", "--base", "missing-revision"],
        "INVALID_REVISION",
        4,
        "--base",
    );
    expect_hint(
        &["show", "--task", "absent"],
        "LEASE_MISMATCH",
        5,
        "groove list",
    );
    std::fs::write(r.slot(1).join("tracked.txt"), "unsaved\n").unwrap();
    expect_hint(
        &["release", "--task", "busy"],
        "UNSAVED_CHANGES",
        5,
        "groove finish",
    );
    assert_eq!(
        check(&cli(&r, &["list"]))["result"]["leases"][0]["task"],
        "busy"
    );
}

#[test]
fn selected_show_runs_git_status_only_in_the_selected_workspace() {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "2"]));
    let first = check(&cli(&r, &["acquire", "--task", "first"]));
    let second = assert_success(&cli(&r, &["acquire", "--task", "second"]));
    let first_path = first["result"]["worktree_path"].as_str().unwrap();
    let second_path = second["result"]["worktree_path"].as_str().unwrap();
    fs::write(
        std::path::Path::new(first_path).join("tracked.txt"),
        "edited\n",
    )
    .unwrap();
    let real_git = Command::new("bash")
        .args(["--noprofile", "--norc", "-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let real_git = String::from_utf8(real_git.stdout).unwrap();
    let bin = r.temp.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let log = r.temp.path().join("git-status-calls");
    let wrapper = bin.join("git");
    fs::write(
        &wrapper,
        "#!/bin/bash\nfor arg in \"$@\"; do\n  if [[ $arg == status ]]; then printf '%s\\n' \"$PWD\" >> \"$GROOVE_STATUS_CALLS\"; fi\ndone\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n",
    ).unwrap();
    fs::set_permissions(wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(paths).unwrap();
    let run = |args: &[&str]| {
        fs::write(&log, "").unwrap();
        let output = support::groove_in(&r.path)
            .env("PATH", &path)
            .env("GROOVE_REAL_GIT", real_git.trim())
            .env("GROOVE_STATUS_CALLS", &log)
            .env("GROOVE_LOG_ENABLED", "false")
            .env("XDG_CONFIG_HOME", r.temp.path().join("config"))
            .args(args)
            .output()
            .unwrap();
        (output, fs::read_to_string(&log).unwrap())
    };
    for selector in [
        ["--task", "first"],
        ["--lease-id", first["result"]["lease_id"].as_str().unwrap()],
        ["--slot", first["result"]["slot_id"].as_str().unwrap()],
    ] {
        let (output, calls) = run(&["show", selector[0], selector[1], "--details"]);
        assert_success(&output);
        let response = check(&output);
        assert_eq!(
            response["result"]["slot"]["git"]["modified"], true,
            "{response}"
        );
        assert_eq!(calls.lines().collect::<Vec<_>>(), [first_path]);
        let (output, calls) = run(&["show", selector[0], selector[1]]);
        assert_success(&output);
        assert!(check(&output)["result"]["slot"]["git"].is_null());
        assert!(calls.is_empty());
    }
    for command in ["show", "list"] {
        let (output, calls) = run(&[command, "--details"]);
        assert_success(&output);
        check(&output);
        let mut expected = [first_path, second_path];
        expected.sort();
        assert_eq!(calls.lines().collect::<Vec<_>>(), expected);
    }
    let (output, calls) = run(&["show", "--slot", "wt-0003", "--details"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(check(&output)["error"]["code"], "INVALID_ARGUMENT");
    assert!(calls.is_empty());
}

#[test]
fn terminal_verbosity_and_file_logging_are_independent() {
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
    for (flag, levels) in [
        (None, vec![]),
        (Some("-q"), vec![]),
        (Some("-v"), vec!["INFO"]),
        (Some("-vv"), vec!["INFO", "DEBUG"]),
        (Some("-vvv"), vec!["INFO", "DEBUG", "TRACE"]),
    ] {
        let logs = r.temp.path().join(flag.unwrap_or("default"));
        let mut cmd = support::groove_in(&r.path);
        cmd.env("XDG_CONFIG_HOME", r.path.join("config-home"))
            .env("GROOVE_LOG_DIR", &logs)
            .env("GROOVE_LOG_ENABLED", "true");
        if let Some(flag) = flag {
            cmd.arg(flag);
        }
        let out = cmd.arg("list").output().unwrap();
        assert_success(&out);
        check(&out);
        let text = String::from_utf8(out.stderr).unwrap();
        for level in ["INFO", "DEBUG", "TRACE"] {
            assert_eq!(
                text.contains(&format!(" {level} ")),
                levels.contains(&level),
                "{flag:?}: {text}"
            );
        }
        let log = std::fs::read_dir(logs)
            .unwrap()
            .flatten()
            .find(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
            .unwrap();
        let contents = std::fs::read_to_string(log.path()).unwrap();
        let events: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert!(events.iter().any(|v| v["level"] == "DEBUG"));
        assert!(!events.iter().any(|v| v["level"] == "TRACE"));
    }
    let blocked = r.temp.path().join("blocked");
    std::fs::write(&blocked, "file").unwrap();
    for quiet in [false, true] {
        for json in [false, true] {
            let mut cmd = support::groove_in(&r.path);
            cmd.env("XDG_CONFIG_HOME", r.path.join("config-home"))
                .env("GROOVE_LOG_DIR", &blocked)
                .env("GROOVE_LOG_ENABLED", "true");
            if quiet {
                cmd.arg("--quiet");
            }
            if !json {
                cmd.env_remove("GROOVE_OUTPUT");
            }
            let out = cmd.args(["show", "--task", "absent"]).output().unwrap();
            assert_eq!(out.status.code(), Some(5));
            if json {
                assert_eq!(check(&out)["error"]["code"], "LEASE_MISMATCH");
            } else {
                assert!(out.stdout.is_empty());
            }
            let stderr = String::from_utf8(out.stderr).unwrap();
            assert_eq!(stderr.contains("WARN File logging unavailable"), !quiet);
            assert_eq!(stderr.matches("(LEASE_MISMATCH)").count(), 1);
            assert!(!stderr.contains("Command failed"));
        }
    }
    let invalid = cli(&r, &["--quiet", "-v", "list"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
    let invalid = cli(&r, &["--quiet", "unknown"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("INVALID_ARGUMENT"));
}

#[test]
fn persistent_trace_can_be_enabled_while_terminal_is_quiet() {
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
    assert_success(&cli(
        &r,
        &["config", "global", "set", "logging.level=trace"],
    ));
    assert_eq!(
        check(&cli(&r, &["config", "global", "show"]))["result"]["config"]["logging"]["level"],
        "trace"
    );
    let out = cli(&r, &["--quiet", "list"]);
    assert_success(&out);
    assert!(out.stderr.is_empty());
    let events: Vec<Value> = std::fs::read_dir(r.path.join("test-logs"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .flat_map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap()
                .lines()
                .map(|s| serde_json::from_str(s).unwrap())
                .collect::<Vec<Value>>()
        })
        .collect();
    assert!(events
        .iter()
        .any(|v| v["command"] == "list" && v["level"] == "TRACE"));
    let invalid = cli(&r, &["config", "global", "set", "logging.level=invalid"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert_eq!(check(&invalid)["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        check(&cli(&r, &["config", "global", "show"]))["result"]["config"]["logging"]["level"],
        "trace"
    );
}

#[test]
fn history_and_storage_commands_preserve_json_contract_and_retention_semantics() {
    let r = Repo::new();
    assert_success(&cli(&r, &["init", "--capacity", "1"]));
    let acquire = cli(&r, &["acquire", "--task", "history-fixture"]);
    assert_success(&acquire);
    check(&acquire);
    let executed = cli(
        &r,
        &[
            "exec",
            "--task",
            "history-fixture",
            "--",
            "bash",
            "-c",
            "printf historic-output; exit 4",
        ],
    );
    assert_eq!(executed.status.code(), Some(4));
    let run = check(&executed)["result"]["execution"]["run"].clone();
    let id = run["run_id"].as_str().unwrap();
    let listed = cli(&r, &["history", "--task", "history-fixture"]);
    assert_success(&listed);
    let history = check(&listed);
    assert_eq!(history["result"]["runs"].as_array().unwrap().len(), 1);
    assert_eq!(history["result"]["runs"][0]["exit_code"], 4);
    let shown = cli(&r, &["history", "show", id]);
    assert_success(&shown);
    assert_eq!(check(&shown)["result"]["run"]["logs"], "present");
    let persisted: Value = serde_json::from_slice(
        &std::fs::read(
            std::path::Path::new(run["stdout_path"].as_str().unwrap())
                .parent()
                .unwrap()
                .join("run.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        persisted,
        check(&shown)["result"]["run"],
        "the persisted record must match the published history schema"
    );
    let usage = cli(&r, &["storage", "show"]);
    assert_success(&usage);
    assert_eq!(check(&usage)["result"]["storage"]["log_bytes"], 15);
    let preview = cli(
        &r,
        &[
            "storage",
            "clean",
            "--logs",
            "--older-than",
            "0s",
            "--dry-run",
        ],
    );
    assert_success(&preview);
    assert_eq!(
        check(&preview)["result"]["cleanup"]["entries"][0]["status"],
        "would_remove"
    );
    assert!(std::path::Path::new(run["stdout_path"].as_str().unwrap()).exists());
    let rejected = cli(&r, &["history", "delete", id]);
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(check(&rejected)["error"]["code"], "INVALID_ARGUMENT");
    let human = support::groove_in(&r.path)
        .env("GROOVE_OUTPUT", "human")
        .args(["history", "show", id])
        .output()
        .unwrap();
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout).unwrap();
    for value in [
        id,
        "exit_code: 4",
        "outcome: completed",
        "logs: present",
        "history-fixture",
    ] {
        assert!(human.contains(value), "{human}");
    }
    let stdout = std::path::Path::new(run["stdout_path"].as_str().unwrap());
    let outside = r.temp.path().join("outside-log");
    std::fs::write(&outside, "external data").unwrap();
    std::fs::remove_file(stdout).unwrap();
    std::os::unix::fs::symlink(&outside, stdout).unwrap();
    let failed = cli(&r, &["storage", "clean", "--logs", "--older-than", "0s"]);
    assert_eq!(failed.status.code(), Some(7));
    let report = check(&failed);
    assert_eq!(report["ok"], true);
    assert_eq!(
        report["result"]["cleanup"]["entries"][0]["status"],
        "failed"
    );
    assert!(report["result"]["cleanup"]["entries"][0]["error_code"].is_string());
    assert_eq!(std::fs::read(&outside).unwrap(), b"external data");
    std::fs::remove_file(stdout).unwrap();
    let cleaned = cli(&r, &["storage", "clean", "--logs", "--older-than", "0s"]);
    assert_success(&cleaned);
    assert_eq!(
        check(&cleaned)["result"]["cleanup"]["entries"][0]["status"],
        "removed"
    );
    let shown = cli(&r, &["history", "show", id]);
    assert_eq!(check(&shown)["result"]["run"]["exit_code"], 4);
    let deleted = cli(&r, &["history", "delete", id]);
    assert_success(&deleted);
    assert_eq!(check(&deleted)["result"]["deleted_run_id"], id);
    assert_eq!(
        check(&cli(&r, &["history"]))["result"]["runs"],
        serde_json::json!([])
    );
    for args in [
        vec!["storage", "clean"],
        vec!["storage", "clean", "--logs", "--older-than", "yesterday"],
        vec!["history", "show", "../outside"],
    ] {
        let rejected = cli(&r, &args);
        assert_eq!(rejected.status.code(), Some(2));
        assert_eq!(check(&rejected)["error"]["code"], "INVALID_ARGUMENT");
    }
}
