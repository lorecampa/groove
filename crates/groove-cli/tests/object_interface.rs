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
