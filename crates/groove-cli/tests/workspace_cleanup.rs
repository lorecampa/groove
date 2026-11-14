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
            "init",
            "--capacity",
            "1",
            "--ready-target",
            "0",
            "--cleanup",
            policy,
            "--idle-timeout",
            "never",
            "--worktree-root",
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
            let id = lease["result"]["lease_id"].as_str().unwrap();
            let path = repo.slot(1).join(format!("leftover-{kind}"));
            make_special(&path, kind);
            let refused = cli(&repo, &["release", "--lease-id", id]);
            let error: Value = serde_json::from_slice(&refused.stdout).unwrap();
            assert_eq!(error["error"]["code"], "UNSAVED_CHANGES", "{error}");
            assert_eq!(error["error"]["details"]["path"], path.to_str().unwrap());
            assert!(path.exists());
            let saved = cli(&repo, &["release", "--save", "--lease-id", id]);
            assert!(!saved.status.success());
            assert!(path.exists());
            assert_success(&cli(&repo, &["release", "--force", "--lease-id", id]));
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
        let id = lease["result"]["lease_id"].as_str().unwrap();
        let directory = repo.slot(1).join("build-output/nested");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("result"), b"untracked output").unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(
            directory.parent().unwrap(),
            fs::Permissions::from_mode(0o555),
        )
        .unwrap();
        assert_success(&cli(&repo, &["release", "--force", "--lease-id", id]));
        let acquired = assert_success(&cli(&repo, &["acquire"]));
        assert!(!directory.exists());
        let id = acquired["result"]["lease_id"].as_str().unwrap();
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("result"), b"disposable output").unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o555)).unwrap();
        assert_success(&cli(&repo, &["release", "--force", "--lease-id", id]));
        assert_success(&cli(&repo, &["destroy", "--discard-changes"]));
        assert!(!repo.slot(1).exists());
    }
}
