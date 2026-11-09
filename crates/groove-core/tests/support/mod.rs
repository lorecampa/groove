#![allow(dead_code)]
use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

pub struct Repo {
    pub temp: TempDir,
    pub path: PathBuf,
}
impl Repo {
    pub fn new() -> Self {
        Self::format("sha1")
    }
    pub fn format(format: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("repo with spaces è");
        fs::create_dir(&path).unwrap();
        let repo = Self { temp, path };
        repo.git(&["init", "-q", &format!("--object-format={format}")]);
        repo.git(&["config", "user.name", "Groove Test"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        fs::write(repo.path.join("tracked.txt"), "baseline\n").unwrap();
        fs::write(repo.path.join(".gitignore"), "ignored/\n").unwrap();
        repo.git(&["add", "."]);
        repo.git(&["commit", "-qm", "baseline"]);
        repo
    }
    pub fn git(&self, args: &[&str]) -> String {
        git(&self.path, args)
    }
    pub fn state(&self) -> PathBuf {
        self.path.join(".git/groove")
    }
    pub fn slot(&self, n: usize) -> PathBuf {
        self.state().join(format!("worktrees/wt-{n:04}"))
    }
}
pub fn command(path: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut c = Command::new(program);
    for (key, _) in std::env::vars_os() {
        use std::os::unix::ffi::OsStrExt;
        if key.as_bytes().starts_with(b"GIT_") {
            c.env_remove(key);
        }
    }
    c.current_dir(path)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}
pub fn git(path: &Path, args: &[&str]) -> String {
    let output = command(path, "git")
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .trim_end_matches('\n')
        .into()
}
pub fn assert_success(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Direct test children are killed and reaped even when an assertion unwinds.
/// Protocols use bounded socket reads so a failed peer cannot stall a reader forever.
pub struct TestChild(Option<std::process::Child>);
impl TestChild {
    pub fn spawn(command: &mut Command) -> Self {
        Self(Some(command.spawn().unwrap()))
    }
    pub fn wait_with_output(mut self) -> std::io::Result<Output> {
        self.0.take().unwrap().wait_with_output()
    }
}
impl std::ops::Deref for TestChild {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for TestChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
