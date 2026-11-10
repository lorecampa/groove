mod support;
use serde_json::Value;
use std::{fs, path::Path, process::Output};
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
fn release_refuses_unsaved_changes_unless_forced_and_finish_saves_them() {
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
    // finish is how unsaved changes are kept, so it does not refuse them.
    let finished = ok(&cli(&repo, &["finish", "--task", "dirty"]));
    let id = finished["release"]["lease_id"].as_str().unwrap().to_owned();
    // Retrying a completed release is not blocked by files left on disk.
    ok(&cli(&repo, &["release", "--lease-id", &id]));
    let path = acquire(&repo, "discard");
    fs::write(Path::new(&path).join("scratch.txt"), "throwaway\n").unwrap();
    ok(&cli(&repo, &["release", "--force", "--task", "discard"]));
}

#[test]
fn processes_using_the_workspace_block_release_and_finish_but_not_the_invoking_shell() {
    let repo = Repo::new();
    ok(&cli(&repo, &["init", "--capacity", "1"]));
    let path = acquire(&repo, "busy");
    let mut watcher = support::TestChild::spawn(
        std::process::Command::new("sleep")
            .arg("60")
            .current_dir(&path),
    );
    for operation in ["release", "finish"] {
        let refused = error(&cli(&repo, &[operation, "--task", "busy"]));
        assert_eq!(refused["code"], "WORKSPACE_IN_USE", "{operation}");
        let processes = refused["details"]["processes"].as_array().unwrap().clone();
        assert!(processes.iter().any(|p| {
            p["pid"] == watcher.id() && p["command"] == "sleep" && p["uses"] == "working_directory"
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
