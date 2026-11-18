mod support;

use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Stdio},
    time::Duration,
};

#[cfg(target_os = "macos")]
#[test]
fn expired_child_deadlines_capture_a_bounded_native_stack_without_passing() {
    let mut child = support::TestChild::spawn(Command::new("/bin/sleep").arg("60"));
    let error = child.wait_timeout(Duration::ZERO).unwrap_err();
    let still_running = child.try_wait().unwrap().is_none();
    child.kill().unwrap();
    child.wait().unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(still_running, "sampling must not terminate the fixture");
    let diagnostic = error.to_string();
    assert!(diagnostic.contains("native stack sample:"), "{diagnostic}");
    assert!(diagnostic.contains("Process:"), "{diagnostic}");
    assert!(diagnostic.len() < 70 * 1024);
}

#[test]
fn harness_peer() {
    match std::env::var("GROOVE_HARNESS_PEER").as_deref() {
        Ok("isolated-git") => {
            use std::os::unix::fs::PermissionsExt;
            let temporary = tempfile::tempdir().unwrap();
            assert_eq!(temporary.path(), temporary.path().canonicalize().unwrap());
            assert_eq!(std::env::var("GIT_CONFIG_GLOBAL").unwrap(), "/dev/null");
            assert_eq!(std::env::var("GIT_CONFIG_SYSTEM").unwrap(), "/dev/null");
            assert_eq!(std::env::var("GIT_CONFIG_NOSYSTEM").unwrap(), "1");
            if let Some(impostor) = std::env::var_os("GROOVE_HARNESS_IMPOSTOR_GIT") {
                assert_ne!(std::env::var_os("GROOVE_TEST_GIT").unwrap(), impostor);
            }
            let output = Command::new("git").arg("--version").output().unwrap();
            assert!(output.status.success(), "{output:?}");
            assert!(output.stdout.starts_with(b"git version "));
            let path = std::env::var_os("GROOVE_HARNESS_MODE_FILE").unwrap();
            std::fs::write(&path, "fixture").unwrap();
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o644
            );
        }
        Ok("partial") => {
            let mut stream =
                UnixStream::connect(std::env::var_os("GROOVE_HARNESS_SOCKET").unwrap()).unwrap();
            stream
                .set_read_timeout(Some(support::PROCESS_TIMEOUT))
                .unwrap();
            stream.write_all(b"partial").unwrap();
            stream.read_exact(&mut [0]).unwrap();
        }
        Ok("output") => {
            std::io::stdout()
                .write_all(&vec![b'x'; 256 * 1024])
                .unwrap();
            std::io::stderr()
                .write_all(&vec![b'y'; 256 * 1024])
                .unwrap();
        }
        Ok("missing-selector") => {
            support::in_subprocess("no_such_isolated_fixture");
        }
        Ok("missing-entry") => {
            support::in_subprocess("subprocess_fixture");
        }
        Ok("concurrent-isolation") => {
            std::thread::scope(|scope| {
                let first = scope.spawn(|| support::in_subprocess("subprocess_fixture"));
                let second = scope.spawn(|| support::in_subprocess("subprocess_fixture"));
                assert!(!first.join().unwrap());
                assert!(!second.join().unwrap());
            });
        }
        _ => (),
    }
}

#[test]
fn cargo_runner_isolates_git_wrappers_configuration_and_umask() {
    use std::os::unix::{fs::PermissionsExt, process::CommandExt};
    let temp = support::tempdir();
    let wrapper = temp.path().join("git");
    std::fs::write(&wrapper, "#!/bin/sh\nexit 91\n").unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/runner.sh");
    let temporary = temp.path().join("temporary");
    std::fs::create_dir(&temporary).unwrap();
    let alias = temp.path().join("temporary-alias");
    std::os::unix::fs::symlink(&temporary, &alias).unwrap();
    let mut command = Command::new("bash");
    command
        .arg(runner)
        .env("TMPDIR", alias)
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "harness_peer", "--nocapture"])
        .env_remove("GROOVE_TEST_GIT")
        .env(
            "PATH",
            format!(
                "{}:{}",
                temp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("GIT_CONFIG_GLOBAL", temp.path().join("unreadable-global"))
        .env("GIT_CONFIG_SYSTEM", temp.path().join("unreadable-system"))
        .env("GIT_CONFIG_NOSYSTEM", "0")
        .env("GROOVE_HARNESS_PEER", "isolated-git")
        .env("GROOVE_HARNESS_MODE_FILE", temp.path().join("mode"));
    // SAFETY: umask is async-signal-safe and changes only the forked child's
    // process state, before Bash and the test runner start.
    unsafe {
        command.pre_exec(|| {
            libc::umask(0o027);
            Ok(())
        });
    }
    let output = support::TestChild::spawn(&mut command)
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn cargo_runner_resolves_native_git_wrappers_to_real_git() {
    // A compiled wrapper passes the ELF/Mach-O check; the runner must still
    // skip a native executable that is not Git and resolve the real binary.
    let temp = support::tempdir();
    let impostor = temp.path().join("git");
    std::os::unix::fs::symlink(std::env::current_exe().unwrap(), &impostor).unwrap();
    let runner = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/runner.sh");
    let mut command = Command::new("bash");
    command
        .arg(runner)
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "harness_peer", "--nocapture"])
        .env_remove("GROOVE_TEST_GIT")
        .env(
            "PATH",
            format!(
                "{}:{}",
                temp.path().display(),
                std::env::var("PATH").unwrap()
            ),
        )
        .env("GROOVE_HARNESS_PEER", "isolated-git")
        .env("GROOVE_HARNESS_IMPOSTOR_GIT", &impostor)
        .env("GROOVE_HARNESS_MODE_FILE", temp.path().join("mode"));
    let output = support::TestChild::spawn(&mut command)
        .wait_with_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn peer(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "harness_peer", "--nocapture"])
        .env("GROOVE_HARNESS_PEER", mode);
    command
}

#[test]
fn subprocess_fixture() {
    let Ok(mode) = std::env::var("GROOVE_HARNESS_ISOLATED_MODE") else {
        return;
    };
    if !support::in_subprocess("subprocess_fixture") {
        println!("fixture parent completed");
        return;
    }
    println!("isolated fixture stdout");
    eprintln!("isolated fixture stderr");
    if let Some(path) = std::env::var_os("GROOVE_HARNESS_ENTRY_WITNESS") {
        std::fs::write(path, std::process::id().to_string()).unwrap();
    }
    match mode.as_str() {
        "success" => (),
        "failure" => panic!("isolated fixture failed after entry"),
        "concurrent" => {
            let mut stream =
                UnixStream::connect(std::env::var_os("GROOVE_HARNESS_SOCKET").unwrap()).unwrap();
            stream
                .set_read_timeout(Some(support::PROCESS_TIMEOUT))
                .unwrap();
            stream
                .set_write_timeout(Some(support::PROCESS_TIMEOUT))
                .unwrap();
            stream.write_all(b"entered").unwrap();
            stream.write_all(&std::process::id().to_ne_bytes()).unwrap();
            let mut reply = [0];
            stream.read_exact(&mut reply).unwrap();
            assert_eq!(&reply, b"g");
        }
        _ => panic!("unknown isolated fixture mode {mode}"),
    }
}

fn isolated_peer(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "subprocess_fixture", "--nocapture"])
        .env("GROOVE_HARNESS_ISOLATED_MODE", mode);
    command
}

#[test]
fn isolation_rejects_a_selector_matching_no_tests() {
    let output = support::TestChild::spawn(&mut peer("missing-selector"))
        .wait_with_output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("no_such_isolated_fixture"));
    assert!(diagnostics.contains("running 0 tests"));
}

#[test]
fn isolation_requires_fixture_entry_even_when_a_selected_test_succeeds() {
    let output = support::TestChild::spawn(&mut peer("missing-entry"))
        .wait_with_output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("subprocess_fixture"));
    assert!(diagnostics.contains("1 passed"));
}

#[test]
fn isolation_accepts_an_entered_fixture_that_exits_successfully() {
    let temp = tempfile::tempdir().unwrap();
    let witness = temp.path().join("fixture-pid");
    let child = support::TestChild::spawn(
        isolated_peer("success").env("GROOVE_HARNESS_ENTRY_WITNESS", &witness),
    );
    let parent_pid = child.id();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stdout).contains("fixture parent completed"));
    let fixture_pid: u32 = std::fs::read_to_string(witness).unwrap().parse().unwrap();
    assert_ne!(fixture_pid, parent_pid);
    assert_ne!(fixture_pid, std::process::id());
}

#[test]
fn isolation_rejects_an_entered_fixture_that_fails_and_preserves_diagnostics() {
    let output = support::TestChild::spawn(&mut isolated_peer("failure"))
        .wait_with_output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    assert!(diagnostics.contains("isolated fixture stdout"));
    assert!(diagnostics.contains("isolated fixture stderr"));
    assert!(diagnostics.contains("isolated fixture failed after entry"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("fixture parent completed"));
}

#[test]
fn concurrent_isolation_of_the_same_fixture_has_independent_entry_markers() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = support::TestChild::spawn(
        peer("concurrent-isolation")
            .env("GROOVE_HARNESS_ISOLATED_MODE", "concurrent")
            .env("GROOVE_HARNESS_SOCKET", socket),
    );
    let mut first = child
        .checkpoint(&listener, "entered", support::PROCESS_TIMEOUT)
        .unwrap();
    // Neither fixture may finish until both have entered. This also exposes
    // accidental serialization of otherwise independent invocations.
    let mut second = child
        .checkpoint(&listener, "entered", support::PROCESS_TIMEOUT)
        .unwrap();
    let mut first_pid = [0; 4];
    let mut second_pid = [0; 4];
    first.read_exact(&mut first_pid).unwrap();
    second.read_exact(&mut second_pid).unwrap();
    assert_ne!(first_pid, second_pid);
    first.write_all(b"g").unwrap();
    second.write_all(b"g").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

#[test]
fn early_exit_reports_status_and_diagnostics_instead_of_waiting_for_a_checkpoint() {
    let temp = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(temp.path().join("peer.sock")).unwrap();
    let mut child = support::TestChild::spawn(
        Command::new("bash").args(["-c", "printf failed-before-checkpoint >&2; exit 23"]),
    );
    let error = child
        .checkpoint(&listener, "ready", support::PROCESS_TIMEOUT)
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(error.to_string().contains("23"));
    assert!(error.to_string().contains("failed-before-checkpoint"));
}

#[test]
fn missing_connection_times_out_and_unwinding_reaps_the_child() {
    let temp = tempfile::tempdir().unwrap();
    let listener = UnixListener::bind(temp.path().join("peer.sock")).unwrap();
    let mut child = support::TestChild::spawn(
        Command::new("bash")
            .args(["-c", "read -r line"])
            .stdin(Stdio::piped()),
    );
    let pid = child.id();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let error = child
            .checkpoint(&listener, "ready", Duration::from_millis(100))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("waiting for readiness"));
        panic!("simulate a failed assertion after readiness timeout");
    }));
    assert_eq!(
        panic.unwrap_err().downcast_ref::<&str>(),
        Some(&"simulate a failed assertion after readiness timeout")
    );
    // SAFETY: WNOHANG never blocks or signals a process. pid identifies our direct
    // child, and the writable status pointer is valid. ECHILD proves it was reaped.
    let waited = unsafe { libc::waitpid(pid as i32, &mut 0, libc::WNOHANG) };
    assert_eq!(waited, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn incomplete_checkpoint_has_a_read_deadline() {
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = support::TestChild::spawn(peer("partial").env("GROOVE_HARNESS_SOCKET", socket));
    // Allow startup its own generous budget; the short deadline applies to the
    // connected peer's incomplete message, not process scheduling.
    let stream = support::accept_child(
        &listener,
        std::slice::from_mut(&mut child),
        support::PROCESS_TIMEOUT,
    )
    .unwrap();
    let error = child
        .read_checkpoint(stream, "partial-checkpoint", Duration::from_millis(500))
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error
        .to_string()
        .contains("reading checkpoint partial-checkpoint"));
}

#[test]
fn output_larger_than_pipe_capacity_is_captured_without_blocking_exit() {
    let output = support::TestChild::spawn(&mut peer("output"))
        .wait_with_output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        output.stdout.iter().filter(|b| **b == b'x').count(),
        256 * 1024
    );
    assert_eq!(
        output.stderr.iter().filter(|b| **b == b'y').count(),
        256 * 1024
    );
}

#[test]
fn accepted_peers_use_blocking_reads_with_explicit_deadlines() {
    use std::os::fd::AsRawFd;
    let temp = tempfile::tempdir().unwrap();
    let socket = temp.path().join("peer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut child = support::TestChild::spawn(peer("partial").env("GROOVE_HARNESS_SOCKET", socket));
    let mut stream = support::accept_child(
        &listener,
        std::slice::from_mut(&mut child),
        support::PROCESS_TIMEOUT,
    )
    .unwrap();
    // SAFETY: stream owns this live descriptor; F_GETFL does not mutate it.
    let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    assert!(flags >= 0);
    assert_eq!(flags & libc::O_NONBLOCK, 0);
    assert_eq!(
        stream.read_timeout().unwrap(),
        Some(support::PROCESS_TIMEOUT)
    );
    assert_eq!(
        stream.write_timeout().unwrap(),
        Some(support::PROCESS_TIMEOUT)
    );
    let mut ready = [0; 7];
    stream.read_exact(&mut ready).unwrap();
    assert_eq!(&ready, b"partial");
    stream.write_all(b"g").unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn waiting_for_a_live_child_has_a_deadline() {
    let mut child = support::TestChild::spawn(
        Command::new("bash")
            .args(["-c", "read -r line"])
            .stdin(Stdio::piped()),
    );
    let error = child.wait_timeout(Duration::from_millis(100)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(error.to_string().contains("waiting for child exit"));
    child.kill().unwrap();
    assert!(!child.wait().unwrap().success());
}
