mod support;
use groove_core::{Pool, PoolOptions};
use std::{
    fs,
    process::{Command, Stdio},
};
use support::Repo;

#[test]
fn cargo_build_and_git_commands_work_inside_git_directory() {
    let r = Repo::new();
    fs::write(r.path.join("Cargo.toml"),"[package]\nname = \"groove-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n").unwrap();
    fs::create_dir(r.path.join("src")).unwrap();
    fs::write(r.path.join("src/main.rs"), "fn main() {}\n").unwrap();
    r.git(&["add", "."]);
    r.git(&["commit", "-qm", "cargo fixture"]);
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let mut lease = pool.acquire(Default::default()).unwrap();
    let mut command = Command::new(env!("CARGO"));
    command
        .args(["check", "--offline", "--quiet", "--target-dir"])
        .arg(r.temp.path().join("cache"));
    assert!(lease.run_worker(command).unwrap().success());
    assert!(
        support::git(&lease.info().worktree_path, &["status", "--porcelain"])
            .contains("Cargo.lock")
    );
    assert!(!support::git(&lease.info().worktree_path, &["log", "--oneline"]).is_empty());
}
#[test]
fn npm_script_works_inside_git_directory_when_node_is_available() {
    if Command::new("npm")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_err()
    {
        return;
    }
    let r = Repo::new();
    fs::write(r.path.join("package.json"),r#"{"name":"groove-fixture","version":"1.0.0","scripts":{"check":"node -e \"require('fs').writeFileSync('result.txt','ok')\""}}"#).unwrap();
    r.git(&["add", "."]);
    r.git(&["commit", "-qm", "npm fixture"]);
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let mut lease = pool.acquire(Default::default()).unwrap();
    let mut command = Command::new("npm");
    command
        .args(["run", "check", "--silent"])
        .env("npm_config_userconfig", "/dev/null")
        .env("npm_config_cache", r.temp.path().join("npm-cache"));
    assert!(lease.run_worker(command).unwrap().success());
    assert_eq!(
        fs::read_to_string(lease.info().worktree_path.join("result.txt")).unwrap(),
        "ok"
    );
}
