//! Read-only disk usage for pool inspection, including data retained from older versions.
use crate::{error::PathContext, repository, Pool, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

/// Logical file bytes in a pool, including retained legacy logs and caches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageUsage {
    /// Bytes in legacy stdout/stderr files identified by their physical filenames.
    pub log_bytes: u64,
    /// Bytes in legacy run metadata files.
    pub history_bytes: u64,
    /// Bytes in task-log directories without a valid history filename.
    pub untracked_log_bytes: u64,
    /// Number of untracked legacy task-log directories.
    pub untracked_runs: u64,
    /// Logical bytes in legacy artifact caches, including cache metadata.
    pub artifact_bytes: u64,
    /// Logical bytes in managed slot directories; symlinks are not followed.
    pub workspace_bytes: u64,
    /// Number of legacy UUID run directories with a regular metadata file.
    pub runs: u64,
}

// Count physical filenames only. Legacy records never authorize following stored paths.
fn tree_bytes(path: &Path) -> Result<u64> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error).at(path),
    };
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut bytes = 0u64;
    for entry in fs::read_dir(path).at(path)? {
        bytes = bytes.saturating_add(tree_bytes(&entry.at(path)?.path())?);
    }
    Ok(bytes)
}

impl Pool {
    /// Report logical bytes without following symlinks, parsing legacy run records or running Git.
    ///
    /// This is a nontransactional observation, not a reclaimable-space estimate or
    /// evidence that writers stopped. Shared Git objects and snapshots are excluded.
    /// Legacy UUID run directories with a regular `run.json` are counted separately
    /// from untracked data; their metadata contents are not interpreted.
    ///
    /// # Errors
    /// Reports invalid pool state, unsafe managed paths and I/O failures.
    pub fn storage_usage(&self) -> Result<StorageUsage> {
        let _operation = self.retained_operation()?;
        let pool = self.refreshed()?;
        let mut usage = StorageUsage::default();
        let root = pool.repo.root.join("task-logs");
        if root.try_exists()? {
            repository::owned_dir(&root)?;
            for entry in fs::read_dir(&root).at(&root)? {
                let entry = entry.at(&root)?;
                let dir = entry.path();
                let is_dir = entry.file_type().at(&dir)?.is_dir();
                let known = is_dir
                    && entry.file_name().to_str().is_some_and(|name| {
                        uuid::Uuid::parse_str(name).is_ok_and(|id| id.to_string() == name)
                    })
                    && match fs::symlink_metadata(dir.join("run.json")) {
                        Ok(metadata) => metadata.is_file(),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                        Err(error) => return Err(error).at(&dir),
                    };
                let total = tree_bytes(&dir)?;
                if known {
                    let history = tree_bytes(&dir.join("run.json"))?;
                    let logs = tree_bytes(&dir.join("stdout.log"))?
                        .saturating_add(tree_bytes(&dir.join("stderr.log"))?);
                    usage.runs += 1;
                    usage.history_bytes = usage.history_bytes.saturating_add(history);
                    usage.log_bytes = usage.log_bytes.saturating_add(logs);
                    usage.untracked_log_bytes = usage
                        .untracked_log_bytes
                        .saturating_add(total.saturating_sub(history.saturating_add(logs)));
                } else {
                    usage.untracked_runs += u64::from(is_dir);
                    usage.untracked_log_bytes = usage.untracked_log_bytes.saturating_add(total);
                }
            }
        }
        let artifacts = pool.repo.root.join("artifacts");
        repository::no_symlinks(&artifacts)?;
        usage.artifact_bytes = tree_bytes(&artifacts)?;
        if pool.config.worktree_root.exists() {
            repository::owned_dir(&pool.config.worktree_root)?;
        }
        for slot in pool.config.slots.keys() {
            usage.workspace_bytes = usage
                .workspace_bytes
                .saturating_add(tree_bytes(&pool.config.worktree_root.join(slot.as_str()))?);
        }
        Ok(usage)
    }
}
