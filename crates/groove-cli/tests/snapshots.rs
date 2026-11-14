mod support;
use serde_json::Value;
use std::{fs, process::Stdio};
use support::{assert_success, Repo};
fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    support::groove_in(&repo.path).args(args).output().unwrap()
}
fn checked(repo: &Repo, args: &[&str]) -> Value {
    let response = assert_success(&cli(repo, args));
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response["result"].clone()
}
#[test]
fn diff_and_restore_honor_the_current_file_capture_limit() {
    let repo = Repo::new();
    let saved = checked(&repo, &["snapshot", "save"]);
    let id = saved["snapshot"]["id"].as_str().unwrap();
    fs::write(repo.path.join("tracked.txt"), b"larger current work\n").unwrap();
    for command in ["diff", "restore"] {
        let output = cli(&repo, &["snapshot", command, id, "--max-file-bytes", "1"]);
        assert_eq!(output.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            b"larger current work\n"
        );
        assert_eq!(
            checked(&repo, &["snapshot", "list"])["snapshots"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
    assert!(
        checked(&repo, &["snapshot", "diff", id, "--max-file-bytes", "0"])["patch"]
            .as_str()
            .unwrap()
            .contains("+larger current work")
    );
    let restored = checked(&repo, &["snapshot", "restore", id, "--max-file-bytes", "0"]);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    let safety = restored["safety_snapshot_id"].as_str().unwrap();
    let recovered = repo.temp.path().join("safety");
    checked(
        &repo,
        &[
            "snapshot",
            "restore",
            safety,
            "--to",
            recovered.to_str().unwrap(),
            "--max-file-bytes",
            "1",
        ],
    );
    assert_eq!(
        fs::read(recovered.join("tracked.txt")).unwrap(),
        b"larger current work\n"
    );
}

#[test]
fn cli_end_to_end_lifecycle() {
    let repo = Repo::new();
    fs::write(repo.path.join("new"), "saved\n").unwrap();
    let saved = checked(&repo, &["snapshot", "save", "checkpoint"]);
    let id = saved["snapshot"]["id"].as_str().unwrap();
    assert_eq!(
        checked(&repo, &["snapshot", "list"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        checked(&repo, &["snapshot", "show", id])["snapshot"]["description"],
        "checkpoint"
    );
    fs::write(repo.path.join("new"), "later\n").unwrap();
    assert!(checked(&repo, &["snapshot", "diff", id])["patch"]
        .as_str()
        .unwrap()
        .contains("+later"));
    let dest = repo.temp.path().join("recover");
    checked(
        &repo,
        &["snapshot", "restore", id, "--to", dest.to_str().unwrap()],
    );
    assert_eq!(fs::read(dest.join("new")).unwrap(), b"saved\n");
    checked(&repo, &["snapshot", "restore", id]);
    assert_eq!(fs::read(repo.path.join("new")).unwrap(), b"saved\n");
    checked(&repo, &["snapshot", "delete", id]);
    assert!(!cli(&repo, &["snapshot", "delete", id]).status.success());
    assert!(!cli(&repo, &["snapshot", "show", "../../heads/main"])
        .status
        .success());
}
#[test]
fn lost_response_still_leaves_snapshot() {
    let repo = Repo::new();
    let status = support::groove()
        .current_dir(&repo.path)
        .args(["snapshot", "save"])
        .stdout(support::failed_output())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(
        checked(&repo, &["snapshot", "list"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[cfg(feature = "test-hooks")]
#[test]
fn crash_publication_and_concurrent_writer_boundaries() {
    use std::{io::Write, os::unix::net::UnixListener};
    for point in [
        "snapshot-captured",
        "snapshot-before-publish",
        "snapshot-published",
    ] {
        let repo = Repo::new();
        let index = fs::read(repo.path.join(".git/index")).unwrap();
        let socket = repo.temp.path().join("checkpoint.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut command = support::groove_in(&repo.path);
        command
            .args(["snapshot", "save"])
            .env("GROOVE_TEST_CHECKPOINT", point)
            .env("GROOVE_TEST_SOCKET", &socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = support::TestChild::spawn(&mut command);
        let stream = child
            .checkpoint(&listener, point, support::PROCESS_TIMEOUT)
            .unwrap();
        // This budget also bounds Git discovery before the lock wait. Allow
        // startup on loaded native runners; the paused checkpoint, rather than
        // elapsed time, guarantees the other process still holds the lock.
        let blocked = cli(&repo, &["--git-timeout-ms", "1000", "snapshot", "save"]);
        assert_eq!(
            blocked.status.code(),
            Some(2),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&blocked.stdout),
            String::from_utf8_lossy(&blocked.stderr)
        );
        let response: Value = serde_json::from_slice(&blocked.stdout).unwrap();
        assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(response["error"]["retryable"], true);
        child.kill().unwrap();
        child.wait().unwrap();
        drop(stream);
        assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
        assert_eq!(
            checked(&repo, &["snapshot", "list"])["snapshots"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(point == "snapshot-published")
        );
        checked(&repo, &["snapshot", "save"]);
        assert!(!fs::read_dir(repo.path.join(".git/groove-snapshots"))
            .unwrap()
            .any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("scratch-")));
    }
    let repo = Repo::new();
    let socket = repo.temp.path().join("writer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["snapshot", "save"])
        .env("GROOVE_TEST_CHECKPOINT", "snapshot-captured")
        .env("GROOVE_TEST_SOCKET", &socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = child
        .checkpoint(&listener, "snapshot-captured", support::PROCESS_TIMEOUT)
        .unwrap();
    fs::write(repo.path.join("tracked.txt"), "concurrent\n").unwrap();
    stream.write_all(&[1]).unwrap();
    assert!(!child.wait_with_output().unwrap().status.success());
    assert!(checked(&repo, &["snapshot", "list"])["snapshots"]
        .as_array()
        .unwrap()
        .is_empty());
}
#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_restore_has_a_recoverable_safety_snapshot() {
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    let baseline = checked(&repo, &["snapshot", "save", "baseline"]);
    let id = baseline["snapshot"]["id"].as_str().unwrap();
    fs::write(repo.path.join("tracked.txt"), "valuable later work\n").unwrap();
    let socket = repo.temp.path().join("restore.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["snapshot", "restore", id])
        .env("GROOVE_TEST_CHECKPOINT", "snapshot-restored-files")
        .env("GROOVE_TEST_SOCKET", &socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(
            &listener,
            "snapshot-restored-files",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    let listed = checked(&repo, &["snapshot", "list"]);
    let safety = listed["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["description"]
                .as_str()
                .unwrap()
                .starts_with("before restoring")
        })
        .unwrap();
    checked(
        &repo,
        &["snapshot", "restore", safety["id"].as_str().unwrap()],
    );
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"valuable later work\n"
    );
}
#[test]
fn custom_global_ignore_rules_are_respected() {
    let repo = Repo::new();
    let config = repo.temp.path().join("global.gitconfig");
    let excludes = repo.temp.path().join("excludes");
    fs::write(&excludes, "private.txt\n").unwrap();
    assert!(support::command(&repo.path, "git")
        .args(["config", "--file"])
        .arg(&config)
        .arg("core.excludesfile")
        .arg(&excludes)
        .status()
        .unwrap()
        .success());
    fs::write(repo.path.join("private.txt"), "excluded").unwrap();
    fs::write(repo.path.join("public.txt"), "included").unwrap();
    let out = support::groove_in(&repo.path)
        .args(["snapshot", "save"])
        .env("GIT_CONFIG_GLOBAL", &config)
        .output()
        .unwrap();
    let value = assert_success(&out);
    let tree = value["result"]["snapshot"]["work_tree"].as_str().unwrap();
    let names = repo.git(&["ls-tree", "-r", "--name-only", tree]);
    assert!(names.contains("public.txt"));
    assert!(!names.contains("private.txt"));
}

#[test]
fn global_lfs_definitions_do_not_strand_an_ordinary_task() {
    let repo = Repo::new();
    let config = repo.temp.path().join("global.gitconfig");
    // Any accidental execution fails; no repository path selects this filter.
    fs::write(
        &config,
        "[filter \"lfs\"]\nclean = false\nsmudge = false\nrequired = true\n",
    )
    .unwrap();
    let invoke = |args: &[&str]| {
        support::groove_in(&repo.path)
            .env("GIT_CONFIG_GLOBAL", &config)
            .args(args)
            .output()
            .unwrap()
    };
    assert_success(&invoke(&["init", "--capacity", "1"]));
    let lease = assert_success(&invoke(&["acquire", "--task", "ordinary"]));
    let workspace = std::path::Path::new(lease["result"]["worktree_path"].as_str().unwrap());
    fs::write(workspace.join("result.txt"), "precious result\n").unwrap();
    let finished = assert_success(&invoke(&["release", "--save", "--task", "ordinary"]));
    let snapshot = finished["result"]["snapshot"]["id"].as_str().unwrap();
    let destination = repo.temp.path().join("recovered");
    assert_success(&invoke(&[
        "snapshot",
        "restore",
        snapshot,
        "--to",
        destination.to_str().unwrap(),
    ]));
    assert_eq!(
        fs::read(destination.join("result.txt")).unwrap(),
        b"precious result\n"
    );
    assert!(assert_success(&invoke(&["list"]))["result"]["leases"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[cfg(feature = "test-hooks")]
#[test]
fn snapshot_wait_honors_timeout_and_preserves_reserved_results() {
    use std::{
        io::Write,
        os::unix::net::UnixListener,
        time::{Duration, Instant},
    };
    let repo = Repo::new();
    checked(&repo, &["init", "--capacity", "2"]);
    for task in ["first", "second"] {
        let lease = checked(&repo, &["acquire", "--task", task]);
        fs::write(
            std::path::Path::new(lease["worktree_path"].as_str().unwrap()).join("result.txt"),
            task,
        )
        .unwrap();
    }
    let second_workspace = checked(&repo, &["show", "--task", "second"])["lease"]["worktree_path"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_socket = repo.temp.path().join("first.sock");
    let first_listener = UnixListener::bind(&first_socket).unwrap();
    let mut first = support::TestChild::spawn(
        support::groove_in(&repo.path)
            .args(["release", "--save", "--task", "first"])
            .env("GROOVE_TEST_CHECKPOINT", "snapshot-captured")
            .env("GROOVE_TEST_SOCKET", &first_socket),
    );
    let mut first_stream = first
        .checkpoint(
            &first_listener,
            "snapshot-captured",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    let second_socket = repo.temp.path().join("second.sock");
    let second_listener = UnixListener::bind(&second_socket).unwrap();
    let mut second = support::TestChild::spawn(
        support::groove_in(std::path::Path::new(&second_workspace))
            .args(["--git-timeout-ms", "10000", "snapshot", "save"])
            .env("GROOVE_TEST_CHECKPOINT", "snapshot-lock-contended")
            .env("GROOVE_TEST_SOCKET", &second_socket),
    );
    let mut second_stream = second
        .checkpoint(
            &second_listener,
            "snapshot-lock-contended",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    second_stream.write_all(&[1]).unwrap();
    // A third caller deliberately exhausts a real lock wait while the first
    // owner stays paused. This establishes contention beyond the old 100 ms cap.
    let started = Instant::now();
    let blocked = cli(&repo, &["--git-timeout-ms", "350", "snapshot", "save"]);
    assert_eq!(blocked.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert_eq!(error["error"]["retryable"], true);
    assert!(started.elapsed() >= Duration::from_millis(350));
    let finish_blocked = cli(
        &repo,
        &[
            "--git-timeout-ms",
            "350",
            "release",
            "--save",
            "--force",
            "--task",
            "second",
        ],
    );
    assert_eq!(finish_blocked.status.code(), Some(2));
    let finish_error: Value = serde_json::from_slice(&finish_blocked.stdout).unwrap();
    assert_eq!(finish_error["error"]["retryable"], true);
    assert_eq!(
        checked(&repo, &["list"])["leases"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(second.try_wait().unwrap().is_none());
    first_stream.write_all(&[1]).unwrap();
    let first_result = assert_success(&first.wait_with_output().unwrap());
    let second_result = assert_success(&second.wait_with_output().unwrap());
    checked(&repo, &["release", "--save", "--task", "second"]);
    for (task, result) in [("first", first_result), ("second", second_result)] {
        let snapshot = result["result"]["snapshot"]["id"].as_str().unwrap();
        let destination = repo.temp.path().join(task);
        checked(
            &repo,
            &[
                "snapshot",
                "restore",
                snapshot,
                "--to",
                destination.to_str().unwrap(),
            ],
        );
        assert_eq!(
            fs::read_to_string(destination.join("result.txt")).unwrap(),
            task
        );
    }
    assert!(checked(&repo, &["list"])["leases"]
        .as_array()
        .unwrap()
        .is_empty());
}
