//! Explicit diagnostic retention. Never removes workspace contents or cache entries.
use crate::{
    error::PathContext, locking, repository, state, HistoryFilter, LogState, Pool, Result,
    RunHistory, RunId, RunOutcome,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, path::Path, time::Duration};

/// Logical file sizes, not allocated/reclaimable disk blocks (reflinks may share blocks).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageUsage {
    /// Bytes in captured stdout/stderr of known runs.
    pub log_bytes: u64,
    /// Bytes in durable run metadata.
    pub history_bytes: u64,
    /// Bytes in task-log directories without a valid history filename.
    pub untracked_log_bytes: u64,
    /// Number of task-log directories without history; cleanup never adopts them.
    pub untracked_runs: u64,
    /// Logical bytes in reusable artifact caches, including cache metadata.
    pub artifact_bytes: u64,
    /// Logical bytes in managed slot directories; symlinks are not followed.
    pub workspace_bytes: u64,
    /// Number of known runs, including unresolved ones.
    pub runs: u64,
}

/// Outcome of evaluating one run for explicit log cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    /// Both log files were removed (or an interrupted removal was completed).
    Removed,
    /// Eligible in a dry run; nothing was deleted.
    WouldRemove,
    /// Not eligible or its slot is busy/protected by an execution record.
    Skipped,
    /// Removal failed; retry after resolving the reported error.
    Failed,
}
/// Per-run report, including partial failures so successful removals remain visible.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanupEntry {
    /// Run evaluated.
    pub run_id: RunId,
    /// Logical bytes in retained log files when evaluated.
    pub log_bytes: u64,
    /// Action taken or proposed.
    pub status: CleanupStatus,
    /// Explanation for skipped/failed entries.
    pub reason: Option<String>,
    /// Typed error category for failed entries.
    pub error_code: Option<crate::ErrorCode>,
}
/// Result of an explicit cleanup; individual failures do not hide successful removals.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogCleanup {
    /// Whether this was a preview without mutations.
    pub dry_run: bool,
    /// One outcome for every known run observed at the start.
    pub entries: Vec<CleanupEntry>,
}

fn unsafe_path(path: &Path) -> crate::Error {
    crate::Error::new(
        crate::ErrorCode::InvalidArgument,
        "Refusing unsafe storage path",
    )
    .with_details(|d| d.path = Some(path.into()))
}

// No symlink traversal, even when measuring user-controlled workspace contents.
fn tree_bytes(path: &Path) -> Result<u64> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e).at(path),
    };
    if meta.is_file() {
        return Ok(meta.len());
    }
    if !meta.is_dir() {
        return Ok(0);
    }
    let mut bytes = 0u64;
    for entry in fs::read_dir(path).at(path)? {
        bytes = bytes.saturating_add(tree_bytes(&entry.at(path)?.path())?);
    }
    Ok(bytes)
}

fn log_bytes(record: &RunHistory) -> Result<u64> {
    let mut bytes = 0u64;
    for path in [&record.stdout_path, &record.stderr_path] {
        match fs::symlink_metadata(path) {
            Ok(_) => {
                // The same owned, regular, singly-linked-file validation as permanent locks.
                let file = locking::open(path, false)?;
                bytes = bytes.saturating_add(file.metadata().at(path)?.len());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).at(path),
        }
    }
    Ok(bytes)
}
impl Pool {
    /// Report logical storage usage without following symlinks or running Git status.
    ///
    /// Sizes are a nontransactional observation and may count shared/reflinked bytes
    /// more than once. Shared Git objects and snapshot storage are not estimated.
    /// # Errors
    /// Reports malformed history, unsafe managed paths, and I/O failures.
    pub fn storage_usage(&self) -> Result<StorageUsage> {
        let pool = self.refreshed()?;
        let records = pool.history(&HistoryFilter::default())?;
        let known_ids: BTreeSet<_> = records
            .iter()
            .map(|record| record.run_id.to_string())
            .collect();
        let mut usage = StorageUsage {
            runs: records.len() as u64,
            ..Default::default()
        };
        for record in &records {
            usage.log_bytes = usage.log_bytes.saturating_add(log_bytes(record)?);
            usage.history_bytes = usage
                .history_bytes
                .saturating_add(tree_bytes(&pool.run_dir(record.run_id).join("run.json"))?);
        }
        let root = pool.repo.root.join("task-logs");
        if root.try_exists()? {
            repository::owned_dir(&root)?;
            for entry in fs::read_dir(&root).at(&root)? {
                let entry = entry.at(&root)?;
                let known = entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| known_ids.contains(name));
                if !known {
                    usage.untracked_runs +=
                        u64::from(entry.file_type().at(&entry.path())?.is_dir());
                    usage.untracked_log_bytes = usage
                        .untracked_log_bytes
                        .saturating_add(tree_bytes(&entry.path())?);
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

    /// Remove captured logs of terminal runs at least `older_than` old, retaining history.
    ///
    /// `dry_run` previews the same eligibility checks without modifying records/files.
    /// Holds the permanent execution lock and refuses any pending execution record
    /// for the slot. Age is only a retention filter, never evidence of ownership.
    /// Interrupted removal is retryable. Unknown/legacy directories are untouched.
    ///
    /// # Errors
    /// Initial scan errors abort before deletion. Per-run failures appear in `entries`;
    /// callers must inspect them. Successfully removed logs cannot be restored by Groove.
    pub fn clean_logs(&self, older_than: Duration, dry_run: bool) -> Result<LogCleanup> {
        let _operation = self.retained_operation()?;
        let pool = self.refreshed()?;
        let records = pool.history(&HistoryFilter::default())?;
        let now = crate::reservation::now_ms();
        let age = older_than.as_millis().min(u128::from(u64::MAX)) as u64;
        let mut report = LogCleanup {
            dry_run,
            entries: Vec::new(),
        };
        for record in records {
            let mut entry = CleanupEntry {
                run_id: record.run_id,
                log_bytes: 0,
                status: CleanupStatus::Skipped,
                reason: None,
                error_code: None,
            };
            if let Err(error) = pool.clean_run(record.run_id, now, age, dry_run, &mut entry) {
                entry.status = CleanupStatus::Failed;
                entry.reason = Some(error.to_string());
                entry.error_code = Some(error.code);
            }
            report.entries.push(entry);
        }
        Ok(report)
    }
    fn clean_run(
        &self,
        id: RunId,
        now: u64,
        age: u64,
        dry_run: bool,
        entry: &mut CleanupEntry,
    ) -> Result<()> {
        let initial = self.read_run(id)?;
        let _guard = match self.execution_guard(&initial.slot_id) {
            Ok(guard) => guard,
            Err(e) if e.code == crate::ErrorCode::InvalidLease => {
                entry.reason = Some("Slot execution lock is busy".into());
                return Ok(());
            }
            Err(e) => return Err(e),
        };
        state::checkpoint("log-cleanup-locked");
        if let Err(e) = self.require_execution_clear(&initial.slot_id) {
            if e.code != crate::ErrorCode::InvalidLease {
                return Err(e);
            }
            entry.reason = Some("Slot needs explicit execution recovery".into());
            return Ok(());
        }
        let mut record = self.read_run(id)?;
        if record.outcome == RunOutcome::Unresolved {
            entry.reason = Some("Run outcome is unresolved".into());
            return Ok(());
        }
        if record.logs == LogState::Removed {
            entry.reason = Some("Logs already removed".into());
            return Ok(());
        }
        if record
            .ended_at_unix_ms
            .is_none_or(|end| now.checked_sub(end).is_none_or(|elapsed| elapsed < age))
        {
            entry.reason = Some("Run is newer than the retention threshold".into());
            return Ok(());
        }
        entry.log_bytes = log_bytes(&record)?;
        if dry_run {
            entry.status = CleanupStatus::WouldRemove;
            return Ok(());
        }
        record.logs = LogState::Removing;
        self.write_run(&record)?;
        for path in [&record.stdout_path, &record.stderr_path] {
            // Revalidate immediately before unlink; the execution lock excludes Groove writers.
            match fs::symlink_metadata(path) {
                Ok(_) => {
                    let _file = locking::open(path, false)?;
                    fs::remove_file(path).at(path)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).at(path),
            }
            state::checkpoint("log-removed");
        }
        state::sync_dir(&self.run_dir(id))?;
        record.logs = LogState::Removed;
        self.write_run(&record)?;
        entry.status = CleanupStatus::Removed;
        Ok(())
    }

    /// Explicitly forget a terminal run whose logs were already removed.
    ///
    /// # Errors
    /// Refuses unresolved runs, retained logs, active/orphaned executions, unexpected
    /// directory contents and unsafe paths. Never releases reservations or deletes receipts.
    pub fn history_delete(&self, id: RunId) -> Result<()> {
        let _operation = self.retained_operation()?;
        let pool = self.refreshed()?;
        let record = pool.read_run(id)?;
        let _guard = pool.execution_guard(&record.slot_id)?;
        pool.require_execution_clear(&record.slot_id)?;
        let record = pool.read_run(id)?;
        if record.outcome == RunOutcome::Unresolved || record.logs != LogState::Removed {
            return Err(crate::Error::new(
                crate::ErrorCode::InvalidArgument,
                "Clean this terminal run's logs before deleting its history",
            ));
        }
        let dir = pool.run_dir(id);
        for entry in fs::read_dir(&dir).at(&dir)? {
            let entry = entry.at(&dir)?;
            if entry.file_name() != "run.json" {
                return Err(unsafe_path(&entry.path()));
            }
        }
        let path = dir.join("run.json");
        let _file = locking::open(&path, false)?;
        fs::remove_file(&path).at(&path)?;
        state::sync_dir(&dir)?;
        fs::remove_dir(&dir).at(&dir)?;
        state::sync_dir(&pool.repo.root.join("task-logs"))
    }
}
