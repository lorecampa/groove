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
    ok(&cli(repo, &["acquire", "--task", task]))["worktree_path"]
        .as_str()
        .unwrap()
        .to_owned()
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
        ok(&cli(&repo, &["list"]))["leases"]
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
    let refused = cli(&repo, &["destroy"]);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!refused.status.success());
    let response: Value = serde_json::from_slice(&refused.stdout).unwrap();
    // The path preflight can refuse before Git reaches its own inventory.
    assert_eq!(response["error"]["code"], "IO_ERROR");
    assert_eq!(
        fs::read(directory.join("valuable.txt")).unwrap(),
        b"must survive"
    );
    assert_eq!(ok(&cli(&repo, &["show"]))["pool"]["capacity"], 1);
}

#[test]
fn successful_git_status_with_warnings_cannot_authorize_release_destroy_or_acquire() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    ok(&cli(
        &repo,
        &[
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
    for (index, args) in [vec!["release", "--task", "warnings"], vec!["destroy"]]
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
                ok(&cli(&repo, &["list"]))["leases"]
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
    assert!(ok(&cli(&repo, &["list"]))["leases"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(repo.slot(1).join("tracked.txt").exists());
    ok(&cli(&repo, &["acquire"]));
}

#[test]
fn release_from_inside_the_slot_can_pipe_its_output_to_a_sibling() {
    let repo = Repo::new();
    ok(&cli(&repo, &["init", "--capacity", "1"]));
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
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    assert!(ok(&cli(&repo, &["list"]))["leases"]
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
                ok(&cli(&repo, &["list"]))["leases"]
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
    ok(&cli(&repo, &["init", "--capacity", "2"]));
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
        ok(&cli(&repo, &["list"]))["leases"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // --save is how unsaved changes are kept, so it does not refuse them.
    let finished = ok(&cli(&repo, &["release", "--save", "--task", "dirty"]));
    let id = finished["release"]["lease_id"].as_str().unwrap().to_owned();
    // Retrying a completed release is not blocked by files left on disk.
    ok(&cli(&repo, &["release", "--lease-id", &id]));
    let path = acquire(&repo, "discard");
    fs::write(Path::new(&path).join("scratch.txt"), "throwaway\n").unwrap();
    ok(&cli(&repo, &["release", "--force", "--task", "discard"]));
}

#[test]
fn plain_release_preserves_detached_commits_until_saved_or_explicitly_discarded() {
    for preservation in ["branch", "snapshot", "force"] {
        let repo = Repo::new();
        ok(&cli(&repo, &["init", "--capacity", "1"]));
        let path = acquire(&repo, "committed");
        let path = Path::new(&path);
        fs::write(path.join("tracked.txt"), "committed result\n").unwrap();
        support::git(path, &["add", "tracked.txt"]);
        support::git(path, &["commit", "-qm", "task result"]);
        let commit = support::git(path, &["rev-parse", "HEAD"]);
        let refused = error(&cli(&repo, &["release", "--task", "committed"]));
        assert_eq!(refused["code"], "UNSAVED_CHANGES");
        assert_eq!(
            ok(&cli(&repo, &["list"]))["leases"]
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
    ok(&cli(&repo, &["init", "--capacity", "1"]));
    let path = acquire(&repo, "busy");
    let ready = repo.temp.path().join("watcher-ready");
    // Signal readiness after exec initialization, then block in a shell builtin
    // so there is no further exec transition during the process checks.
    let mut watcher = support::TestChild::spawn(
        support::command(Path::new(&path), "bash")
            .process_group(0)
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
