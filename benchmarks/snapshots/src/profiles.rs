//! Versioned, closed benchmark scenarios. CLI options cannot alter their workload.
use crate::util::{hash, require, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub(crate) const CATALOG_BYTES: &[u8] = include_bytes!("../profiles-v1.json");
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Catalog {
    pub(crate) schema_version: u32,
    pub(crate) fixture_version: u32,
    pub(crate) profiles: Vec<Profile>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Profile {
    pub(crate) id: String,
    pub(crate) version: u32,
    pub(crate) purpose: String,
    pub(crate) environment: String,
    pub(crate) sampling: Sampling,
    pub(crate) estimated_minutes: [u32; 2],
    pub(crate) cases: Vec<Case>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Sampling {
    pub(crate) blocks: u32,
    pub(crate) warmup_blocks: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Case {
    pub(crate) id: String,
    pub(crate) files: usize,
    pub(crate) contents: Contents,
    pub(crate) workload: Workload,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Contents {
    Paths,
    Unique,
    Binary { bytes: usize },
    Monorepo { history_commits: usize },
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub(crate) enum Workload {
    SnapshotSave {
        history_depth: usize,
        mutation: Mutation,
    },
    SnapshotRestore,
    WorkspaceReuse {
        cleanup: Cleanup,
    },
    MonorepoReuse {
        cleanup: Cleanup,
        ignored_files: usize,
        ignored_file_bytes: usize,
    },
    WorkspaceSetup {
        artifact_files: usize,
        artifact_file_bytes: usize,
        artifact_binary_bytes: usize,
    },
    WorkspaceConcurrency {
        workers: usize,
    },
    WorkspaceMultiPool {
        pools: usize,
        workers_per_pool: usize,
    },
    WorkspaceDeletion {
        slots: usize,
        materialized: bool,
        dirty: bool,
        external: bool,
        artifact_files: usize,
        artifact_file_bytes: usize,
        artifact_binary_bytes: usize,
    },
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Mutation {
    Edits,
    Mixed,
    Binary,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Cleanup {
    Pristine,
    Warm,
}
impl Cleanup {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Pristine => "pristine",
            Self::Warm => "warm",
        }
    }
}
impl Profile {
    pub(crate) fn sha256(&self) -> Result<String> {
        Ok(hash(&serde_json::to_vec(self)?))
    }
}
impl Case {
    pub(crate) fn path(&self, index: usize) -> String {
        if matches!(self.contents, Contents::Monorepo { .. }) {
            crate::monorepo::path(index)
        } else {
            crate::fixture::spec().path(index)
        }
    }
    fn description(&self) -> String {
        let contents = match self.contents {
            Contents::Paths => "small repeated text".into(),
            Contents::Unique => "unique 1.5 KiB text".into(),
            Contents::Binary { bytes } => format!("plus a {} MiB binary", bytes / 1048576),
            Contents::Monorepo { history_commits } => format!(
                "heterogeneous text/binary, {:.2} GiB tracked, {history_commits} history commits",
                crate::monorepo::content_bytes(self.files) as f64 / 1073741824.0
            ),
        };
        let operations = match self.workload {
            Workload::SnapshotSave { history_depth, .. } => format!(
                "first, changed, unchanged and {} history saves; verify recovered contents",
                history_depth - 1
            ),
            Workload::SnapshotRestore => {
                "clean/dirty restores into new and existing checkouts".into()
            }
            Workload::WorkspaceReuse { cleanup } => {
                format!("first/clean/dirty reuse with {} cleanup", cleanup.name())
            }
            Workload::MonorepoReuse { cleanup, ignored_files, ignored_file_bytes } => format!(
                "first/clean/dirty reuse and backward/forward commit changes; {} cleanup; {ignored_files} ignored files ({:.1} MiB)",
                cleanup.name(), (ignored_files * ignored_file_bytes) as f64 / 1048576.0
            ),
            Workload::WorkspaceSetup {
                artifact_files,
                artifact_file_bytes,
                artifact_binary_bytes,
            } => format!(
                "cache miss/hit, {} artifacts, {:.1} MiB",
                artifact_files + 1,
                (artifact_files * artifact_file_bytes + artifact_binary_bytes) as f64 / 1048576.0
            ),
            Workload::WorkspaceConcurrency { workers } => format!(
                "{workers} simultaneous acquire/execute/release workers and full-pool rejection"
            ),
            Workload::WorkspaceMultiPool { pools, workers_per_pool } => format!(
                "{pools} independent pools with {workers_per_pool} workers each; first/warm acquisition, execution, release and catalog"
            ),
            Workload::WorkspaceDeletion { slots, materialized, dirty, external, artifact_files, .. } => format!(
                "delete {slots} {} slots, {} residue, {} checkout root, {artifact_files} cache files; retain protected sibling, snapshots/history/locks and reinitialize",
                if materialized { "materialized" } else { "lazy" }, if dirty { "dirty" } else { "clean" }, if external { "external" } else { "default" }
            ),
        };
        format!(
            "{} generated source paths ({contents}); {operations}",
            self.files
        )
    }
    pub(crate) fn capacity(&self) -> usize {
        match self.workload {
            Workload::WorkspaceConcurrency { workers } => workers,
            Workload::WorkspaceMultiPool {
                pools,
                workers_per_pool,
            } => pools * workers_per_pool,
            Workload::WorkspaceDeletion { slots, .. } => slots,
            _ => 1,
        }
    }
    pub(crate) fn scratch_budget(&self) -> (u64, u64) {
        let copies = match self.workload {
            Workload::SnapshotRestore => 3,
            Workload::SnapshotSave { .. } => 2,
            Workload::WorkspaceDeletion { .. } => self.capacity() + 3,
            _ => self.capacity() + 1,
        } as u64;
        if let Contents::Monorepo { history_commits } = self.contents {
            let ignored = match self.workload {
                Workload::MonorepoReuse {
                    ignored_files,
                    ignored_file_bytes,
                    ..
                } => (ignored_files * ignored_file_bytes) as u64,
                _ => 0,
            };
            // Include objects/history, independent full-tree oracles, checkouts,
            // directory allocation and ignored output. These are conservative
            // admission budgets, not claimed disk-usage measurements.
            let objects_and_copies = copies + history_commits as u64 + 4;
            return (
                crate::monorepo::content_bytes(self.files) * objects_and_copies
                    + self.files as u64 * objects_and_copies * 8192
                    + ignored * 2,
                self.files as u64 * objects_and_copies * 3 + 20000,
            );
        }
        let binary = if let Contents::Binary { bytes } = self.contents {
            bytes as u64
        } else {
            0
        };
        let artifact = if let Workload::WorkspaceSetup {
            artifact_files,
            artifact_file_bytes,
            artifact_binary_bytes,
        }
        | Workload::WorkspaceDeletion {
            artifact_files,
            artifact_file_bytes,
            artifact_binary_bytes,
            ..
        } = self.workload
        {
            (artifact_files * artifact_file_bytes + artifact_binary_bytes) as u64 * copies.max(4)
        } else {
            0
        };
        (
            self.files as u64 * copies * 4096 + binary * 12 + artifact + 512 * 1024 * 1024,
            self.files as u64 * copies + 20000,
        )
    }
}
pub(crate) fn validate(profile: &Profile) -> Result<()> {
    require(
        !profile.id.is_empty()
            && profile.id != "all"
            && profile.version == 1
            && !profile.purpose.is_empty()
            && profile.environment == crate::fixture::lock()["environment_id"],
        "Invalid profile provenance/version",
    )?;
    require(
        (6..=100).contains(&profile.sampling.blocks) && profile.sampling.warmup_blocks <= 10,
        "Invalid sampling policy",
    )?;
    require(!profile.cases.is_empty(), "Empty profile")?;
    let mut cases = BTreeSet::new();
    for case in &profile.cases {
        require(
            !case.id.is_empty() && cases.insert(&case.id) && (12..=1_000_000).contains(&case.files),
            "Invalid profile case",
        )?;
        match &case.workload {
            Workload::SnapshotSave {
                history_depth,
                mutation,
            } => {
                require(
                    (2..=100).contains(history_depth),
                    "Invalid checkpoint history",
                )?;
                require(
                    (*mutation == Mutation::Binary)
                        == matches!(case.contents, Contents::Binary { .. }),
                    "Binary mutation/fixture mismatch",
                )?;
            }
            Workload::WorkspaceConcurrency { workers } => {
                require((1..=32).contains(workers), "Invalid worker count")?
            }
            Workload::MonorepoReuse {
                ignored_files,
                ignored_file_bytes,
                ..
            } => {
                require(
                    matches!(case.contents, Contents::Monorepo { .. }),
                    "Monorepo workload needs monorepo contents",
                )?;
                require(
                    (1..=10000).contains(ignored_files)
                        && (1..=1048576).contains(ignored_file_bytes),
                    "Invalid monorepo ignored output",
                )?;
            }
            Workload::WorkspaceMultiPool {
                pools,
                workers_per_pool,
            } => require(
                (2..=8).contains(pools)
                    && (1..=8).contains(workers_per_pool)
                    && pools * workers_per_pool <= 32,
                "Invalid multi-pool worker count",
            )?,
            Workload::WorkspaceDeletion {
                slots,
                materialized,
                dirty,
                artifact_files,
                artifact_file_bytes,
                artifact_binary_bytes,
                ..
            } => {
                require(
                    (1..=4096).contains(slots) && (!dirty || *materialized),
                    "Invalid deletion slot/residue policy",
                )?;
                require(
                    (*artifact_files == 0
                        && *artifact_file_bytes == 0
                        && *artifact_binary_bytes == 0)
                        || (*materialized
                            && *dirty
                            && *artifact_files > 0
                            && *artifact_files <= 10000
                            && *artifact_file_bytes > 0
                            && *artifact_file_bytes <= 1024 * 1024
                            && *artifact_binary_bytes <= 64 * 1024 * 1024),
                    "Invalid deletion cache fixture",
                )?;
            }
            Workload::WorkspaceSetup {
                artifact_files,
                artifact_file_bytes,
                artifact_binary_bytes,
            } => require(
                *artifact_files > 0
                    && *artifact_files <= 10000
                    && *artifact_file_bytes > 0
                    && *artifact_file_bytes <= 1024 * 1024
                    && *artifact_binary_bytes <= 64 * 1024 * 1024,
                "Invalid setup artifacts",
            )?,
            _ => (),
        }
        if let Contents::Binary { bytes } = case.contents {
            require(
                (65536..=96 * 1024 * 1024).contains(&bytes),
                "Invalid binary fixture",
            )?;
        }
        if let Contents::Monorepo { history_commits } = case.contents {
            require(
                (2..=32).contains(&history_commits) && case.files >= 100,
                "Invalid monorepo history or size",
            )?;
            require(
                matches!(
                    case.workload,
                    Workload::MonorepoReuse { .. }
                        | Workload::SnapshotSave { .. }
                        | Workload::SnapshotRestore
                ),
                "Unsupported monorepo workload",
            )?;
        }
    }
    Ok(())
}
pub(crate) fn catalog() -> Result<Catalog> {
    let catalog: Catalog = serde_json::from_slice(CATALOG_BYTES)?;
    require(
        catalog.schema_version == 1 && catalog.fixture_version == 1,
        "Unsupported profile catalog version",
    )?;
    let mut names = BTreeSet::new();
    for profile in &catalog.profiles {
        require(names.insert(&profile.id), "Duplicate profile identity")?;
        validate(profile)?;
    }
    Ok(catalog)
}
pub(crate) fn select(names: &[String]) -> Result<Vec<Profile>> {
    let catalog = catalog()?;
    if names == ["all"] {
        return Ok(catalog.profiles);
    }
    require(
        !names.is_empty() && !names.iter().any(|n| n == "all"),
        "Select named profiles or 'all' on its own",
    )?;
    let mut selected = Vec::new();
    let mut seen = BTreeSet::new();
    for name in names {
        require(seen.insert(name), "A profile may only be selected once")?;
        selected.push(
            catalog
                .profiles
                .iter()
                .find(|p| &p.id == name)
                .cloned()
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "Unknown profile '{name}'; run 'profiles' to list scenarios"
                    ))
                })?,
        );
    }
    Ok(selected)
}
pub(crate) fn display(name: Option<&str>) -> Result<()> {
    let profiles = if let Some(name) = name {
        select(&[name.to_owned()])?
    } else {
        catalog()?.profiles
    };
    for profile in profiles {
        let bytes = profile
            .cases
            .iter()
            .map(|c| c.scratch_budget().0)
            .max()
            .unwrap_or(0);
        let inodes = profile
            .cases
            .iter()
            .map(|c| c.scratch_budget().1)
            .max()
            .unwrap_or(0);
        println!("{} (v{})\n  {}\n  {} cases; {} measured repetitions + {} discarded warmup per case and binary\n  Each repetition runs the complete case on a fresh disposable repository.\n  Estimated {}–{} min, at least {:.1} GiB scratch and {} free inodes (unpaired; host dependent)",profile.id,profile.version,profile.purpose,profile.cases.len(),profile.sampling.blocks,profile.sampling.warmup_blocks,profile.estimated_minutes[0],profile.estimated_minutes[1],bytes as f64/1073741824.0,inodes);
        for case in &profile.cases {
            println!("  {}: {}", case.id, case.description());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monorepo_profiles_budget_content_and_reject_incompatible_workloads() {
        let mut profile = select(&["monorepo-reuse".into()]).unwrap().remove(0);
        let case = &profile.cases[0];
        assert!(crate::monorepo::content_bytes(case.files) > 2 * 1024 * 1024 * 1024);
        assert!(case.scratch_budget().0 > crate::monorepo::content_bytes(case.files) * 4);
        assert!(case.scratch_budget().1 > case.files as u64 * 4);
        profile.cases[0].contents = Contents::Unique;
        assert!(validate(&profile).is_err());
        profile.cases[0].contents = Contents::Monorepo { history_commits: 0 };
        assert!(validate(&profile).is_err());
        profile.cases[0].contents = Contents::Monorepo { history_commits: 8 };
        profile.cases[0].workload = Workload::WorkspaceConcurrency { workers: 2 };
        assert!(validate(&profile).is_err());
    }
    #[test]
    fn named_profiles_cover_distinct_user_workflows_with_real_sampling() {
        let all = select(&["all".into()]).unwrap();
        let names: BTreeSet<_> = all.iter().map(|p| p.id.as_str()).collect();
        assert!(names.contains("workspace-multi-pool") && names.contains("workspace-deletion"));
        let scale = all.iter().find(|p| p.id == "snapshot-scale").unwrap();
        assert!(scale.cases.iter().any(|c| c.files == 1_000_000));
        assert!(all.iter().all(|p| p.sampling.blocks >= 6));
        assert!(select(&["million".into()]).is_err());
        assert!(select(&["all".into(), "snapshot-scale".into()]).is_err());
        assert!(select(&["snapshot-scale".into(), "snapshot-scale".into()]).is_err());
        let mut changed = scale.clone();
        changed.sampling.blocks += 1;
        assert_ne!(scale.sha256().unwrap(), changed.sha256().unwrap());
    }

    #[test]
    fn lifecycle_profiles_reject_impossible_residue_and_worker_definitions() {
        let mut multi = select(&["workspace-multi-pool".into()]).unwrap().remove(0);
        multi.cases[0].workload = Workload::WorkspaceMultiPool {
            pools: 2,
            workers_per_pool: 0,
        };
        assert!(validate(&multi).is_err());
        let mut deletion = select(&["workspace-deletion".into()]).unwrap().remove(0);
        if let Workload::WorkspaceDeletion { dirty, .. } = &mut deletion.cases[0].workload {
            *dirty = true;
        }
        assert!(validate(&deletion).is_err());
        let mut deletion = select(&["workspace-deletion".into()]).unwrap().remove(0);
        if let Workload::WorkspaceDeletion {
            materialized,
            dirty,
            artifact_binary_bytes,
            ..
        } = &mut deletion.cases[0].workload
        {
            *materialized = true;
            *dirty = true;
            *artifact_binary_bytes = 1;
        }
        assert!(validate(&deletion).is_err());
    }
}
