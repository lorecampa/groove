//! Native hook contracts, without invoking a model or mutating a developer repo.
#[path = "../../../crates/groove-cli/tests/support/mod.rs"]
mod support;

use serde_json::{json, Value};
use std::{fs, path::Path, process::Stdio};
use support::{Repo, TestChild};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .unwrap()
}

fn hook(repo: &Repo, cwd: &Path, input: &str, foreign: Option<&Repo>) -> std::process::Output {
    let input_file = repo.temp.path().join("hook-input.json");
    fs::write(&input_file, input).unwrap();
    let mut command = support::command(&repo.path, "bash");
    command
        .arg(root().join("plugins/groove-project/hooks/project-context.sh"))
        .stdin(Stdio::from(fs::File::open(input_file).unwrap()))
        // Hooks must honor the payload's cwd even if the host pins their process
        // elsewhere and exports the Git context of an enclosing Git command.
        .current_dir(cwd)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "groove.agent.enabled")
        .env("GIT_CONFIG_VALUE_0", "true");
    if let Some(foreign) = foreign {
        command.env("GIT_DIR", foreign.path.join(".git"));
    }
    TestChild::spawn(&mut command).wait_with_output().unwrap()
}

fn event(cwd: &Path, name: &str) -> String {
    json!({"hook_event_name": name, "cwd": cwd}).to_string()
}

#[test]
fn plugin_install_payload_contains_the_complete_portable_skill_as_real_files() {
    let output = TestChild::spawn(
        support::command(root(), "bash").args(["scripts/sync-plugins.sh", "--check"]),
    )
    .wait_with_output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    for name in ["groove", "groove-project"] {
        for harness in ["claude", "codex"] {
            let manifest = root().join(format!("plugins/{name}/.{harness}-plugin/plugin.json"));
            let value: Value = serde_json::from_slice(&fs::read(manifest).unwrap()).unwrap();
            assert_eq!(value["name"], name);
        }
    }
    for path in [
        "plugins/groove/skills/groove/SKILL.md",
        "plugins/groove/skills/groove/references/project.md",
        "plugins/groove/skills/groove/references/recovery.md",
    ] {
        assert!(fs::symlink_metadata(root().join(path)).unwrap().is_file());
    }
}

#[test]
fn only_private_clone_opt_in_enables_context_including_linked_worktrees() {
    let repo = Repo::new();
    let foreign = Repo::new();
    foreign.git(&["config", "--local", "groove.agent.enabled", "true"]);
    let status = repo.git(&["status", "--porcelain"]);
    let input = event(&repo.path, "SessionStart");
    let output = hook(&repo, &foreign.path, &input, Some(&foreign));
    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.is_empty(),
        "ambient Git context enabled the wrong repo"
    );

    repo.git(&["config", "--local", "groove.agent.enabled", "true"]);
    let instructions = repo.temp.path().join("personal workflow.md");
    fs::write(
        &instructions,
        "Rebase on the latest upstream before opening a PR.\n",
    )
    .unwrap();
    repo.git(&[
        "config",
        "--local",
        "groove.agent.instructionsFile",
        instructions.to_str().unwrap(),
    ]);
    let linked = repo.temp.path().join("linked checkout with spaces");
    repo.git(&[
        "worktree",
        "add",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    for cwd in [&repo.path, &linked] {
        for name in ["SessionStart", "SubagentStart"] {
            let output = hook(&repo, &foreign.path, &event(cwd, name), Some(&foreign));
            let value = support::assert_success(&output);
            assert_eq!(value["hookSpecificOutput"]["hookEventName"], name);
            let context = value["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap();
            assert!(context.contains("Assigned workers reuse"));
            assert!(context.contains("Read-only investigation"));
            assert!(context.contains("Rebase on the latest upstream"));
            assert!(
                !cwd.join(".git/groove").exists(),
                "context hook acquired a reservation"
            );
        }
    }
    assert_eq!(repo.git(&["status", "--porcelain"]), status);
    repo.git(&["config", "--local", "groove.agent.enabled", "false"]);
    assert!(hook(&repo, &repo.path, &input, None).stdout.is_empty());
}

#[test]
fn context_hook_rejects_invalid_payloads_and_unreadable_or_oversized_instructions() {
    let repo = Repo::new();
    repo.git(&["config", "--local", "groove.agent.enabled", "true"]);
    for input in [
        "broken JSON".to_owned(),
        event(&repo.path, "WorktreeRemove"),
        event(Path::new("relative"), "SessionStart"),
        format!(
            "{}{}",
            event(&repo.path, "SessionStart"),
            "\n".repeat(65536)
        ),
    ] {
        let output = hook(&repo, &repo.path, &input, None);
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
    }
    let instructions = repo.temp.path().join("workflow.md");
    for path in [
        "relative.md",
        instructions.to_str().unwrap(),
        repo.path.to_str().unwrap(),
    ] {
        repo.git(&["config", "--local", "groove.agent.instructionsFile", path]);
        let output = hook(&repo, &repo.path, &event(&repo.path, "SessionStart"), None);
        assert!(!output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty());
    }
    repo.git(&[
        "config",
        "--local",
        "groove.agent.instructionsFile",
        instructions.to_str().unwrap(),
    ]);
    fs::write(&instructions, "\n".repeat(32769)).unwrap();
    let output = hook(&repo, &repo.path, &event(&repo.path, "SessionStart"), None);
    assert!(!output.status.success(), "{output:?}");
    assert!(output.stdout.is_empty());
}
