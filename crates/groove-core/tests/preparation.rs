mod support;
use groove_core::{
    ErrorCode, ExecOptions, NamedCommand, Pool, PoolOptions, PreparationOptions, PreparationStatus,
    ReservationOptions,
};
use std::{fs, time::Duration};
use support::Repo;

const PROJECT: &str = r#"
schema_version=1
[workspace]
port_base=8000
[workspace.environment]
WORKSPACE_LABEL="{slot}:{task}"
[caches.deps]
paths=["ignored/deps"]
inputs=["deps.lock"]
key="fixture-toolchain-v1"
[setup.dev]
cache="deps"
[[setup.dev.steps]]
name="dependencies"
command=["bash","-c","mkdir -p ignored/deps; cat deps.lock > ignored/deps/artifact; chmod 755 ignored/deps/artifact"]
when="cache_miss"
[[setup.dev.steps]]
name="verify"
command=["bash","-c","test -s ignored/deps/artifact; printf '%s:%s:%s' \"$GROOVE_SLOT_ID\" \"$GROOVE_PORT\" \"$WORKSPACE_LABEL\""]
[recipes.test]
command=["bash","-c","cat ignored/deps/artifact"]
requires_setup="dev"
[agents.fixture]
command=["bash","-c","printf '%s:%s' \"$GROOVE_LEASE_ID\" \"$GROOVE_TASK\""]
requires_setup="dev"
"#;
fn repo(project: &str) -> (Repo, Pool) {
    let repo = Repo::new();
    fs::write(repo.path.join(".groove.toml"), project).unwrap();
    fs::write(repo.path.join("deps.lock"), "dependencies v1\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "project workflows"]);
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&repo.path).unwrap();
    (repo, pool)
}
fn options() -> PreparationOptions {
    PreparationOptions {
        execution: ExecOptions {
            grace: Duration::ZERO,
            ..Default::default()
        },
        ..Default::default()
    }
}

#[test]
fn cached_setup_is_independent_across_slots_and_ready_retries_do_not_run_commands() {
    if !support::in_subprocess(
        "cached_setup_is_independent_across_slots_and_ready_retries_do_not_run_commands",
    ) {
        return;
    }
    use std::os::unix::fs::MetadataExt;
    let (repo, pool) = repo(PROJECT);
    let first = pool
        .reserve(ReservationOptions {
            task: Some("first".into()),
            ..Default::default()
        })
        .unwrap();
    assert!(pool
        .exec_named(
            first.lease_id,
            NamedCommand::Recipe("test"),
            &[],
            ExecOptions::default()
        )
        .is_err());
    let prepared = pool.prepare(first.lease_id, "dev", options()).unwrap();
    assert_eq!(prepared.status, PreparationStatus::Ready);
    assert!(!prepared.cache_hit);
    assert!(!prepared.steps[0].skipped);
    let log = prepared.steps[1]
        .execution
        .as_ref()
        .unwrap()
        .run
        .stdout_path
        .clone();
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        format!(
            "{}:{}:{}:first",
            first.slot_id,
            8000 + first.slot_id.number(),
            first.slot_id
        )
    );
    let artifact = first.worktree_path.join("ignored/deps/artifact");
    let original_modified = fs::metadata(&artifact).unwrap().modified().unwrap();
    let receipt = pool.prepare(first.lease_id, "dev", options()).unwrap();
    assert_eq!(receipt.started_at_unix_ms, prepared.started_at_unix_ms);
    fs::write(&artifact, "private mutation").unwrap();
    let second = pool
        .reserve(ReservationOptions {
            task: Some("second".into()),
            ..Default::default()
        })
        .unwrap();
    let seeded = pool.prepare(second.lease_id, "dev", options()).unwrap();
    assert_eq!(seeded.status, PreparationStatus::Ready);
    assert!(seeded.cache_hit);
    assert!(seeded.steps[0].skipped);
    let second_artifact = second.worktree_path.join("ignored/deps/artifact");
    assert_eq!(fs::read(&second_artifact).unwrap(), b"dependencies v1\n");
    assert_eq!(
        fs::metadata(&second_artifact).unwrap().modified().unwrap(),
        original_modified
    );
    assert_eq!(
        fs::metadata(&second_artifact).unwrap().mode() & 0o777,
        0o755
    );
    fs::write(&second_artifact, "second private mutation").unwrap();
    assert_eq!(fs::read(&artifact).unwrap(), b"private mutation");
    assert_eq!(pool.protected_leases().unwrap().len(), 2);
    let run = pool
        .exec_named(
            second.lease_id,
            NamedCommand::Agent("fixture"),
            &[],
            ExecOptions {
                grace: Duration::ZERO,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        fs::read_to_string(run.run.stdout_path).unwrap(),
        format!("{}:second", second.lease_id)
    );
    let history = pool
        .history(&groove_core::HistoryFilter::default())
        .unwrap();
    assert_eq!(
        history.len(),
        4,
        "ready retries and cache-skipped steps must not invent runs"
    );
    assert_eq!(history.iter().filter(|run| matches!(&run.origin, groove_core::RunOrigin::Setup { profile, .. } if profile == "dev")).count(), 3);
    assert!(history.iter().any(
        |run| matches!(&run.origin, groove_core::RunOrigin::Agent { name } if name == "fixture")
    ));
    let recipe = pool
        .exec_named(
            second.lease_id,
            NamedCommand::Recipe("test"),
            &[],
            ExecOptions::default(),
        )
        .unwrap();
    assert!(
        matches!(pool.history_show(recipe.run.run_id.parse().unwrap()).unwrap().origin, groove_core::RunOrigin::Recipe { name } if name == "test")
    );
    fs::write(second.worktree_path.join("deps.lock"), "dependencies v2\n").unwrap();
    assert_eq!(
        pool.exec_named(
            second.lease_id,
            NamedCommand::Recipe("test"),
            &[],
            ExecOptions::default()
        )
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    assert!(pool.prepare(second.lease_id, "dev", options()).is_err());
    let mut retry = options();
    retry.retry = true;
    let changed = pool.prepare(second.lease_id, "dev", retry).unwrap();
    assert!(!changed.cache_hit);
    assert_ne!(changed.cache_key, seeded.cache_key);
    assert_eq!(fs::read(second_artifact).unwrap(), b"dependencies v2\n");
    pool.release_lease(first.lease_id).unwrap();
    pool.release_lease(second.lease_id).unwrap();
    let third = pool.reserve(Default::default()).unwrap();
    assert!(pool.preparation(third.lease_id).unwrap().is_none());
    assert!(!third.worktree_path.join("ignored").exists());
    assert!(repo.state().join("artifacts").is_dir());
}

#[test]
fn failure_stops_later_steps_and_retry_is_explicit() {
    if !support::in_subprocess("failure_stops_later_steps_and_retry_is_explicit") {
        return;
    }
    let project=PROJECT.replace("mkdir -p ignored/deps; cat deps.lock > ignored/deps/artifact; chmod 755 ignored/deps/artifact","exit 19");
    let (_repo, pool) = repo(&project);
    let lease = pool.reserve(Default::default()).unwrap();
    let failed = pool.prepare(lease.lease_id, "dev", options()).unwrap();
    assert_eq!(failed.status, PreparationStatus::Failed);
    assert_eq!(failed.steps.len(), 1);
    assert_eq!(
        failed.steps[0].execution.as_ref().unwrap().exit_code,
        Some(19)
    );
    assert_eq!(pool.protected_leases().unwrap().len(), 1);
    assert!(pool.prepare(lease.lease_id, "dev", options()).is_err());
    fs::write(lease.worktree_path.join(".groove.toml"), PROJECT).unwrap();
    let mut retry = options();
    retry.retry = true;
    assert_eq!(
        pool.prepare(lease.lease_id, "dev", retry).unwrap().status,
        PreparationStatus::Ready
    );
}

#[test]
fn explicit_external_seed_requires_matching_inputs_and_preserves_source() {
    if !support::in_subprocess(
        "explicit_external_seed_requires_matching_inputs_and_preserves_source",
    ) {
        return;
    }
    let (repo, pool) = repo(PROJECT);
    fs::create_dir_all(repo.path.join("ignored/deps")).unwrap();
    fs::write(
        repo.path.join("ignored/deps/artifact"),
        "external approved artifact",
    )
    .unwrap();
    let lease = pool.reserve(Default::default()).unwrap();
    let mut prepare = options();
    prepare.cache_source = Some(repo.path.clone());
    let result = pool.prepare(lease.lease_id, "dev", prepare).unwrap();
    assert!(result.cache_hit);
    assert!(result.steps[0].skipped);
    assert_eq!(
        fs::read(lease.worktree_path.join("ignored/deps/artifact")).unwrap(),
        b"external approved artifact"
    );
    assert_eq!(
        fs::read(repo.path.join("ignored/deps/artifact")).unwrap(),
        b"external approved artifact"
    );
}

#[test]
fn external_seed_rejects_tracked_artifacts_and_incompatible_inputs() {
    if !support::in_subprocess("external_seed_rejects_tracked_artifacts_and_incompatible_inputs") {
        return;
    }
    for tracked in [true, false] {
        let (repo, pool) = repo(PROJECT);
        fs::create_dir_all(repo.path.join("ignored/deps")).unwrap();
        fs::write(repo.path.join("ignored/deps/artifact"), "source").unwrap();
        let lease = pool.reserve(Default::default()).unwrap();
        if tracked {
            repo.git(&["add", "-f", "ignored/deps/artifact"]);
        } else {
            fs::write(repo.path.join("deps.lock"), "incompatible\n").unwrap();
        }
        let mut prepare = options();
        prepare.cache_source = Some(repo.path.clone());
        assert!(pool.prepare(lease.lease_id, "dev", prepare).is_err());
        assert!(!lease.worktree_path.join("ignored/deps/artifact").exists());
        assert_eq!(
            fs::read(repo.path.join("ignored/deps/artifact")).unwrap(),
            b"source"
        );
        assert_eq!(pool.protected_leases().unwrap().len(), 1);
    }
}

#[test]
fn corrupted_caches_and_artifact_quotas_fail_without_marking_ready() {
    if !support::in_subprocess("corrupted_caches_and_artifact_quotas_fail_without_marking_ready") {
        return;
    }
    let (fixture, pool) = repo(PROJECT);
    let first = pool.reserve(Default::default()).unwrap();
    let prepared = pool.prepare(first.lease_id, "dev", options()).unwrap();
    let cache = fixture
        .state()
        .join("artifacts")
        .join(prepared.cache_key.unwrap())
        .join("data/ignored/deps/artifact");
    fs::write(cache, "corrupted").unwrap();
    let second = pool.reserve(Default::default()).unwrap();
    assert!(pool.prepare(second.lease_id, "dev", options()).is_err());
    assert_eq!(
        pool.preparation(second.lease_id).unwrap().unwrap().status,
        PreparationStatus::Failed
    );
    for quota in ["max_bytes=1", "max_files=1"] {
        let (_repo, pool) = repo(&PROJECT.replace(
            "key=\"fixture-toolchain-v1\"",
            &format!("key=\"fixture-toolchain-v1\"\n{quota}"),
        ));
        let lease = pool.reserve(Default::default()).unwrap();
        assert!(pool.prepare(lease.lease_id, "dev", options()).is_err());
        assert_eq!(pool.protected_leases().unwrap().len(), 1);
    }
}

#[test]
fn unsafe_artifact_roots_and_tracked_paths_fail_before_execution() {
    if !support::in_subprocess("unsafe_artifact_roots_and_tracked_paths_fail_before_execution") {
        return;
    }
    for paths in ["../escape", "tracked.txt"] {
        let project = PROJECT.replace("ignored/deps", paths);
        let (repo, pool) = if paths == "tracked.txt" {
            repo(&project)
        } else {
            assert!(groove_core::Project::parse(&project).is_err());
            continue;
        };
        let lease = pool.reserve(Default::default()).unwrap();
        assert!(pool.prepare(lease.lease_id, "dev", options()).is_err());
        assert_eq!(
            fs::read(lease.worktree_path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        assert!(repo
            .state()
            .join(format!("slots/{}.protected.json", lease.slot_id))
            .exists());
    }
    let (_repo, pool) = repo(PROJECT);
    let lease = pool.reserve(Default::default()).unwrap();
    fs::create_dir(lease.worktree_path.join("ignored")).unwrap();
    std::os::unix::fs::symlink("../../outside", lease.worktree_path.join("ignored/deps")).unwrap();
    assert!(pool.prepare(lease.lease_id, "dev", options()).is_err());
}

#[test]
#[ignore = "requires GROOVE_TEST_REFLINK_ROOT on disposable reflink-capable storage"]
fn reflink_cache_artifacts_are_independent_copies() {
    let source = Repo::new();
    fs::write(
        source.path.join(".groove.toml"),
        PROJECT.replace(
            "key=\"fixture-toolchain-v1\"",
            "key=\"fixture-toolchain-v1\"\nmode=\"reflink\"",
        ),
    )
    .unwrap();
    fs::write(source.path.join("deps.lock"), "dependencies v1\n").unwrap();
    source.git(&["add", "."]);
    source.git(&["commit", "-qm", "project"]);
    let root = std::env::var_os("GROOVE_TEST_REFLINK_ROOT").expect("explicit disposable storage");
    let temp = tempfile::tempdir_in(root).unwrap();
    let path = temp.path().join("repo");
    support::git(
        source.temp.path(),
        &[
            "clone",
            "-q",
            source.path.to_str().unwrap(),
            path.to_str().unwrap(),
        ],
    );
    Pool::init(
        &path,
        PoolOptions {
            capacity: 2,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&path).unwrap();
    let first = pool.reserve(Default::default()).unwrap();
    assert_eq!(
        pool.prepare(first.lease_id, "dev", options())
            .unwrap()
            .status,
        PreparationStatus::Ready
    );
    let second = pool.reserve(Default::default()).unwrap();
    assert!(
        pool.prepare(second.lease_id, "dev", options())
            .unwrap()
            .cache_hit
    );
    fs::write(first.worktree_path.join("ignored/deps/artifact"), "private").unwrap();
    assert_eq!(
        fs::read(second.worktree_path.join("ignored/deps/artifact")).unwrap(),
        b"dependencies v1\n"
    );
}

#[test]
fn explicit_setup_retry_rejects_another_generations_control_endpoint() {
    if !support::in_subprocess("explicit_setup_retry_rejects_another_generations_control_endpoint")
    {
        return;
    }
    let (repo, pool) = repo(PROJECT);
    let lease = pool.reserve(Default::default()).unwrap();
    assert_eq!(
        pool.prepare(lease.lease_id, "dev", options())
            .unwrap()
            .status,
        PreparationStatus::Ready
    );
    fs::write(lease.worktree_path.join("result"), b"keep").unwrap();
    let path = repo
        .path
        .join(".git/groove/slots")
        .join(format!("{}.setup.json", lease.slot_id));
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["control_name"] = format!(
        "groove-setup-{}-{}",
        uuid::Uuid::new_v4(),
        uuid::Uuid::new_v4()
    )
    .into();
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let error = pool
        .prepare(
            lease.lease_id,
            "dev",
            PreparationOptions {
                retry: true,
                ..options()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::ProtocolError);
    assert_eq!(
        fs::read(lease.worktree_path.join("result")).unwrap(),
        b"keep"
    );
    assert_eq!(pool.protected_leases().unwrap()[0].lease_id, lease.lease_id);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(path).unwrap()).unwrap(),
        record
    );
}
