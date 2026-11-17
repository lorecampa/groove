mod support;
use serde_json::Value;
use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::{Output, Stdio},
    time::{Duration, Instant},
};
use support::Repo;

fn cli(repo: &Repo, args: &[&str]) -> Output {
    support::groove_in(&repo.path)
        .env("GROOVE_LOG_ENABLED", "false")
        .args(args)
        .output()
        .unwrap()
}
fn ok(out: &Output) -> Value {
    support::assert_success(out)["result"].clone()
}
fn error(out: &Output) -> Value {
    assert!(!out.status.success());
    assert_eq!(out.status.code(), Some(5));
    serde_json::from_slice::<Value>(&out.stdout).unwrap()["error"].clone()
}
fn acquire(repo: &Repo, task: &str) -> String {
    ok(&cli(repo, &["acquire", "--task", task]))["workspace_path"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[cfg(feature = "test-hooks")]
#[test]
fn release_rechecks_process_liveness_after_scanning() {
    use std::{io::Write, os::unix::net::UnixListener};
    for exit_after_scan in [false, true] {
        let repo = Repo::new();
        ok(&cli(
            &repo,
            &["pool", "init", "--capacity", "1", "--idle-timeout", "never"],
        ));
        let workspace = acquire(&repo, "scan-race");
        let mut writer =
            support::TestChild::spawn(support::command(Path::new(&workspace), "sleep").arg("60"));
        let socket = repo.temp.path().join("scan.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut releaser = support::TestChild::spawn(
            support::groove_in(&repo.path)
                .args(["release", "--task", "scan-race"])
                .env("GROOVE_TEST_CHECKPOINT", "workspace-processes-scanned")
                .env("GROOVE_TEST_SOCKET", &socket),
        );
        let mut scan = releaser
            .checkpoint(
                &listener,
                "workspace-processes-scanned",
                support::PROCESS_TIMEOUT,
            )
            .unwrap();
        if exit_after_scan {
            writer.kill().unwrap();
            writer.wait().unwrap();
        }
        scan.write_all(b"continue").unwrap();
        drop(scan);
        let result = releaser.wait_with_output().unwrap();
        if !exit_after_scan {
            writer.kill().unwrap();
            writer.wait().unwrap();
            assert_eq!(error(&result)["code"], "WORKSPACE_IN_USE");
            assert_eq!(
                ok(&cli(&repo, &["list"]))["reservations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            ok(&cli(&repo, &["release", "--task", "scan-race"]));
        } else {
            ok(&result);
            assert!(ok(&cli(&repo, &["list"]))["reservations"]
                .as_array()
                .unwrap()
                .is_empty());
        }
    }
}

#[test]
fn unreadable_work_is_preserved_by_release_and_destruction() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid observes the caller's effective user ID.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let repo = Repo::new();
    ok(&cli(
        &repo,
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
    let path = acquire(&repo, "unreadable");
    let directory = Path::new(&path).join("unreadable");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("valuable.txt"), b"must survive").unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).unwrap();
    let refused = cli(&repo, &["release", "--task", "unreadable"]);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(refused.status.code(), Some(7));
    let response: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(response["error"]["code"], "GIT_ERROR");
    assert_eq!(
        ok(&cli(&repo, &["list"]))["reservations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        fs::read(directory.join("valuable.txt")).unwrap(),
        b"must survive"
    );
    ok(&cli(&repo, &["release", "--force", "--task", "unreadable"]));
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).unwrap();
    let refused = cli(&repo, &["pool", "delete", "default", "--yes"]);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!refused.status.success());
    let response: Value = serde_json::from_slice(&refused.stdout).unwrap();
    // The path preflight can refuse before Git reaches its own inventory.
    assert_eq!(response["error"]["code"], "IO_ERROR");
    assert_eq!(
        fs::read(directory.join("valuable.txt")).unwrap(),
        b"must survive"
    );
    assert_eq!(ok(&cli(&repo, &["pool", "show"]))["pool"]["capacity"], 1);
}

#[test]
fn successful_git_status_with_warnings_cannot_authorize_release_destroy_or_acquire() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    ok(&cli(
        &repo,
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
    acquire(&repo, "warnings");
    let real = support::command(&repo.path, "sh")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real.status.success());
    let bin = repo.temp.path().join("warning-git");
    fs::create_dir(&bin).unwrap();
    let shim = bin.join("git");
    fs::write(&shim, "#!/bin/sh\nfor arg do\n if [ \"$arg\" = status ]; then printf 'warning: incomplete inventory\\n' >&2; exit 0; fi\ndone\nexec \"$GROOVE_REAL_GIT\" \"$@\"\n").unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
    let warned = |args: &[&str]| {
        support::groove_in(&repo.path)
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .env(
                "GROOVE_REAL_GIT",
                String::from_utf8_lossy(&real.stdout).trim(),
            )
            .args(args)
            .output()
            .unwrap()
    };
    for (index, args) in [
        vec!["release", "--task", "warnings"],
        vec!["pool", "delete", "default", "--yes"],
    ]
    .into_iter()
    .enumerate()
    {
        let output = warned(&args);
        assert_eq!(output.status.code(), Some(7));
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["error"]["code"], "GIT_ERROR");
        assert_eq!(error["error"]["details"]["git"]["exit_code"], 0);
        assert!(repo.slot(1).join("tracked.txt").exists());
        if index == 0 {
            assert_eq!(
                ok(&cli(&repo, &["list"]))["reservations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            ok(&cli(&repo, &["release", "--force", "--task", "warnings"]));
        }
    }
    let output = warned(&["acquire"]);
    assert_eq!(output.status.code(), Some(6));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "POOL_UNHEALTHY");
    assert!(ok(&cli(&repo, &["list"]))["reservations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(repo.slot(1).join("tracked.txt").exists());
    ok(&cli(&repo, &["acquire"]));
}

#[test]
fn release_from_inside_the_slot_can_pipe_its_output_to_a_sibling() {
    let repo = Repo::new();
    ok(&cli(&repo, &["pool", "init", "--capacity", "1"]));
    let path = acquire(&repo, "piped");
    let fifo = repo.temp.path().join("pipeline-ready");
    assert!(support::command(repo.temp.path(), "mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let mut shell = support::command(Path::new(&path), "sh");
    shell.process_group(0)
        .args(["-c", "{ read -r ready < \"$2\"; exec \"$1\" --json release --task piped; } | { printf 'ready\\n' > \"$2\"; exec cat; }", "pipeline"])
        .arg(support::groove_binary()).arg(&fifo);
    let output = support::TestChild::spawn(&mut shell)
        .wait_with_output()
        .unwrap();
    let response = support::assert_success(&output);
    assert_eq!(response["ok"], true, "{response}");
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    assert!(ok(&cli(&repo, &["list"]))["reservations"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn private_worktree_and_bisect_refs_do_not_preserve_a_released_commit() {
    for preservation in ["branch", "tag", "stash", "snapshot"] {
        let repo = Repo::new();
        ok(&cli(
            &repo,
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
        let path = acquire(&repo, "private-ref");
        let path = Path::new(&path);
        fs::write(path.join("tracked.txt"), b"detached result\n").unwrap();
        support::git(path, &["add", "tracked.txt"]);
        support::git(path, &["commit", "-qm", "detached work"]);
        for reference in ["refs/worktree/private", "refs/bisect/private"] {
            support::git(path, &["update-ref", reference, "HEAD"]);
            let refused = error(&cli(&repo, &["release", "--task", "private-ref"]));
            assert_eq!(refused["code"], "UNSAVED_CHANGES", "{reference}");
            assert_eq!(
                ok(&cli(&repo, &["list"]))["reservations"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                fs::read(path.join("tracked.txt")).unwrap(),
                b"detached result\n"
            );
        }
        match preservation {
            "branch" => {
                support::git(path, &["branch", "saved-result"]);
            }
            "tag" => {
                support::git(path, &["tag", "saved-result"]);
            }
            "stash" => {
                fs::write(path.join("tracked.txt"), b"stash contents\n").unwrap();
                support::git(path, &["stash", "push", "-qm", "preserve HEAD"]);
            }
            "snapshot" => {
                support::assert_success(
                    &support::groove_in(path)
                        .args(["snapshot", "save"])
                        .output()
                        .unwrap(),
                );
            }
            _ => unreachable!(),
        }
        ok(&cli(&repo, &["release", "--task", "private-ref"]));
    }
}

#[test]
fn release_refuses_unsaved_changes_unless_forced_and_save_preserves_them() {
    let repo = Repo::new();
    ok(&cli(&repo, &["pool", "init", "--capacity", "2"]));
    let path = acquire(&repo, "dirty");
    // Clean workspaces release without --force.
    let clean = acquire(&repo, "clean");
    assert!(Path::new(&clean).is_dir());
    ok(&cli(&repo, &["release", "--task", "clean"]));
    for (file, contents) in [("tracked.txt", "edited\n"), ("new.txt", "new\n")] {
        fs::write(Path::new(&path).join(file), contents).unwrap();
        let refused = error(&cli(&repo, &["release", "--task", "dirty"]));
        assert_eq!(refused["code"], "UNSAVED_CHANGES", "{file}");
        assert_eq!(
            refused["details"]["changes"]["untracked"],
            file == "new.txt"
        );
    }
    // The reservation is untouched by a refused release.
    assert_eq!(
        ok(&cli(&repo, &["list"]))["reservations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // --save is how unsaved changes are kept, so it does not refuse them.
    let finished = ok(&cli(&repo, &["release", "--save", "--task", "dirty"]));
    let id = finished["release"]["reservation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Retrying a completed release is not blocked by files left on disk.
    ok(&cli(&repo, &["release", "--reservation-id", &id]));
    let path = acquire(&repo, "discard");
    fs::write(Path::new(&path).join("scratch.txt"), "throwaway\n").unwrap();
    ok(&cli(&repo, &["release", "--force", "--task", "discard"]));
}

#[test]
fn plain_release_preserves_detached_commits_until_saved_or_explicitly_discarded() {
    for preservation in ["branch", "snapshot", "force"] {
        let repo = Repo::new();
        ok(&cli(&repo, &["pool", "init", "--capacity", "1"]));
        let path = acquire(&repo, "committed");
        let path = Path::new(&path);
        fs::write(path.join("tracked.txt"), "committed result\n").unwrap();
        support::git(path, &["add", "tracked.txt"]);
        support::git(path, &["commit", "-qm", "task result"]);
        let commit = support::git(path, &["rev-parse", "HEAD"]);
        let refused = error(&cli(&repo, &["release", "--task", "committed"]));
        assert_eq!(refused["code"], "UNSAVED_CHANGES");
        assert_eq!(
            ok(&cli(&repo, &["list"]))["reservations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert!(!cli(&repo, &["acquire"]).status.success());
        assert_eq!(support::git(path, &["rev-parse", "HEAD"]), commit);
        assert_eq!(
            fs::read(path.join("tracked.txt")).unwrap(),
            b"committed result\n"
        );
        match preservation {
            "branch" => {
                support::git(path, &["branch", "task-result"]);
                ok(&cli(&repo, &["release", "--task", "committed"]));
            }
            "snapshot" => {
                ok(&cli(&repo, &["release", "--save", "--task", "committed"]));
            }
            _ => {
                ok(&cli(&repo, &["release", "--force", "--task", "committed"]));
            }
        }
        acquire(&repo, "next");
        if preservation != "force" {
            assert!(!repo
                .git(&["for-each-ref", "--contains", &commit])
                .is_empty());
        }
    }
}

#[test]
fn processes_using_the_workspace_block_release_with_or_without_save_but_not_the_invoking_shell() {
    let repo = Repo::new();
    ok(&cli(&repo, &["pool", "init", "--capacity", "1"]));
    let path = acquire(&repo, "busy");
    let ready = repo.temp.path().join("watcher-ready");
    // Signal readiness after exec initialization, then block in a shell builtin
    // so there is no further exec transition during the process checks.
    let mut watcher = support::TestChild::spawn(
        support::command(Path::new(&path), "bash")
            .args(["-c", "printf ready > \"$1\"; read -r _", "watcher"])
            .arg(&ready)
            .stdin(Stdio::piped()),
    );
    let deadline = Instant::now() + support::PROCESS_TIMEOUT;
    while !ready.exists() {
        assert!(
            watcher.try_wait().unwrap().is_none(),
            "watcher exited before readiness"
        );
        assert!(
            Instant::now() < deadline,
            "watcher did not signal readiness"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    for mut args in [vec!["release"], vec!["release", "--save"]] {
        args.extend(["--task", "busy"]);
        let refused = error(&cli(&repo, &args));
        assert_eq!(refused["code"], "WORKSPACE_IN_USE", "{args:?}");
        assert_eq!(refused["retryable"], true);
        let processes = refused["details"]["processes"].as_array().unwrap().clone();
        assert!(processes.iter().any(|p| {
            p["pid"] == watcher.id() && p["command"] == "bash" && p["uses"] == "working_directory"
        }));
    }
    watcher.kill().unwrap();
    watcher.wait().unwrap();
    // A shell whose working directory is the workspace runs groove from there.
    let shell = support::command(Path::new(&path), "sh")
        .args(["-c", "\"$0\" --json release --task busy"])
        .arg(
            std::env::var_os("GROOVE_CLI_BINARY")
                .unwrap_or_else(|| env!("CARGO_BIN_EXE_groove").into()),
        )
        .env("GROOVE_LOG_ENABLED", "false")
        .output()
        .unwrap();
    ok(&shell);
}

#[cfg(target_os = "linux")]
#[test]
fn mapping_holder_process() {
    use std::{io::Write, os::fd::AsRawFd};
    let Some(path) = std::env::var_os("GROOVE_TEST_MAPPING_PATH") else {
        return;
    };
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    // SAFETY: file owns a readable/writable descriptor, the offset is aligned,
    // and the fixture file covers the requested mapping length.
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            8,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    assert_ne!(mapping, libc::MAP_FAILED);
    drop(file);
    let socket = std::env::var_os("GROOVE_TEST_MAPPING_SOCKET").unwrap();
    let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
    stream.write_all(b"mapped\n").unwrap();
    let mut byte = [0];
    std::io::Read::read_exact(&mut stream, &mut byte).unwrap();
    // SAFETY: the mapping is still owned, writable and exactly eight bytes long.
    unsafe {
        std::ptr::write_volatile(mapping.cast::<u8>(), b'X');
        assert_eq!(libc::munmap(mapping, 8), 0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_shared_writable_mapping_blocks_release_and_reacquisition_after_fd_close() {
    use std::{io::Write, os::unix::net::UnixListener};
    let repo = Repo::new();
    ok(&cli(
        &repo,
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
    let path = acquire(&repo, "mapped");
    let socket = repo.temp.path().join("mapping.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::command(&repo.path, std::env::current_exe().unwrap());
    command
        .args(["--exact", "mapping_holder_process", "--nocapture"])
        .env(
            "GROOVE_TEST_MAPPING_PATH",
            Path::new(&path).join("tracked.txt"),
        )
        .env("GROOVE_TEST_MAPPING_SOCKET", &socket);
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = support::accept_child(
        &listener,
        std::slice::from_mut(&mut child),
        support::PROCESS_TIMEOUT,
    )
    .unwrap();
    let mut ready = [0; 7];
    stream
        .set_read_timeout(Some(support::PROCESS_TIMEOUT))
        .unwrap();
    std::io::Read::read_exact(&mut stream, &mut ready).unwrap();
    assert_eq!(&ready, b"mapped\n");
    let refused = error(&cli(&repo, &["release", "--task", "mapped"]));
    assert_eq!(refused["code"], "WORKSPACE_IN_USE");
    assert!(refused["details"]["processes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["pid"] == child.id() && p["uses"] == "open_file"));
    let forced = error(&cli(&repo, &["release", "--force", "--task", "mapped"]));
    assert_eq!(forced["code"], "WORKSPACE_IN_USE");
    let refused = cli(&repo, &["acquire"]);
    assert!(!refused.status.success());
    let response: Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert_eq!(response["error"]["code"], "POOL_EXHAUSTED");
    stream.write_all(b"x").unwrap();
    assert!(child.wait().unwrap().success());
    ok(&cli(&repo, &["release", "mapped", "--force"]));
    acquire(&repo, "next");
    assert_eq!(
        fs::read(Path::new(&path).join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
}

#[cfg(feature = "test-hooks")]
#[test]
fn release_waits_for_a_concurrent_read_only_inspection() {
    use std::{io::Write, os::unix::net::UnixListener};
    let repo = Repo::new();
    ok(&cli(
        &repo,
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
    acquire(&repo, "observed");
    let socket = repo.temp.path().join("inspection.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["show", "--details", "--task", "observed"])
        .env("GROOVE_TEST_CHECKPOINT", "inspection-before-git")
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut inspector = support::TestChild::spawn(&mut command);
    let mut stream = inspector
        .checkpoint(&listener, "inspection-before-git", support::PROCESS_TIMEOUT)
        .unwrap();
    let waiting = UnixListener::bind(repo.temp.path().join("waiting.sock")).unwrap();
    let mut releaser = support::TestChild::spawn(
        support::groove_in(&repo.path)
            .args(["release", "--task", "observed"])
            .env("GROOVE_TEST_CHECKPOINT", "waiting-for-inspection")
            .env("GROOVE_TEST_SOCKET", repo.temp.path().join("waiting.sock")),
    );
    let mut waiting_stream = releaser
        .checkpoint(&waiting, "waiting-for-inspection", support::PROCESS_TIMEOUT)
        .unwrap();
    waiting_stream.write_all(b"continue\n").unwrap();
    drop(waiting_stream);
    stream.write_all(b"continue\n").unwrap();
    drop(stream);
    assert_success_inspection(&inspector.wait_with_output().unwrap());
    ok(&releaser.wait_with_output().unwrap());
    assert!(ok(&cli(&repo, &["list"]))["reservations"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[cfg(feature = "test-hooks")]
fn assert_success_inspection(output: &Output) {
    let result = ok(output);
    assert!(result["slot"]["git"]["error"].is_null(), "{result}");
}

#[cfg(feature = "test-hooks")]
#[test]
fn release_recovers_after_an_inspection_process_is_killed() {
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    ok(&cli(
        &repo,
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
    acquire(&repo, "observed");
    let socket = repo.temp.path().join("inspection-kill.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["show", "--details", "--task", "observed"])
        .env("GROOVE_TEST_CHECKPOINT", "inspection-before-git")
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut inspector = support::TestChild::spawn(&mut command);
    let stream = inspector
        .checkpoint(&listener, "inspection-before-git", support::PROCESS_TIMEOUT)
        .unwrap();
    inspector.kill().unwrap();
    inspector.wait().unwrap();
    drop(stream);
    ok(&cli(&repo, &["release", "--task", "observed"]));
    acquire(&repo, "next");
    assert_eq!(
        fs::read(repo.slot(1).join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
}
