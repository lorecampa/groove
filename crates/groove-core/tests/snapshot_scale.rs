//! Deterministic scale regressions. No sleeps, random seeds, or latency assertions.
mod support;
use groove_core::snapshot::{SaveOptions, Snapshots};
use serde::Deserialize;
use std::{collections::BTreeSet, fs, path::Path, time::Duration};

#[derive(Deserialize)]
struct Fixture {
    fixture_version: u32,
    expected_smoke_tree_sha1: String,
    seed: usize,
    files_per_directory: usize,
    content_pool: usize,
    content_repetitions: usize,
    changed_files: usize,
    profiles: std::collections::BTreeMap<String, usize>,
}
fn specification() -> Fixture {
    serde_json::from_str(include_str!("fixtures/snapshot-workloads-v1.json")).unwrap()
}
fn name(spec: &Fixture, i: usize) -> String {
    format!("d{:05}/f{i:07}.txt", i / spec.files_per_directory)
}
fn git(repo: &Path, args: &[&str]) -> String {
    support::git(repo, args)
}
fn blobs(repo: &Path) -> BTreeSet<String> {
    git(
        repo,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objecttype) %(objectname)",
        ],
    )
    .lines()
    .filter(|s| s.starts_with("blob "))
    .map(str::to_owned)
    .collect()
}
// Ordinary Git staging in a separate index is the independent full-tree oracle.
fn working_tree(repo: &Path, temp: &Path) -> String {
    let index = temp.join("oracle-index");
    if index.exists() {
        fs::remove_file(&index).unwrap();
    }
    for args in [&["read-tree", "HEAD"][..], &["add", "-A"][..]] {
        let out = support::command(repo, "git")
            .env("GIT_INDEX_FILE", &index)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let out = support::command(repo, "git")
        .env("GIT_INDEX_FILE", &index)
        .arg("write-tree")
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}
fn exercise(profile: &str) {
    let spec = specification();
    assert_eq!(spec.fixture_version, 1);
    let count = spec.profiles[profile];
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("source");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "--object-format=sha1"]);
    git(&repo, &["config", "user.name", "Snapshot Scale"]);
    git(&repo, &["config", "user.email", "scale@example.invalid"]);
    git(&repo, &["config", "gc.auto", "0"]);
    for i in 0..count {
        if i % spec.files_per_directory == 0 {
            fs::create_dir(repo.join(format!("d{:05}", i / spec.files_per_directory))).unwrap();
        }
        fs::write(
            repo.join(name(&spec, i)),
            format!("fixture-v1:{}:{}\n", spec.seed, i % spec.content_pool)
                .repeat(spec.content_repetitions),
        )
        .unwrap();
    }
    fs::write(repo.join(".gitignore"), "ignored/\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "fixture-v1"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    if profile == "smoke" {
        assert_eq!(
            git(&repo, &["rev-parse", "HEAD^{tree}"]),
            spec.expected_smoke_tree_sha1
        );
    }
    let positions: Vec<_> = (0..spec.changed_files)
        .map(|j| (spec.seed + j * (count / spec.changed_files)) % count)
        .collect();
    for (j, i) in positions.iter().enumerate() {
        let path = name(&spec, *i);
        if j < 4 {
            fs::write(repo.join(&path), format!("staged-{j}\n")).unwrap();
            git(&repo, &["add", "--", &path]);
        }
        if j < 8 {
            fs::write(repo.join(&path), format!("working-{j}\n")).unwrap();
        } else {
            fs::remove_file(repo.join(&path)).unwrap();
        }
    }
    fs::write(repo.join("new\nfile"), b"binary\0\xff").unwrap();
    fs::create_dir(repo.join("ignored")).unwrap();
    for i in 0..1000 {
        fs::write(repo.join(format!("ignored/{i}")), "cache").unwrap();
    }
    let expected_index = git(&repo, &["write-tree"]);
    let expected_work = working_tree(&repo, temp.path());
    let index = fs::read(repo.join(".git/index")).unwrap();
    let status = git(
        &repo,
        &["status", "--porcelain=v1", "--untracked-files=all"],
    );
    let s = Snapshots::open(&repo, Duration::from_secs(300)).unwrap();
    let first = s.save(SaveOptions::default()).unwrap();
    assert_eq!(first.index_tree, expected_index);
    assert_eq!(first.work_tree, expected_work);
    assert_eq!(fs::read(repo.join(".git/index")).unwrap(), index);
    assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(
            &repo,
            &["status", "--porcelain=v1", "--untracked-files=all"]
        ),
        status
    );
    let before = blobs(&repo);
    let repeated = s.save(SaveOptions::default()).unwrap();
    assert_eq!(first.work_tree, repeated.work_tree);
    assert_eq!(
        before,
        blobs(&repo),
        "unchanged save must not duplicate blobs"
    );
    let recovery = temp.path().join("recovered");
    s.restore(&first.id, Some(&recovery)).unwrap();
    assert_eq!(git(&recovery, &["write-tree"]), expected_index);
    assert_eq!(working_tree(&recovery, temp.path()), expected_work);
    assert!(!recovery.join("ignored").exists());
    fs::write(repo.join(name(&spec, positions[0])), "later").unwrap();
    fs::write(repo.join("later-untracked"), "later").unwrap();
    s.restore(&first.id, None).unwrap();
    assert_eq!(git(&repo, &["write-tree"]), expected_index);
    assert_eq!(working_tree(&repo, temp.path()), expected_work);
    assert!(!repo.join("later-untracked").exists());
    assert_eq!(fs::read(repo.join("ignored/999")).unwrap(), b"cache");
    for other in s.list().unwrap().into_iter().filter(|o| o.id != first.id) {
        s.delete(&other.id, false).unwrap();
    }
    assert_eq!(s.list().unwrap().len(), 1);
    git(&repo, &["gc", "--prune=now"]); // Only this disposable test uses immediate expiry.
    assert_eq!(s.show(&first.id).unwrap().work_tree, expected_work);
}
#[test]
fn scale_smoke() {
    exercise("smoke");
}
#[test]
#[ignore = "opt-in local workload: 100k files; see benchmarks/snapshots/README.md"]
fn scale_100k() {
    exercise("large");
}
#[test]
#[ignore = "opt-in local workload: 1M files; see benchmarks/snapshots/README.md"]
fn scale_1m() {
    exercise("million");
}
