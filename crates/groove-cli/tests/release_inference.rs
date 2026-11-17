//! Release selection uses registered workspaces and retains generation/lock checks.
mod support;
use groove_core::{ErrorCode, LeaseInfo, Pool, PoolName, PoolOptions, ReservationOptions};
use serde_json::Value;
use std::{fs, path::Path, process::Output};
use support::Repo;

fn pool(repo: &Repo, name: &str) -> Pool {
    let name: PoolName = name.parse().unwrap();
    Pool::init_named(
        &repo.path,
        &name,
        PoolOptions {
            capacity: 1,
            worktree_root: Some(repo.temp.path().join(format!("workspaces-{name}"))),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open_named(&repo.path, &name).unwrap()
}
fn reserve(pool: &Pool, task: Option<&str>) -> LeaseInfo {
    pool.reserve(ReservationOptions {
        task: task.map(str::to_owned),
        ..Default::default()
    })
    .unwrap()
}
fn run(path: &Path, args: &[&str]) -> Output {
    support::TestChild::spawn(support::groove_in(path).args(args))
        .wait_with_output()
        .unwrap()
}
fn checked(output: &Output) -> Value {
    assert_eq!(
        output.stdout.iter().filter(|byte| **byte == b'\n').count(),
        1,
        "{output:?}"
    );
    let value: Value = serde_json::from_slice(&output.stdout).unwrap();
    let schema: Value =
        serde_json::from_str(include_str!("../schemas/response-v2.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&value)
        .unwrap();
    assert_eq!(value["api_version"], 2);
    value
}
fn success(output: &Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    let value = checked(output);
    assert_eq!(value["ok"], true);
    value["result"].clone()
}
fn failure(output: &Output, status: i32, code: &str) -> Value {
    assert_eq!(output.status.code(), Some(status), "{output:?}");
    let value = checked(output);
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], code);
    value["error"].clone()
}
fn protected(pool: &Pool, reservation: &LeaseInfo) {
    assert_eq!(
        pool.protected_leases().unwrap()[0].lease_id,
        reservation.lease_id
    );
    assert_eq!(
        pool.reserve(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
}

#[test]
fn saving_from_a_named_workspace_subdirectory_infers_even_an_unlabelled_reservation() {
    if !support::in_subprocess(
        "saving_from_a_named_workspace_subdirectory_infers_even_an_unlabelled_reservation",
    ) {
        return;
    }
    let repo = Repo::new();
    let default = pool(&repo, "default");
    let frontend = pool(&repo, "frontend");
    let other = reserve(&default, Some("other"));
    let current = reserve(&frontend, None);
    let nested = current.worktree_path.join("nested");
    fs::create_dir(&nested).unwrap();
    fs::write(
        nested.join("result.txt"),
        "saved from the correct reservation\n",
    )
    .unwrap();
    let output = support::TestChild::spawn(
        support::groove_in(&nested)
            .env("GROOVE_LEASE_ID", other.lease_id.to_string())
            .env("GROOVE_TASK", "other")
            .env("GROOVE_POOL", "default")
            .env("GROOVE_WORKTREE", &other.worktree_path)
            .args(["release", "--save"]),
    )
    .wait_with_output()
    .unwrap();
    let saved = success(&output);
    assert_eq!(
        saved["release"]["reservation_id"],
        current.lease_id.to_string()
    );
    assert_eq!(saved["snapshot"]["session"], current.lease_id.to_string());
    assert_eq!(saved["snapshot"]["pinned"], true);
    assert!(frontend.protected_leases().unwrap().is_empty());
    protected(&default, &other);
    let recovered = repo.temp.path().join("recovered");
    success(&run(
        &repo.path,
        &[
            "snapshot",
            "restore",
            saved["snapshot"]["id"].as_str().unwrap(),
            "--to",
            recovered.to_str().unwrap(),
        ],
    ));
    assert_eq!(
        fs::read_to_string(recovered.join("nested/result.txt")).unwrap(),
        "saved from the correct reservation\n"
    );
}

#[test]
fn unsaved_changes_keep_protection_and_report_the_inferred_id_for_explicit_retries() {
    if !support::in_subprocess(
        "unsaved_changes_keep_protection_and_report_the_inferred_id_for_explicit_retries",
    ) {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, "default");
    let first = reserve(&pool, Some("first"));
    fs::write(
        first.worktree_path.join("tracked.txt"),
        "keep these edits\n",
    )
    .unwrap();
    let error = failure(
        &run(&first.worktree_path, &["release"]),
        5,
        "UNSAVED_CHANGES",
    );
    assert_eq!(
        error["details"]["reservation_id"],
        first.lease_id.to_string()
    );
    protected(&pool, &first);
    assert_eq!(
        fs::read_to_string(first.worktree_path.join("tracked.txt")).unwrap(),
        "keep these edits\n"
    );
    let saved = success(&run(
        &repo.path,
        &[
            "release",
            "--save",
            "--reservation-id",
            &first.lease_id.to_string(),
        ],
    ));
    let next = reserve(&pool, Some("successor"));
    let retried = success(&run(
        &repo.path,
        &[
            "release",
            "--save",
            "--reservation-id",
            &first.lease_id.to_string(),
        ],
    ));
    assert_eq!(saved["snapshot"]["id"], retried["snapshot"]["id"]);
    protected(&pool, &next);
}

#[test]
fn explicit_selectors_win_and_an_explicit_wrong_pool_never_falls_back() {
    if !support::in_subprocess("explicit_selectors_win_and_an_explicit_wrong_pool_never_falls_back")
    {
        return;
    }
    let repo = Repo::new();
    let default = pool(&repo, "default");
    let frontend = pool(&repo, "frontend");
    let other = reserve(&default, Some("other"));
    let current = reserve(&frontend, Some("current"));
    failure(
        &run(&current.worktree_path, &["--pool", "default", "release"]),
        2,
        "INVALID_ARGUMENT",
    );
    protected(&default, &other);
    protected(&frontend, &current);
    let output = success(&run(
        &current.worktree_path,
        &["--pool", "default", "release", "--task", "other"],
    ));
    assert_eq!(
        output["release"]["reservation_id"],
        other.lease_id.to_string()
    );
    protected(&frontend, &current);
    let output = success(&run(
        &repo.path,
        &["--repo", current.worktree_path.to_str().unwrap(), "release"],
    ));
    assert_eq!(
        output["release"]["reservation_id"],
        current.lease_id.to_string()
    );
}

#[test]
fn released_slots_and_live_in_process_owners_do_not_supply_an_inferred_reservation() {
    if !support::in_subprocess(
        "released_slots_and_live_in_process_owners_do_not_supply_an_inferred_reservation",
    ) {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, "default");
    let first = reserve(&pool, None);
    success(&run(&first.worktree_path, &["release"]));
    failure(
        &run(&first.worktree_path, &["release", "--save"]),
        2,
        "INVALID_ARGUMENT",
    );
    let owner = pool.acquire(Default::default()).unwrap();
    failure(
        &run(&owner.info().worktree_path, &["release", "--force"]),
        2,
        "INVALID_ARGUMENT",
    );
    assert!(owner.is_active());
    assert_eq!(
        pool.reserve(Default::default()).unwrap_err().code,
        ErrorCode::PoolExhausted
    );
}

#[test]
fn main_foreign_and_copied_worktrees_require_an_explicit_selector() {
    if !support::in_subprocess("main_foreign_and_copied_worktrees_require_an_explicit_selector") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, "default");
    let current = reserve(&pool, Some("current"));
    failure(&run(&repo.path, &["release"]), 2, "INVALID_ARGUMENT");
    let foreign = repo.temp.path().join("ordinary-worktree");
    repo.git(&[
        "worktree",
        "add",
        "--detach",
        foreign.to_str().unwrap(),
        "HEAD",
    ]);
    failure(&run(&foreign, &["release"]), 2, "INVALID_ARGUMENT");
    let copied = repo.temp.path().join("copy/wt-0001");
    fs::create_dir_all(&copied).unwrap();
    fs::copy(current.worktree_path.join(".git"), copied.join(".git")).unwrap();
    failure(
        &run(&copied, &["release", "--force"]),
        2,
        "INVALID_ARGUMENT",
    );
    protected(&pool, &current);
    assert_eq!(
        fs::read_to_string(current.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
}

#[test]
fn a_live_descriptor_owner_cannot_be_bypassed_by_inference_or_force() {
    if !support::in_subprocess("a_live_descriptor_owner_cannot_be_bypassed_by_inference_or_force") {
        return;
    }
    let repo = Repo::new();
    let pool = pool(&repo, "default");
    let current = reserve(&pool, Some("current"));
    let owner = pool.hold_reserved(current.lease_id).unwrap();
    let error = failure(
        &run(&current.worktree_path, &["release", "--force"]),
        5,
        "INVALID_RESERVATION",
    );
    assert_eq!(
        error["details"]["reservation_id"],
        current.lease_id.to_string()
    );
    protected(&pool, &current);
    drop(owner);
    success(&run(&current.worktree_path, &["release"]));
}

#[cfg(feature = "test-hooks")]
fn selection_race(save: bool) {
    use std::{io::Write, os::unix::net::UnixListener};
    let repo = Repo::new();
    let pool = pool(&repo, "default");
    let first = reserve(&pool, Some("old"));
    let socket = repo.temp.path().join("infer.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    // Keep the observer outside the slot so ordinary stopped-writer checks
    // permit reuse. --repo still exercises inference for the selected workspace.
    let mut command = support::groove_in(&repo.path);
    command.args(["--repo", first.worktree_path.to_str().unwrap()]);
    command
        .args(if save {
            vec!["release", "--save"]
        } else {
            vec!["release"]
        })
        .env("GROOVE_TEST_CHECKPOINT", "workspace-reservation-selected")
        .env("GROOVE_TEST_SOCKET", &socket);
    let mut child = support::TestChild::spawn(&mut command);
    let mut stream = child
        .checkpoint(
            &listener,
            "workspace-reservation-selected",
            support::PROCESS_TIMEOUT,
        )
        .unwrap();
    // The paused CLI is a read-only observer at this boundary: it owns no slot
    // descriptor and cannot mutate files until selection resumes. There are no
    // external writers; exercise the ordinary core release and acquisition APIs.
    pool.release_lease(first.lease_id).unwrap();
    let next = reserve(&pool, Some("successor"));
    fs::write(next.worktree_path.join("tracked.txt"), "successor data\n").unwrap();
    stream.write_all(&[0]).unwrap();
    drop(stream);
    let output = child.wait_with_output().unwrap();
    let error = failure(&output, 5, "RESERVATION_MISMATCH");
    assert_eq!(
        error["details"]["reservation_id"],
        first.lease_id.to_string()
    );
    protected(&pool, &next);
    assert_eq!(
        fs::read_to_string(next.worktree_path.join("tracked.txt")).unwrap(),
        "successor data\n"
    );
}
#[cfg(feature = "test-hooks")]
#[test]
fn a_generation_selected_before_reuse_cannot_release_the_successor() {
    if !support::in_subprocess("a_generation_selected_before_reuse_cannot_release_the_successor") {
        return;
    }
    selection_race(false);
}
#[cfg(feature = "test-hooks")]
#[test]
fn a_generation_selected_before_reuse_cannot_save_or_release_the_successor() {
    if !support::in_subprocess(
        "a_generation_selected_before_reuse_cannot_save_or_release_the_successor",
    ) {
        return;
    }
    selection_race(true);
}

#[test]
fn workspace_aliases_resolve_the_registered_reservation_and_broken_backlinks_refuse_release() {
    if !support::in_subprocess(
        "workspace_aliases_resolve_the_registered_reservation_and_broken_backlinks_refuse_release",
    ) {
        return;
    }
    use std::os::unix::fs::symlink;
    let repo = Repo::new();
    let pool = pool(&repo, "default");
    let current = reserve(&pool, Some("current"));
    let alias = repo.temp.path().join("alias");
    symlink(&current.worktree_path, &alias).unwrap();
    assert_eq!(
        pool.lease_for_workspace(&alias).unwrap().lease_id,
        current.lease_id
    );
    let admin = support::git(&current.worktree_path, &["rev-parse", "--absolute-git-dir"]);
    let backlink = Path::new(&admin).join("gitdir");
    let original = fs::read(&backlink).unwrap();
    fs::write(
        &backlink,
        repo.path.join(".git").as_os_str().as_encoded_bytes(),
    )
    .unwrap();
    failure(&run(&alias, &["release", "--force"]), 6, "RECOVERY_FAILED");
    protected(&pool, &current);
    assert_eq!(
        fs::read_to_string(current.worktree_path.join("tracked.txt")).unwrap(),
        "baseline\n"
    );
    fs::write(backlink, original).unwrap();
    let result = success(&run(
        &repo.path,
        &["--repo", alias.to_str().unwrap(), "release"],
    ));
    assert_eq!(
        result["release"]["reservation_id"],
        current.lease_id.to_string()
    );
}

#[test]
fn release_retry_guidance_keeps_the_inferred_named_pool() {
    if !support::in_subprocess("release_retry_guidance_keeps_the_inferred_named_pool") {
        return;
    }
    let repo = Repo::new();
    let default = pool(&repo, "default");
    let frontend = pool(&repo, "frontend");
    let other = reserve(&default, Some("other"));
    let current = reserve(&frontend, Some("current"));
    fs::write(
        current.worktree_path.join("tracked.txt"),
        "keep named-pool changes\n",
    )
    .unwrap();
    let error = failure(
        &run(&current.worktree_path, &["release"]),
        5,
        "UNSAVED_CHANGES",
    );
    assert_eq!(
        error["details"]["reservation_id"],
        current.lease_id.to_string()
    );
    let hint = error["hint"].as_str().unwrap();
    assert!(hint.contains(&current.lease_id.to_string()), "{hint}");
    assert!(!hint.contains("--pool"), "{hint}");
    protected(&default, &other);
    protected(&frontend, &current);
    success(&run(
        &repo.path,
        &[
            "--pool",
            "frontend",
            "release",
            "--save",
            "--reservation-id",
            &current.lease_id.to_string(),
        ],
    ));
    protected(&default, &other);
}
