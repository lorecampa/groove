//! Installer peers live entirely in disposable roots: no network or user upgrade.
//! Isolate fixtures so a concurrent fork cannot inherit a descriptor writing a
//! copied executable (ETXTBSY). The fixtures themselves still run concurrently.
mod support;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
}
fn future_version(offset: u64) -> String {
    format!(
        "{}.0.0",
        env!("CARGO_PKG_VERSION_MAJOR").parse::<u64>().unwrap() + offset
    )
}
fn target() -> String {
    future_version(1)
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path())
            .unwrap()
            .join("installation with spaces");
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::create_dir(root.join("peers")).unwrap();
        fs::copy(support::groove_binary(), root.join("bin/groove")).unwrap();
        let receipt = json!({"installs":{format!("groove-cli {} (registry+https://github.com/rust-lang/crates.io-index)",env!("CARGO_PKG_VERSION")):{
            "bins":["groove"],"features":["custom"],"all_features":false,"no_default_features":true,"profile":"release","target":"x86_64-unknown-linux-gnu"}}});
        fs::write(
            root.join(".crates2.json"),
            serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        fs::write(
            root.join("index"),
            format!(
                "{}\n{}\n{}\n",
                json!({"vers":target(),"yanked":false}),
                json!({"vers":future_version(10),"yanked":true}),
                json!({"vers":format!("{}-rc.1",future_version(11)),"yanked":false})
            ),
        )
        .unwrap();
        script(
            &root.join("peers/curl"),
            r#"printf '%s\n' "$@" >> "$FIXTURE_ROOT/curl-args"
cat "$FIXTURE_ROOT/index"
"#,
        );
        script(
            &root.join("peers/cargo"),
            r#"printf '%s\n' "$@" > "$FIXTURE_ROOT/cargo-args"
if [ "${FAIL_CARGO:-}" = 1 ]; then echo 'build failed' >&2; exit 1; fi
if [ "${WAIT_CARGO:-}" = 1 ]; then
  : > "$FIXTURE_ROOT/ready"
  read -r permit < "$FIXTURE_ROOT/continue"
fi
if [ "${NO_REPLACE:-}" = 1 ]; then : > "$FIXTURE_ROOT/writer-done"; exit 0; fi
printf '#!/bin/sh\nprintf "groove %s (revision fixture)\\n"\n' "$FIXTURE_TARGET" > "$FIXTURE_ROOT/bin/groove.new"
chmod +x "$FIXTURE_ROOT/bin/groove.new"
mv "$FIXTURE_ROOT/bin/groove.new" "$FIXTURE_ROOT/bin/groove"
echo 'installer noise must not escape into JSON stdout'
"#,
        );
        Self { _temp: temp, root }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = self.human_command(&[]);
        command
            .args(["--json", "--request-id", "upgrade-test"])
            .args(args);
        command
    }
    fn human_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(self.root.join("bin/groove"));
        command
            .current_dir(&self.root)
            .env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("GROOVE_LOG_ENABLED", "false")
            .env_remove("GROOVE_LOG_DIR")
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("peers").display()),
            )
            .env("FIXTURE_ROOT", &self.root)
            .env("FIXTURE_TARGET", target())
            .args(args);
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        support::TestChild::spawn(&mut self.command(args))
            .wait_with_output()
            .unwrap()
    }
    fn plugin(&self, program: &str, local: bool) {
        let catalog = self.root.join(format!("{program}-catalog"));
        let plugin = catalog.join(format!(
            "plugins/groove/.{}-plugin",
            if program == "codex" {
                "codex"
            } else {
                "claude"
            }
        ));
        fs::create_dir_all(&plugin).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            serde_json::to_vec(&json!({"version":target()})).unwrap(),
        )
        .unwrap();
        let source = if program == "codex" {
            if local {
                json!({"sourceType":"local","source":catalog})
            } else {
                json!({"sourceType":"git","source":"https://github.com/lorecampa/groove.git"})
            }
        } else if local {
            json!({"source":"directory","path":catalog})
        } else {
            json!({"source":"github","repo":"lorecampa/groove"})
        };
        let catalogs = if program == "codex" {
            json!({"marketplaces":[{"name":"groove","root":catalog,"marketplaceSource":source}]})
        } else {
            let mut entry = source;
            entry["name"] = json!("groove");
            entry["installLocation"] = json!(catalog);
            json!([entry])
        };
        fs::write(
            self.root.join(format!("{program}-catalog.json")),
            serde_json::to_vec(&catalogs).unwrap(),
        )
        .unwrap();
        let target = target();
        for (suffix, version) in [
            ("before", env!("CARGO_PKG_VERSION")),
            ("after", target.as_str()),
        ] {
            let mut entry = json!({"version":version,"enabled":true});
            let installed = if program == "codex" {
                entry["pluginId"] = json!("groove@groove");
                json!({"installed":[entry],"available":[]})
            } else {
                entry["id"] = json!("groove@groove");
                entry["scope"] = json!("user");
                json!([entry])
            };
            fs::write(
                self.root.join(format!("{program}-{suffix}.json")),
                serde_json::to_vec(&installed).unwrap(),
            )
            .unwrap();
        }
        let body = format!(
            r#"program={program}
printf '%s\n' "$*" >> "$FIXTURE_ROOT/$program-calls"
if [ "$2" = list ]; then
  suffix=before
  if [ -f "$FIXTURE_ROOT/$program-updated" ]; then suffix=after; fi
  cat "$FIXTURE_ROOT/$program-$suffix.json"
elif [ "$2 $3" = 'marketplace list' ]; then
  cat "$FIXTURE_ROOT/$program-catalog.json"
elif [ "$2" = add ] || [ "$2" = update ]; then
  if [ "${{FAIL_PLUGIN:-}}" = "$program" ]; then echo 'plugin failure' >&2; exit 1; fi
  : > "$FIXTURE_ROOT/$program-updated"
  echo '{{"updated":true}}'
else
  echo '{{"refreshed":true}}'
fi
"#
        );
        script(&self.root.join("peers").join(program), &body);
    }
}
fn script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/bash\nset -eu\n{body}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn response(output: &Output) -> Value {
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "{output:?}"
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["api_version"], 2);
    assert_eq!(value["request_id"], "upgrade-test");
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    value
}

#[test]
fn check_and_preview_select_stable_versions_without_installing_or_creating_locks() {
    if !support::in_subprocess(
        "check_and_preview_select_stable_versions_without_installing_or_creating_locks",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture.plugin("codex", false);
    for mode in ["--check", "--dry-run"] {
        let output = fixture.run(&["upgrade", mode, "--with-plugins"]);
        assert!(output.status.success(), "{output:?}");
        let result = response(&output)["result"].clone();
        assert_eq!(result["components"][0]["target_version"], target());
        assert_eq!(result["components"][0]["update_available"], true);
        assert_eq!(result["breaking_change"], true);
        assert_eq!(
            result["components"][1]["status"],
            if mode == "--check" {
                "unchecked"
            } else {
                "planned"
            }
        );
        let args = result["components"][0]["commands"][0]["args"]
            .as_array()
            .unwrap();
        assert!(args.contains(&json!("--no-default-features")));
        assert!(args.contains(&json!("custom")));
        assert!(args.contains(&json!(fixture.root)));
        assert!(!fixture.root.join("cargo-args").exists());
        assert!(!fixture.root.join("codex-updated").exists());
        assert!(!fixture.root.join("cache").exists());
        let calls = fs::read_to_string(fixture.root.join("codex-calls")).unwrap();
        assert!(!calls.contains("marketplace upgrade"));
    }
}

#[test]
fn install_preserves_source_options_and_verifies_the_destination_without_touching_pool_state() {
    if !support::in_subprocess(
        "install_preserves_source_options_and_verifies_the_destination_without_touching_pool_state",
    ) {
        return;
    }
    let fixture = Fixture::new();
    // A sentinel that would expose accidental pool discovery/migration.
    fs::create_dir_all(fixture.root.join(".git/groove")).unwrap();
    fs::write(fixture.root.join(".git/groove/protected"), "keep").unwrap();
    let output = fixture.run(&["upgrade"]);
    assert!(output.status.success(), "{output:?}");
    let result = response(&output)["result"].clone();
    assert_eq!(result["components"][0]["status"], "upgraded");
    let args = fs::read_to_string(fixture.root.join("cargo-args")).unwrap();
    assert!(args.contains(&format!(
        "--registry\ncrates-io\n--version\n={}\n",
        target()
    )));
    assert!(args.contains("--root\n"));
    assert!(args.contains("--no-default-features\n--features\ncustom\n"));
    assert_eq!(
        fs::read_to_string(fixture.root.join(".git/groove/protected")).unwrap(),
        "keep"
    );
    assert!(!fixture.root.join("codex-calls").exists());
}

#[test]
fn failed_build_retains_the_executable_and_the_planned_component_for_retry() {
    if !support::in_subprocess(
        "failed_build_retains_the_executable_and_the_planned_component_for_retry",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let original = fs::read(fixture.root.join("bin/groove")).unwrap();
    let output = support::TestChild::spawn(fixture.command(&["upgrade"]).env("FAIL_CARGO", "1"))
        .wait_with_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let value = response(&output);
    assert_eq!(value["ok"], false);
    let component = &value["error"]["details"]["upgrade"]["components"][0];
    assert_eq!(component["status"], "failed");
    assert_eq!(component["target_version"], target());
    assert!(!component["commands"].as_array().unwrap().is_empty());
    assert_eq!(fs::read(fixture.root.join("bin/groove")).unwrap(), original);
}

#[test]
fn unsupported_sources_and_downgrades_never_run_an_installer() {
    if !support::in_subprocess("unsupported_sources_and_downgrades_never_run_an_installer") {
        return;
    }
    let fixture = Fixture::new();
    for target in ["0.3.0", "0.5.0-rc.1", ">=0.5", "0.5.0+build"] {
        let output = fixture.run(&["upgrade", "--target-version", target]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(response(&output)["error"]["code"], "INVALID_ARGUMENT");
    }
    assert!(!fixture.root.join("curl-args").exists());
    fs::write(fixture.root.join(".crates2.json"), r#"{"installs":{}}"#).unwrap();
    let output = fixture.run(&["upgrade"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        response(&output)["error"]["code"],
        "UNSUPPORTED_CONFIGURATION"
    );
    assert!(!fixture.root.join("cargo-args").exists());
}

#[test]
fn git_installs_report_their_source_without_querying_an_unrelated_registry() {
    if !support::in_subprocess(
        "git_installs_report_their_source_without_querying_an_unrelated_registry",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let path = fixture.root.join(".crates2.json");
    let mut receipt: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let installs = receipt["installs"].as_object_mut().unwrap();
    let old = installs.keys().next().unwrap().clone();
    let entry = installs.remove(&old).unwrap();
    installs.insert(format!("groove-cli {} (git+https://github.com/lorecampa/groove?tag=v0.5.0#a010bce74fbe24f42ed67d08ffc9c3ec62032880)", env!("CARGO_PKG_VERSION")), entry);
    fs::write(path, serde_json::to_vec(&receipt).unwrap()).unwrap();
    script(
        &fixture.root.join("peers/curl"),
        "echo 'curl: could not resolve host: index.crates.io' >&2\nexit 6\n",
    );
    let output = fixture.run(&["upgrade", "--check"]);
    assert!(output.status.success(), "{output:?}");
    let value = response(&output);
    let component = &value["result"]["components"][0];
    assert_eq!(component["source"], "cargo:git");
    assert!(component["update_available"].is_null());
    assert!(component["message"].as_str().unwrap().contains("--tag"));
    assert!(!fixture.root.join("cargo-args").exists());
}

#[test]
fn offline_registry_failures_explain_network_and_mirror_recovery_without_installing() {
    if !support::in_subprocess(
        "offline_registry_failures_explain_network_and_mirror_recovery_without_installing",
    ) {
        return;
    }
    let fixture = Fixture::new();
    script(
        &fixture.root.join("peers/curl"),
        "echo 'curl: could not resolve host: index.crates.io' >&2\nexit 6\n",
    );
    let output = fixture.run(&["upgrade", "--check"]);
    assert_eq!(output.status.code(), Some(7));
    let value = response(&output);
    let message = value["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("network/DNS")
            && message.contains("mirrored")
            && message.contains("--tag"),
        "{message}"
    );
    assert_eq!(
        value["error"]["details"]["upgrade"]["components"][0]["source"],
        "cargo:crates-io"
    );
    assert!(!fixture.root.join("cargo-args").exists());
}

#[test]
fn plugin_failures_preserve_the_cli_upgrade_and_continue_other_harnesses() {
    if !support::in_subprocess(
        "plugin_failures_preserve_the_cli_upgrade_and_continue_other_harnesses",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture.plugin("codex", false);
    fixture.plugin("claude", false);
    let output = support::TestChild::spawn(
        fixture
            .command(&["upgrade", "--with-plugins"])
            .env("FAIL_PLUGIN", "codex"),
    )
    .wait_with_output()
    .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let value = response(&output);
    let components = value["error"]["details"]["upgrade"]["components"]
        .as_array()
        .unwrap();
    assert_eq!(components[0]["status"], "upgraded");
    assert_eq!(components[1]["status"], "failed");
    assert_eq!(components[2]["status"], "upgraded");
    assert!(fixture.root.join("claude-updated").exists());
    assert!(!fixture.root.join("codex-updated").exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("fresh agent session"));
}

#[test]
fn local_plugins_and_catalogs_for_an_incompatible_cli_are_not_overwritten() {
    if !support::in_subprocess(
        "local_plugins_and_catalogs_for_an_incompatible_cli_are_not_overwritten",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture.plugin("codex", true);
    fixture.plugin("claude", false);
    fs::write(
        fixture
            .root
            .join("claude-catalog/plugins/groove/.claude-plugin/plugin.json"),
        serde_json::to_vec(&json!({"version":future_version(2)})).unwrap(),
    )
    .unwrap();
    let output = fixture.run(&["upgrade", "--with-plugins"]);
    assert_eq!(output.status.code(), Some(2));
    let value = response(&output);
    let components = value["error"]["details"]["upgrade"]["components"]
        .as_array()
        .unwrap();
    assert_eq!(components[1]["status"], "skipped");
    assert_eq!(components[2]["status"], "failed");
    assert!(!fixture.root.join("codex-updated").exists());
    assert!(!fixture.root.join("claude-updated").exists());
    assert!(!fs::read_to_string(fixture.root.join("codex-calls"))
        .unwrap()
        .contains("marketplace upgrade"));
}

#[test]
fn concurrent_upgrade_cannot_replace_the_permanent_lock_or_run_another_installer() {
    if !support::in_subprocess(
        "concurrent_upgrade_cannot_replace_the_permanent_lock_or_run_another_installer",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let fifo = fixture.root.join("continue");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let first = support::TestChild::spawn(fixture.command(&["upgrade"]).env("WAIT_CARGO", "1"));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.root.join("ready").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // Always unblock the writer before asserting, including failed readiness.
    let ready = fixture.root.join("ready").exists();
    if !ready {
        drop(first);
        panic!("installer did not reach readiness");
    }
    let inode = fs::metadata(fixture.root.join("state/groove/upgrade.lock"))
        .unwrap()
        .ino();
    let second = fixture.run(&["upgrade"]);
    fs::write(&fifo, "continue\n").unwrap();
    let first = first.wait_with_output().unwrap();
    assert!(first.status.success(), "{first:?}");
    assert_eq!(second.status.code(), Some(4));
    assert_eq!(response(&second)["error"]["code"], "CONFIG_CONFLICT");
    assert_eq!(
        fs::metadata(fixture.root.join("state/groove/upgrade.lock"))
            .unwrap()
            .ino(),
        inode
    );
}

#[test]
fn timeout_stops_the_installer_and_releases_exclusion_without_replacing_the_binary() {
    if !support::in_subprocess(
        "timeout_stops_the_installer_and_releases_exclusion_without_replacing_the_binary",
    ) {
        return;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let fifo = fixture.root.join("continue");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let original = fs::read(fixture.root.join("bin/groove")).unwrap();
    let output = support::TestChild::spawn(
        fixture
            .command(&["upgrade", "--timeout", "1s"])
            .env("WAIT_CARGO", "1"),
    )
    .wait_with_output()
    .unwrap();
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(response(&output)["error"]["code"], "IO_ERROR");
    assert!(fixture.root.join("ready").exists());
    assert_eq!(fs::read(fixture.root.join("bin/groove")).unwrap(), original);
    // No process still has the FIFO's reading end after process-group teardown.
    assert_eq!(
        fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&fifo)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENXIO)
    );
    let retry = fixture.run(&["upgrade"]);
    assert!(retry.status.success(), "{retry:?}");
}

#[test]
fn installer_keeps_upgrade_exclusion_when_its_coordinator_is_killed() {
    if !support::in_subprocess("installer_keeps_upgrade_exclusion_when_its_coordinator_is_killed") {
        return;
    }
    use std::{
        io::Write,
        os::fd::{AsFd, AsRawFd},
    };
    let fixture = Fixture::new();
    let fifo = fixture.root.join("continue");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    let mut coordinator = support::TestChild::spawn(
        fixture
            .command(&["upgrade"])
            .env("WAIT_CARGO", "1")
            .env("NO_REPLACE", "1"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fixture.root.join("ready").exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        fixture.root.join("ready").exists(),
        "installer never entered its handshake"
    );
    coordinator.kill().unwrap();
    coordinator.wait().unwrap();
    let competing = fixture.run(&["upgrade"]);
    // Complete the externally surviving writer before assertions/fixture cleanup.
    fs::OpenOptions::new()
        .write(true)
        .open(&fifo)
        .unwrap()
        .write_all(b"continue\n")
        .unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.root.join("state/groove/upgrade.lock"))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let closed = loop {
        // SAFETY: this is a distinct descriptor for our disposable lock file;
        // LOCK_NB observes actual ownership without unlocking another owner.
        if unsafe { libc::flock(lock.as_fd().as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(lock);
    assert!(
        closed,
        "surviving installer did not finish and close its inherited descriptor"
    );
    assert!(fixture.root.join("writer-done").exists());
    assert_eq!(competing.status.code(), Some(4));
    assert_eq!(response(&competing)["error"]["code"], "CONFIG_CONFLICT");
}

/// Use a real terminal while retaining TestChild's bounded exit ownership.
fn terminal_run(command: &mut Command) -> (Output, String) {
    use std::{
        io::Read,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::process::CommandExt,
        },
    };
    let mut master = -1;
    let mut slave = -1;
    assert_eq!(
        // SAFETY: openpty receives valid output pointers and null optional settings.
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    // SAFETY: successful openpty returned two new, exclusively owned descriptors.
    let mut master = unsafe { fs::File::from_raw_fd(master) };
    // SAFETY: slave is the other newly owned openpty descriptor.
    let slave = unsafe { fs::File::from_raw_fd(slave) };
    for fd in [master.as_raw_fd(), slave.as_raw_fd()] {
        // SAFETY: both descriptors are valid. CLOEXEC prevents PTY inheritance
        // beyond the dup2 calls; NONBLOCK keeps the post-exit drain bounded.
        unsafe {
            assert_eq!(libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC), 0);
        }
    }
    let slave_fd = slave.as_raw_fd();
    // SAFETY: slave remains valid through spawn. pre_exec only calls
    // async-signal-safe dup2; TestChild's file capture is redirected to the PTY.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(slave_fd, 1) < 0 || libc::dup2(slave_fd, 2) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = support::TestChild::spawn(command);
    drop(slave);
    let output = child.wait_with_output().unwrap();
    // SAFETY: master is live and exclusively used by this test.
    unsafe {
        assert_eq!(
            libc::fcntl(master.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK),
            0
        );
    }
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        match master.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
        }
    }
    (output, String::from_utf8_lossy(&bytes).into_owned())
}
fn initialize_notice_pool(fixture: &Fixture) -> support::Repo {
    let repo = support::Repo::new();
    let output = fixture.run(&[
        "--repo",
        repo.path.to_str().unwrap(),
        "pool",
        "init",
        "--capacity",
        "1",
        "--ready-target",
        "0",
        "--idle-timeout",
        "never",
    ]);
    assert!(output.status.success(), "{output:?}");
    repo
}
fn seed_notice(fixture: &Fixture) -> PathBuf {
    let directory = fixture.root.join("cache/groove");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let path = directory.join("update.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({"checked_at_ms":now,"latest":target(),"notified_at_ms":0}))
            .unwrap(),
    )
    .unwrap();
    path
}
fn enable_notices(command: &mut Command) -> &mut Command {
    for key in [
        "CI",
        "CONTINUOUS_INTEGRATION",
        "GITHUB_ACTIONS",
        "BUILD_NUMBER",
        "GROOVE_NO_UPDATE_NOTIFIER",
    ] {
        command.env_remove(key);
    }
    command
}

#[test]
fn cached_notices_use_interactive_stderr_and_are_displayed_at_most_daily() {
    if !support::in_subprocess(
        "cached_notices_use_interactive_stderr_and_are_displayed_at_most_daily",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let repo = initialize_notice_pool(&fixture);
    let cache = seed_notice(&fixture);
    for expected in [true, false] {
        let (output, text) = terminal_run(enable_notices(&mut fixture.human_command(&[
            "--repo",
            repo.path.to_str().unwrap(),
            "list",
        ])));
        assert!(output.status.success(), "{output:?} {text}");
        assert_eq!(
            text.contains(&format!("Groove {} is available", target())),
            expected,
            "{text}"
        );
        if expected {
            assert!(!text.contains("breaking changes"));
            assert!(text.contains("groove upgrade"));
        }
    }
    let cache: Value = serde_json::from_slice(&fs::read(cache).unwrap()).unwrap();
    assert!(cache["notified_at_ms"].as_u64().unwrap() > 0);
    assert!(
        !fixture.root.join("curl-args").exists(),
        "fresh cache must suppress network checks"
    );
}

#[test]
fn json_ci_quiet_and_user_preferences_suppress_automatic_discovery_and_notices() {
    if !support::in_subprocess(
        "json_ci_quiet_and_user_preferences_suppress_automatic_discovery_and_notices",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let repo = initialize_notice_pool(&fixture);
    for mode in ["json", "ci", "quiet", "environment", "preferences"] {
        let cache = seed_notice(&fixture);
        let before = fs::read(&cache).unwrap();
        let args = ["--repo", repo.path.to_str().unwrap(), "list"];
        let mut command = if mode == "json" {
            fixture.command(&args)
        } else {
            fixture.human_command(&args)
        };
        enable_notices(&mut command);
        match mode {
            "ci" => {
                command.env("CI", "1");
            }
            "quiet" => {
                command.arg("--quiet");
            }
            "environment" => {
                command.env("GROOVE_NO_UPDATE_NOTIFIER", "1");
            }
            "preferences" => {
                fs::create_dir_all(fixture.root.join("config/groove")).unwrap();
                fs::write(
                    fixture.root.join("config/groove/config.toml"),
                    "[updates]\ncheck = false\n",
                )
                .unwrap();
            }
            _ => {}
        }
        let (output, text) = terminal_run(&mut command);
        assert!(output.status.success(), "{mode}: {text}");
        assert!(
            !text.contains(&format!("Groove {} is available", target())),
            "{mode}: {text}"
        );
        assert_eq!(fs::read(&cache).unwrap(), before);
        assert!(!fixture.root.join("curl-args").exists());
    }
    // Opt-out affects automatic notices, not the user's explicit request.
    let output = fixture.run(&["upgrade", "--check"]);
    assert!(output.status.success());
    assert_eq!(
        response(&output)["result"]["components"][0]["update_available"],
        true
    );
}

#[test]
fn updating_the_user_plugin_does_not_touch_or_verify_another_installation_scope() {
    if !support::in_subprocess(
        "updating_the_user_plugin_does_not_touch_or_verify_another_installation_scope",
    ) {
        return;
    }
    let fixture = Fixture::new();
    fixture.plugin("claude", false);
    for suffix in ["before", "after"] {
        let path = fixture.root.join(format!("claude-{suffix}.json"));
        let mut entries: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        entries.as_array_mut().unwrap().insert(
            0,
            json!({"id":"groove@groove", "scope":"project", "enabled":true,"version":"0.3.0"}),
        );
        fs::write(path, serde_json::to_vec(&entries).unwrap()).unwrap();
    }
    let output = fixture.run(&["upgrade", "--with-plugins"]);
    assert!(output.status.success(), "{output:?}");
    let result = response(&output)["result"].clone();
    assert_eq!(result["components"][1]["status"], "skipped");
    assert_eq!(result["components"][2]["status"], "upgraded");
    assert_eq!(result["components"][2]["target_version"], target());
    let calls = fs::read_to_string(fixture.root.join("claude-calls")).unwrap();
    assert!(calls.contains("plugin update groove@groove --scope user"));
    assert!(!calls.contains("--scope project"));
}

#[test]
fn optional_network_discovery_is_cancelled_and_throttled_without_failing_the_pool_command() {
    if !support::in_subprocess(
        "optional_network_discovery_is_cancelled_and_throttled_without_failing_the_pool_command",
    ) {
        return;
    }
    use std::os::unix::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let repo = initialize_notice_pool(&fixture);
    let fifo = fixture.root.join("curl-continue");
    assert!(Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap()
        .success());
    script(
        &fixture.root.join("peers/curl"),
        r#"read -r permit < "$FIXTURE_ROOT/curl-continue"
exit 1
"#,
    );
    let args = ["--repo", repo.path.to_str().unwrap(), "list"];
    let (output, text) = terminal_run(enable_notices(&mut fixture.human_command(&args)));
    assert!(output.status.success(), "{output:?} {text}");
    assert!(!text.contains("is available"));
    let cache_path = fixture.root.join("cache/groove/update.json");
    let before = fs::read(&cache_path).unwrap();
    let cache: Value = serde_json::from_slice(&before).unwrap();
    assert!(cache["checked_at_ms"].as_u64().unwrap() > 0);
    assert_eq!(
        fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(fifo)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENXIO)
    );
    let (output, _) = terminal_run(enable_notices(&mut fixture.human_command(&args)));
    assert!(output.status.success());
    assert_eq!(fs::read(cache_path).unwrap(), before);
}

#[test]
fn patch_releases_stay_quiet_but_remain_available_for_explicit_checks_and_upgrades() {
    if !support::in_subprocess(
        "patch_releases_stay_quiet_but_remain_available_for_explicit_checks_and_upgrades",
    ) {
        return;
    }
    let fixture = Fixture::new();
    let repo = initialize_notice_pool(&fixture);
    let patch = format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH").parse::<u64>().unwrap() + 1
    );
    let cache = seed_notice(&fixture);
    let mut cached: Value = serde_json::from_slice(&fs::read(&cache).unwrap()).unwrap();
    cached["latest"] = json!(patch);
    fs::write(&cache, serde_json::to_vec(&cached).unwrap()).unwrap();
    let before = fs::read(&cache).unwrap();
    let (output, text) = terminal_run(enable_notices(&mut fixture.human_command(&[
        "--repo",
        repo.path.to_str().unwrap(),
        "list",
    ])));
    assert!(output.status.success(), "{output:?} {text}");
    assert!(!text.contains("is available"), "{text}");
    assert_eq!(
        fs::read(cache).unwrap(),
        before,
        "a suppressed notice must not consume the daily display allowance"
    );
    fs::write(
        fixture.root.join("index"),
        format!("{}\n", json!({"vers":patch,"yanked":false})),
    )
    .unwrap();
    let output = fixture.run(&["upgrade", "--check"]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        response(&output)["result"]["components"][0]["update_available"],
        true
    );
    let output =
        support::TestChild::spawn(fixture.command(&["upgrade"]).env("FIXTURE_TARGET", &patch))
            .wait_with_output()
            .unwrap();
    assert!(output.status.success(), "{output:?}");
    let result = response(&output)["result"].clone();
    assert_eq!(result["components"][0]["target_version"], patch);
    assert_eq!(result["components"][0]["status"], "upgraded");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("breaking changes"));
}
