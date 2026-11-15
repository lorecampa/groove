mod support;

use serde_json::Value;
use std::fs;
use support::Repo;

fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    support::groove_in(&repo.path).args(args).output().unwrap()
}

fn saved(repo: &Repo) -> String {
    support::assert_success(&cli(repo, &["snapshot", "save"]))["result"]["snapshot"]["id"]
        .as_str()
        .unwrap()
        .into()
}

fn git_error(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(7));
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["error"]["code"], "GIT_ERROR");
}

#[test]
fn invalid_pin_reads_report_one_error_and_preserve_the_snapshot() {
    let repo = Repo::new();
    let id = saved(&repo);
    let reference = format!("refs/groove/snapshots/{id}");
    let pin = format!("refs/groove/snapshot-pins/{id}");
    let oid = repo.git(&["rev-parse", &reference]);
    repo.git(&["update-ref", &pin, &oid]);
    let path = repo.path.join(".git").join(&pin);
    fs::write(&path, "broken\n").unwrap();
    for args in [
        vec!["snapshot", "show", &id],
        vec!["snapshot", "list"],
        vec!["snapshot", "delete", &id],
        vec!["snapshot", "delete", &id, "--force"],
    ] {
        git_error(&cli(&repo, &args));
        assert_eq!(fs::read_to_string(&path).unwrap(), "broken\n");
        assert_eq!(repo.git(&["rev-parse", &reference]), oid);
    }
}

#[cfg(feature = "test-hooks")]
#[test]
fn a_pin_created_or_changed_before_delete_aborts_the_whole_transaction() {
    use std::{io::Write, os::unix::net::UnixListener};
    for (format, was_pinned, mutation) in ["sha1", "sha256"].into_iter().flat_map(|format| {
        [false, true].into_iter().flat_map(move |was_pinned| {
            ["direct", "dangling", "symbolic", "malformed", "zero"]
                .into_iter()
                .map(move |mutation| (format, was_pinned, mutation))
        })
    }) {
        let repo = Repo::format(format);
        fs::write(
            repo.path.join("tracked.txt"),
            "recoverable snapshot bytes\n",
        )
        .unwrap();
        let id = saved(&repo);
        let other = saved(&repo);
        let reference = format!("refs/groove/snapshots/{id}");
        let pin = format!("refs/groove/snapshot-pins/{id}");
        let oid = repo.git(&["rev-parse", &reference]);
        let other_oid = repo.git(&["rev-parse", &format!("refs/groove/snapshots/{other}")]);
        let target = "refs/heads/precious-pin-target";
        repo.git(&["update-ref", target, &oid]);
        let original_head = repo.git(&["rev-parse", "HEAD"]);
        let original_index = fs::read(repo.path.join(".git/index")).unwrap();
        if was_pinned {
            repo.git(&["update-ref", &pin, &oid]);
        }
        let socket = repo.temp.path().join("pin-race.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut command = support::groove_in(&repo.path);
        command.args(["snapshot", "delete", &id]);
        if was_pinned {
            command.arg("--force");
        }
        command
            .env("GROOVE_TEST_CHECKPOINT", "snapshot-before-delete")
            .env("GROOVE_TEST_SOCKET", &socket);
        let mut child = support::TestChild::spawn(&mut command);
        let mut stream = child
            .checkpoint(
                &listener,
                "snapshot-before-delete",
                support::PROCESS_TIMEOUT,
            )
            .unwrap();
        let pin_path = repo.path.join(".git").join(&pin);
        match mutation {
            "direct" => {
                let racing_oid = if was_pinned { &other_oid } else { &oid };
                repo.git(&["update-ref", &pin, racing_oid]);
            }
            "dangling" => {
                repo.git(&["symbolic-ref", &pin, "refs/heads/missing-pin-target"]);
            }
            "symbolic" => {
                // Same resolved OID defeats an OID-only check. The unrelated
                // referent must survive both forced and unforced deletion.
                repo.git(&["symbolic-ref", &pin, target]);
            }
            "malformed" | "zero" => {
                fs::create_dir_all(pin_path.parent().unwrap()).unwrap();
                fs::write(
                    &pin_path,
                    if mutation == "zero" {
                        format!("{}\n", "0".repeat(oid.len()))
                    } else {
                        "broken\n".into()
                    },
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        let racing_pin = fs::read(&pin_path).unwrap();
        stream.write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        git_error(&output);
        assert_eq!(repo.git(&["rev-parse", &reference]), oid);
        assert_eq!(
            fs::read(&pin_path).unwrap(),
            racing_pin,
            "{format}/{was_pinned}/{mutation}"
        );
        assert_eq!(repo.git(&["rev-parse", target]), oid);
        assert_eq!(repo.git(&["rev-parse", "HEAD"]), original_head);
        assert_eq!(
            fs::read(repo.path.join(".git/index")).unwrap(),
            original_index
        );
        assert_eq!(
            repo.git(&["show", &format!("{oid}:work/tracked.txt")]),
            "recoverable snapshot bytes"
        );
        assert_eq!(
            fs::read(repo.path.join("tracked.txt")).unwrap(),
            b"recoverable snapshot bytes\n"
        );
        // Restoring the inspected pin state makes the operation retryable;
        // an aborted prepared transaction must not leave Git lock files behind.
        if was_pinned {
            fs::write(&pin_path, format!("{oid}\n")).unwrap();
        } else {
            fs::remove_file(&pin_path).unwrap();
        }
        let mut retry = vec!["snapshot", "delete", &id];
        if was_pinned {
            retry.push("--force");
        }
        support::assert_success(&cli(&repo, &retry));
        assert!(!pin_path.exists());
        assert_eq!(repo.git(&["rev-parse", target]), oid);
    }
}
