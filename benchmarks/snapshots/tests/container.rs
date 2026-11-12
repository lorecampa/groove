//! Opt-in real engine contract and interruption regression. Runs only fixed profiles.
use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct OwnedContainer(Option<String>);
impl Drop for OwnedContainer {
    fn drop(&mut self) {
        if let Some(id) = &self.0 {
            let _ = Command::new("docker")
                .args(["rm", "--force", id])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
struct Launcher(Child);
impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn wait(child: &mut Child, deadline: Instant) -> std::process::ExitStatus {
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "Benchmark launcher exceeded deadline"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
#[test]
#[ignore = "requires native Linux, Docker BuildKit, four CPUs and local disk; verifies interrupted profile cleanup"]
fn canonical_profile_interruption_stops_container_before_scratch_cleanup() {
    let disk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let temp = tempfile::Builder::new()
        .prefix("groove-container-test-")
        .tempdir_in(disk)
        .unwrap();
    let scratch = temp.path().join("scratch");
    let report = temp.path().join("report.json");
    let child = Command::new(env!("CARGO_BIN_EXE_groove-bench"))
        .args(["benchmark", "snapshot-scale", "--temp-root"])
        .arg(&scratch)
        .arg("--output")
        .arg(&report)
        .stdout(fs::File::create(temp.path().join("stdout.log")).unwrap())
        .stderr(fs::File::create(temp.path().join("stderr.log")).unwrap())
        .spawn()
        .unwrap();
    let mut launcher = Launcher(child);
    let deadline = Instant::now() + Duration::from_secs(1800);
    let mut owned = OwnedContainer(None);
    loop {
        if let Ok(entries) = fs::read_dir(&scratch) {
            for entry in entries.flatten() {
                if let Ok(bytes) = fs::read(entry.path().join("runtime.json")) {
                    if let Ok(contract) = serde_json::from_slice::<Value>(&bytes) {
                        owned.0 = contract["inspection"]["Id"].as_str().map(str::to_owned);
                    }
                }
            }
        }
        let current = fs::read(&report)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        if owned.0.is_some()
            && current
                .as_ref()
                .is_some_and(|r| r["environment"]["canonical"] == true && r["progress"].is_object())
        {
            break;
        }
        assert!(Instant::now() < deadline, "No canonical runtime handshake");
        assert!(
            launcher.0.try_wait().unwrap().is_none(),
            "Launcher exited before handshake: {}",
            fs::read_to_string(temp.path().join("stderr.log")).unwrap()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: launcher is our owned unreaped direct child.
    let result = unsafe { libc::kill(launcher.0.id() as i32, libc::SIGTERM) };
    assert_eq!(result, 0);
    assert!(!wait(&mut launcher.0, Instant::now() + Duration::from_secs(40)).success());
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
    assert!(
        !Command::new("docker")
            .args(["inspect", owned.0.as_ref().unwrap()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "Owned container survived interruption"
    );
    let failed: Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    assert_eq!(failed["status"], "failed");
    owned.0 = None;
}
