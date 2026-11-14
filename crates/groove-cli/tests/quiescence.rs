mod support;
use serde_json::Value;
use std::{
    fs,
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
