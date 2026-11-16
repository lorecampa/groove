mod support;
use serde_json::Value;
use std::fs;
use support::Repo;
fn checked(repo: &Repo, args: &[&str]) -> Value {
    let output = support::groove_in(&repo.path).args(args).output().unwrap();
    let response = support::assert_success(&output);
    validate(&response);
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1
    );
    let result = response["result"].clone();
    result.get("prune").cloned().unwrap_or(result)
}
fn validate(response: &Value) {
    assert_eq!(response["api_version"], 2);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(response)
        .unwrap();
}
fn save(repo: &Repo) -> String {
    checked(repo, &["snapshot", "save"])["snapshot"]["id"]
        .as_str()
        .unwrap()
        .into()
}
fn prune<'a>(extra: &'a [&'a str]) -> Vec<&'a str> {
    let mut args = vec![
        "snapshot",
        "prune",
        "--older-than",
        "0s",
        "--keep-last",
        "0",
    ];
    args.extend_from_slice(extra);
    args
}
#[test]
fn default_preview_preserves_pinned_and_unpinned_recovery() {
    let repo = Repo::new();
    fs::write(repo.path.join("tracked.txt"), "recoverable\n").unwrap();
    let id = save(&repo);
    let reference = format!("refs/groove/snapshots/{id}");
    let oid = repo.git(&["rev-parse", &reference]);
    let preview = checked(&repo, &prune(&[]));
    assert_eq!(preview["applied"], false);
    assert_eq!(preview["selected"], serde_json::json!([id]));
    assert_eq!(preview["deleted"], serde_json::json!([]));
    assert_eq!(repo.git(&["rev-parse", &reference]), oid);
    repo.git(&[
        "update-ref",
        &format!("refs/groove/snapshot-pins/{id}"),
        &oid,
    ]);
    let applied = checked(&repo, &prune(&["--apply"]));
    assert_eq!(applied["deleted"], serde_json::json!([]));
    assert_eq!(applied["retained"][0]["reason"], "pinned");
    let recovery = repo.temp.path().join("recovery");
    checked(
        &repo,
        &[
            "snapshot",
            "restore",
            &id,
            "--to",
            recovery.to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(recovery.join("tracked.txt")).unwrap(),
        b"recoverable\n"
    );
}

#[test]
fn retention_arguments_require_an_age_and_reject_conflicting_or_invalid_policies() {
    let repo = Repo::new();
    let cases: &[&[&str]] = &[
        &["snapshot", "prune"],
        &["snapshot", "prune", "--older-than", "-1d"],
        &["snapshot", "prune", "--older-than", "1.5d"],
        &["snapshot", "prune", "--older-than", "999999999999999999d"],
        &["snapshot", "prune", "--older-than", "30"],
        &[
            "snapshot",
            "prune",
            "--older-than",
            "30d",
            "--keep-last",
            "-1",
        ],
        &[
            "snapshot",
            "prune",
            "--older-than",
            "30d",
            "--apply",
            "--dry-run",
        ],
        &["snapshot", "prune", "--older-than", "30d", "--force"],
    ];
    for args in cases {
        let output = support::groove_in(&repo.path).args(*args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}: {output:?}");
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        validate(&response);
        assert_eq!(response["error"]["code"], "INVALID_ARGUMENT");
    }
    for age in ["0s", "1ms", "1m", "24h", "30d"] {
        assert_eq!(
            checked(
                &repo,
                &["snapshot", "prune", "--older-than", age, "--dry-run"]
            )["applied"],
            false
        );
    }
}

#[cfg(feature = "test-hooks")]
#[test]
fn release_checkpoint_is_retained_during_the_unpinned_receipt_interval() {
    use std::{io::Write, os::unix::net::UnixListener};
    let repo = Repo::new();
    checked(
        &repo,
        &["pool", "init", "--capacity", "1", "--ready-target", "0"],
    );
    let acquired = checked(&repo, &["acquire", "--task", "release-gap"]);
    let workspace = std::path::Path::new(acquired["workspace_path"].as_str().unwrap());
    fs::write(workspace.join("result.txt"), b"saved result\n").unwrap();
    let listener = UnixListener::bind(repo.temp.path().join("release-gap.sock")).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["release", "--save", "--task", "release-gap"])
        .env("GROOVE_TEST_CHECKPOINT", "finish-before-pin")
        .env(
            "GROOVE_TEST_SOCKET",
            repo.temp.path().join("release-gap.sock"),
        );
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = child
        .checkpoint(&listener, "finish-before-pin", support::PROCESS_TIMEOUT)
        .unwrap();
    let listed = checked(&repo, &["snapshot", "list"]);
    let saved = &listed["snapshots"][0];
    assert_eq!(saved["pinned"], false);
    let applied = checked(&repo, &prune(&["--apply"]));
    assert_eq!(applied["deleted"], serde_json::json!([]));
    assert_eq!(applied["retained"][0]["id"], saved["id"]);
    assert_eq!(applied["retained"][0]["reason"], "protected_workspace");
    stream.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    let released = support::assert_success(&output);
    validate(&released);
    assert_eq!(released["result"]["snapshot"]["id"], saved["id"]);
    assert_eq!(
        checked(&repo, &prune(&["--apply"]))["retained"][0]["reason"],
        "pinned"
    );
}

#[cfg(feature = "test-hooks")]
fn raced_selection(mutation: &str) {
    use std::{io::Write, os::unix::net::UnixListener};
    let repo = Repo::new();
    let id = save(&repo);
    let reference = format!("refs/groove/snapshots/{id}");
    let oid = repo.git(&["rev-parse", &reference]);
    let socket = repo.temp.path().join("prune.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(prune(&["--apply"]))
        .env("GROOVE_TEST_CHECKPOINT", "snapshot-prune-selected")
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = child
        .checkpoint(
            &listener,
            "snapshot-prune-selected",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    let pin = format!("refs/groove/snapshot-pins/{id}");
    let replacement = repo.git(&["rev-parse", "HEAD"]);
    match mutation {
        "pin" => {
            repo.git(&["update-ref", &pin, &oid]);
        }
        "dangling" => {
            repo.git(&["symbolic-ref", &pin, "refs/heads/missing-pin"]);
        }
        "ref" => {
            repo.git(&["update-ref", &reference, &replacement]);
        }
        _ => unreachable!(),
    }
    stream.write_all(&[1]).unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(7));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "GIT_ERROR");
    validate(&response);
    assert_eq!(
        repo.git(&["rev-parse", &reference]),
        if mutation == "ref" {
            replacement.as_str()
        } else {
            oid.as_str()
        }
    );
    if mutation == "dangling" {
        assert_eq!(repo.git(&["symbolic-ref", &pin]), "refs/heads/missing-pin");
    }
    if mutation == "pin" {
        assert_eq!(repo.git(&["rev-parse", &pin]), oid);
    }
    // Repair only the injected race, then prove transaction cleanup permits retry.
    if mutation == "ref" {
        repo.git(&["update-ref", &reference, &oid]);
    } else {
        repo.git(&["update-ref", "--no-deref", "-d", &pin]);
    }
    assert_eq!(
        checked(&repo, &prune(&["--apply"]))["deleted"],
        serde_json::json!([id])
    );
}
#[cfg(feature = "test-hooks")]
#[test]
fn concurrent_pin_creation_preserves_selected_snapshot() {
    raced_selection("pin");
}
#[cfg(feature = "test-hooks")]
#[test]
fn concurrent_dangling_pin_preserves_selected_snapshot() {
    raced_selection("dangling");
}
#[cfg(feature = "test-hooks")]
#[test]
fn concurrent_ref_replacement_preserves_successor() {
    raced_selection("ref");
}

#[cfg(feature = "test-hooks")]
#[test]
fn killed_apply_retries_remaining_selection() {
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    let first = save(&repo);
    let second = save(&repo);
    let socket = repo.temp.path().join("kill.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(prune(&["--apply"]))
        .env("GROOVE_TEST_CHECKPOINT", "snapshot-prune-deleted")
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(
            &listener,
            "snapshot-prune-deleted",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    let listed = checked(&repo, &["snapshot", "list"]);
    assert_eq!(listed["snapshots"].as_array().unwrap().len(), 1);
    assert_eq!(listed["snapshots"][0]["id"], first);
    let retry = checked(&repo, &prune(&["--apply"]));
    assert_eq!(retry["deleted"], serde_json::json!([first]));
    assert_ne!(first, second);
    assert!(checked(&repo, &["snapshot", "list"])["snapshots"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[cfg(feature = "test-hooks")]
fn interrupted_restore_safety(point: &str) {
    use std::os::unix::net::UnixListener;
    let repo = Repo::new();
    let baseline = save(&repo);
    fs::write(repo.path.join("tracked.txt"), "precious later bytes\n").unwrap();
    let socket = repo.temp.path().join("restore.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .args(["snapshot", "restore", &baseline])
        .env("GROOVE_TEST_CHECKPOINT", point)
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(&listener, point, support::PROCESS_TIMEOUT)
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    let listed = checked(&repo, &["snapshot", "list"]);
    let safety = listed["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] != baseline)
        .unwrap();
    assert_eq!(safety["pinned"], true);
    let safety_id = safety["id"].as_str().unwrap();
    let preview = checked(&repo, &prune(&[]));
    assert_eq!(preview["retained"][0]["id"], safety_id);
    assert_eq!(preview["retained"][0]["reason"], "pinned");
    checked(&repo, &prune(&["--apply"]));
    let recovery = repo.temp.path().join("recover-safety");
    checked(
        &repo,
        &[
            "snapshot",
            "restore",
            safety_id,
            "--to",
            recovery.to_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(recovery.join("tracked.txt")).unwrap(),
        b"precious later bytes\n"
    );
}

#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_restore_safety_pin_survives_prune_and_recovers_bytes() {
    interrupted_restore_safety("snapshot-restored-files");
}

#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_restore_before_mutation_keeps_recovery_pinned() {
    interrupted_restore_safety("snapshot-safety-pinned");
}
