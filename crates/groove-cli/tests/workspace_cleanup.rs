mod support;
use serde_json::Value;
use std::{
    ffi::CString,
    fs,
    os::unix::{ffi::OsStrExt, net::UnixListener},
    path::Path,
};
use support::{assert_success, Repo};

fn cli(repo: &Repo, args: &[&str]) -> std::process::Output {
    support::groove_in(&repo.path).args(args).output().unwrap()
}
fn initialize(repo: &Repo, policy: &str) {
    assert_success(&cli(
        repo,
        &[
            "pool",
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--cleanup",
            policy,
            "--idle-timeout",
            "never",
            "--workspace-root",
            repo.temp.path().join("w").to_str().unwrap(),
        ],
    ));
}
fn make_special(path: &Path, kind: &str) {
    if kind == "fifo" {
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: path is NUL-terminated and 0600 is a valid creation mode.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    } else {
        drop(UnixListener::bind(path).unwrap());
    }
}

#[test]
fn special_files_are_unsaved_and_do_not_survive_reuse() {
    for policy in ["warm", "pristine"] {
        for kind in ["fifo", "socket"] {
            let repo = Repo::new();
            initialize(&repo, policy);
            let lease = assert_success(&cli(&repo, &["acquire"]));
            let id = lease["result"]["reservation_id"].as_str().unwrap();
            let path = repo.slot(1).join(format!("leftover-{kind}"));
            make_special(&path, kind);
            let refused = cli(&repo, &["release", "--reservation-id", id]);
            let error: Value = serde_json::from_slice(&refused.stdout).unwrap();
            assert_eq!(error["error"]["code"], "UNSAVED_CHANGES", "{error}");
            assert_eq!(error["error"]["details"]["path"], path.to_str().unwrap());
            assert!(path.exists());
            let saved = cli(&repo, &["release", "--save", "--reservation-id", id]);
            assert!(!saved.status.success());
            assert!(path.exists());
            assert_success(&cli(&repo, &["release", "--force", "--reservation-id", id]));
            assert_success(&cli(&repo, &["acquire"]));
            assert!(!path.exists(), "{policy} retained {kind}");
            assert_eq!(
                fs::read(repo.slot(1).join("tracked.txt")).unwrap(),
                b"baseline\n"
            );
        }
    }
}

#[test]
fn read_only_directories_do_not_quarantine_reuse_or_block_destruction() {
    use std::os::unix::fs::PermissionsExt;
    for policy in ["warm", "pristine"] {
        let repo = Repo::new();
        initialize(&repo, policy);
        let lease = assert_success(&cli(&repo, &["acquire"]));
        let id = lease["result"]["reservation_id"].as_str().unwrap();
        let directory = repo.slot(1).join("build-output/nested");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("result"), b"untracked output").unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(
            directory.parent().unwrap(),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        assert_success(&cli(&repo, &["release", "--force", "--reservation-id", id]));
        let acquired = assert_success(&cli(&repo, &["acquire"]));
        assert!(!directory.exists());
        let id = acquired["result"]["reservation_id"].as_str().unwrap();
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("result"), b"disposable output").unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        assert_success(&cli(&repo, &["release", "--force", "--reservation-id", id]));
        assert_success(&cli(&repo, &["pool", "destroy", "--force"]));
        assert!(!repo.slot(1).exists());
    }
}

#[test]
fn acquisition_does_not_normalize_a_released_checkout_under_a_lingering_shell() {
    use std::{
        os::unix::{fs::MetadataExt, process::CommandExt},
        process::Stdio,
        time::{Duration, Instant},
    };
    for capacity in ["1", "2"] {
        let repo = Repo::new();
        assert_success(&cli(
            &repo,
            &[
                "pool",
                "init",
                "--capacity",
                capacity,
                "--ready-target",
                "0",
                "--idle-timeout",
                "never",
                "--workspace-root",
                repo.temp.path().join("w").to_str().unwrap(),
            ],
        ));
        let lease = assert_success(&cli(&repo, &["acquire"]));
        let id = lease["result"]["reservation_id"].as_str().unwrap();
        let slot = Path::new(lease["result"]["workspace_path"].as_str().unwrap());
        let before = fs::metadata(slot.join("tracked.txt")).unwrap();
        let ready = repo.temp.path().join("released-shell-ready");
        let response = repo.temp.path().join("shell-release.json");
        let mut command = support::command(slot, "bash");
        command.process_group(0).stdin(Stdio::piped())
            .args(["-c", "\"$1\" --json release --reservation-id \"$2\" > \"$3\" || exit; printf ready > \"$4\"; read -r _", "lingering-shell"])
            .arg(support::groove_binary()).arg(id).arg(&response).arg(&ready);
        let mut shell = support::TestChild::spawn(&mut command);
        let deadline = Instant::now() + support::PROCESS_TIMEOUT;
        while !ready.exists() {
            assert!(
                shell.try_wait().unwrap().is_none(),
                "shell did not release its lease"
            );
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        let released: Value = serde_json::from_slice(&fs::read(response).unwrap()).unwrap();
        assert_eq!(released["ok"], true);
        let output = cli(&repo, &["acquire"]);
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        if capacity == "1" {
            assert_eq!(output.status.code(), Some(3), "{result}");
            assert_eq!(result["error"]["code"], "POOL_EXHAUSTED");
            assert_eq!(result["error"]["retryable"], true);
            assert!(result["error"]["details"]["processes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|process| process["pid"] == shell.id()));
        } else {
            assert_success(&output);
            assert_ne!(
                result["result"]["workspace_path"].as_str().unwrap(),
                slot.to_str().unwrap()
            );
        }
        let after = fs::metadata(slot.join("tracked.txt")).unwrap();
        assert_eq!(
            (after.ino(), after.mtime(), after.mtime_nsec()),
            (before.ino(), before.mtime(), before.mtime_nsec())
        );
        shell.kill().unwrap();
        shell.wait().unwrap();
        assert_success(&cli(&repo, &["acquire"]));
    }
}

#[test]
fn ignored_special_files_do_not_block_plain_or_saved_release() {
    for kind in ["fifo", "socket"] {
        for save in [false, true] {
            let repo = Repo::new();
            initialize(&repo, "pristine");
            let lease = assert_success(&cli(&repo, &["acquire"]));
            let id = lease["result"]["reservation_id"].as_str().unwrap();
            let ignored = repo.slot(1).join("ignored");
            fs::create_dir(&ignored).unwrap();
            make_special(&ignored.join("server"), kind);
            let mut args = vec!["release", "--reservation-id", id];
            if save {
                args.push("--save");
            }
            assert_success(&cli(&repo, &args));
            assert_success(&cli(&repo, &["acquire"]));
            assert!(!ignored.exists());
        }
    }
}

#[test]
fn empty_untracked_directories_are_removed_even_when_git_status_is_clean() {
    for policy in ["pristine", "warm"] {
        let repo = Repo::new();
        initialize(&repo, policy);
        let lease = assert_success(&cli(&repo, &["acquire"]));
        let id = lease["result"]["reservation_id"].as_str().unwrap();
        let empty = repo.slot(1).join("untracked-empty");
        fs::create_dir(&empty).unwrap();
        assert!(support::git(&repo.slot(1), &["status", "--porcelain", "--ignored"]).is_empty());
        assert_success(&cli(&repo, &["release", "--reservation-id", id]));
        assert_success(&cli(&repo, &["acquire"]));
        assert!(!empty.exists(), "{policy}");
    }
}
