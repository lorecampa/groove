//! Exercise the installer wrapper with a disposable checkout and Cargo peer.
mod support;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    caller: PathBuf,
    peers: PathBuf,
    log: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path())
            .unwrap()
            .join("checkout with spaces è");
        fs::create_dir_all(root.join("scripts")).unwrap();
        fs::create_dir_all(root.join("crates/groove-cli")).unwrap();
        fs::write(
            root.join("crates/groove-cli/local-edits"),
            "keep the checkout unchanged\n",
        )
        .unwrap();
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install-cli.sh"),
            root.join("scripts/install-cli.sh"),
        )
        .unwrap();
        let peers = temp.path().join("peers");
        fs::create_dir(&peers).unwrap();
        let log = temp.path().join("cargo.args");
        let cargo = peers.join("cargo");
        fs::write(&cargo, "#!/bin/bash\nset -eu\nprintf '%s\\n' \"$PWD\" \"$@\" > \"$INSTALL_TEST_LOG\"\nexit \"${INSTALL_TEST_STATUS:-0}\"\n").unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        let caller = temp.path().join("unrelated caller");
        fs::create_dir(&caller).unwrap();
        Self {
            _temp: temp,
            root,
            caller,
            peers,
            log,
        }
    }
    fn command(&self) -> Command {
        let mut command = Command::new("bash");
        let mut paths = vec![self.peers.clone()];
        paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
        command
            .current_dir(&self.caller)
            .arg(self.root.join("scripts/install-cli.sh"))
            .env("PATH", std::env::join_paths(paths).unwrap())
            .env("INSTALL_TEST_LOG", &self.log);
        command
    }
}

#[test]
fn install_uses_its_checkout_from_any_directory_and_preserves_cargo_arguments() {
    if !support::in_subprocess(
        "install_uses_its_checkout_from_any_directory_and_preserves_cargo_arguments",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let destination = fixture.caller.join("install destination with spaces");
    let output = support::TestChild::spawn(
        fixture
            .command()
            .args(["--offline", "--root"])
            .arg(&destination)
            .args(["--features", "first,second"]),
    )
    .wait_with_output()
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    let actual = fs::read_to_string(&fixture.log).unwrap();
    let expected = [
        fixture.root.to_str().unwrap(),
        "install",
        "--path",
        fixture.root.join("crates/groove-cli").to_str().unwrap(),
        "--locked",
        "--force",
        "--offline",
        "--root",
        destination.to_str().unwrap(),
        "--features",
        "first,second",
    ]
    .join("\n")
        + "\n";
    assert_eq!(actual, expected);
    assert_eq!(
        fs::read_to_string(fixture.root.join("crates/groove-cli/local-edits")).unwrap(),
        "keep the checkout unchanged\n"
    );
    assert!(
        !destination.exists(),
        "the Cargo peer never performs a real installation"
    );
}

#[test]
fn install_help_explains_replacement_and_local_source_without_running_cargo() {
    if !support::in_subprocess(
        "install_help_explains_replacement_and_local_source_without_running_cargo",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let output = support::TestChild::spawn(fixture.command().arg("--help"))
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let help = String::from_utf8(output.stdout).unwrap();
    for phrase in [
        "Usage:",
        "local edits",
        "--locked --force",
        "replaces",
        "--root",
        "groove --version",
    ] {
        assert!(help.contains(phrase), "missing {phrase}: {help}");
    }
    assert!(!fixture.log.exists());
}

#[test]
fn install_propagates_cargo_failures_without_altering_the_checkout() {
    if !support::in_subprocess("install_propagates_cargo_failures_without_altering_the_checkout") {
        return;
    }
    let fixture = Fixture::new();
    let output = support::TestChild::spawn(fixture.command().env("INSTALL_TEST_STATUS", "42"))
        .wait_with_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(42));
    assert!(fixture.log.exists());
    assert_eq!(
        fs::read_to_string(fixture.root.join("crates/groove-cli/local-edits")).unwrap(),
        "keep the checkout unchanged\n"
    );
}
