//! CLI contracts for situations users and agents create outside Groove: deleted
//! working directories and checkouts, reused labels, hostile label text and
//! undeliverable responses. Each asserts the exit category, the versioned JSON
//! error and the preserved reservation state.

#[path = "support/json_progress.rs"]
mod json_progress;
mod support;
use serde_json::Value;
use std::{fs, path::Path, process::Output};
use support::{assert_success, Repo};
#[test]
fn common_setup_errors_name_the_missing_requirement_or_path() {
    let repo = Repo::new();
    let absent = repo.temp.path().join("nonexistent repository");
    let missing = failure(
        &cli(&repo.path, &["--repo", absent.to_str().unwrap(), "list"]),
        2,
        "INVALID_ARGUMENT",
    );
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains(absent.to_str().unwrap()));
    assert_eq!(
        missing["error"]["details"]["path"],
        absent.to_str().unwrap()
    );
    assert!(!absent.exists());
    let outside = failure(&cli(repo.temp.path(), &["list"]), 7, "GIT_ERROR");
    assert!(outside["error"]["message"]
        .as_str()
        .unwrap()
        .contains("not a git repository"));
    let no_git = repo.temp.path().join("empty-path");
    fs::create_dir(&no_git).unwrap();
    let output = support::groove_in(&repo.path)
        .env("PATH", no_git)
        .arg("list")
        .output()
        .unwrap();
    let missing_git = failure(&output, 7, "IO_ERROR");
    assert!(missing_git["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Could not start Git"));
    assert!(missing_git["error"]["message"]
        .as_str()
        .unwrap()
        .contains("PATH"));
    let bare = repo.temp.path().join("bare.git");
    repo.git(&["init", "--bare", "-q", bare.to_str().unwrap()]);
    let output = cli(&bare, &["snapshot", "save"]);
    let error = failure(&output, 2, "UNSUPPORTED_CONFIGURATION");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("bare repositories"));
    assert!(!bare.join("groove-snapshots").exists());
}

#[test]
fn slot_table_exposes_failure_details_and_recovery_actions() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    ));
    fs::write(repo.state().join("slots/wt-0001.json"), "{broken").unwrap();
    let output = support::command(&repo.path, support::groove_binary())
        .env("HOME", repo.temp.path())
        .env("XDG_CONFIG_HOME", repo.temp.path().join("config"))
        .args(["list", "--slots"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("FAILURE"));
    assert!(text.contains("NEXT ACTION"));
    let inspected = checked(&cli(&repo.path, &["list", "--slots"]));
    let slot = &inspected["result"]["inspection"]["slots"][0];
    assert!(text.contains(slot["failure"].as_str().unwrap()));
    assert!(text.contains(slot["next_action"].as_str().unwrap()));
}

fn cli(path: &Path, args: &[&str]) -> Output {
    support::groove_in(path).args(args).output().unwrap()
}

fn checked(output: &Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&response)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {response}");
    response
}

fn failure(output: &Output, exit: i32, code: &str) -> Value {
    let response = checked(output);
    assert_eq!(output.status.code(), Some(exit), "{response}");
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], code, "{response}");
    response
}

#[test]
fn filter_validation_checks_the_whole_commit_from_a_subdirectory() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), b"*.txt filter=audit\n").unwrap();
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "-qm", "root filter rule"]);
    assert_success(&cli(
        &repo.path,
        &[
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let subdir = repo.path.join("subdir");
    fs::create_dir(&subdir).unwrap();
    let config = repo.temp.path().join("filters.gitconfig");
    fs::write(
        &config,
        "[filter \"audit\"]\nsmudge = false\nrequired = true\n",
    )
    .unwrap();
    let output = support::groove_in(&subdir)
        .env("GIT_CONFIG_GLOBAL", config)
        .arg("acquire")
        .output()
        .unwrap();
    failure(&output, 2, "UNSUPPORTED_CONFIGURATION");
    assert!(!repo.slot(1).exists());
}

#[test]
fn unused_global_filters_work_in_acquisition_and_the_common_directory_worker() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &[
            "pool",
            "init",
            "--capacity",
            "2",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let config = repo.temp.path().join("unused-filter.gitconfig");
    fs::write(
        &config,
        "[filter \"lfs\"]\nprocess = false\nrequired = true\n",
    )
    .unwrap();
    let pool = groove_core::Pool::open(&repo.path).unwrap();
    pool.configure(groove_core::PoolUpdate {
        ready_target: Some(1),
        ..Default::default()
    })
    .unwrap();
    let generation = pool.request_ready().unwrap().unwrap();
    let mut command = support::command(&repo.path, support::groove_binary());
    command
        .arg("--internal-ready-worker")
        .arg(pool.common_git_dir())
        .arg("default")
        .arg(generation.to_string())
        .arg("5000")
        .current_dir(pool.common_git_dir())
        .env("GROOVE_INTERNAL_READY_WORKER", "1")
        .env("GIT_CONFIG_GLOBAL", &config);
    let output = support::TestChild::spawn(&mut command)
        .wait_with_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = pool.ready_report().unwrap();
    assert_eq!(report.ready, 1, "{report:?}");
    assert!(report.error.is_none(), "{report:?}");
    assert_success(
        &support::groove_in(&repo.path)
            .env("GIT_CONFIG_GLOBAL", config)
            .arg("acquire")
            .output()
            .unwrap(),
    );
}

#[test]
#[cfg(feature = "test-hooks")]
fn filter_scratch_is_private_even_with_a_permissive_umask() {
    use std::{
        io::Write,
        os::unix::{fs::PermissionsExt, net::UnixListener},
    };
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    ));
    let config = repo.temp.path().join("unused-filter.gitconfig");
    fs::write(&config, "[filter \"lfs\"]\nprocess = false\n").unwrap();
    let socket = repo.temp.path().join("scratch.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::command(&repo.path, "sh");
    command
        .args(["-c", "umask 000; exec \"$@\"", "groove-private-scratch"])
        .arg(support::groove_binary())
        .args(["--json", "acquire"])
        .env("GIT_CONFIG_GLOBAL", config)
        .env("GROOVE_TEST_CHECKPOINT", "filter-scratch-created")
        .env("GROOVE_TEST_SOCKET", socket);
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = child
        .checkpoint(
            &listener,
            "filter-scratch-created",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    let scratch: Vec<_> = fs::read_dir(repo.path.join(".git"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("groove-filter-")
        })
        .collect();
    assert_eq!(scratch.len(), 1);
    assert_eq!(
        fs::metadata(&scratch[0]).unwrap().permissions().mode() & 0o777,
        0o700
    );
    stream.write_all(b"continue\n").unwrap();
    drop(stream);
    assert_success(&child.wait_with_output().unwrap());
    assert!(!scratch[0].exists());
}

#[test]
fn filter_scratch_layout_does_not_collide_with_an_index_directory() {
    let repo = Repo::new();
    fs::create_dir(repo.path.join("index")).unwrap();
    fs::write(
        repo.path.join("index/.gitattributes"),
        "*.txt text eol=lf\n",
    )
    .unwrap();
    repo.git(&["add", "index/.gitattributes"]);
    repo.git(&["commit", "-qm", "attributes below index directory"]);
    assert_success(&cli(
        &repo.path,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    ));
    let config = repo.temp.path().join("unused-filter.gitconfig");
    fs::write(
        &config,
        "[filter \"lfs\"]\nprocess = false\nrequired = true\n",
    )
    .unwrap();
    let acquired = assert_success(
        &support::groove_in(&repo.path)
            .env("GIT_CONFIG_GLOBAL", config)
            .arg("acquire")
            .output()
            .unwrap(),
    );
    let slot = Path::new(acquired["result"]["workspace_path"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(slot.join("index/.gitattributes")).unwrap(),
        "*.txt text eol=lf\n"
    );
}

#[test]
fn unused_filters_do_not_require_space_in_the_system_temporary_directory() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    ));
    let config = repo.temp.path().join("unused-filter.gitconfig");
    fs::write(
        &config,
        "[filter \"lfs\"]\nprocess = false\nrequired = true\n",
    )
    .unwrap();
    assert_success(
        &support::groove_in(&repo.path)
            .env("GIT_CONFIG_GLOBAL", config)
            .env("TMPDIR", repo.temp.path().join("unavailable-tmp"))
            .arg("acquire")
            .output()
            .unwrap(),
    );
}

#[test]
fn odd_filter_driver_names_are_refused_before_checkout() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &[
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let config = repo.temp.path().join("odd-filter.gitconfig");
    fs::write(&config, "[filter \"odd driver)\"]\nsmudge = false\n").unwrap();
    let output = support::groove_in(&repo.path)
        .env("GIT_CONFIG_GLOBAL", config)
        .arg("acquire")
        .output()
        .unwrap();
    let error = failure(&output, 2, "UNSUPPORTED_CONFIGURATION");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("odd driver)"));
    assert!(!repo.slot(1).exists());
}

#[test]
fn pool_acquisition_rejects_globally_defined_content_filters() {
    let repo = Repo::new();
    fs::write(repo.path.join(".gitattributes"), "*.txt filter=audit\n").unwrap();
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "-qm", "filter attributes"]);
    assert_success(&cli(
        &repo.path,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    ));
    let config = repo.temp.path().join("global.gitconfig");
    fs::write(
        &config,
        "[filter \"audit\"]\nclean = false\nsmudge = false\nrequired = true\n",
    )
    .unwrap();
    let output = support::groove_in(&repo.path)
        .env("GIT_CONFIG_GLOBAL", &config)
        .arg("acquire")
        .output()
        .unwrap();
    failure(&output, 2, "UNSUPPORTED_CONFIGURATION");
    assert!(!repo.slot(1).exists());
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    assert!(
        assert_success(&cli(&repo.path, &["list"]))["result"]["reservations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // The filter definition is checked, never executed by isolated maintenance.
    assert_success(&cli(&repo.path, &["acquire"]));
}

#[test]
fn json_argument_errors_survive_non_utf8_arguments_before_the_flag() {
    use std::os::unix::ffi::OsStringExt;
    let repo = Repo::new();
    let output = support::command(&repo.path, support::groove_binary())
        .env("HOME", repo.temp.path())
        .env("XDG_CONFIG_HOME", repo.temp.path().join("config"))
        .arg(std::ffi::OsString::from_vec(vec![0xff]))
        .args(["--json", "--request-id", "invalid-utf8"])
        .output()
        .unwrap();
    let response = failure(&output, 2, "INVALID_ARGUMENT");
    assert_eq!(response["request_id"], "invalid-utf8");
}

#[test]
fn argument_error_codes_stay_with_the_summary_before_usage() {
    let repo = Repo::new();
    let output = cli(&repo.path, &["unknown-command"]);
    failure(&output, 2, "INVALID_ARGUMENT");
    let stderr = String::from_utf8(output.stderr).unwrap();
    let summary = stderr
        .lines()
        .find(|line| line.starts_with("error: "))
        .unwrap();
    assert!(summary.ends_with("(INVALID_ARGUMENT)"));
    assert!(stderr.contains("Usage:"));
    assert!(!stderr
        .lines()
        .any(|line| line.trim() == "(INVALID_ARGUMENT)"));
}

#[test]
fn a_deleted_working_directory_is_reported_plainly() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["pool", "init", "--capacity", "1"]));
    let gone = repo.path.join("gone");
    fs::create_dir(&gone).unwrap();
    // The shell enters the directory, which is then removed underneath it, as when
    // a released checkout is rebuilt while an agent's shell still sits inside.
    let output = support::command(&gone, "sh")
        .args(["-c", "rmdir \"$PWD\" && exec \"$0\" --json list"])
        .arg(support::groove_binary())
        .output()
        .unwrap();
    let response = failure(&output, 2, "INVALID_ARGUMENT");
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no longer exists"),
        "{response}"
    );
}

#[test]
fn reusing_an_active_task_label_names_the_existing_reservation() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["pool", "init", "--capacity", "2"]));
    let first = assert_success(&cli(&repo.path, &["acquire", "--task", "fix"]));
    let response = failure(
        &cli(&repo.path, &["acquire", "--task", "fix"]),
        2,
        "INVALID_ARGUMENT",
    );
    assert_eq!(
        response["error"]["details"]["reservation_id"],
        first["result"]["reservation_id"]
    );
    assert_eq!(
        response["error"]["details"]["workspace_path"],
        first["result"]["workspace_path"]
    );
    let listed = checked(&cli(&repo.path, &["list"]));
    assert_eq!(
        listed["result"]["reservations"].as_array().unwrap().len(),
        1
    );
    // The existing reservation is still selectable by its label.
    assert_success(&cli(&repo.path, &["release", "--task", "fix"]));
}

#[test]
fn labels_cannot_carry_terminal_control_sequences() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["pool", "init", "--capacity", "2"]));
    failure(
        &cli(&repo.path, &["acquire", "--task", "\u{1b}]0;title\u{7}"]),
        2,
        "INVALID_ARGUMENT",
    );
    // Labels stored before validation existed are still escaped in human output.
    let lease = assert_success(&cli(&repo.path, &["acquire", "--task", "plain"]));
    let slot = lease["result"]["slot_id"].as_str().unwrap();
    let protection = repo.state().join(format!("slots/{slot}.protected.json"));
    let mut record: Value = serde_json::from_slice(&fs::read(&protection).unwrap()).unwrap();
    let hostile = "\u{1b}[31mred\u{1b}]0;pwned\u{7}\nfake row";
    record["lease"]["task"] = hostile.into();
    fs::write(&protection, serde_json::to_vec(&record).unwrap()).unwrap();
    for args in [
        &["list"][..],
        &["list", "--slots"],
        &["show", "--task", hostile],
    ] {
        let output = support::groove_human_in(&repo.path)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{args:?}: {output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(
            !text.contains(['\u{1b}', '\u{7}']),
            "{args:?} emitted control characters: {text:?}"
        );
        assert!(text.contains("red"), "{text:?}");
    }
    // JSON keeps the exact stored value, escaped by the JSON encoder.
    let listed = checked(&cli(&repo.path, &["list"]));
    assert_eq!(listed["result"]["reservations"][0]["task"], hostile);
}

#[test]
fn a_reserved_checkout_deleted_by_hand_needs_an_explicit_forced_release() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["pool", "init", "--capacity", "1"]));
    let lease = assert_success(&cli(&repo.path, &["acquire", "--task", "lost"]));
    let path = lease["result"]["workspace_path"].as_str().unwrap();
    fs::remove_dir_all(path).unwrap();

    for args in [
        &["release", "--task", "lost"][..],
        &["release", "--save", "--task", "lost"],
    ] {
        let response = failure(&cli(&repo.path, args), 6, "RECOVERY_FAILED");
        assert_eq!(response["error"]["details"]["workspace_path"], path);
        assert!(response["error"]["hint"].is_string());
    }
    let slots = checked(&cli(&repo.path, &["list", "--slots"]));
    assert_eq!(
        slots["result"]["inspection"]["slots"][0]["status"],
        "unhealthy"
    );
    // The reservation was kept; --force acknowledges the lost work.
    assert_success(&cli(&repo.path, &["release", "--force", "--task", "lost"]));
    let next = assert_success(&cli(&repo.path, &["acquire", "--task", "next"]));
    assert_eq!(next["result"]["workspace_path"], path);
    assert!(Path::new(path).join("tracked.txt").exists());
}

#[test]
fn undeliverable_responses_exit_seven_and_explain_real_write_failures() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["pool", "init", "--capacity", "1"]));
    // A closed reader: the caller must learn the response was lost, quietly.
    let closed = support::groove_in(&repo.path)
        .arg("list")
        .stdout(support::failed_output())
        .output()
        .unwrap();
    assert_eq!(closed.status.code(), Some(7));
    assert!(
        !String::from_utf8_lossy(&closed.stderr).contains("could not write"),
        "{closed:?}"
    );
    // Any other write failure is explained on stderr.
    if Path::new("/dev/full").exists() {
        for format in ["json", "human"] {
            let full = support::groove_human_in(&repo.path)
                .args(if format == "json" {
                    vec!["--json"]
                } else {
                    vec![]
                })
                .arg("list")
                .stdout(
                    fs::OpenOptions::new()
                        .write(true)
                        .open("/dev/full")
                        .unwrap(),
                )
                .output()
                .unwrap();
            assert_eq!(full.status.code(), Some(7), "{format}");
            assert!(
                String::from_utf8_lossy(&full.stderr).contains("could not write"),
                "{format}: {full:?}"
            );
        }
    }
}

#[test]
fn human_patches_reach_stdout_byte_for_byte() {
    // Escaping protects Groove-composed text, never file content: a patch with
    // CRLF lines must still apply after `groove snapshot diff ID > file`.
    let repo = Repo::new();
    fs::write(repo.path.join("tracked.txt"), "crlf line\r\nsecond\r\n").unwrap();
    let saved = assert_success(&cli(&repo.path, &["snapshot", "save", "crlf"]));
    let id = saved["result"]["snapshot"]["id"].as_str().unwrap();
    repo.git(&["checkout", "--", "tracked.txt"]);
    let output = support::groove_human_in(&repo.path)
        .args(["snapshot", "diff", id])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let json = checked(&cli(&repo.path, &["snapshot", "diff", id]));
    let patch = json["result"]["patch"].as_str().unwrap();
    assert!(patch.contains("\r\n"), "{patch:?}");
    // Human output only completes a missing final newline.
    let expected = if patch.ends_with('\n') {
        patch.to_owned()
    } else {
        format!("{patch}\n")
    };
    assert_eq!(output.stdout, expected.as_bytes());
    let file = repo.temp.path().join("crlf.patch");
    fs::write(&file, &output.stdout).unwrap();
    repo.git(&["apply", "-R", file.to_str().unwrap()]);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"crlf line\r\nsecond\r\n"
    );
}

#[test]
fn redirected_human_and_json_patches_apply_with_forced_color_and_custom_prefixes() {
    let repo = Repo::new();
    repo.git(&["config", "color.ui", "always"]);
    repo.git(&["config", "diff.noprefix", "true"]);
    repo.git(&["config", "diff.srcprefix", "old/"]);
    repo.git(&["config", "diff.dstprefix", "new/"]);
    let before = assert_success(&cli(&repo.path, &["snapshot", "save", "before"]));
    fs::write(repo.path.join("tracked.txt"), b"changed\r\nsecond\r\n").unwrap();
    fs::write(repo.path.join("binary"), b"binary\0\xff").unwrap();
    let after = assert_success(&cli(&repo.path, &["snapshot", "save", "after"]));
    let args = [
        "snapshot",
        "diff",
        before["result"]["snapshot"]["id"].as_str().unwrap(),
        after["result"]["snapshot"]["id"].as_str().unwrap(),
    ];
    let human_file = repo.temp.path().join("human.patch");
    let human = support::groove_human_in(&repo.path)
        .env("GROOVE_LOG_ENABLED", "false")
        .args(args)
        .stdout(fs::File::create(&human_file).unwrap())
        .output()
        .unwrap();
    assert!(human.status.success(), "{human:?}");
    assert!(human.stderr.is_empty(), "{human:?}");
    let output = support::groove_in(&repo.path)
        .env("GROOVE_LOG_ENABLED", "false")
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    json_progress::assert_only(&output, "snapshot");
    let json = checked(&output);
    assert_eq!(json["ok"], true);
    let patch = json["result"]["patch"].as_str().unwrap();
    assert!(!patch.contains('\u{1b}'));
    assert_eq!(fs::read(&human_file).unwrap(), patch.as_bytes());
    let json_file = repo.temp.path().join("json.patch");
    fs::write(&json_file, patch).unwrap();
    let index = repo.temp.path().join("scratch.index");
    for patch_file in [&human_file, &json_file] {
        for git_args in [
            vec![
                "read-tree",
                before["result"]["snapshot"]["work_tree"].as_str().unwrap(),
            ],
            vec!["apply", "--cached", patch_file.to_str().unwrap()],
            vec!["write-tree"],
        ] {
            let applied = support::command(&repo.path, "git")
                .env("GIT_INDEX_FILE", &index)
                .args(&git_args)
                .output()
                .unwrap();
            assert!(applied.status.success(), "{git_args:?}: {applied:?}");
            if git_args[0] == "write-tree" {
                assert_eq!(
                    String::from_utf8(applied.stdout).unwrap().trim(),
                    after["result"]["snapshot"]["work_tree"].as_str().unwrap()
                );
            }
        }
    }
}

/// An agent's own `git status` with `core.fsmonitor=true` leaves a Git filesystem
/// monitor daemon running for the slot. Git moves the daemon's working directory
/// out of the checkout and watches it by path, so it must neither block release as a
/// process still using the workspace nor serve stale state to the next reservation
/// after in-place cleanup. Linux Git has no built-in monitor daemon.
#[cfg(target_os = "macos")]
#[test]
fn a_filesystem_monitor_daemon_neither_blocks_release_nor_goes_stale() {
    struct Daemon<'a>(&'a Path);
    impl Drop for Daemon<'_> {
        fn drop(&mut self) {
            let _ = support::command(self.0, "git")
                .args(["fsmonitor--daemon", "stop"])
                .output();
        }
    }

    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["pool", "init", "--capacity", "1"]));
    let lease = assert_success(&cli(&repo.path, &["acquire", "--task", "watched"]));
    let path = Path::new(lease["result"]["workspace_path"].as_str().unwrap()).to_owned();
    support::git(&path, &["fsmonitor--daemon", "start"]);
    let _daemon = Daemon(&path);
    assert!(
        support::git(&path, &["fsmonitor--daemon", "status"]).contains("watching"),
        "the daemon did not start"
    );
    fs::write(path.join("seen-by-daemon.txt"), "temporary\n").unwrap();
    let monitored = [
        "-c",
        "core.fsmonitor=true",
        "status",
        "--porcelain",
        "-uall",
    ];
    assert!(support::git(&path, &monitored).contains("seen-by-daemon.txt"));
    fs::remove_file(path.join("seen-by-daemon.txt")).unwrap();

    assert_success(&cli(&repo.path, &["release", "--task", "watched"]));

    let next = assert_success(&cli(&repo.path, &["acquire", "--task", "next"]));
    assert_eq!(
        Path::new(next["result"]["workspace_path"].as_str().unwrap()),
        path
    );
    assert_eq!(support::git(&path, &monitored), "");
    fs::write(path.join("after-reuse.txt"), "new\n").unwrap();
    assert_eq!(support::git(&path, &monitored), "?? after-reuse.txt");
    fs::remove_file(path.join("after-reuse.txt")).unwrap();
    assert_success(&cli(&repo.path, &["release", "--task", "next"]));
}

#[test]
fn released_lease_history_is_explicitly_inactive() {
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    ));
    let lease = assert_success(&cli(&repo.path, &["acquire", "--task", "former-task"]));
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    assert_eq!(
        assert_success(&cli(&repo.path, &["show", "--reservation-id", id]))["result"]["active"],
        true
    );
    assert_success(&cli(&repo.path, &["release", "--reservation-id", id]));
    let shown = checked(&cli(&repo.path, &["show", "--reservation-id", id]));
    assert_eq!(shown["result"]["active"], false);
    let slots = checked(&cli(&repo.path, &["list", "--slots"]));
    assert_eq!(
        slots["result"]["inspection"]["slots"][0]["reservation_active"],
        false
    );
    let human = support::command(&repo.path, support::groove_binary())
        .env("HOME", repo.temp.path())
        .env("XDG_CONFIG_HOME", repo.temp.path().join("config"))
        .args(["list", "--slots"])
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(!String::from_utf8(human.stdout)
        .unwrap()
        .contains("former-task"));
}

#[test]
#[cfg(feature = "test-hooks")]
fn killed_filter_validation_scratch_is_reclaimed_by_the_next_owner() {
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    assert_success(&cli(
        &repo.path,
        &[
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    ));
    let config = repo.temp.path().join("unused-filter.gitconfig");
    fs::write(&config, "[filter \"lfs\"]\nprocess = false\n").unwrap();
    let socket = repo.temp.path().join("scratch-kill.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["acquire"])
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GROOVE_TEST_CHECKPOINT", "filter-scratch-created")
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(
            &listener,
            "filter-scratch-created",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    let scratch: Vec<_> = fs::read_dir(repo.path.join(".git"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("groove-filter-")
        })
        .collect();
    assert_eq!(scratch.len(), 1);
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    assert!(scratch[0].exists());
    assert_success(
        &support::groove_in(&repo.path)
            .arg("acquire")
            .env("GIT_CONFIG_GLOBAL", &config)
            .output()
            .unwrap(),
    );
    assert!(!scratch[0].exists());
}
