//! Deterministic heterogeneous repositories, with history and commit-switch workloads.
use crate::{
    evidence::Evidence,
    harness::{command, fingerprint, verify_trees},
    profiles::{Case, Cleanup},
    statistics::Observation,
    util::{require, require_reused, Error, Result, Runner},
};
use serde_json::Value;
use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

/// Clone normalized history into independent object storage and a fresh checkout.
/// No hardlinks, alternates or existing index/stat cache cross block boundaries.
pub(crate) fn clone_seed(runner: &Runner, seed: &Path, repo: &Path) -> Result<()> {
    let parent = repo
        .parent()
        .ok_or_else(|| Error::Invalid("Missing clone parent".into()))?;
    runner.git(
        parent,
        [
            std::ffi::OsStr::new("clone"),
            std::ffi::OsStr::new("--quiet"),
            std::ffi::OsStr::new("--no-hardlinks"),
            std::ffi::OsStr::new("--no-checkout"),
            std::ffi::OsStr::new("--"),
            seed.as_os_str(),
            repo.as_os_str(),
        ],
    )?;
    runner.git(repo, ["branch", "bench-base", "origin/bench-base"])?;
    runner.git(repo, ["remote", "remove", "origin"])?;
    runner.git(repo, ["config", "user.name", "Monorepo Benchmark"])?;
    runner.git(repo, ["config", "user.email", "bench@example.invalid"])?;
    runner.git(repo, ["reset", "--hard", "HEAD"])?;
    Ok(())
}

pub(crate) fn selected(case: &Case) -> Vec<PathBuf> {
    crate::fixture::spec()
        .positions(case.files)
        .into_iter()
        .map(|i| PathBuf::from(path(i)))
        .collect()
}

fn file_bytes(index: usize) -> usize {
    match index % 100 {
        0..=79 => 1024,
        80..=94 => 16 * 1024,
        95..=98 => 256 * 1024,
        _ => 1024 * 1024,
    }
}
pub(crate) fn content_bytes(files: usize) -> u64 {
    (0..files).map(|i| file_bytes(i) as u64).sum()
}
pub(crate) fn path(index: usize) -> String {
    let depth = if index.is_multiple_of(7) {
        "generated/api/v1/internal"
    } else {
        "src"
    };
    let extension = if index % 100 == 99 { "bin" } else { "rs" };
    format!(
        "packages/p{:04}/{depth}/m{:02}/f{index:07}.{extension}",
        index / 100,
        index % 10
    )
}
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}
fn contents(index: usize, generation: usize) -> Vec<u8> {
    let size = file_bytes(index);
    let mut state = 17 + index as u64 + ((generation as u64) << 32);
    if index % 100 == 99 {
        return (0..size).map(|_| next(&mut state) as u8).collect();
    }
    let mut bytes = Vec::with_capacity(size + 128);
    while bytes.len() < size {
        bytes.extend_from_slice(
            format!(
                "pub const ITEM_{:06}: u64 = 0x{:016x}; // module {index}, revision {generation}\n",
                bytes.len(),
                next(&mut state)
            )
            .as_bytes(),
        );
    }
    bytes.truncate(size);
    bytes
}
fn write_source(repo: &Path, index: usize, generation: usize) -> Result<()> {
    let file = repo.join(path(index));
    fs::create_dir_all(file.parent().expect("generated path has a parent"))?;
    fs::write(file, contents(index, generation))?;
    Ok(())
}
pub(crate) fn create(
    runner: &Runner,
    repo: &Path,
    case: &Case,
    history: usize,
) -> Result<(String, Vec<PathBuf>)> {
    fs::create_dir(repo)?;
    runner.git(repo, ["init", "-q", "--object-format=sha1"])?;
    runner.git(repo, ["config", "user.name", "Monorepo Benchmark"])?;
    runner.git(repo, ["config", "user.email", "bench@example.invalid"])?;
    fs::write(repo.join(".gitignore"), "/ignored/\n**/dist/\n")?;
    for i in 0..case.files {
        write_source(repo, i, 0)?;
    }
    fs::create_dir(repo.join("history"))?;
    for generation in 1..=history {
        fs::write(
            repo.join(format!("history/obsolete-{generation}.rs")),
            b"obsolete\n",
        )?;
    }
    runner.git(repo, ["add", "-A"])?;
    runner.git(repo, ["commit", "-qm", "monorepo-v1-base"])?;
    runner.git(repo, ["branch", "bench-base"])?;
    for generation in 1..=history {
        // Each commit edits a distributed block of source paths, adds a file and
        // removes one. Large binary assets remain stable across this history.
        for offset in 0..case.files / 100 {
            let i = (generation * (case.files / 16) + offset) % case.files;
            if i % 100 < 99 {
                write_source(repo, i, generation)?;
            }
        }
        fs::write(
            repo.join(format!("history/added-{generation}.rs")),
            format!("revision {generation}\n"),
        )?;
        fs::remove_file(repo.join(format!("history/obsolete-{generation}.rs")))?;
        runner.git(repo, ["add", "-A"])?;
        runner.git(
            repo,
            [
                "commit",
                "-qm",
                &format!("monorepo-v1-history-{generation}"),
            ],
        )?;
    }
    runner.git(repo, ["repack", "-ad", "--depth=50", "--window=10"])?;
    Ok((
        runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?,
        selected(case),
    ))
}
fn call(runner: &Runner, binary: &Path, repo: &Path, args: &[&str]) -> Result<(Value, Value)> {
    command(
        runner,
        binary,
        repo,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
        false,
    )
}
fn workspace(lease: &Value) -> Result<PathBuf> {
    lease["worktree_path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| Error::Invalid("Missing workspace".into()))
}
fn verify(runner: &Runner, lease: &Value, oid: &str, tree: &str) -> Result<()> {
    let root = workspace(lease)?;
    require(
        runner.git_text(&root, &["rev-parse", "HEAD"])? == oid,
        "Wrong monorepo commit",
    )?;
    require(
        runner
            .capture("git", ["symbolic-ref", "-q", "HEAD"], &root)?
            .status
            .code()
            == Some(1),
        "Monorepo checkout is not detached",
    )?;
    verify_trees(runner, &root, tree, tree)
}
pub(crate) fn measure(
    runner: &Runner,
    binary: &Path,
    repo: &Path,
    case: &Case,
    cleanup: Cleanup,
    ignored_files: usize,
    ignored_file_bytes: usize,
) -> Result<Observation> {
    let before = fingerprint(runner, repo, true)?;
    let tip = runner.git_text(repo, &["rev-parse", "HEAD"])?;
    let tree = runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?;
    let old = runner.git_text(repo, &["rev-parse", "bench-base"])?;
    let old_tree = runner.git_text(repo, &["rev-parse", "bench-base^{tree}"])?;
    crate::lifecycle::init_pool(
        runner,
        binary,
        repo,
        None,
        &["init", "--capacity", "1", "--cleanup", cleanup.name()],
    )?;
    let mut evidence = Evidence::default();
    for operation in ["first_acquire", "warm_acquire"] {
        let (lease, timing) = call(runner, binary, repo, &["acquire", "--task", "bench"])?;
        evidence.record(operation, timing)?;
        if operation == "warm_acquire" {
            require_reused(&lease, operation)?;
        }
        verify(runner, &lease, &tip, &tree)?;
        let (_, time) = call(runner, binary, repo, &["release", "--task", "bench"])?;
        if operation == "warm_acquire" {
            evidence.record("release", time)?;
        }
    }
    let (lease, _) = call(runner, binary, repo, &["acquire", "--task", "dirty"])?;
    let root = workspace(&lease)?;
    // Exercise staged and unstaged tracked edits plus untracked/ignored output.
    fs::write(root.join(case.path(0)), b"staged\n")?;
    runner.git(&root, ["add", "--", &case.path(0)])?;
    fs::write(root.join(case.path(0)), b"unstaged\n")?;
    fs::write(root.join("untracked-build-output"), b"remove\n")?;
    fs::create_dir(root.join("ignored"))?;
    let residue = vec![0x5a; ignored_file_bytes];
    for i in 0..ignored_files {
        fs::write(root.join(format!("ignored/cache-{i}")), &residue)?;
    }
    call(
        runner,
        binary,
        repo,
        &["release", "--task", "dirty", "--force"],
    )?;
    for (operation, revision, expected_tree) in [
        ("dirty_acquire", tip.as_str(), tree.as_str()),
        ("backward_acquire", old.as_str(), old_tree.as_str()),
        ("forward_acquire", tip.as_str(), tree.as_str()),
    ] {
        let (lease, time) = call(
            runner,
            binary,
            repo,
            &["acquire", "--task", "bench", "--base", revision],
        )?;
        evidence.record(operation, time)?;
        require_reused(&lease, operation)?;
        verify(runner, &lease, revision, expected_tree)?;
        let root = workspace(&lease)?;
        require(
            !root.join("untracked-build-output").exists(),
            "Untracked residue survived",
        )?;
        require(
            root.join("ignored").exists() == (cleanup == Cleanup::Warm),
            "Ignored cleanup policy violated",
        )?;
        if cleanup == Cleanup::Warm {
            require(
                fs::read_dir(root.join("ignored"))?.count() == ignored_files,
                "Ignored file count changed",
            )?;
            for i in 0..ignored_files {
                require(
                    fs::read(root.join(format!("ignored/cache-{i}")))? == residue,
                    "Ignored content changed",
                )?;
            }
        }
        call(runner, binary, repo, &["release", "--task", "bench"])?;
    }
    let (list, _) = call(runner, binary, repo, &["list"])?;
    require(
        list["leases"].as_array().is_some_and(Vec::is_empty),
        "Monorepo workload left reservations",
    )?;
    require(
        fingerprint(runner, repo, true)? == before,
        "Source checkout or refs changed",
    )?;
    for check in [
        "full_checkout_trees",
        "cleanup_policy",
        "commit_switches",
        "released",
        "source_preserved",
    ] {
        evidence.checked(check);
    }
    crate::evidence::observation(case, evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::{Contents, Workload};
    #[test]
    #[ignore = "opt-in release CLI: verifies both monorepo cleanup policies and commit switches"]
    fn small_monorepo_reuse_checks_full_trees_and_ignored_output() {
        let binary = crate::util::root().join("target/release/groove");
        assert!(binary.is_file(), "build the release CLI first");
        for cleanup in [Cleanup::Warm, Cleanup::Pristine] {
            let temp = tempfile::tempdir().unwrap();
            let runner = Runner::new().isolated(temp.path()).unwrap();
            let case = Case {
                id: "small-monorepo".into(),
                files: 100,
                contents: Contents::Monorepo { history_commits: 2 },
                workload: Workload::MonorepoReuse {
                    cleanup,
                    ignored_files: 2,
                    ignored_file_bytes: 16,
                },
            };
            let repo = temp.path().join("repo");
            create(&runner, &repo, &case, 2).unwrap();
            let observation = measure(&runner, &binary, &repo, &case, cleanup, 2, 16).unwrap();
            assert!(observation.verified);
            assert!(observation.metrics.contains_key("backward_acquire_seconds"));
            assert!(observation.metrics.contains_key("forward_acquire_seconds"));
        }
    }
    #[test]
    fn fixture_has_diverse_sizes_contents_paths_and_reproducible_history() {
        let temp = tempfile::tempdir().unwrap();
        let runner = Runner::new().isolated(temp.path()).unwrap();
        let case = Case {
            id: "small-monorepo".into(),
            files: 100,
            contents: Contents::Monorepo { history_commits: 2 },
            workload: Workload::MonorepoReuse {
                cleanup: Cleanup::Warm,
                ignored_files: 2,
                ignored_file_bytes: 16,
            },
        };
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        create(&runner, &a, &case, 2).unwrap();
        create(&runner, &b, &case, 2).unwrap();
        let clone = temp.path().join("clone");
        clone_seed(&runner, &a, &clone).unwrap();
        assert_eq!(
            runner.git_text(&clone, &["rev-parse", "HEAD"]).unwrap(),
            runner.git_text(&a, &["rev-parse", "HEAD"]).unwrap()
        );
        assert_eq!(
            runner
                .git_text(&clone, &["rev-parse", "bench-base"])
                .unwrap(),
            runner.git_text(&a, &["rev-parse", "bench-base"]).unwrap()
        );
        assert!(!clone.join(".git/objects/info/alternates").exists());
        use std::os::unix::fs::MetadataExt;
        for entry in fs::read_dir(a.join(".git/objects/pack")).unwrap() {
            let entry = entry.unwrap();
            let copied = clone.join(".git/objects/pack").join(entry.file_name());
            assert_ne!(
                entry.metadata().unwrap().ino(),
                fs::metadata(&copied).unwrap().ino()
            );
            assert_eq!(
                crate::util::digest(&entry.path()).unwrap(),
                crate::util::digest(&copied).unwrap()
            );
        }
        fs::write(clone.join(path(0)), "independent\n").unwrap();
        assert_ne!(
            fs::read(clone.join(path(0))).unwrap(),
            fs::read(a.join(path(0))).unwrap()
        );
        assert_eq!(
            runner.git_text(&a, &["rev-parse", "HEAD"]).unwrap(),
            runner.git_text(&b, &["rev-parse", "HEAD"]).unwrap()
        );
        assert_eq!(
            runner
                .git_text(&a, &["rev-list", "--count", "HEAD"])
                .unwrap(),
            "3"
        );
        assert_ne!(contents(0, 0), contents(1, 0));
        assert_ne!(contents(99, 0), contents(199, 0));
        for (i, expected) in [(0, 1024), (80, 16384), (95, 262144), (99, 1048576)] {
            assert_eq!(fs::metadata(a.join(path(i))).unwrap().len(), expected);
        }
        assert!(
            Path::new(&path(0)).components().count() > Path::new(&path(1)).components().count()
        );
        assert!(a.join("history/added-2.rs").exists());
        assert!(!a.join("history/obsolete-2.rs").exists());
        assert_eq!(content_bytes(100_000), 2_424_832_000);
    }
}
