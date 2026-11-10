mod support;

use std::{
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Command, Stdio},
    time::Duration,
};

#[test]
fn harness_peer() {
    match std::env::var("GROOVE_HARNESS_PEER").as_deref() {
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
        _ => (),
    }
}

fn peer(mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "harness_peer", "--nocapture"])
        .env("GROOVE_HARNESS_PEER", mode);
    command
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
