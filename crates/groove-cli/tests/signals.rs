#![cfg(feature = "test-hooks")]
mod support;
use std::{
    os::unix::{
        net::UnixListener,
        process::{CommandExt, ExitStatusExt},
    },
    time::Instant,
};
use support::{assert_success, Repo};

#[test]
fn sigint_terminates_a_waiting_acquire_without_a_timeout_response_or_new_reservation() {
    let repo = Repo::new();
    assert_success(
        &support::groove_in(&repo.path)
            .args(["init", "--capacity", "1", "--ready-target", "0"])
            .output()
            .unwrap(),
    );
    let lease = assert_success(
        &support::groove_in(&repo.path)
            .arg("acquire")
            .output()
            .unwrap(),
    );
    let socket = repo.temp.path().join("waiting.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut command = support::groove_in(&repo.path);
    command
        .process_group(0)
        .args(["acquire", "--wait-ms", "30000"])
        .env("GROOVE_TEST_CHECKPOINT", "acquire-waiting")
        .env("GROOVE_TEST_SOCKET", socket);
    let started = Instant::now();
    let mut child = support::TestChild::spawn(&mut command);
    let stream = child
        .checkpoint(&listener, "acquire-waiting", support::PROCESS_TIMEOUT)
        .unwrap();
    // SAFETY: this PID belongs to our still-owned, unreaped child. kill only
    // sends SIGINT; it cannot be reused while that child remains unreaped.
    let signalled = unsafe { libc::kill(i32::try_from(child.id()).unwrap(), libc::SIGINT) };
    assert_eq!(signalled, 0);
    let output = child.wait_with_output().unwrap();
    drop(stream);
    assert_eq!(output.status.signal(), Some(libc::SIGINT));
    assert!(started.elapsed().as_secs() < 30);
    assert!(
        output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let listed = assert_success(&support::groove_in(&repo.path).arg("list").output().unwrap());
    let reservations = listed["result"]["leases"].as_array().unwrap();
    assert_eq!(reservations.len(), 1);
    assert_eq!(reservations[0]["lease_id"], lease["result"]["lease_id"]);
}
