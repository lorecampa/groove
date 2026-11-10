//! Input validation, corrupted-metadata handling and less common lease paths.
mod support;
use groove_core::{
    snapshot::SaveOptions, ErrorCode, Pool, PoolOptions, PoolUpdate, SlotStatus, MAX_CAPACITY,
};
use std::{fs, process::Command, time::Duration};
use support::Repo;

fn pool(repo: &Repo, capacity: usize) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity,
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}

fn edit_json(path: &std::path::Path, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    edit(&mut value);
    fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

#[test]
fn init_rejects_invalid_options_before_touching_the_repository() {
    let repo = Repo::new();
    for options in [
        PoolOptions {
            capacity: 0,
            ..Default::default()
        },
        PoolOptions {
            capacity: MAX_CAPACITY + 1,
            ..Default::default()
        },
        PoolOptions {
            git_timeout: Duration::ZERO,
            ..Default::default()
        },
    ] {
        let error = Pool::init(&repo.path, options).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
    assert!(!repo.state().exists());
    let error = Pool::init(
        &repo.path,
        PoolOptions {
            resume: true,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigConflict, "nothing to resume");
}

#[test]
fn init_rejects_unsafe_or_nonempty_checkout_roots() {
    let repo = Repo::new();
    let inside_git = repo.path.join(".git/elsewhere");
    let error = Pool::init(
        &repo.path,
        PoolOptions {
            worktree_root: Some(inside_git),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    let occupied = repo.temp.path().join("occupied");
    fs::create_dir(&occupied).unwrap();
    fs::write(occupied.join("someone-elses-file"), "keep").unwrap();
    let error = Pool::init(
        &repo.path,
        PoolOptions {
            worktree_root: Some(occupied.clone()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigConflict);
    assert_eq!(
        fs::read_to_string(occupied.join("someone-elses-file")).unwrap(),
        "keep"
    );
}

#[test]
fn open_reports_missing_pools_bad_timeouts_and_corrupt_manifests() {
    let repo = Repo::new();
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::NotInitialized
    );
    pool(&repo, 2);
    assert_eq!(
        Pool::open_with_timeout(&repo.path, Duration::ZERO)
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    let manifest = repo.state().join("config.json");
    let original = fs::read(&manifest).unwrap();
    edit_json(&manifest, |m| m["capacity"] = 3.into());
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    fs::write(&manifest, &original).unwrap();
    edit_json(&manifest, |m| {
        m["execution_locks"]
            .as_object_mut()
            .unwrap()
            .remove("wt-0002");
    });
    assert_eq!(
        Pool::open(&repo.path).unwrap_err().code,
        ErrorCode::ConfigConflict
    );
    fs::write(&manifest, &original).unwrap();
    Pool::open(&repo.path).unwrap();
}

#[test]
fn settings_updates_reject_shrinking_and_report_unchanged_values() {
    let repo = Repo::new();
    let pool = pool(&repo, 2);
    let unchanged = pool
        .configure(PoolUpdate {
            capacity: Some(2),
            ..Default::default()
        })
        .unwrap();
    assert!(!unchanged.updated);
    let error = pool
        .configure(PoolUpdate {
            capacity: Some(1),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
    assert_eq!(pool.settings().unwrap().capacity, 2);
}

#[test]
fn run_worker_runs_in_the_checkout_and_keeps_the_slot_until_release() {
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let mut lease = pool.acquire(Default::default()).unwrap();
    let mut worker = Command::new("sh");
    worker.args(["-c", "pwd > where.txt; exit 3"]);
    let status = lease.run_worker(worker).unwrap();
    assert_eq!(status.code(), Some(3));
    let checkout = fs::canonicalize(&lease.info().worktree_path).unwrap();
    assert_eq!(
        fs::read_to_string(checkout.join("where.txt"))
            .unwrap()
            .trim(),
        checkout.to_str().unwrap()
    );
    assert_eq!(
        pool.acquire(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
    lease.release().unwrap();
    // A released handle can no longer run workers or protect the slot.
    assert_eq!(
        lease.run_worker(Command::new("true")).unwrap_err().code,
        ErrorCode::InvalidLease
    );
    assert_eq!(
        lease.protect_from_reuse().unwrap_err().code,
        ErrorCode::InvalidLease
    );
}

#[test]
fn mismatched_protection_blocks_release_and_keeps_the_reservation() {
    if !support::in_subprocess("mismatched_protection_blocks_release_and_keeps_the_reservation") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    let protection = repo.state().join("slots/wt-0001.protected.json");
    edit_json(&protection, |p| {
        p["lease"]["lease_id"] = "00000000-0000-4000-8000-000000000000".into();
    });
    assert_eq!(
        pool.release_lease(lease.lease_id).unwrap_err().code,
        ErrorCode::LeaseMismatch
    );
    assert_eq!(
        pool.acquire(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
}

#[test]
fn corrupt_slot_records_are_reported_unhealthy_with_a_reason() {
    let repo = Repo::new();
    let pool = pool(&repo, 2);
    fs::write(repo.state().join("slots/wt-0002.json"), "{ not json").unwrap();
    let inspection = pool.inspect().unwrap();
    let broken = inspection
        .slots
        .iter()
        .find(|slot| slot.slot_id.as_str() == "wt-0002")
        .unwrap();
    assert_eq!(broken.status, SlotStatus::Unhealthy);
    assert!(broken.failure.is_some());
    assert!(broken.reason.contains("attention"));
    let healthy = &inspection.slots[0];
    assert_eq!(healthy.status, SlotStatus::Available);
}

#[test]
fn finish_rejects_corrupt_receipts() {
    // Another fixture's fork can retain this fixture's CLOEXEC lease until exec.
    // This receipt test needs no unrelated owner; test real inheritance separately.
    if !support::in_subprocess("finish_rejects_corrupt_receipts") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, 1);
    let lease = pool.reserve(Default::default()).unwrap();
    pool.finish(lease.lease_id, SaveOptions::default()).unwrap();
    let receipt = repo
        .state()
        .join(format!("slots/finish-{}.json", lease.lease_id));
    edit_json(&receipt, |r| r["schema_version"] = 9.into());
    assert_eq!(
        pool.finish(lease.lease_id, SaveOptions::default())
            .unwrap_err()
            .code,
        ErrorCode::ProtocolError
    );
}
