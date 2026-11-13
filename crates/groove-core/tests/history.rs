mod support;
use groove_core::{
    CleanupStatus, ExecOptions, HistoryFilter, LogState, Pool, PoolOptions, ReservationOptions,
    RunId, RunOutcome,
};
use std::{fs, time::Duration};
use support::Repo;

fn fixture() -> (Repo, Pool) {
    let repo = Repo::new();
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    (repo, pool)
}
fn execute(pool: &Pool, lease: groove_core::LeaseId) -> groove_core::ExecResult {
    pool.exec(
        lease,
        ExecOptions {
            command: [
                "bash",
                "-c",
                "printf kept-output; printf diagnostic >&2; exit 23",
            ]
            .map(Into::into)
            .to_vec(),
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn history_survives_release_label_reuse_and_explicit_log_cleanup() {
    let (repo, pool) = fixture();
    let options = || ReservationOptions {
        task: Some("reused".into()),
        ..Default::default()
    };
    let first = pool.reserve(options()).unwrap();
    let execution = execute(&pool, first.lease_id);
    let id = execution.run.run_id.parse().unwrap();
    pool.release_lease(first.lease_id).unwrap();
    let second = pool.reserve(options()).unwrap();
    let other = execute(&pool, second.lease_id);
    fs::write(second.worktree_path.join("tracked.txt"), "valuable edits").unwrap();
    let records = pool
        .history(&HistoryFilter {
            task: Some("reused".into()),
            lease_id: None,
        })
        .unwrap();
    assert_eq!(records.len(), 2);
    let selected = pool
        .history(&HistoryFilter {
            lease_id: Some(first.lease_id),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].exit_code, Some(23));
    assert_eq!(selected[0].outcome, RunOutcome::Completed);
    assert!(selected[0].duration_ms.is_some());
    assert_eq!(selected[0].program, "bash");
    assert!(pool.history_delete(id).is_err());
    let before = fs::read(&execution.run.stdout_path).unwrap();
    let recent = pool.clean_logs(Duration::from_secs(86400), false).unwrap();
    assert!(recent
        .entries
        .iter()
        .all(|e| e.status == CleanupStatus::Skipped));
    let preview = pool.clean_logs(Duration::ZERO, true).unwrap();
    assert!(preview
        .entries
        .iter()
        .all(|e| e.status == CleanupStatus::WouldRemove));
    assert_eq!(preview.entries.iter().map(|e| e.log_bytes).sum::<u64>(), 42);
    assert_eq!(fs::read(&execution.run.stdout_path).unwrap(), before);
    assert_eq!(pool.history_show(id).unwrap().logs, LogState::Present);
    let legacy = repo.state().join("task-logs/legacy");
    fs::create_dir(&legacy).unwrap();
    fs::write(legacy.join("stdout.log"), "unknown").unwrap();
    let usage = pool.storage_usage().unwrap();
    assert_eq!(usage.log_bytes, 42);
    assert_eq!(usage.untracked_runs, 1);
    assert_eq!(usage.untracked_log_bytes, 7);
    let cleaned = pool.clean_logs(Duration::ZERO, false).unwrap();
    assert!(cleaned
        .entries
        .iter()
        .all(|e| e.status == CleanupStatus::Removed));
    assert!(!execution.run.stdout_path.exists());
    assert!(!other.run.stderr_path.exists());
    assert_eq!(pool.history_show(id).unwrap().logs, LogState::Removed);
    assert_eq!(pool.history_show(id).unwrap().exit_code, Some(23));
    assert_eq!(
        fs::read_to_string(second.worktree_path.join("tracked.txt")).unwrap(),
        "valuable edits"
    );
    assert_eq!(
        pool.lease_info(second.lease_id).unwrap().lease_id,
        second.lease_id
    );
    assert_eq!(
        fs::read_to_string(legacy.join("stdout.log")).unwrap(),
        "unknown"
    );
    pool.history_delete(id).unwrap();
    assert_eq!(pool.history(&HistoryFilter::default()).unwrap().len(), 1);
    assert!(pool.history_show(id).is_err());
}

#[test]
fn spawn_failures_remain_discoverable_and_do_not_claim_an_exit_code() {
    let (_repo, pool) = fixture();
    let lease = pool.reserve(Default::default()).unwrap();
    assert!(pool
        .exec(
            lease.lease_id,
            ExecOptions {
                command: vec!["/definitely/missing/groove-program".into()],
                ..Default::default()
            }
        )
        .is_err());
    let runs = pool.history(&HistoryFilter::default()).unwrap();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].outcome, RunOutcome::SpawnFailed);
    assert!(runs[0].failure.is_some());
    assert!(runs[0].exit_code.is_none());
    execute(&pool, lease.lease_id);
    assert_eq!(pool.history(&HistoryFilter::default()).unwrap().len(), 2);
}

#[test]
fn cleanup_rejects_symlinks_and_hardlinks_without_touching_external_files() {
    for hardlink in [false, true] {
        let (repo, pool) = fixture();
        let lease = pool.reserve(Default::default()).unwrap();
        let execution = execute(&pool, lease.lease_id);
        let outside = repo.temp.path().join("outside");
        fs::write(&outside, "preserve me").unwrap();
        fs::remove_file(&execution.run.stdout_path).unwrap();
        if hardlink {
            fs::hard_link(&outside, &execution.run.stdout_path).unwrap();
        } else {
            std::os::unix::fs::symlink(&outside, &execution.run.stdout_path).unwrap();
        }
        let result = pool.clean_logs(Duration::ZERO, false).unwrap();
        assert_eq!(result.entries[0].status, CleanupStatus::Failed);
        assert!(result.entries[0].error_code.is_some());
        assert_eq!(fs::read(&outside).unwrap(), b"preserve me");
        assert_eq!(fs::read(&execution.run.stderr_path).unwrap(), b"diagnostic");
        assert_eq!(
            pool.history_show(execution.run.run_id.parse().unwrap())
                .unwrap()
                .logs,
            LogState::Present
        );
    }
}

#[test]
fn run_ids_cannot_escape_owned_storage() {
    for invalid in [
        "../outside",
        "/tmp/outside",
        "",
        "00000000-0000-1000-8000-000000000001",
    ] {
        assert!(invalid.parse::<RunId>().is_err());
    }
}

#[test]
fn malformed_history_cannot_redirect_cleanup_outside_its_run() {
    let (repo, pool) = fixture();
    let lease = pool.reserve(Default::default()).unwrap();
    let execution = execute(&pool, lease.lease_id);
    let path = execution.run.stdout_path.parent().unwrap().join("run.json");
    let outside = repo.temp.path().join("outside");
    fs::write(&outside, "valuable").unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["stdout_path"] = serde_json::json!(outside);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(
        pool.clean_logs(Duration::ZERO, false).unwrap_err().code,
        groove_core::ErrorCode::ProtocolError
    );
    assert_eq!(fs::read(&outside).unwrap(), b"valuable");
    assert_eq!(
        fs::read(&execution.run.stdout_path).unwrap(),
        b"kept-output"
    );
    assert_eq!(fs::read(&execution.run.stderr_path).unwrap(), b"diagnostic");
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().lease_id,
        lease.lease_id
    );
}
