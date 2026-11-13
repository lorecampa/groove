mod support;

use groove_core::{
    ExecOptions, LeaseInfo, Pool, PoolOptions, Preparation, PreparationOptions, PreparationStatus,
};
use std::{
    collections::BTreeMap,
    fs::{self, File, FileTimes},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use support::{Repo, TestChild};

const BINARY: &str = "ignored/target/debug/groove-cache-fixture";
const SOURCE_A: &str = "fn main() { println!(\"A\"); }\n";
const SOURCE_B: &str = "fn main() { println!(\"B\"); }\n";

fn fixture(inputs: &[&str]) -> Repo {
    let repo = Repo::new();
    fs::create_dir(repo.path.join("src")).unwrap();
    fs::write(repo.path.join("src/main.rs"), SOURCE_A).unwrap();
    fs::write(
        repo.path.join("Cargo.toml"),
        "[package]\nname = \"groove-cache-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n[profile.dev]\ndebug = 0\n",
    )
    .unwrap();
    fs::write(
        repo.path.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"groove-cache-fixture\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    fs::write(repo.path.join("toolchain.txt"), "fixture-toolchain-v1\n").unwrap();
    // The executable is the installed contributor Cargo; there are no registry dependencies.
    let command = serde_json::to_string(&[
        env!("CARGO"),
        "build",
        "--offline",
        "--locked",
        "--quiet",
        "--target-dir",
        "ignored/target",
    ])
    .unwrap();
    fs::write(
        repo.path.join(".groove.toml"),
        format!(
            r#"schema_version=1
[caches.build]
paths=["ignored/target"]
inputs={inputs}
key="offline-cargo-fixture-v1"
mode="copy"
[setup.dev]
cache="build"
[[setup.dev.steps]]
name="build"
command={command}
when="always"
[[setup.dev.steps]]
name="execute"
command=["{BINARY}"]
when="always"
"#,
            inputs = serde_json::to_string(inputs).unwrap(),
        ),
    )
    .unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "offline Cargo fixture"]);
    repo
}

fn environment(repo: &Repo) -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "CARGO_HOME".into(),
            repo.temp.path().join("cargo-home").display().to_string(),
        ),
        ("CARGO_TARGET_DIR".into(), "ignored/target".into()),
        ("CARGO_INCREMENTAL".into(), "0".into()),
        ("CARGO_ENCODED_RUSTFLAGS".into(), String::new()),
        ("RUSTC_WRAPPER".into(), String::new()),
        ("RUSTC_WORKSPACE_WRAPPER".into(), String::new()),
        (
            "RUSTC".into(),
            Path::new(env!("CARGO"))
                .with_file_name("rustc")
                .display()
                .to_string(),
        ),
    ])
}

fn pool(repo: &Repo) -> Pool {
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
            ..Default::default()
        },
    )
    .unwrap();
    Pool::open(&repo.path).unwrap()
}

fn prepare(repo: &Repo, pool: &Pool, lease: &LeaseInfo, import: bool) -> Preparation {
    let result = pool
        .prepare(
            lease.lease_id,
            "dev",
            PreparationOptions {
                cache_source: import.then(|| repo.path.clone()),
                execution: ExecOptions {
                    environment: environment(repo),
                    grace: Duration::ZERO,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    for step in &result.steps {
        let run = step.execution.as_ref().expect("always steps must run");
        assert!(
            run.succeeded(),
            "{}: {:?}; stderr={}",
            step.name,
            run,
            fs::read_to_string(&run.run.stderr_path).unwrap()
        );
    }
    assert_eq!(result.status, PreparationStatus::Ready, "{result:?}");
    assert_eq!(result.steps.len(), 2);
    result
}

fn assert_output(preparation: &Preparation, expected: &[u8]) {
    let run = preparation.steps[1].execution.as_ref().unwrap();
    assert_eq!(fs::read(&run.run.stdout_path).unwrap(), expected);
}

fn set_modified(path: &Path, time: SystemTime) {
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(time))
        .unwrap();
    assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), time);
}

fn artifact_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files.extend(artifact_files(&entry.path()));
        } else if entry.file_type().unwrap().is_file() {
            files.push(entry.path());
        }
    }
    files
}

#[test]
fn imported_older_cargo_artifacts_rebuild_changed_source_in_fresh_and_reused_slots() {
    if !support::in_subprocess(
        "imported_older_cargo_artifacts_rebuild_changed_source_in_fresh_and_reused_slots",
    ) {
        return;
    }
    let repo = fixture(&["Cargo.toml", "Cargo.lock", "toolchain.txt"]);
    let mut build = support::command(&repo.path, env!("CARGO"));
    build
        .args([
            "build",
            "--offline",
            "--locked",
            "--quiet",
            "--target-dir",
            "ignored/target",
        ])
        .envs(environment(&repo));
    let output = TestChild::spawn(&mut build).wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let output = TestChild::spawn(&mut support::command(&repo.path, repo.path.join(BINARY)))
        .wait_with_output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"A\n");

    // Place artifacts in the past and B strictly between their mtimes and copy time.
    // A copy that stamps files with the current time can falsely make A look fresh.
    let artifact_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let source_time = artifact_time + Duration::from_secs(10);
    assert!(source_time < SystemTime::now());
    let files = artifact_files(&repo.path.join("ignored/target"));
    assert!(!files.is_empty());
    for file in &files {
        set_modified(file, artifact_time);
    }
    let source_binary = fs::read(repo.path.join(BINARY)).unwrap();
    fs::write(repo.path.join("src/main.rs"), SOURCE_B).unwrap();
    set_modified(&repo.path.join("src/main.rs"), source_time);
    repo.git(&["add", "src/main.rs"]);
    repo.git(&[
        "commit",
        "-qm",
        "change to B without rebuilding source artifacts",
    ]);
    let pool = pool(&repo);
    let first = pool.reserve(Default::default()).unwrap();
    set_modified(&first.worktree_path.join("src/main.rs"), source_time);
    let imported = prepare(&repo, &pool, &first, true);
    assert!(imported.cache_hit);
    assert_output(&imported, b"B\n");
    let first_binary = fs::read(first.worktree_path.join(BINARY)).unwrap();

    let second = pool.reserve(Default::default()).unwrap();
    set_modified(&second.worktree_path.join("src/main.rs"), source_time);
    let seeded = prepare(&repo, &pool, &second, false);
    assert!(seeded.cache_hit);
    assert_eq!(seeded.cache_key, imported.cache_key);
    assert_output(&seeded, b"B\n");
    fs::write(
        second.worktree_path.join(BINARY),
        "private artifact mutation",
    )
    .unwrap();
    assert_eq!(
        fs::read(first.worktree_path.join(BINARY)).unwrap(),
        first_binary
    );

    pool.release_lease(first.lease_id).unwrap();
    let reused = pool.reserve(Default::default()).unwrap();
    assert_eq!(reused.slot_id, first.slot_id);
    assert_ne!(reused.lease_id, first.lease_id);
    set_modified(&reused.worktree_path.join("src/main.rs"), source_time);
    let prepared = prepare(&repo, &pool, &reused, false);
    assert!(prepared.cache_hit);
    assert_output(&prepared, b"B\n");
    assert_eq!(fs::read(repo.path.join(BINARY)).unwrap(), source_binary);
    for file in files {
        assert_eq!(
            fs::metadata(file).unwrap().modified().unwrap(),
            artifact_time
        );
    }
}

#[test]
fn declared_source_and_toolchain_inputs_invalidate_compiled_artifacts() {
    if !support::in_subprocess("declared_source_and_toolchain_inputs_invalidate_compiled_artifacts")
    {
        return;
    }
    let repo = fixture(&["Cargo.toml", "Cargo.lock", "src/main.rs", "toolchain.txt"]);
    let pool = pool(&repo);
    let first = pool.reserve(Default::default()).unwrap();
    let a = prepare(&repo, &pool, &first, false);
    assert!(!a.cache_hit);
    assert_output(&a, b"A\n");

    fs::write(repo.path.join("src/main.rs"), SOURCE_B).unwrap();
    repo.git(&["add", "src/main.rs"]);
    repo.git(&["commit", "-qm", "change declared source input"]);
    let second = pool.reserve(Default::default()).unwrap();
    let b = prepare(&repo, &pool, &second, false);
    assert!(!b.cache_hit);
    assert_ne!(a.cache_key, b.cache_key);
    assert_output(&b, b"B\n");
    assert_eq!(
        fs::read(first.worktree_path.join("src/main.rs")).unwrap(),
        SOURCE_A.as_bytes()
    );

    fs::write(repo.path.join("toolchain.txt"), "fixture-toolchain-v2\n").unwrap();
    repo.git(&["add", "toolchain.txt"]);
    repo.git(&["commit", "-qm", "change declared toolchain constraint"]);
    pool.release_lease(first.lease_id).unwrap();
    let third = pool.reserve(Default::default()).unwrap();
    let toolchain = prepare(&repo, &pool, &third, false);
    assert!(!toolchain.cache_hit);
    assert_ne!(toolchain.cache_key, b.cache_key);
    assert_output(&toolchain, b"B\n");
    assert_eq!(
        fs::read(second.worktree_path.join("toolchain.txt")).unwrap(),
        b"fixture-toolchain-v1\n"
    );
}
