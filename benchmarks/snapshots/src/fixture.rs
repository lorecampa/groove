use crate::{
    profiles::{Case, Contents},
    util::{require, Result, Runner},
};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(crate) const SPEC_BYTES: &[u8] = include_bytes!("../workloads-v1.json");
pub(crate) const LOCK_BYTES: &[u8] = include_bytes!("../environment.lock.json");
#[derive(Deserialize)]
pub(crate) struct Spec {
    pub(crate) fixture_version: u64,
    seed: usize,
    files_per_directory: usize,
    content_pool: usize,
    content_repetitions: usize,
    pub(crate) changed_files: usize,
    pub(crate) profiles: BTreeMap<String, usize>,
    pub(crate) expected_smoke_tree_sha1: String,
}
pub(crate) fn spec() -> Spec {
    serde_json::from_slice(SPEC_BYTES).expect("versioned fixture is valid")
}
pub(crate) fn lock() -> serde_json::Value {
    serde_json::from_slice(LOCK_BYTES).expect("versioned environment is valid")
}
impl Spec {
    pub(crate) fn create_workload(
        &self,
        runner: &Runner,
        repo: &Path,
        case: &Case,
    ) -> Result<(String, Vec<PathBuf>)> {
        if let Contents::Monorepo { history_commits } = case.contents {
            return crate::monorepo::create(runner, repo, case, history_commits);
        }
        let count = case.files;
        self.create(runner, repo, count)?;
        if case.contents == Contents::Unique {
            for i in 0..count {
                fs::write(
                    repo.join(self.path(i)),
                    format!("unique-{i:016x}\n").repeat(64),
                )?;
            }
        }
        let mut selected: Vec<_> = self
            .positions(count)
            .into_iter()
            .map(|i| PathBuf::from(self.path(i)))
            .collect();
        if let Contents::Binary { bytes: size } = case.contents {
            let mut state = 17_u64;
            let bytes: Vec<u8> = (0..size)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    state as u8
                })
                .collect();
            fs::write(repo.join("large.bin"), bytes)?;
            selected = vec!["large.bin".into()];
        }
        runner.git(repo, ["add", "-A"])?;
        runner.git(
            repo,
            [
                "-c",
                "user.name=Snapshot Scale",
                "-c",
                "user.email=scale@example.invalid",
                "commit",
                "--allow-empty",
                "-qm",
                "profile-fixture-v1",
            ],
        )?;
        runner.git(repo, ["repack", "-ad", "--depth=50", "--window=10"])?;
        Ok((
            runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?,
            selected,
        ))
    }
    pub(crate) fn path(&self, index: usize) -> String {
        format!("d{:05}/f{index:07}.txt", index / self.files_per_directory)
    }
    pub(crate) fn content(&self, index: usize) -> String {
        format!("fixture-v1:{}:{}\n", self.seed, index % self.content_pool)
            .repeat(self.content_repetitions)
    }
    pub(crate) fn positions(&self, count: usize) -> Vec<usize> {
        let total = self.changed_files.min(count);
        (0..total)
            .map(|j| (self.seed + j * (count / total)) % count)
            .collect()
    }
    pub(crate) fn create(&self, runner: &Runner, repo: &Path, count: usize) -> Result<String> {
        fs::create_dir(repo)?;
        runner.git(repo, ["init", "-q", "--object-format=sha1"])?;
        runner.git(repo, ["config", "user.name", "Snapshot Scale"])?;
        runner.git(repo, ["config", "user.email", "scale@example.invalid"])?;
        for i in 0..count {
            let path = repo.join(self.path(i));
            if i % self.files_per_directory == 0 {
                fs::create_dir(path.parent().expect("fixture parent"))?;
            }
            fs::write(path, self.content(i))?;
        }
        fs::write(repo.join(".gitignore"), "ignored/\n")?;
        runner.git(repo, ["add", "."])?;
        runner.git(repo, ["commit", "-qm", "fixture-v1"])?;
        let tree = runner.git_text(repo, &["rev-parse", "HEAD^{tree}"])?;
        if count == self.profiles["smoke"] {
            require(
                tree == self.expected_smoke_tree_sha1,
                "Fixture differs from golden tree",
            )?;
        }
        Ok(tree)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deterministic_generator_matches_independent_golden_tree() {
        let temp = tempfile::tempdir().unwrap();
        let runner = Runner::new().isolated(temp.path()).unwrap();
        let spec = spec();
        let a = spec
            .create(&runner, &temp.path().join("a"), spec.profiles["smoke"])
            .unwrap();
        let b = spec
            .create(&runner, &temp.path().join("b"), spec.profiles["smoke"])
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(a, spec.expected_smoke_tree_sha1);
    }
    #[test]
    fn distributed_edits_and_shared_contracts() {
        let spec = spec();
        for count in spec.profiles.values().copied() {
            let positions = spec.positions(count);
            assert_eq!(
                positions
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                positions.len()
            );
            assert!(
                positions.iter().max().unwrap() - positions.iter().min().unwrap() > count * 8 / 10
            );
        }
        assert!(spec.positions(0).is_empty());
        assert_eq!(spec.positions(1), [0]);
        let independent: serde_json::Value = serde_json::from_str(include_str!(
            "../../../crates/groove-core/tests/fixtures/snapshot-workloads-v1.json"
        ))
        .unwrap();
        assert_eq!(
            independent,
            serde_json::from_slice::<serde_json::Value>(SPEC_BYTES).unwrap()
        );
        let dockerfile = include_str!("../Dockerfile");
        for key in ["base_image", "debian_snapshot", "git_source_sha256"] {
            assert!(dockerfile.contains(lock()[key].as_str().unwrap()));
        }
    }
}
