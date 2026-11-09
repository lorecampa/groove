mod support;
use serde_json::Value;
use std::{
    fs,
    process::{Command, Stdio},
};
use support::{assert_success, Repo};
fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    support::command(&repo.path, env!("CARGO_BIN_EXE_groove"))
        .arg("snapshot")
        .args(args)
        .output()
        .unwrap()
}
fn checked(repo: &Repo, args: &[&str]) -> Value {
    let response = assert_success(&cli(repo, args));
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v1.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&response)
        .unwrap();
    response["result"]["snapshot_result"].clone()
}
#[test]
fn cli_end_to_end_lifecycle() {
    let repo = Repo::new();
    fs::write(repo.path.join("new"), "saved\n").unwrap();
    let saved = checked(&repo, &["save", "checkpoint"]);
    let id = saved["snapshot"]["id"].as_str().unwrap();
    assert_eq!(
        checked(&repo, &["list"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        checked(&repo, &["show", id])["snapshot"]["description"],
        "checkpoint"
    );
    fs::write(repo.path.join("new"), "later\n").unwrap();
    assert!(checked(&repo, &["diff", id])["patch"]
        .as_str()
        .unwrap()
        .contains("+later"));
    let dest = repo.temp.path().join("recover");
    checked(&repo, &["restore", id, "--to", dest.to_str().unwrap()]);
    assert_eq!(fs::read(dest.join("new")).unwrap(), b"saved\n");
    checked(&repo, &["restore", id]);
    assert_eq!(fs::read(repo.path.join("new")).unwrap(), b"saved\n");
    checked(&repo, &["pin", id]);
    assert!(!cli(&repo, &["delete", id]).status.success());
    checked(&repo, &["prune", "--keep-last", "0", "--dry-run"]);
    checked(&repo, &["unpin", id]);
    checked(&repo, &["delete", id]);
    checked(&repo, &["retention", "--keep-last", "2"]);
    checked(&repo, &["retention", "--disable"]);
    checked(&repo, &["compact"]);
    assert!(!cli(&repo, &["show", "../../heads/main"]).status.success());
    assert!(!cli(
        &repo,
        &["prune", "--older-than", "999999999999999999999999d"]
    )
    .status
    .success());
}
#[test]
fn lost_response_still_leaves_snapshot() {
    let repo = Repo::new();
    let status = Command::new(env!("CARGO_BIN_EXE_groove"))
        .current_dir(&repo.path)
        .args(["snapshot", "save"])
        .stdout(
            fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap(),
        )
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(
        checked(&repo, &["list"])["snapshots"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[cfg(feature = "test-hooks")]
#[test]
fn crash_publication_and_concurrent_writer_boundaries() {
    use std::{
        io::{Read, Write},
        os::unix::net::UnixListener,
    };
    for point in [
        "snapshot-captured",
        "snapshot-before-publish",
        "snapshot-published",
    ] {
        let repo = Repo::new();
        let index = fs::read(repo.path.join(".git/index")).unwrap();
        let socket = repo.temp.path().join("checkpoint.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut command = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"));
        command
            .args(["snapshot", "save"])
            .env("GROOVE_TEST_CHECKPOINT", point)
            .env("GROOVE_TEST_SOCKET", &socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = support::TestChild::spawn(&mut command);
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(20)))
            .unwrap();
        let mut bytes = vec![0; point.len()];
        stream.read_exact(&mut bytes).unwrap();
        assert!(!cli(&repo, &["save"]).status.success());
        child.kill().unwrap();
        child.wait().unwrap();
        drop(stream);
        assert_eq!(fs::read(repo.path.join(".git/index")).unwrap(), index);
        assert_eq!(
            checked(&repo, &["list"])["snapshots"]
                .as_array()
                .unwrap()
                .len(),
            usize::from(point == "snapshot-published")
        );
        checked(&repo, &["save"]);
        assert!(!fs::read_dir(repo.path.join(".git/groove-snapshots"))
            .unwrap()
            .any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("scratch-")));
    }
    let repo = Repo::new();
    let socket = repo.temp.path().join("writer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"));
    command
        .args(["snapshot", "save"])
        .env("GROOVE_TEST_CHECKPOINT", "snapshot-captured")
        .env("GROOVE_TEST_SOCKET", &socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = support::TestChild::spawn(&mut command);
    let (mut stream, _) = listener.accept().unwrap();
    let mut bytes = vec![0; "snapshot-captured".len()];
    stream.read_exact(&mut bytes).unwrap();
    fs::write(repo.path.join("tracked.txt"), "concurrent\n").unwrap();
    stream.write_all(&[1]).unwrap();
    assert!(!child.wait_with_output().unwrap().status.success());
    assert!(checked(&repo, &["list"])["snapshots"]
        .as_array()
        .unwrap()
        .is_empty());
}
#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_restore_has_a_recoverable_safety_snapshot() {
    use std::{io::Read, os::unix::net::UnixListener};
    let repo = Repo::new();
    let baseline = checked(&repo, &["save", "baseline"]);
    let id = baseline["snapshot"]["id"].as_str().unwrap();
    fs::write(repo.path.join("tracked.txt"), "valuable later work\n").unwrap();
    let socket = repo.temp.path().join("restore.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"));
    command
        .args(["snapshot", "restore", id])
        .env("GROOVE_TEST_CHECKPOINT", "snapshot-restored-files")
        .env("GROOVE_TEST_SOCKET", &socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = support::TestChild::spawn(&mut command);
    let (mut stream, _) = listener.accept().unwrap();
    let mut bytes = vec![0; "snapshot-restored-files".len()];
    stream.read_exact(&mut bytes).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    let listed = checked(&repo, &["list"]);
    let safety = listed["snapshots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["description"]
                .as_str()
                .unwrap()
                .starts_with("before restoring")
        })
        .unwrap();
    checked(&repo, &["restore", safety["id"].as_str().unwrap()]);
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"valuable later work\n"
    );
}
#[test]
fn custom_global_ignore_rules_are_respected() {
    let repo = Repo::new();
    let config = repo.temp.path().join("global.gitconfig");
    let excludes = repo.temp.path().join("excludes");
    fs::write(&excludes, "private.txt\n").unwrap();
    assert!(support::command(&repo.path, "git")
        .args(["config", "--file"])
        .arg(&config)
        .arg("core.excludesfile")
        .arg(&excludes)
        .status()
        .unwrap()
        .success());
    fs::write(repo.path.join("private.txt"), "excluded").unwrap();
    fs::write(repo.path.join("public.txt"), "included").unwrap();
    let out = support::command(&repo.path, env!("CARGO_BIN_EXE_groove"))
        .args(["snapshot", "save"])
        .env("GIT_CONFIG_GLOBAL", &config)
        .output()
        .unwrap();
    let value = assert_success(&out);
    let tree = value["result"]["snapshot_result"]["snapshot"]["work_tree"]
        .as_str()
        .unwrap();
    let names = repo.git(&["ls-tree", "-r", "--name-only", tree]);
    assert!(names.contains("public.txt"));
    assert!(!names.contains("private.txt"));
}
