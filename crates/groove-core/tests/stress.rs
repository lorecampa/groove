mod support;
use groove_core::{Pool, PoolOptions};
use std::fs;
use support::{git, Repo};

#[test]
fn seeded_contamination_and_recovery_sequences() {
    let seed = std::env::var("GROOVE_STRESS_SEED")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(42);
    let steps = std::env::var("GROOVE_STRESS_STEPS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(8);
    assert!(steps <= 1000, "Stress steps must be <=1000");
    let r = Repo::new();
    Pool::init(
        &r.path,
        PoolOptions {
            capacity: 1,
            ..Default::default()
        },
    )
    .unwrap();
    let pool = Pool::open(&r.path).unwrap();
    let mut random = seed;
    for step in 0..steps {
        random = random
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let mut lease = pool
            .acquire(Default::default())
            .unwrap_or_else(|e| panic!("seed={seed} step={step}: {e}"));
        let path = lease.info().worktree_path.clone();
        assert_eq!(
            fs::read_to_string(path.join("tracked.txt")).unwrap(),
            "baseline\n",
            "seed={seed} step={step}"
        );
        assert!(git(&path, &["status", "--porcelain", "--ignored"]).is_empty());
        match random % 6 {
            0 => {
                fs::write(path.join("tracked.txt"), "dirty staged").unwrap();
                git(&path, &["add", "."]);
            }
            1 => {
                fs::remove_file(path.join("tracked.txt")).unwrap();
            }
            2 => {
                fs::create_dir(path.join("ignored")).unwrap();
                fs::write(path.join("ignored/artifact"), "residual").unwrap();
            }
            3 => {
                git(&path, &["update-index", "--skip-worktree", "tracked.txt"]);
                fs::write(path.join("tracked.txt"), "hidden").unwrap();
            }
            4 => {
                let admin = git(&path, &["rev-parse", "--absolute-git-dir"]);
                fs::write(
                    std::path::Path::new(&admin).join("MERGE_HEAD"),
                    lease.info().base_oid.as_str(),
                )
                .unwrap();
            }
            _ => {
                fs::create_dir(path.join("nested")).unwrap();
                git(&path.join("nested"), &["init", "-q"]);
            }
        }
        if random & 1 == 0 {
            lease.release().unwrap();
        } // Other half exercise implicit closure.
    }
    let _final = pool.acquire(Default::default()).unwrap();
}
