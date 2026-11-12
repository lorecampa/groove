mod support;
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use support::{Repo, TestChild};

const PROJECT: &str = r#"
schema_version=1
[workspace]
port_base=9000
[workspace.environment]
LABEL="{slot}:{task}"
[caches.deps]
paths=["ignored/deps"]
inputs=["tracked.txt"]
key="fixture-v1"
[setup.dev]
cache="deps"
[[setup.dev.steps]]
name="dependencies"
command=["bash","-c","mkdir -p ignored/deps; printf artifact > ignored/deps/file"]
when="cache_miss"
[[setup.dev.steps]]
name="environment"
command=["bash","-c","printf '%s:%s' \"$LABEL\" \"$GROOVE_PORT\""]
[recipes.test]
command=["bash","-c","test -s ignored/deps/file"]
requires_setup="dev"
[agents.fixture]
command=["bash","-c","printf '%s' \"$1\"","fixture"]
requires_setup="dev"
"#;
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
        .map(|error| error.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {value}");
    value
}
fn success(out: &Output) -> Value {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    checked(out)["result"].clone()
}
fn repo(project: &str) -> Repo {
    let repo = Repo::new();
    fs::write(repo.path.join(".groove.toml"), project).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "project"]);
    success(&cli(&repo, &["init", "--capacity", "2"]));
    repo
}
fn wait_file(path: &Path) {
    let start = Instant::now();
    while !path.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "missing {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn explicit_setup_cache_recipe_agent_navigation_and_handoff_conform() {
    let repo = repo(PROJECT);
    let project = success(&cli(&repo, &["config", "project"]));
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/project-v1.schema.json")).unwrap();
    assert!(jsonschema::validator_for(&schema)
        .unwrap()
        .is_valid(&project["project"]));
    let first = success(&cli(
        &repo,
        &[
            "acquire",
            "--request-key",
            "first",
            "--task",
            "first",
            "--setup",
            "dev",
        ],
    ));
    assert_eq!(first["preparation"]["status"], "ready");
    assert_eq!(first["preparation"]["cache_hit"], false);
    let second = success(&cli(
        &repo,
        &["acquire", "--task", "second", "--setup", "dev"],
    ));
    assert_eq!(second["preparation"]["cache_hit"], true);
    let id = first["lease_id"].as_str().unwrap();
    assert_eq!(
        success(&cli(&repo, &["show", "--task", "first"]))["lease"]["worktree_path"],
        first["worktree_path"]
    );
    let slots = success(&cli(&repo, &["list", "--details"]));
    assert!(slots["inspection"]["slots"]
        .as_array()
        .unwrap()
        .iter()
        .all(|slot| slot["preparation"]["status"] == "ready" && slot["git"].is_object()));
    success(&cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--recipe",
            "test",
            "--grace-ms",
            "0",
        ],
    ));
    let run = success(&cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--agent",
            "fixture",
            "--grace-ms",
            "0",
            "--",
            "literal $(touch NOT_EXECUTED)",
        ],
    ));
    assert_eq!(
        fs::read_to_string(run["execution"]["run"]["stdout_path"].as_str().unwrap()).unwrap(),
        "literal $(touch NOT_EXECUTED)"
    );
    assert!(!Path::new(first["worktree_path"].as_str().unwrap())
        .join("NOT_EXECUTED")
        .exists());
    fs::write(
        Path::new(first["worktree_path"].as_str().unwrap()).join("tracked.txt"),
        "task result\n",
    )
    .unwrap();
    let finish = success(&cli(&repo, &["release", "--save", "--lease-id", id]));
    assert!(finish.get("patch_path").is_none());
    success(&cli(&repo, &["acquire", "--task", "successor"]));
    assert_eq!(
        success(&cli(&repo, &["release", "--save", "--lease-id", id])),
        finish
    );
    let recovered = repo.temp.path().join("recovered");
    success(&cli(
        &repo,
        &[
            "snapshot",
            "restore",
            finish["snapshot"]["id"].as_str().unwrap(),
            "--to",
            recovered.to_str().unwrap(),
        ],
    ));
    assert_eq!(
        fs::read(recovered.join("tracked.txt")).unwrap(),
        b"task result\n"
    );
}

#[test]
fn setup_failure_retains_identifiers_exit_code_and_explicit_retry() {
    let repo = repo(&PROJECT.replace(
        "mkdir -p ignored/deps; printf artifact > ignored/deps/file",
        "exit 23",
    ));
    let out = cli(
        &repo,
        &[
            "acquire",
            "--request-key",
            "failure",
            "--task",
            "broken",
            "--setup",
            "dev",
        ],
    );
    assert_eq!(out.status.code(), Some(23));
    let failed = checked(&out)["result"].clone();
    assert_eq!(failed["preparation"]["status"], "failed");
    assert_eq!(
        success(&cli(&repo, &["list"]))["leases"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let id = failed["lease_id"].as_str().unwrap();
    assert_eq!(
        checked(&cli(&repo, &["exec", "--lease-id", id, "--setup", "dev"]))["ok"],
        false
    );
    fs::write(
        Path::new(failed["worktree_path"].as_str().unwrap()).join(".groove.toml"),
        PROJECT,
    )
    .unwrap();
    let ready = success(&cli(
        &repo,
        &[
            "exec",
            "--lease-id",
            id,
            "--setup",
            "dev",
            "--retry",
            "--grace-ms",
            "0",
        ],
    ));
    assert_eq!(ready["preparation"]["status"], "ready");
    for args in [
        vec![
            "exec",
            "--lease-id",
            id,
            "--setup",
            "dev",
            "--recipe",
            "test",
        ],
        vec!["config", "project", "--capacity", "3"],
        vec!["show", "--task", "broken", "--slot", "wt-0001"],
    ] {
        assert_eq!(checked(&cli(&repo, &args))["ok"], false);
    }
}

#[test]
fn removed_patch_flags_fail_before_finishing_or_restoring() {
    let repo = repo(PROJECT);
    let lease = success(&cli(&repo, &["acquire"]));
    let id = lease["lease_id"].as_str().unwrap();
    fs::write(
        Path::new(lease["worktree_path"].as_str().unwrap()).join("tracked.txt"),
        "preserved result\n",
    )
    .unwrap();
    let patch = Path::new(lease["worktree_path"].as_str().unwrap()).join("result.patch");
    let failed = checked(&cli(
        &repo,
        &[
            "release",
            "--save",
            "--lease-id",
            id,
            "--patch",
            patch.to_str().unwrap(),
        ],
    ));
    assert_eq!(failed["ok"], false);
    assert_eq!(failed["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        success(&cli(&repo, &["show", "--lease-id", id]))["lease"]["lease_id"],
        id
    );
    assert!(!patch.exists());
    assert!(success(&cli(&repo, &["snapshot", "list"]))["snapshots"]
        .as_array()
        .unwrap()
        .is_empty());
    let finished = success(&cli(&repo, &["release", "--save", "--lease-id", id]));
    let outside = repo.temp.path().join("recovered.patch");
    let rejected = checked(&cli(
        &repo,
        &[
            "snapshot",
            "restore",
            finished["snapshot"]["id"].as_str().unwrap(),
            "--patch",
            outside.to_str().unwrap(),
        ],
    ));
    assert_eq!(rejected["error"]["code"], "INVALID_ARGUMENT");
    assert!(!outside.exists());
}

#[test]
fn live_setup_can_be_inspected_and_stopped_and_blocks_release() {
    let repo = repo(&PROJECT.replace(
        "mkdir -p ignored/deps; printf artifact > ignored/deps/file",
        "echo ready > started; while :; do sleep 1; done",
    ));
    let lease = success(&cli(&repo, &["acquire", "--task", "long"]));
    let id = lease["lease_id"].as_str().unwrap();
    let worker = TestChild::spawn(
        command(
            &repo,
            &[
                "exec",
                "--lease-id",
                id,
                "--setup",
                "dev",
                "--grace-ms",
                "0",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()),
    );
    wait_file(&Path::new(lease["worktree_path"].as_str().unwrap()).join("started"));
    let shown = success(&cli(&repo, &["show", "--task", "long"]));
    assert_eq!(shown["slot"]["preparation"]["status"], "preparing");
    assert_eq!(
        checked(&cli(&repo, &["release", "--lease-id", id]))["ok"],
        false
    );
    assert_eq!(
        success(&cli(&repo, &["stop", "--lease-id", id]))["preparation_stop_requested"]["status"],
        "preparing"
    );
    let out = worker.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143));
    assert_eq!(checked(&out)["result"]["preparation"]["status"], "failed");
    assert_eq!(
        success(&cli(&repo, &["list"]))["leases"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    success(&cli(&repo, &["release", "--force", "--lease-id", id]));
}

#[test]
fn setup_sigkill_retains_child_exclusion_and_requires_recovery_then_retry() {
    let repo = repo(&PROJECT.replace(
        "mkdir -p ignored/deps; printf artifact > ignored/deps/file",
        "echo ready > started; exec sleep 1",
    ));
    let lease = success(&cli(&repo, &["acquire"]));
    let id = lease["lease_id"].as_str().unwrap();
    let mut supervisor = TestChild::spawn(
        command(
            &repo,
            &[
                "exec",
                "--lease-id",
                id,
                "--setup",
                "dev",
                "--grace-ms",
                "0",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()),
    );
    wait_file(&Path::new(lease["worktree_path"].as_str().unwrap()).join("started"));
    supervisor.kill().unwrap();
    supervisor.wait().unwrap();
    for operation in ["release", "recover", "stop"] {
        assert_eq!(
            checked(&cli(&repo, &[operation, "--lease-id", id]))["ok"],
            false
        );
    }
    assert_eq!(
        checked(&cli(
            &repo,
            &["exec", "--lease-id", id, "--setup", "dev", "--retry"]
        ))["ok"],
        false
    );
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(
        checked(&cli(&repo, &["release", "--lease-id", id]))["ok"],
        false
    );
    success(&cli(&repo, &["recover", "--lease-id", id]));
    // Interrupted readiness is historical state, not a live supervisor. A new
    // managed repair command must still be stoppable before setup is retried.
    let raw = TestChild::spawn(
        command(
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
                "echo ready > raw.started; exec sleep 60",
            ],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()),
    );
    wait_file(&Path::new(lease["worktree_path"].as_str().unwrap()).join("raw.started"));
    let stopped = success(&cli(&repo, &["stop", "--lease-id", id]));
    assert!(stopped["stop_requested"].is_object());
    let out = raw.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143));
    assert_eq!(checked(&out)["result"]["execution"]["cancel_signal"], 15);
    assert_eq!(
        checked(&cli(&repo, &["exec", "--lease-id", id, "--setup", "dev"]))["ok"],
        false
    );
    fs::write(
        Path::new(lease["worktree_path"].as_str().unwrap()).join(".groove.toml"),
        PROJECT,
    )
    .unwrap();
    assert_eq!(
        success(&cli(
            &repo,
            &[
                "exec",
                "--lease-id",
                id,
                "--setup",
                "dev",
                "--retry",
                "--grace-ms",
                "0"
            ]
        ))["preparation"]["status"],
        "ready"
    );
}

#[test]
fn stop_cancels_cache_lock_contention_without_affecting_the_other_setup() {
    let repo = repo(&PROJECT.replace(
        "mkdir -p ignored/deps; printf artifact > ignored/deps/file",
        "echo ready > started; exec sleep 60",
    ));
    let first = success(&cli(&repo, &["acquire"]));
    let a = first["lease_id"].as_str().unwrap();
    let second = success(&cli(&repo, &["acquire"]));
    let b = second["lease_id"].as_str().unwrap();
    let one = TestChild::spawn(
        command(
            &repo,
            &["exec", "--lease-id", a, "--setup", "dev", "--grace-ms", "0"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()),
    );
    wait_file(&Path::new(first["worktree_path"].as_str().unwrap()).join("started"));
    let two = TestChild::spawn(
        command(
            &repo,
            &["exec", "--lease-id", b, "--setup", "dev", "--grace-ms", "0"],
        )
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()),
    );
    let pool = groove_core::Pool::open(&repo.path).unwrap();
    let bid = b.parse().unwrap();
    let start = Instant::now();
    while pool.preparation(bid).unwrap().is_none() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    success(&cli(&repo, &["stop", "--lease-id", b]));
    let out = two.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(143));
    assert_eq!(checked(&out)["result"]["preparation"]["status"], "failed");
    assert!(!Path::new(second["worktree_path"].as_str().unwrap())
        .join("started")
        .exists());
    assert_eq!(
        pool.preparation(a.parse().unwrap())
            .unwrap()
            .unwrap()
            .status,
        groove_core::PreparationStatus::Preparing
    );
    success(&cli(&repo, &["stop", "--lease-id", a]));
    assert_eq!(one.wait_with_output().unwrap().status.code(), Some(143));
    assert_eq!(pool.protected_leases().unwrap().len(), 2);
}

#[test]
fn setup_recipes_and_agents_obey_worker_output_policy() {
    let definition = r#"
schema_version=1
[[setup.dev.steps]]
name="announce"
command=["printf","setup-bytes"]
[recipes.test]
command=["printf","recipe-bytes"]
[agents.fixture]
command=["printf","agent-bytes"]
"#;
    for quiet in [false, true] {
        let repo = repo(definition);
        let invoke = |args: &[&str]| {
            let mut cmd = command(&repo, args);
            if quiet {
                cmd.arg("--quiet");
            }
            cmd.output().unwrap()
        };
        let acquired = invoke(&["acquire", "--task", "acquire-setup", "--setup", "dev"]);
        success(&acquired);
        assert_eq!(
            acquired.stderr,
            if quiet { &b""[..] } else { &b"setup-bytes"[..] }
        );
        success(&invoke(&["acquire", "--task", "exec-setup"]));
        for (flag, name, bytes) in [
            ("--setup", "dev", "setup-bytes"),
            ("--recipe", "test", "recipe-bytes"),
            ("--agent", "fixture", "agent-bytes"),
        ] {
            let out = invoke(&["exec", "--task", "exec-setup", flag, name]);
            let result = success(&out);
            assert_eq!(out.stderr, if quiet { &b""[..] } else { bytes.as_bytes() });
            let run = if flag == "--setup" {
                &result["preparation"]["steps"][0]["execution"]["run"]
            } else {
                &result["execution"]["run"]
            };
            assert_eq!(
                fs::read_to_string(run["stdout_path"].as_str().unwrap()).unwrap(),
                bytes
            );
        }
    }
}
