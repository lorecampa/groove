mod support;

use groove_core::{
    Error, ErrorCode, ExecOptions, HistoryFilter, LeaseInfo, Pool, PoolOptions, PreparationOptions,
    PreparationStatus,
};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};
use support::{git, Repo};

const PROJECT: &str = r#"
schema_version=1
[caches.deps]
paths=["ignored/first", "ignored/second"]
inputs=["deps.lock"]
key="cleanup-guard"
[setup.dev]
cache="deps"
[[setup.dev.steps]]
name="dependencies"
command=["bash", "-c", "mkdir -p ignored/first ignored/second; printf first > ignored/first/artifact; printf second > ignored/second/artifact"]
when="cache_miss"
[[setup.dev.steps]]
name="setup-marker"
command=["bash", "-c", "printf ran > setup-ran"]
"#;

fn fixture(project: &str) -> (Repo, Pool) {
    let repo = Repo::new();
    fs::write(repo.path.join(".groove.toml"), project).unwrap();
    fs::write(repo.path.join("deps.lock"), "compatible\n").unwrap();
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "setup definition"]);
    Pool::init(
        &repo.path,
        PoolOptions {
            capacity: 2,
            worktree_root: Some(support::worktree_root(&repo.path, "default")),
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

struct Originals {
    first: PathBuf,
    second: PathBuf,
    modified: SystemTime,
}
impl Originals {
    fn new(checkout: &Path) -> Self {
        let first = checkout.join("ignored/first/sentinel");
        let second = checkout.join("ignored/second/sentinel");
        fs::create_dir_all(first.parent().unwrap()).unwrap();
        fs::create_dir_all(second.parent().unwrap()).unwrap();
        fs::write(&first, b"first original\0\xff\n").unwrap();
        fs::write(&second, b"second original\0\xfe\n").unwrap();
        fs::set_permissions(&first, fs::Permissions::from_mode(0o751)).unwrap();
        let modified = fs::metadata(&first).unwrap().modified().unwrap();
        Self {
            first,
            second,
            modified,
        }
    }

    fn assert_preserved(&self) {
        assert_eq!(fs::read(&self.first).unwrap(), b"first original\0\xff\n");
        assert_eq!(fs::read(&self.second).unwrap(), b"second original\0\xfe\n");
        let metadata = fs::metadata(&self.first).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o751);
        assert_eq!(metadata.modified().unwrap(), self.modified);
    }
}

fn assert_refused(
    pool: &Pool,
    lease: &LeaseInfo,
    error: &Error,
    unsafe_root: &Path,
    originals: &Originals,
    runs_before: usize,
) {
    assert!(
        matches!(
            error.code,
            ErrorCode::InvalidArgument | ErrorCode::UnsupportedConfiguration
        ),
        "{error:?}"
    );
    assert!(
        error
            .details
            .path
            .as_ref()
            .is_some_and(|path| path.starts_with(unsafe_root)),
        "{error:?}"
    );
    assert_eq!(error.details.lease_id, Some(lease.lease_id));
    assert_eq!(
        error.details.worktree_path.as_ref(),
        Some(&lease.worktree_path)
    );
    let preparation = error.details.preparation.as_ref().unwrap();
    assert_eq!(preparation.status, PreparationStatus::Failed);
    assert!(preparation.error.is_some());
    assert!(preparation.steps.is_empty());
    let persisted = pool.preparation(lease.lease_id).unwrap().unwrap();
    assert_eq!(persisted.status, PreparationStatus::Failed);
    assert_eq!(persisted.error, preparation.error);
    assert!(persisted.steps.is_empty());
    assert_eq!(
        pool.lease_info(lease.lease_id).unwrap().worktree_path,
        lease.worktree_path
    );
    assert!(pool
        .protected_leases()
        .unwrap()
        .iter()
        .any(|protected| protected.lease_id == lease.lease_id));
    assert_eq!(
        pool.history(&HistoryFilter::default()).unwrap().len(),
        runs_before,
        "refused setup must never execute a worker"
    );
    assert!(!lease.worktree_path.join("setup-ran").exists());
    originals.assert_preserved();
}

#[derive(Clone, Copy, Debug)]
enum Unsafe {
    Clone,
    Linked,
    ForeignLinked,
    RegisteredWithoutPointer,
    RegisteredRootWithoutPointer,
    Bare,
    BareRoot,
    GitFile,
    GitDirectory,
    GitSymlink,
    DanglingGitSymlink,
    EscapingSymlink,
    RootSymlink,
}

struct Occupant {
    foreign: Repo,
    path: PathBuf,
    revision: Option<String>,
    registrations: String,
    symlink: Option<(PathBuf, PathBuf)>,
}
impl Occupant {
    fn new(repo: &Repo, checkout: &Path, kind: Unsafe) -> Self {
        let foreign = Repo::new();
        let second = checkout.join("ignored/second");
        let path = if matches!(
            kind,
            Unsafe::BareRoot | Unsafe::RegisteredRootWithoutPointer
        ) {
            second.clone()
        } else {
            // Exercise NUL-delimited registration paths too.
            second.join("package with spaces\nand è")
        };
        let mut link = None;
        let repository = match kind {
            Unsafe::Clone => {
                git(
                    &repo.path,
                    &[
                        "clone",
                        "-q",
                        foreign.path.to_str().unwrap(),
                        path.to_str().unwrap(),
                    ],
                );
                true
            }
            Unsafe::Linked
            | Unsafe::ForeignLinked
            | Unsafe::RegisteredWithoutPointer
            | Unsafe::RegisteredRootWithoutPointer => {
                let owner = if matches!(kind, Unsafe::ForeignLinked) {
                    &foreign
                } else {
                    repo
                };
                if path == second {
                    // Git requires an absent destination. Restore the original
                    // sentinel after creating the real linked checkout.
                    fs::remove_file(second.join("sentinel")).unwrap();
                    fs::remove_dir(&second).unwrap();
                }
                owner.git(&[
                    "worktree",
                    "add",
                    "-q",
                    "--detach",
                    path.to_str().unwrap(),
                    "HEAD",
                ]);
                if path == second {
                    fs::write(second.join("sentinel"), b"second original\0\xfe\n").unwrap();
                }
                true
            }
            Unsafe::Bare | Unsafe::BareRoot => {
                let destination = if path == second {
                    second.join("store")
                } else {
                    path.clone()
                };
                git(
                    &repo.path,
                    &[
                        "clone",
                        "-q",
                        "--bare",
                        foreign.path.to_str().unwrap(),
                        destination.to_str().unwrap(),
                    ],
                );
                if path == second {
                    for entry in fs::read_dir(second.join("store")).unwrap() {
                        let entry = entry.unwrap();
                        fs::rename(entry.path(), second.join(entry.file_name())).unwrap();
                    }
                    fs::remove_dir(second.join("store")).unwrap();
                }
                true
            }
            Unsafe::RootSymlink => {
                let outside = foreign.temp.path().join("outside");
                fs::rename(&second, &outside).unwrap();
                symlink(&outside, &second).unwrap();
                link = Some((second.clone(), outside));
                false
            }
            _ => {
                fs::create_dir_all(&path).unwrap();
                let dotgit = path.join(".git");
                match kind {
                    Unsafe::GitFile => fs::write(&dotgit, "not even a valid pointer\n").unwrap(),
                    Unsafe::GitDirectory => fs::create_dir(&dotgit).unwrap(),
                    Unsafe::GitSymlink | Unsafe::DanglingGitSymlink => {
                        let target = if matches!(kind, Unsafe::GitSymlink) {
                            foreign.path.join(".git")
                        } else {
                            foreign.path.join("absent")
                        };
                        symlink(&target, &dotgit).unwrap();
                        link = Some((dotgit, target));
                    }
                    Unsafe::EscapingSymlink => {
                        let target = foreign.path.clone();
                        let path = path.join("outside");
                        symlink(&target, &path).unwrap();
                        link = Some((path, target));
                    }
                    _ => unreachable!(),
                }
                false
            }
        };
        let revision = if repository {
            if !matches!(kind, Unsafe::Bare | Unsafe::BareRoot) {
                fs::write(path.join("unique.txt"), "unpublished history\n").unwrap();
                git(&path, &["add", "unique.txt"]);
                git(
                    &path,
                    &[
                        "-c",
                        "user.name=Cache Test",
                        "-c",
                        "user.email=cache@example.invalid",
                        "commit",
                        "-qm",
                        "unpublished nested result",
                    ],
                );
            }
            fs::write(path.join("result.txt"), "precious untracked result\n").unwrap();
            Some(git(&path, &["rev-parse", "HEAD"]))
        } else {
            None
        };
        if matches!(
            kind,
            Unsafe::RegisteredWithoutPointer | Unsafe::RegisteredRootWithoutPointer
        ) {
            fs::remove_file(path.join(".git")).unwrap();
            // Leave ordinary cache bytes, so only the native registration
            // establishes that cleanup would destroy a checkout.
            if path.join(".groove.toml").exists() {
                fs::remove_file(path.join(".groove.toml")).unwrap();
            }
        }
        let registrations = foreign.git(&["worktree", "list", "--porcelain", "-z"]);
        Self {
            foreign,
            path,
            revision,
            registrations,
            symlink: link,
        }
    }

    fn assert_preserved(&self, repo: &Repo, kind: Unsafe) {
        assert_eq!(
            self.foreign.git(&["worktree", "list", "--porcelain", "-z"]),
            self.registrations
        );
        assert_eq!(
            fs::read(self.foreign.path.join("tracked.txt")).unwrap(),
            b"baseline\n"
        );
        if let Some(revision) = &self.revision {
            assert_eq!(
                fs::read(self.path.join("result.txt")).unwrap(),
                b"precious untracked result\n"
            );
            if matches!(
                kind,
                Unsafe::RegisteredWithoutPointer | Unsafe::RegisteredRootWithoutPointer
            ) {
                assert_eq!(
                    repo.git(&["show", &format!("{revision}:unique.txt")]),
                    "unpublished history"
                );
                assert_eq!(
                    fs::read(self.path.join("unique.txt")).unwrap(),
                    b"unpublished history\n"
                );
                assert!(!self.path.join(".git").exists());
            } else {
                assert_eq!(git(&self.path, &["rev-parse", "HEAD"]), *revision);
                git(
                    &self.path,
                    &["cat-file", "-e", &format!("{revision}^{{commit}}")],
                );
                if !matches!(kind, Unsafe::Bare | Unsafe::BareRoot) {
                    assert_eq!(
                        fs::read(self.path.join("unique.txt")).unwrap(),
                        b"unpublished history\n"
                    );
                }
            }
        }
        if let Some((path, target)) = &self.symlink {
            assert_eq!(fs::read_link(path).unwrap(), *target);
        }
        match kind {
            Unsafe::GitFile => assert_eq!(
                fs::read(self.path.join(".git")).unwrap(),
                b"not even a valid pointer\n"
            ),
            Unsafe::GitDirectory => assert!(self.path.join(".git").is_dir()),
            _ => (),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Attempt {
    Miss,
    Hit,
    Retry,
}

fn reject_unsafe_second_root(attempt: Attempt) {
    for kind in [
        Unsafe::Clone,
        Unsafe::Linked,
        Unsafe::ForeignLinked,
        Unsafe::RegisteredWithoutPointer,
        Unsafe::RegisteredRootWithoutPointer,
        Unsafe::Bare,
        Unsafe::BareRoot,
        Unsafe::GitFile,
        Unsafe::GitDirectory,
        Unsafe::GitSymlink,
        Unsafe::DanglingGitSymlink,
        Unsafe::EscapingSymlink,
        Unsafe::RootSymlink,
    ] {
        let (repo, pool) = fixture(PROJECT);
        let producer = pool.reserve(Default::default()).unwrap();
        if matches!(attempt, Attempt::Hit) {
            let prepared = pool.prepare(producer.lease_id, "dev", options()).unwrap();
            assert_eq!(prepared.status, PreparationStatus::Ready);
            assert!(!prepared.cache_hit);
        }
        let lease = pool.reserve(Default::default()).unwrap();
        let originals = Originals::new(&lease.worktree_path);
        let occupant = Occupant::new(&repo, &lease.worktree_path, kind);
        let registrations = repo.git(&["worktree", "list", "--porcelain", "-z"]);
        let runs_before = pool.history(&HistoryFilter::default()).unwrap().len();
        let error = pool.prepare(lease.lease_id, "dev", options()).unwrap_err();
        let unsafe_root = lease.worktree_path.join("ignored/second");
        assert_refused(&pool, &lease, &error, &unsafe_root, &originals, runs_before);
        if matches!(attempt, Attempt::Retry) {
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
            assert_refused(&pool, &lease, &error, &unsafe_root, &originals, runs_before);
        }
        assert_eq!(
            repo.git(&["worktree", "list", "--porcelain", "-z"]),
            registrations,
            "{attempt:?}: {kind:?}"
        );
        occupant.assert_preserved(&repo, kind);
        assert_eq!(
            pool.reserve(Default::default()).unwrap_err().code,
            ErrorCode::PoolExhausted,
            "refusal must retain reservation exclusion"
        );
    }
}

#[test]
fn cache_miss_preserves_all_roots_when_the_second_contains_git_or_unsafe_links() {
    if support::in_subprocess(
        "cache_miss_preserves_all_roots_when_the_second_contains_git_or_unsafe_links",
    ) {
        reject_unsafe_second_root(Attempt::Miss);
    }
}

#[test]
fn cache_hit_preserves_all_roots_when_the_second_contains_git_or_unsafe_links() {
    if support::in_subprocess(
        "cache_hit_preserves_all_roots_when_the_second_contains_git_or_unsafe_links",
    ) {
        reject_unsafe_second_root(Attempt::Hit);
    }
}

#[test]
fn explicit_retry_preserves_all_roots_and_never_runs_setup_after_refusal() {
    if support::in_subprocess(
        "explicit_retry_preserves_all_roots_and_never_runs_setup_after_refusal",
    ) {
        reject_unsafe_second_root(Attempt::Retry);
    }
}

#[test]
fn rejected_import_preserves_the_source_and_every_destination_root() {
    if !support::in_subprocess("rejected_import_preserves_the_source_and_every_destination_root") {
        return;
    }
    for kind in [Unsafe::Clone, Unsafe::Bare, Unsafe::GitSymlink] {
        let (repo, pool) = fixture(PROJECT);
        let source = Originals::new(&repo.path);
        let occupant = Occupant::new(&repo, &repo.path, kind);
        let lease = pool.reserve(Default::default()).unwrap();
        let destination = Originals::new(&lease.worktree_path);
        let registrations = repo.git(&["worktree", "list", "--porcelain", "-z"]);
        let error = pool
            .prepare(
                lease.lease_id,
                "dev",
                PreparationOptions {
                    cache_source: Some(repo.path.clone()),
                    ..options()
                },
            )
            .unwrap_err();
        assert_refused(
            &pool,
            &lease,
            &error,
            &repo.path.join("ignored/second"),
            &destination,
            0,
        );
        source.assert_preserved();
        occupant.assert_preserved(&repo, kind);
        assert_eq!(
            repo.git(&["worktree", "list", "--porcelain", "-z"]),
            registrations
        );
    }
}

#[test]
fn imports_check_the_sources_own_registrations_even_without_a_git_pointer() {
    if !support::in_subprocess(
        "imports_check_the_sources_own_registrations_even_without_a_git_pointer",
    ) {
        return;
    }
    let (_repo, pool) = fixture(PROJECT);
    let source = Repo::new();
    fs::write(source.path.join("deps.lock"), "compatible\n").unwrap();
    let originals = Originals::new(&source.path);
    let occupant = Occupant::new(&source, &source.path, Unsafe::RegisteredWithoutPointer);
    let lease = pool.reserve(Default::default()).unwrap();
    let destination = Originals::new(&lease.worktree_path);
    let registrations = source.git(&["worktree", "list", "--porcelain", "-z"]);
    let error = pool
        .prepare(
            lease.lease_id,
            "dev",
            PreparationOptions {
                cache_source: Some(source.path.clone()),
                ..options()
            },
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_refused(
        &pool,
        &lease,
        &error,
        &source.path.join("ignored/second"),
        &destination,
        0,
    );
    originals.assert_preserved();
    occupant.assert_preserved(&source, Unsafe::RegisteredWithoutPointer);
    assert_eq!(
        source.git(&["worktree", "list", "--porcelain", "-z"]),
        registrations
    );
}

#[test]
fn preflight_limits_refuse_cleanup_before_deleting_any_root() {
    if !support::in_subprocess("preflight_limits_refuse_cleanup_before_deleting_any_root") {
        return;
    }
    for limit in ["max_files=6", "max_files=1000"] {
        let project = PROJECT.replace(
            "key=\"cleanup-guard\"",
            &format!("key=\"cleanup-guard\"\n{limit}"),
        );
        let (_repo, pool) = fixture(&project);
        let lease = pool.reserve(Default::default()).unwrap();
        let originals = Originals::new(&lease.worktree_path);
        let second = lease.worktree_path.join("ignored/second");
        if limit == "max_files=6" {
            for n in 0..10 {
                fs::write(second.join(format!("dependency-{n}")), "keep").unwrap();
            }
        } else {
            fs::create_dir_all(second.join("d/".repeat(129))).unwrap();
        }
        let error = pool.prepare(lease.lease_id, "dev", options()).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_refused(&pool, &lease, &error, &second, &originals, 0);
    }
}

#[test]
fn healthy_import_hit_and_retry_preserve_internal_links_and_independent_artifacts() {
    if !support::in_subprocess(
        "healthy_import_hit_and_retry_preserve_internal_links_and_independent_artifacts",
    ) {
        return;
    }
    // Structural cleanup must accept old files larger than the new snapshot's
    // byte quota, without reading them. New publications still obey that quota.
    let project = PROJECT.replace(
        "key=\"cleanup-guard\"",
        "key=\"cleanup-guard\"\nmax_bytes=1024",
    );
    let (repo, pool) = fixture(&project);
    for root in ["ignored/first", "ignored/second"] {
        let root = repo.path.join(root);
        fs::create_dir_all(root.join("package/objects")).unwrap();
        // A dependency can use ordinary Git-like names without being a store.
        fs::write(root.join("package/HEAD"), "package data").unwrap();
        fs::write(root.join("artifact"), "source artifact").unwrap();
        fs::set_permissions(root.join("artifact"), fs::Permissions::from_mode(0o751)).unwrap();
        symlink("artifact", root.join("current")).unwrap();
    }
    let modified = fs::metadata(repo.path.join("ignored/first/artifact"))
        .unwrap()
        .modified()
        .unwrap();
    let first = pool.reserve(Default::default()).unwrap();
    fs::create_dir_all(first.worktree_path.join("ignored/first")).unwrap();
    fs::File::create(first.worktree_path.join("ignored/first/old-large-file"))
        .unwrap()
        .set_len(2 * 1024 * 1024)
        .unwrap();
    // A dangling internal cycle must be inspected as a link, never followed.
    symlink("cycle", first.worktree_path.join("ignored/first/cycle")).unwrap();
    let imported = pool
        .prepare(
            first.lease_id,
            "dev",
            PreparationOptions {
                cache_source: Some(repo.path.clone()),
                ..options()
            },
        )
        .unwrap();
    assert_eq!(imported.status, PreparationStatus::Ready);
    assert!(imported.cache_hit);
    assert!(imported.steps[0].skipped);
    assert!(!first
        .worktree_path
        .join("ignored/first/old-large-file")
        .exists());
    let second = pool.reserve(Default::default()).unwrap();
    let seeded = pool.prepare(second.lease_id, "dev", options()).unwrap();
    assert_eq!(seeded.status, PreparationStatus::Ready);
    assert!(seeded.cache_hit);
    for checkout in [&repo.path, &first.worktree_path, &second.worktree_path] {
        for root in ["ignored/first", "ignored/second"] {
            let root = checkout.join(root);
            assert_eq!(fs::read(root.join("current")).unwrap(), b"source artifact");
            assert_eq!(
                fs::read_link(root.join("current")).unwrap(),
                Path::new("artifact")
            );
            assert_eq!(
                fs::metadata(root.join("artifact")).unwrap().mode() & 0o777,
                0o751
            );
        }
    }
    assert_eq!(
        fs::metadata(second.worktree_path.join("ignored/first/artifact"))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    fs::write(
        first.worktree_path.join("ignored/first/artifact"),
        "private change",
    )
    .unwrap();
    assert_eq!(
        fs::read(repo.path.join("ignored/first/artifact")).unwrap(),
        b"source artifact"
    );
    assert_eq!(
        fs::read(second.worktree_path.join("ignored/first/artifact")).unwrap(),
        b"source artifact"
    );
    let retried = pool
        .prepare(
            first.lease_id,
            "dev",
            PreparationOptions {
                retry: true,
                ..options()
            },
        )
        .unwrap();
    assert_eq!(retried.status, PreparationStatus::Ready);
    assert!(retried.cache_hit);
    assert_eq!(
        fs::read(first.worktree_path.join("ignored/first/artifact")).unwrap(),
        b"source artifact"
    );
}
