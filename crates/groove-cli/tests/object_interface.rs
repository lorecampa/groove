mod support;

use serde_json::Value;
use support::Repo;

fn invoke(repo: &Repo, args: &[&str], code: i32) -> Value {
    let output = support::groove_in(&repo.path).args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response
}

#[test]
fn pool_objects_accept_positional_names_and_legacy_flags_without_ambiguity() {
    let repo = Repo::new();
    for name in ["default", "frontend"] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                name,
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
            ],
            0,
        );
    }
    assert_eq!(
        invoke(&repo, &["pool", "show", "frontend"], 0)["result"]["pool"]["pool_name"],
        "frontend"
    );
    assert_eq!(
        invoke(&repo, &["pool", "get", "frontend", "capacity"], 0)["result"]["value"],
        1
    );
    assert_eq!(
        invoke(&repo, &["pool", "get", "capacity"], 0)["result"]["value"],
        1
    );
    invoke(&repo, &["pool", "set", "frontend", "--capacity", "2"], 0);
    assert_eq!(
        invoke(&repo, &["--pool", "frontend", "pool", "get", "capacity"], 0)["result"]["value"],
        2
    );
    assert_eq!(
        invoke(&repo, &["--pool", "default", "pool", "show", "frontend"], 2)["error"]["code"],
        "INVALID_ARGUMENT"
    );
    invoke(&repo, &["pool", "destroy", "frontend", "--dry-run"], 0);
    invoke(&repo, &["pool", "destroy", "frontend"], 0);
    assert_eq!(
        invoke(&repo, &["pool", "get", "default", "capacity"], 0)["result"]["value"],
        1
    );
}

#[test]
fn reservation_selectors_search_pools_and_explicit_misses_explain_the_location() {
    let repo = Repo::new();
    for name in ["default", "frontend"] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                name,
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
            ],
            0,
        );
    }
    for selector in ["--task", "--reservation-id"] {
        let lease = invoke(
            &repo,
            &["--pool", "frontend", "acquire", "--task", "fix"],
            0,
        );
        let id = lease["result"]["reservation_id"].as_str().unwrap();
        let value = if selector == "--task" { "fix" } else { id };
        assert_eq!(
            invoke(&repo, &["show", selector, value], 0)["result"]["reservation"]["reservation_id"],
            id
        );
        let wrong = invoke(&repo, &["--pool", "default", "release", selector, value], 5);
        assert_eq!(wrong["error"]["code"], "RESERVATION_MISMATCH");
        assert!(wrong["error"]["message"]
            .as_str()
            .unwrap()
            .contains("frontend"));
        assert!(!wrong["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("--pool default"));
        assert_eq!(
            invoke(&repo, &["show", selector, value], 0)["result"]["active"],
            true
        );
        invoke(&repo, &["release", selector, value], 0);
    }
    let missing = invoke(&repo, &["show", "--task", "missing"], 5);
    assert!(missing["error"]["message"]
        .as_str()
        .unwrap()
        .contains("any repository pool"));
    assert!(!missing["error"]["hint"]
        .as_str()
        .unwrap()
        .contains("--pool default"));
}

#[test]
fn saved_release_retries_find_the_original_pool_after_slot_reuse() {
    let repo = Repo::new();
    invoke(
        &repo,
        &[
            "pool",
            "init",
            "frontend",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
        0,
    );
    let lease = invoke(
        &repo,
        &["--pool", "frontend", "acquire", "--task", "first"],
        0,
    );
    let id = lease["result"]["reservation_id"].as_str().unwrap();
    let saved = invoke(&repo, &["release", "--reservation-id", id, "--save"], 0);
    let next = invoke(
        &repo,
        &["--pool", "frontend", "acquire", "--task", "next"],
        0,
    );
    assert_eq!(
        invoke(&repo, &["release", "--reservation-id", id, "--save"], 0)["result"]["snapshot"]
            ["id"],
        saved["result"]["snapshot"]["id"]
    );
    assert_eq!(
        invoke(&repo, &["show", "--task", "next"], 0)["result"]["reservation"]["reservation_id"],
        next["result"]["reservation_id"]
    );
    invoke(&repo, &["release", "--task", "next"], 0);
}

#[test]
fn listing_includes_every_pool_before_global_pagination() {
    let repo = Repo::new();
    for (name, task) in [("frontend", "nav"), ("backend", "api")] {
        invoke(
            &repo,
            &[
                "pool",
                "init",
                name,
                "--capacity",
                "1",
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
            ],
            0,
        );
        invoke(&repo, &["--pool", name, "acquire", "--task", task], 0);
    }
    let all = invoke(&repo, &["list", "--unlimited"], 0);
    assert_eq!(all["result"]["reservations"].as_array().unwrap().len(), 2);
    assert_eq!(all["result"]["reservations"][0]["pool"], "backend");
    assert_eq!(all["result"]["reservations"][1]["pool"], "frontend");
    let page = invoke(&repo, &["list", "--limit", "1", "--offset", "1"], 0);
    assert_eq!(page["result"]["pagination"]["total"], 2);
    assert_eq!(page["result"]["reservations"][0]["pool"], "frontend");
    let filtered = invoke(&repo, &["list", "--pool", "frontend"], 0);
    assert_eq!(
        filtered["result"]["reservations"].as_array().unwrap().len(),
        1
    );
    let human = support::groove_human_in(&repo.path)
        .arg("list")
        .output()
        .unwrap();
    assert!(human.status.success());
    assert!(String::from_utf8(human.stdout).unwrap().contains("POOL"));
    invoke(&repo, &["release", "--task", "nav"], 0);
    invoke(&repo, &["release", "--task", "api"], 0);
}
