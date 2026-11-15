//! Opt-in sustained real-process coverage; ordinary tests cover individual contracts.
#![cfg(feature = "test-hooks")]
mod support;

use serde_json::Value;
use std::{
    fs,
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
}

fn call(repo: &Path, args: &[&str], success: bool) -> Value {
    let mut command = support::groove_in(repo);
    command.args(args);
    let output = support::TestChild::spawn(&mut command)
        .wait_with_output()
        .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{args:?}: {error}; {output:?}"));
    assert_eq!(value["api_version"], 2);
    assert!(value.is_object());
    if success {
        assert!(output.status.success(), "{args:?}: {output:?}");
        assert_eq!(value["ok"], true);
    } else if !output.status.success() {
        assert_eq!(value["ok"], false);
        match value["error"]["code"].as_str() {
            Some("CONFIG_CONFLICT") => assert_eq!(output.status.code(), Some(4)),
            // Once workers finish, released saved/forced edits can legitimately
            // remain in idle slots; preview must refuse their destruction too.
            Some("UNSAVED_CHANGES") => assert_eq!(output.status.code(), Some(5)),
            _ => panic!("unexpected preview failure: {value}"),
        }
    }
    value
}

fn worker(repo: &Path, root: &Path, index: usize, deadline: u128) {
    use std::os::unix::net::UnixListener;
    let mut cycles = 0;
    while now() < deadline {
        let task = format!("worker-{index}-{cycles}");
        let reservation = call(
            repo,
            &[
                "acquire",
                "--task",
                &task,
                "--idempotency-key",
                &task,
                "--wait",
                "20s",
            ],
            true,
        );
        let reservation = &reservation["result"];
        let workspace = Path::new(reservation["workspace_path"].as_str().unwrap());
        let claim = root
            .join("claims")
            .join(reservation["slot_id"].as_str().unwrap());
        let claimed = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&claim);
        assert!(
            claimed.is_ok(),
            "two workers received {}",
            workspace.display()
        );
        assert_eq!(
            support::git(workspace, &["status", "--porcelain"]),
            "",
            "dirty acquisition"
        );
        assert_eq!(
            fs::read(workspace.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        fs::write(workspace.join("tracked.txt"), format!("{task}\n")).unwrap();
        let mode = (cycles + index) % 4;
        if mode == 0 {
            // Plain release requires the edit to be preserved on a branch.
            support::git(workspace, &["switch", "-c", &task]);
            support::git(workspace, &["add", "tracked.txt"]);
            support::git(workspace, &["commit", "-qm", "preserve task"]);
        }
        if mode == 3 {
            let socket = root.join(format!("kill-{index}.sock"));
            let listener = UnixListener::bind(&socket).unwrap();
            let mut command = support::groove_in(repo);
            command
                .args(["release", "--save", "--task", &task])
                .env("GROOVE_TEST_CHECKPOINT", "finish-snapshot-saved")
                .env("GROOVE_TEST_SOCKET", &socket);
            let mut child = support::TestChild::spawn(&mut command);
            let stream = child
                .checkpoint(&listener, "finish-snapshot-saved", support::PROCESS_TIMEOUT)
                .unwrap();
            child.kill().unwrap();
            child.wait().unwrap();
            drop(stream);
            drop(listener);
            fs::remove_file(socket).unwrap();
            assert_eq!(
                fs::read(workspace.join("tracked.txt")).unwrap(),
                format!("{task}\n").as_bytes()
            );
        }
        // Editing is finished before release. A successor may acquire as soon
        // as release drops protection, even before its response reaches us.
        drop(claimed);
        fs::remove_file(claim).unwrap();
        let mut args = vec!["release", "--task", &task];
        match mode {
            1 | 3 => args.push("--save"),
            2 => args.push("--force"),
            _ => (),
        }
        call(repo, &args, true);
        cycles += 1;
    }
    println!("worker {index}: {cycles} cycles");
}

fn observer(repo: &Path, deadline: u128) {
    let mut cycles = 0;
    while now() < deadline {
        call(repo, &["list", "--slots", "--details"], true);
        call(repo, &["pool", "show", "--details"], true);
        call(repo, &["pool", "list", "--disk-usage"], true);
        call(
            repo,
            &["pool", "set", "--ready-target", &(cycles % 3).to_string()],
            true,
        );
        call(repo, &["pool", "destroy", "--dry-run"], false);
        cycles += 1;
    }
    println!("observer: {cycles} cycles");
}

#[test]
#[ignore = "six workers and one observer for 155 seconds"]
fn concurrent_reservations_and_administration() {
    if let Ok(role) = std::env::var("GROOVE_SOAK_ROLE") {
        let repo = std::path::PathBuf::from(std::env::var_os("GROOVE_SOAK_REPO").unwrap());
        let root = std::path::PathBuf::from(std::env::var_os("GROOVE_SOAK_ROOT").unwrap());
        let deadline = std::env::var("GROOVE_SOAK_DEADLINE")
            .unwrap()
            .parse()
            .unwrap();
        if role == "observer" {
            observer(&repo, deadline);
        } else {
            worker(&repo, &root, role.parse().unwrap(), deadline);
        }
        return;
    }
    let repo = support::Repo::new();
    fs::create_dir(repo.temp.path().join("claims")).unwrap();
    call(
        &repo.path,
        &[
            "pool",
            "init",
            "--capacity",
            "8",
            "--ready-target",
            "0",
            "--idle-timeout",
            "never",
        ],
        true,
    );
    let deadline = now() + 155_000;
    let mut children = Vec::new();
    for role in (0..6)
        .map(|index| index.to_string())
        .chain(["observer".into()])
    {
        let mut command = support::command(&repo.path, std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "concurrent_reservations_and_administration",
                "--ignored",
                "--nocapture",
            ])
            .env("GROOVE_SOAK_ROLE", role)
            .env("GROOVE_SOAK_REPO", &repo.path)
            .env("GROOVE_SOAK_ROOT", repo.temp.path())
            .env("GROOVE_SOAK_DEADLINE", deadline.to_string());
        children.push(support::TestChild::spawn(&mut command));
    }
    let finish = Instant::now() + Duration::from_secs(215);
    loop {
        let finished = children
            .iter_mut()
            .map(|child| child.try_wait().unwrap().is_some())
            .collect::<Vec<_>>();
        if finished.iter().all(|finished| *finished) {
            break;
        }
        assert!(Instant::now() < finish, "soak workers did not stop");
        std::thread::sleep(Duration::from_millis(100));
    }
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    for output in &outputs {
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }
    assert!(
        outputs.iter().all(|output| output.status.success()),
        "{outputs:?}"
    );
    assert!(now() >= deadline);
    assert_eq!(
        call(&repo.path, &["list"], true)["result"]["reservations"],
        serde_json::json!([])
    );
    assert_eq!(
        fs::read_dir(repo.temp.path().join("claims"))
            .unwrap()
            .count(),
        0
    );
}
