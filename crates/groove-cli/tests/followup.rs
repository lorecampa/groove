mod support;

use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    time::{Duration, Instant, SystemTime},
};
use support::Repo;

fn response(path: &Path, args: &[&str]) -> Value {
    let output = support::groove_in(path).args(args).output().unwrap();
    let value = support::assert_success(&output);
    assert_eq!(value["api_version"], 2);
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    value["result"].clone()
}

fn init(repo: &Repo) {
    response(
        &repo.path,
        &[
            "pool",
            "init",
            "--capacity",
            "2",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
    );
}

fn age(path: &Path) {
    fs::File::open(path)
        .unwrap()
        .set_times(
            fs::FileTimes::new()
                .set_modified(SystemTime::now() - Duration::from_secs(8 * 24 * 3600)),
        )
        .unwrap();
}

fn journal(repo: &Repo, key: &str) -> std::path::PathBuf {
    fs::read_dir(repo.state().join("requests"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
                && serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["key"] == key
        })
        .unwrap()
}

#[test]
fn invalid_root_does_not_publish_init_and_fixed_permissions_need_no_resume() {
    use std::os::unix::fs::PermissionsExt;
    let repo = Repo::new();
    let root = repo.temp.path().join("workspaces");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o775)).unwrap();
    let args = [
        "pool",
        "init",
        "--capacity",
        "2",
        "--workspace-root",
        root.to_str().unwrap(),
        "--idle-timeout",
        "never",
    ];
    let output = support::groove_in(&repo.path).args(args).output().unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "UNSUPPORTED_CONFIGURATION");
    assert_eq!(
        response(&repo.path, &["pool", "list", "--all"])["pools"],
        json!([])
    );
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    response(&repo.path, &args);
    assert_eq!(
        response(&repo.path, &["pool", "show", "default"])["pool"]["capacity"],
        2
    );
}

#[test]
fn get_settings_preserves_types_and_handles_unset_and_unknown_keys() {
    let repo = Repo::new();
    init(&repo);
    response(&repo.path, &["config", "set", "logging.level=info"]);
    for (args, expected) in [
        (["config", "get", "logging.level"], json!("info")),
        (["config", "get", "worktrees.root"], Value::Null),
        (["pool", "get", "capacity"], json!(2)),
        (["pool", "get", "cleanup"], json!("pristine")),
        (["pool", "get", "ready-target"], json!(0)),
        (["pool", "get", "idle-timeout"], json!("never")),
    ] {
        assert_eq!(
            response(&repo.path, &args),
            json!({"key": args[2], "value": expected})
        );
    }
    for key in ["ready-base", "workspace-root"] {
        assert!(response(&repo.path, &["pool", "get", key])["value"].is_string());
    }
    let output = support::groove_human_in(&repo.path)
        .args(["config", "get", "worktrees.root"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"\n");
    let output = support::groove_human_in(&repo.path)
        .args(["config", "show"])
        .output()
        .unwrap();
    assert!(!String::from_utf8(output.stdout)
        .unwrap()
        .contains("worktrees:"));
    for command in ["config", "pool"] {
        let output = support::groove_in(&repo.path)
            .args([command, "get", "unknown"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let error: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(error["api_version"], 2);
        assert_eq!(error["error"]["code"], "INVALID_ARGUMENT");
        assert_eq!(error["error"]["message"], "Unknown setting: unknown");
    }
}

#[cfg(feature = "test-hooks")]
#[test]
fn interrupted_init_resumes_identical_settings_without_resume_flag() {
    use std::os::unix::{fs::MetadataExt, net::UnixListener};
    let repo = Repo::new();
    let socket = repo.temp.path().join("init.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let args = ["pool", "init", "--capacity", "2", "--idle-timeout", "never"];
    let mut command = support::groove_in(&repo.path);
    command
        .args(args)
        .env("GROOVE_TEST_CHECKPOINT", "before-ready-manifest")
        .env("GROOVE_TEST_SOCKET", socket);
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(&listener, "before-ready-manifest", support::PROCESS_TIMEOUT)
        .unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stream);
    let lock = repo.state().join("locks/wt-0001.lock");
    let inode = fs::metadata(&lock).unwrap().ino();
    let output = support::groove_in(&repo.path)
        .args(["pool", "init", "--capacity", "3", "--idle-timeout", "never"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let error: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(error["error"]["code"], "CONFIG_CONFLICT");
    assert!(error["error"]["message"]
        .as_str()
        .unwrap()
        .contains("capacity"));
    response(&repo.path, &args);
    assert_eq!(fs::metadata(lock).unwrap().ino(), inode);
    response(&repo.path, &["acquire", "--task", "after-resume"]);
    response(&repo.path, &["release", "--task", "after-resume"]);
}

#[test]
fn workspace_selects_owning_pool_and_explicit_selection_wins() {
    let repo = Repo::new();
    init(&repo);
    response(
        &repo.path,
        &[
            "--pool",
            "one",
            "pool",
            "init",
            "--capacity",
            "1",
            "--idle-timeout",
            "never",
        ],
    );
    let lease = response(&repo.path, &["--pool", "one", "acquire", "--task", "c"]);
    let workspace = Path::new(lease["workspace_path"].as_str().unwrap());
    let nested = workspace.join("nested");
    fs::create_dir(&nested).unwrap();
    assert_eq!(response(&nested, &["pool", "show"])["pool"]["capacity"], 1);
    assert_eq!(
        response(
            &repo.path,
            &["--repo", nested.to_str().unwrap(), "pool", "show"]
        )["pool"]["capacity"],
        1
    );
    assert_eq!(
        response(&nested, &["--pool", "default", "pool", "show"])["pool"]["capacity"],
        2
    );
    assert_eq!(
        response(&repo.path, &["pool", "show", "default"])["pool"]["capacity"],
        2
    );
    response(&nested, &["release", "--task", "c"]);
    assert_eq!(
        response(&repo.path, &["--pool", "one", "list"])["reservations"],
        json!([])
    );
}

#[test]
fn old_released_requests_are_pruned_while_active_replays_remain_unique() {
    let repo = Repo::new();
    init(&repo);
    response(
        &repo.path,
        &["acquire", "--task", "old", "--idempotency-key", "old"],
    );
    response(&repo.path, &["release", "--task", "old"]);
    let old = journal(&repo, "old");
    age(&old);
    let active = response(
        &repo.path,
        &["acquire", "--task", "live", "--idempotency-key", "live"],
    );
    age(&journal(&repo, "live"));
    // Worker pruning and a real concurrent retry serialize on the same key lock.
    let mut command = support::groove_in(&repo.path);
    command.args(["acquire", "--task", "live", "--idempotency-key", "live"]);
    let child = support::TestChild::spawn(&mut command);
    response(&repo.path, &["pool", "set", "--idle-timeout", "1d"]);
    let replay = support::assert_success(&child.wait_with_output().unwrap());
    assert_eq!(replay["result"]["reservation_id"], active["reservation_id"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while old.exists() {
        assert!(
            Instant::now() < deadline,
            "expired request journal was not pruned"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(journal(&repo, "live").exists());
    let active_path = journal(&repo, "live");
    age(&active_path);
    let active_bytes = fs::read(&active_path).unwrap();
    let previous = response(&repo.path, &["pool", "show"])["ready"]["attempted_at_unix_ms"]
        .as_u64()
        .unwrap();
    response(&repo.path, &["pool", "set", "--idle-timeout", "2d"]);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let report = response(&repo.path, &["pool", "show"]);
        if report["ready"]["attempted_at_unix_ms"].as_u64().unwrap() > previous
            && report["ready"]["running"] == false
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "second prune pass did not finish: {report}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read(&active_path).unwrap(),
        active_bytes,
        "an active request must not be pruned or tombstoned"
    );
    assert_eq!(
        response(&repo.path, &["list"])["reservations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // Expired keys may allocate again; the active key still returns its original owner.
    let new = response(
        &repo.path,
        &["acquire", "--task", "old", "--idempotency-key", "old"],
    );
    assert_ne!(new["reservation_id"], active["reservation_id"]);
    response(&repo.path, &["release", "--task", "old"]);
    response(&repo.path, &["release", "--task", "live"]);
}

#[test]
fn destroy_prunes_expired_requests_and_retains_recent_replay_protection() {
    let repo = Repo::new();
    init(&repo);
    for key in ["expired", "recent"] {
        response(
            &repo.path,
            &["acquire", "--task", key, "--idempotency-key", key],
        );
        response(&repo.path, &["release", "--task", key]);
    }
    let expired = journal(&repo, "expired");
    let recent = journal(&repo, "recent");
    age(&expired);
    response(&repo.path, &["pool", "delete", "default", "--yes"]);
    assert!(!expired.exists());
    assert!(recent.exists());
}

#[test]
fn in_place_restore_preserves_untouched_file_identity_and_does_not_read_large_binary() {
    use std::os::unix::fs::MetadataExt;
    let repo = Repo::new();
    let large = repo.path.join("untouched.bin");
    fs::write(&large, vec![0; 8 * 1024 * 1024]).unwrap();
    age(&large); // Avoid Git's intentional racy-index rehash of newly written files.
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "large untouched file"]);
    fs::write(repo.path.join("tracked.txt"), "snapshot content\n").unwrap();
    let snapshot = response(&repo.path, &["snapshot", "save"]);
    repo.git(&["reset", "--hard", "HEAD"]);
    let identities: Vec<_> = ["untouched.bin", ".gitignore"]
        .into_iter()
        .map(|name| {
            let m = fs::metadata(repo.path.join(name)).unwrap();
            (name, m.ino(), m.mtime(), m.mtime_nsec())
        })
        .collect();
    #[cfg(target_os = "linux")]
    let mut accesses = {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let path = std::ffi::CString::new(large.as_os_str().as_bytes()).unwrap();
        // SAFETY: inotify_init1 has no pointer arguments and returns a new descriptor.
        let fd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        assert!(fd >= 0);
        // SAFETY: fd is newly owned and is transferred exactly once to File.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        // SAFETY: file owns the descriptor; path is a valid NUL-terminated string.
        let watch =
            unsafe { libc::inotify_add_watch(file.as_raw_fd(), path.as_ptr(), libc::IN_OPEN) };
        assert!(watch >= 0);
        file
    };
    response(
        &repo.path,
        &[
            "snapshot",
            "restore",
            snapshot["snapshot"]["id"].as_str().unwrap(),
        ],
    );
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"snapshot content\n"
    );
    for (name, ino, seconds, nanos) in identities {
        let m = fs::metadata(repo.path.join(name)).unwrap();
        assert_eq!(
            (m.ino(), m.mtime(), m.mtime_nsec()),
            (ino, seconds, nanos),
            "{name}"
        );
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let mut events = [0; 4096];
        let error = accesses
            .read(&mut events)
            .expect_err("restore read the untouched large file");
        assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
    }
}
