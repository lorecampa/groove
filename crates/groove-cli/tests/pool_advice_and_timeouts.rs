mod support;

use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
    sync::OnceLock,
    time::{Duration, Instant},
};
use support::Repo;

fn check(output: Output, code: i32) -> Value {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    static SCHEMA: OnceLock<jsonschema::Validator> = OnceLock::new();
    SCHEMA
        .get_or_init(|| {
            let schema: Value =
                serde_json::from_str(include_str!("../../../schemas/response-v2.schema.json"))
                    .unwrap();
            jsonschema::validator_for(&schema).unwrap()
        })
        .validate(&value)
        .unwrap();
    assert_eq!(value["ok"], code == 0);
    value
}

fn cli(path: &Path, args: &[&str]) -> Value {
    check(support::groove_in(path).args(args).output().unwrap(), 0)["result"].clone()
}

fn init(repo: &Repo, name: &str, capacity: &str) -> Value {
    check(
        support::groove_in(&repo.path)
            .args([
                "--pool",
                name,
                "pool",
                "init",
                "--capacity",
                capacity,
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
                "--workspace-root",
            ])
            .arg(support::worktree_root(&repo.path, name))
            .output()
            .unwrap(),
        0,
    )["result"]
        .clone()
}

fn slow_git(repo: &Repo) -> Command {
    let wrapper = repo.temp.path().join("wrapper");
    fs::create_dir_all(&wrapper).unwrap();
    let script = wrapper.join("git");
    fs::write(
        &script,
        r#"#!/bin/bash
if [[ -z "${GROOVE_SLOW_WHEN:-}" || "${!GROOVE_SLOW_WHEN:-}" == 1 ]]; then
  case " $* " in *" checkout --detach "*) sleep 3 ;; esac
fi
exec "$GROOVE_REAL_GIT" "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
    let real_git = Command::new("bash")
        .args(["-c", "command -v git"])
        .output()
        .unwrap();
    assert!(real_git.status.success());
    let mut command = support::groove_in(&repo.path);
    command
        .env(
            "PATH",
            format!("{}:{}", wrapper.display(), std::env::var("PATH").unwrap()),
        )
        .env(
            "GROOVE_REAL_GIT",
            String::from_utf8(real_git.stdout).unwrap().trim(),
        )
        .env_remove("GROOVE_SLOW_WHEN");
    command
}

#[test]
fn ready_git_timeout_is_optional_normalized_and_pool_local() {
    let repo = Repo::new();
    assert!(init(&repo, "default", "1")
        .get("ready_git_timeout")
        .is_none());
    assert!(cli(&repo.path, &["pool", "get", "ready-git-timeout"])["value"].is_null());
    let set = cli(&repo.path, &["pool", "set", "--ready-git-timeout", "10m"]);
    assert_eq!(set["updated"], true);
    assert_eq!(set["pool"]["ready_git_timeout"], "10m");
    assert_eq!(
        cli(&repo.path, &["pool", "get", "ready-git-timeout"])["value"],
        "10m"
    );
    assert_eq!(
        cli(&repo.path, &["pool", "show"])["pool"]["ready_git_timeout"],
        "10m"
    );
    assert_eq!(
        cli(&repo.path, &["pool", "set", "--ready-git-timeout", "120s"])["pool"]
            ["ready_git_timeout"],
        "2m"
    );
    for value in ["0s", "5", "-1m", "never"] {
        let error = check(
            support::groove_in(&repo.path)
                .args(["pool", "set", &format!("--ready-git-timeout={value}")])
                .output()
                .unwrap(),
            2,
        );
        assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
    }
    assert_eq!(
        cli(&repo.path, &["pool", "set", "--ready-git-timeout", "2m"])["updated"],
        false
    );
    init(&repo, "other", "1");
    assert!(cli(
        &repo.path,
        &["--pool", "other", "pool", "get", "ready-git-timeout"]
    )["value"]
        .is_null());
}

#[test]
fn worker_uses_the_persisted_git_timeout_and_reports_the_budget() {
    let repo = Repo::new();
    init(&repo, "default", "2");
    cli(&repo.path, &["pool", "set", "--ready-git-timeout", "300ms"]);
    check(
        slow_git(&repo)
            .env("GROOVE_SLOW_WHEN", "GROOVE_INTERNAL_READY_WORKER")
            .args(["pool", "set", "--ready-target", "1"])
            .output()
            .unwrap(),
        0,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let ready = loop {
        let shown = cli(&repo.path, &["pool", "show"]);
        let ready = &shown["ready"];
        if ready["running"] == false && ready["attempted_at_unix_ms"].as_u64().unwrap() > 0 {
            break ready.clone();
        }
        assert!(Instant::now() < deadline, "{shown}");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(ready["ready"], 0);
    assert!(!ready["failures"].as_object().unwrap().is_empty());
    for failure in ready["failures"].as_object().unwrap().values() {
        let failure = failure.as_str().unwrap();
        for expected in ["timed out", "after 300ms", "--ready-git-timeout"] {
            assert!(failure.contains(expected), "{failure}");
        }
    }
}
