mod support;

use serde_json::Value;
use std::{ffi::OsStr, fs, os::unix::ffi::OsStrExt, path::Path, process::Command};
use support::{Repo, TestChild};

const OPAQUE: &[u8] = b"ordinary value\xff\xfe with spaces\n";
const PROJECT: &str = r#"
schema_version=1
[[setup.dev.steps]]
name="environment"
command=["bash", "environment-peer.sh", "setup"]
[recipes.check]
command=["bash", "environment-peer.sh", "recipe"]
requires_setup="dev"
[agents.fixture]
command=["bash", "environment-peer.sh", "agent"]
requires_setup="dev"
"#;
const PEER: &str = r#"set -eu
test "${GIT_DIR+present}" != present
# Control user configuration explicitly while retaining the worker's repository
# selection and ordinary inherited environment for the real Git invocations.
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null
git rev-parse --show-toplevel > "$1.root"
printf '%s\n' "$1" >> tracked.txt
git add -- tracked.txt
printf '%s' "$AUDIT_OPAQUE"
"#;

fn command(repo: &Repo, args: &[&str]) -> Command {
    let mut command = support::groove_in(&repo.path);
    command
        .arg("--repo")
        .arg(&repo.path)
        .args(args)
        .env("GROOVE_LOG_ENABLED", "false");
    command
}

fn success(command: &mut Command) -> Value {
    let output = TestChild::spawn(command).wait_with_output().unwrap();
    let response = support::assert_success(&output);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    assert_eq!(response["ok"], true);
    assert!(output.stderr.is_empty(), "{output:?}");
    response["result"].clone()
}

#[test]
fn setup_recipe_and_agent_scrub_hostile_git_selection_and_preserve_non_utf8_environment() {
    let repo = Repo::new();
    fs::write(repo.path.join(".groove.toml"), PROJECT).unwrap();
    fs::write(repo.path.join("environment-peer.sh"), PEER).unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "environment workflows"]);
    success(&mut command(&repo, &["init", "--capacity", "1"]));
    let lease = success(&mut command(&repo, &["acquire", "--task", "environment"]));
    let id = lease["lease_id"].as_str().unwrap();
    let checkout = Path::new(lease["worktree_path"].as_str().unwrap());

    let other = Repo::new();
    fs::write(other.path.join("tracked.txt"), "foreign staged\n").unwrap();
    other.git(&["add", "tracked.txt"]);
    fs::write(other.path.join("tracked.txt"), "foreign working\n").unwrap();
    fs::write(other.path.join("precious.txt"), b"untouched\0\xff").unwrap();
    let other_status = other.git(&["status", "--porcelain=v1", "-z"]);
    let other_refs = other.git(&["show-ref"]);
    let primary_refs = repo.git(&["show-ref"]);
    let mut expected = String::from("baseline\n");

    for (flag, name, phase) in [
        ("--setup", "dev", "setup"),
        ("--recipe", "check", "recipe"),
        ("--agent", "fixture", "agent"),
    ] {
        // Set these AFTER groove_in's fixture scrubbing. They reach the actual
        // CLI parent, exercising discovery as well as each managed entry point.
        let result = success(
            command(
                &repo,
                &[
                    "exec",
                    "--lease-id",
                    id,
                    flag,
                    name,
                    "--quiet",
                    "--timeout-ms",
                    "10000",
                    "--grace-ms",
                    "0",
                ],
            )
            .env("GIT_DIR", other.path.join(".git"))
            .env("AUDIT_OPAQUE", OsStr::from_bytes(OPAQUE)),
        );
        let execution = if flag == "--setup" {
            assert_eq!(result["preparation"]["status"], "ready");
            &result["preparation"]["steps"][0]["execution"]
        } else {
            &result["execution"]
        };
        assert_eq!(execution["lease_id"], id);
        assert_eq!(execution["termination"], "exited");
        assert_eq!(execution["exit_code"], 0);
        assert_eq!(
            fs::read(execution["run"]["stdout_path"].as_str().unwrap()).unwrap(),
            OPAQUE
        );
        let root = fs::read_to_string(checkout.join(format!("{phase}.root"))).unwrap();
        assert_eq!(
            fs::canonicalize(root.trim_end_matches('\n')).unwrap(),
            fs::canonicalize(checkout).unwrap()
        );
        expected.push_str(&format!("{phase}\n"));
        assert_eq!(
            fs::read(checkout.join("tracked.txt")).unwrap(),
            expected.as_bytes()
        );
        assert_eq!(
            support::git(checkout, &["show", ":tracked.txt"]),
            expected.trim_end_matches('\n')
        );
        assert_eq!(
            fs::read(other.path.join("tracked.txt")).unwrap(),
            b"foreign working\n"
        );
        assert_eq!(
            fs::read(other.path.join("precious.txt")).unwrap(),
            b"untouched\0\xff"
        );
        assert_eq!(other.git(&["show", ":tracked.txt"]), "foreign staged");
        assert_eq!(other.git(&["status", "--porcelain=v1", "-z"]), other_status);
        assert_eq!(other.git(&["show-ref"]), other_refs);
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        assert!(repo.git(&["status", "--porcelain=v1"]).is_empty());
        assert_eq!(repo.git(&["show-ref"]), primary_refs);
    }
}
