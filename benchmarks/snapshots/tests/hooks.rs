//! Exercise real Git hooks in disposable repositories; mock only the tool gates.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
struct Fixture {
    temp: tempfile::TempDir,
    repo: PathBuf,
    env: BTreeMap<OsString, OsString>,
    log: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        fs::create_dir(&repo).unwrap();
        let tools = temp.path().join("tools");
        fs::create_dir(&tools).unwrap();
        let log = temp.path().join("cargo.log");
        let mut env: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(k, _)| !k.to_string_lossy().starts_with("GIT_"))
            .collect();
        for (k, v) in [
            ("GIT_CONFIG_GLOBAL", "/dev/null"),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_AUTHOR_NAME", "Hooks"),
            ("GIT_AUTHOR_EMAIL", "hooks@example.invalid"),
            ("GIT_COMMITTER_NAME", "Hooks"),
            ("GIT_COMMITTER_EMAIL", "hooks@example.invalid"),
        ] {
            env.insert(k.into(), v.into());
        }
        let mut path = tools.as_os_str().to_owned();
        path.push(":");
        path.push(env.get(&OsString::from("PATH")).unwrap());
        env.insert("PATH".into(), path);
        env.insert("TEST_CARGO_LOG".into(), log.as_os_str().to_owned());
        executable(&tools.join("cargo"),"#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TEST_CARGO_LOG\"\n[ \"${TEST_FAIL_CARGO:-}\" != \"$1\" ]\n");
        let f = Self {
            temp,
            repo,
            env,
            log,
        };
        f.ok(&["git", "init", "-q"]);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for dir in [".githooks", "scripts"] {
            fs::create_dir(f.repo.join(dir)).unwrap();
        }
        for path in [
            ".githooks/pre-commit",
            ".githooks/pre-push",
            "scripts/check.sh",
            "scripts/install-hooks.sh",
        ] {
            fs::copy(root.join(path), f.repo.join(path)).unwrap();
        }
        f.ok(&["./scripts/install-hooks.sh"]);
        fs::write(f.repo.join("file"), "first\n").unwrap();
        f.ok(&["git", "add", "."]);
        f
    }
    fn call_at(&self, cwd: &Path, args: &[&str]) -> Output {
        let started = std::time::Instant::now();
        loop {
            let result = Command::new(args[0])
                .args(&args[1..])
                .current_dir(cwd)
                .env_clear()
                .envs(&self.env)
                .output();
            // Concurrent fork/exec can briefly retain another fixture's writable
            // script descriptor until CLOEXEC takes effect. Retry only ETXTBSY,
            // bounded to one second; actual hook/gate exit failures are never retried.
            if result
                .as_ref()
                .is_err_and(|e| e.raw_os_error() == Some(libc::ETXTBSY))
                && started.elapsed() < std::time::Duration::from_secs(1)
            {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
            return result.unwrap();
        }
    }
    fn call(&self, args: &[&str]) -> Output {
        self.call_at(&self.repo, args)
    }
    fn ok(&self, args: &[&str]) -> Output {
        let out = self.call(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    fn remote(&self) {
        let remote = self.temp.path().join("remote.git");
        self.ok(&["git", "init", "--bare", "-q", remote.to_str().unwrap()]);
        self.ok(&["git", "remote", "add", "origin", remote.to_str().unwrap()]);
    }
}
fn executable(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
#[test]
fn precommit_blocks_failed_formatting_and_accepts_success() {
    let mut f = Fixture::new();
    f.env.insert("TEST_FAIL_CARGO".into(), "fmt".into());
    assert!(!f
        .call(&["git", "commit", "-qm", "blocked"])
        .status
        .success());
    assert!(!f
        .call(&["git", "rev-parse", "--verify", "HEAD"])
        .status
        .success());
    f.env.remove(&OsString::from("TEST_FAIL_CARGO"));
    f.ok(&["git", "commit", "-qm", "accepted"]);
    assert!(fs::read_to_string(&f.log)
        .unwrap()
        .contains("fmt --all -- --check"));
}
#[test]
fn precommit_blocks_staged_whitespace_before_cargo() {
    let f = Fixture::new();
    fs::write(f.repo.join("file"), "trailing  \n").unwrap();
    f.ok(&["git", "add", "file"]);
    assert!(!f
        .call(&["git", "commit", "-qm", "blocked"])
        .status
        .success());
    assert!(!f.log.exists());
}
#[test]
fn prepush_blocks_failed_gate_without_updating_remote() {
    let mut f = Fixture::new();
    f.ok(&["git", "commit", "-qm", "first"]);
    f.remote();
    f.env.insert("TEST_FAIL_CARGO".into(), "clippy".into());
    assert!(!f
        .call(&["git", "push", "origin", "HEAD:refs/heads/main"])
        .status
        .success());
    assert!(f
        .ok(&["git", "ls-remote", "origin", "refs/heads/main"])
        .stdout
        .is_empty());
    f.env.remove(&OsString::from("TEST_FAIL_CARGO"));
    f.ok(&["git", "push", "origin", "HEAD:refs/heads/main"]);
    let log = fs::read_to_string(&f.log).unwrap();
    assert!(log.contains("test --workspace --all-targets --all-features --locked"));
    assert!(!log.contains("test-threads=1"));
    assert!(log.contains("doc --workspace --no-deps --locked"));
}
#[test]
fn installer_is_idempotent_and_preserves_custom_hooks() {
    let f = Fixture::new();
    f.ok(&["./scripts/install-hooks.sh"]);
    f.ok(&["git", "config", "core.hooksPath", "custom-hooks"]);
    assert!(!f.call(&["./scripts/install-hooks.sh"]).status.success());
    assert_eq!(
        f.ok(&["git", "config", "--get", "core.hooksPath"]).stdout,
        b"custom-hooks\n"
    );
}
#[test]
fn linked_worktree_push_does_not_leak_git_context() {
    let f = Fixture::new();
    f.ok(&["git", "commit", "-qm", "first"]);
    f.remote();
    let linked = f.temp.path().join("linked");
    f.ok(&[
        "git",
        "worktree",
        "add",
        "--detach",
        linked.to_str().unwrap(),
        "HEAD",
    ]);
    executable(&f.temp.path().join("tools/cargo"),"#!/bin/sh\nfor name in $(git rev-parse --local-env-vars); do\n printenv \"$name\" >/dev/null && exit 1\ndone\nexit 0\n");
    let out = f.call_at(&linked, &["git", "push", "origin", "HEAD:refs/heads/main"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        f.ok(&["git", "config", "--get", "core.bare"]).stdout,
        b"false\n"
    );
}
