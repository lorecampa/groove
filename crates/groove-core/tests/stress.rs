mod support;
use groove_core::{AcquireOptions, Lease, Pool, PoolOptions};
use proptest::{
    prelude::*,
    test_runner::{Config, FileFailurePersistence, RngSeed, TestRunner},
};
use std::{fs, time::Duration};
use support::{git, Repo};

#[derive(Clone, Copy, Debug)]
enum Contamination {
    Staged,
    Deleted,
    Ignored,
    SkipWorktree,
    InterruptedMerge,
    NestedRepository,
}
const CONTAMINATIONS: [Contamination; 6] = [
    Contamination::Staged,
    Contamination::Deleted,
    Contamination::Ignored,
    Contamination::SkipWorktree,
    Contamination::InterruptedMerge,
    Contamination::NestedRepository,
];

fn assert_pristine(lease: &Lease, repo: &Repo) {
    let path = &lease.info().worktree_path;
    assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"baseline\n");
    assert!(git(path, &["status", "--porcelain", "--ignored"]).is_empty());
    assert_eq!(
        git(path, &["rev-parse", "HEAD"]),
        repo.git(&["rev-parse", "HEAD"])
    );
    assert_eq!(
        git(path, &["write-tree"]),
        repo.git(&["rev-parse", "HEAD^{tree}"])
    );
    // A successful cleanup must never affect the source checkout.
    assert_eq!(
        fs::read(repo.path.join("tracked.txt")).unwrap(),
        b"baseline\n"
    );
    assert!(repo.git(&["status", "--porcelain", "--ignored"]).is_empty());
}

fn run_sequence(operations: &[(Contamination, bool)]) {
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
    let acquire = || {
        pool.acquire(AcquireOptions {
            // Other concurrent fixtures may be between fork and exec with inherited
            // descriptors. Wait for actual closure; never force an unlock.
            wait: Duration::from_secs(5),
            ..Default::default()
        })
        .unwrap()
    };
    for (contamination, explicit_release) in operations {
        let mut lease = acquire();
        assert_pristine(&lease, &repo);
        let path = &lease.info().worktree_path;
        match contamination {
            Contamination::Staged => {
                fs::write(path.join("tracked.txt"), "dirty staged").unwrap();
                git(path, &["add", "."]);
            }
            Contamination::Deleted => fs::remove_file(path.join("tracked.txt")).unwrap(),
            Contamination::Ignored => {
                fs::create_dir(path.join("ignored")).unwrap();
                fs::write(path.join("ignored/artifact"), "residual").unwrap();
            }
            Contamination::SkipWorktree => {
                git(path, &["update-index", "--skip-worktree", "tracked.txt"]);
                fs::write(path.join("tracked.txt"), "hidden").unwrap();
            }
            Contamination::InterruptedMerge => {
                let admin = git(path, &["rev-parse", "--absolute-git-dir"]);
                fs::write(
                    std::path::Path::new(&admin).join("MERGE_HEAD"),
                    lease.info().base_oid.as_str(),
                )
                .unwrap();
            }
            Contamination::NestedRepository => {
                fs::create_dir(path.join("nested")).unwrap();
                git(&path.join("nested"), &["init", "-q"]);
            }
        }
        if *explicit_release {
            lease.release().unwrap();
        }
    }
    assert_pristine(&acquire(), &repo);
}

#[test]
fn every_contamination_is_cleaned_after_both_release_and_drop() {
    // Deterministic coverage of all 12 pairs, regardless of generated seeds.
    let operations: Vec<_> = CONTAMINATIONS
        .into_iter()
        .flat_map(|contamination| [false, true].map(|release| (contamination, release)))
        .collect();
    run_sequence(&operations);
}

#[test]
fn seeded_contamination_and_recovery_sequences() {
    let steps = std::env::var("GROOVE_STRESS_STEPS")
        .map(|v| {
            v.parse::<usize>()
                .expect("GROOVE_STRESS_STEPS must be an integer")
        })
        .unwrap_or(8);
    assert!((1..=1000).contains(&steps), "Stress steps must be 1..=1000");
    let mut config = Config {
        cases: std::env::var("PROPTEST_CASES")
            .map(|v| v.parse().expect("PROPTEST_CASES must be an integer"))
            .unwrap_or(16),
        source_file: Some(file!()),
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "proptest-regressions/stress.txt",
        ))),
        ..Config::default()
    };
    if let Ok(seed) = std::env::var("GROOVE_STRESS_SEED") {
        config.rng_seed =
            RngSeed::Fixed(seed.parse().expect("GROOVE_STRESS_SEED must be an integer"));
    }
    // Product strategies independently generate the action and release method;
    // shrinking removes operations and simplifies choices without sharing RNG bits.
    let operations = proptest::collection::vec(
        (
            proptest::sample::select(CONTAMINATIONS.to_vec()),
            any::<bool>(),
        ),
        1..=steps,
    );
    TestRunner::new(config)
        .run(&operations, |operations| {
            run_sequence(&operations);
            Ok(())
        })
        .unwrap();
}
