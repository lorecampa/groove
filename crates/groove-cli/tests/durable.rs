mod support;
use serde_json::Value;
use std::fs;
use support::{assert_success, Repo};

fn cli(path: &std::path::Path, args: &[&str]) -> std::process::Output {
    support::groove_in(path).args(args).output().unwrap()
}
fn checked(output: &std::process::Output) -> Value {
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let errors: Vec<_> = validator
        .iter_errors(&response)
        .map(|e| e.to_string())
        .collect();
    assert!(errors.is_empty(), "{errors:?}: {response}");
    response
}
fn release_args(lease: &Value) -> Vec<&str> {
    vec!["release", "--lease-id", lease["lease_id"].as_str().unwrap()]
}

#[test]
fn shell_calls_retain_reservations_and_reuse_only_after_explicit_release() {
    let repo = Repo::new();
    // Discover from a subdirectory, without --repo.
    let subdir = repo.path.join("nested");
    fs::create_dir(&subdir).unwrap();
    assert_success(&cli(&subdir, &["init", "--capacity", "1"]));
    let out = cli(&subdir, &["acquire"]);
    assert_success(&out);
    let first = checked(&out)["result"].clone();
    assert_eq!(first["ownership"], "durable");
    let path = std::path::Path::new(first["worktree_path"].as_str().unwrap());
    fs::write(path.join("tracked.txt"), "task work\n").unwrap();
    let listed = cli(&repo.path, &["list"]);
    assert_success(&listed);
    assert_eq!(checked(&listed)["result"]["leases"][0], first);
    let blocked = cli(&repo.path, &["acquire"]);
    assert!(!blocked.status.success());
    assert_eq!(checked(&blocked)["error"]["code"], "POOL_EXHAUSTED");
    // Neither CLI nor direct core acquisition can clean the reserved checkout.
    assert_eq!(
        groove_core::Pool::open(&repo.path)
            .unwrap()
            .acquire(Default::default())
            .unwrap_err()
            .code,
        groove_core::ErrorCode::PoolExhausted
    );
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "task work\n"
    );
    // Removed options fail as JSON argument errors without releasing the reservation.
    let mut obsolete = release_args(&first);
    obsolete.push("--writers-stopped");
    let rejected = cli(&repo.path, &obsolete);
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(checked(&rejected)["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        checked(&cli(&repo.path, &["list"]))["result"]["leases"][0],
        first
    );
    // Preserve the result on a branch before returning the workspace.
    support::git(path, &["switch", "-c", "task-result"]);
    support::git(path, &["add", "tracked.txt"]);
    support::git(path, &["commit", "-qm", "saved task"]);
    let released = cli(&repo.path, &release_args(&first));
    assert_success(&released);
    checked(&released);
    assert_success(&cli(&repo.path, &release_args(&first))); // lost-response retry
    let out = cli(
        &repo.path,
        &["acquire", "--base", &repo.git(&["rev-parse", "HEAD"])],
    );
    assert_success(&out);
    let second = checked(&out)["result"].clone();
    assert_eq!(first["slot_id"], second["slot_id"]);
    assert_ne!(first["lease_id"], second["lease_id"]);
    assert_eq!(
        fs::read_to_string(path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    assert_eq!(repo.git(&["show", "task-result:tracked.txt"]), "task work");
    assert!(!cli(&repo.path, &release_args(&first)).status.success());
    assert_eq!(
        checked(&cli(&repo.path, &["list"]))["result"]["leases"][0],
        second
    );
    assert_success(&cli(&repo.path, &release_args(&second)));
}

#[test]
fn concurrent_shell_acquisitions_obey_capacity() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["init", "--capacity", "1"]));
    let mut children = Vec::new();
    for _ in 0..6 {
        children.push(support::TestChild::spawn(
            support::groove_in(&repo.path)
                .current_dir(&repo.path)
                .arg("acquire")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        ));
    }
    let mut successes = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        checked(&out);
        successes += usize::from(out.status.success());
    }
    assert_eq!(successes, 1);
    let leases = checked(&cli(&repo.path, &["list"]));
    assert_success(&cli(
        &repo.path,
        &release_args(&leases["result"]["leases"][0]),
    ));
}

#[test]
fn lost_stdout_keeps_a_discoverable_reservation() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["init", "--capacity", "1"]));
    let status = support::groove_in(&repo.path)
        .current_dir(&repo.path)
        .arg("acquire")
        .stdout(support::failed_output())
        .status()
        .unwrap();
    assert!(!status.success());
    let leases = checked(&cli(&repo.path, &["list"]));
    assert_eq!(leases["result"]["leases"].as_array().unwrap().len(), 1);
    assert!(!cli(&repo.path, &["acquire"]).status.success());
    assert_success(&cli(
        &repo.path,
        &release_args(&leases["result"]["leases"][0]),
    ));
}

#[test]
fn protected_release_respects_live_owners_and_requires_protection() {
    let repo = Repo::new();
    assert_success(&cli(&repo.path, &["init", "--capacity", "1"]));
    let pool = groove_core::Pool::open(&repo.path).unwrap();
    let lease = pool.acquire(Default::default()).unwrap();
    let info = serde_json::to_value(lease.info()).unwrap();
    assert!(!cli(&repo.path, &release_args(&info)).status.success());
    drop(lease);
    // Even after descriptor closure, an unprotected leased record is not a release receipt.
    assert!(!cli(&repo.path, &release_args(&info)).status.success());
    let next = pool.acquire(Default::default()).unwrap();
    next.protect_from_reuse().unwrap();
    let info = serde_json::to_value(next.info()).unwrap();
    // A long-lived library caller can hold exactly this: a live descriptor and protection.
    assert!(!cli(&repo.path, &release_args(&info)).status.success());
    drop(next);
    assert_success(&cli(&repo.path, &release_args(&info)));
}
