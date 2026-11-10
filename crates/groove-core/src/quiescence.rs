//! Best-effort checks that a workspace is safe to hand back: no unsaved changes and
//! no other processes still using it.
//!
//! These are observations, not ownership: a process can start, or a file can change,
//! right after a check. They exist to catch the common mistakes (a forgotten watcher,
//! uncommitted work), not to replace stopping writers.

#[cfg(target_os = "linux")]
#[path = "quiescence/linux.rs"]
mod os;
#[cfg(target_os = "macos")]
#[path = "quiescence/macos.rs"]
mod os;
use os::{parent_of, processes_using};

use crate::{Error, ErrorCode, LeaseId, Pool, Result, WorkspaceGitStatus};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, time::Duration};

/// How a process was observed using a workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessUse {
    /// Its current working directory is inside the workspace.
    WorkingDirectory,
    /// Its executable lives inside the workspace.
    Executable,
    /// It holds an open file inside the workspace.
    OpenFile,
}

/// A process observed using a workspace. PIDs are diagnostic only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceProcess {
    /// Process ID at the time of the scan.
    pub pid: u32,
    /// Short command name reported by the operating system.
    pub command: String,
    /// First way the process was seen using the workspace.
    pub uses: ProcessUse,
}

impl Pool {
    /// List other processes whose working directory, executable or open files are
    /// inside the reservation's workspace. The calling process and its ancestors (the
    /// shell that ran this command) are excluded. Only processes this user may
    /// inspect are seen.
    ///
    /// # Errors
    /// Rejects unknown or stale lease IDs and reports process-enumeration failures.
    pub fn workspace_processes(&self, id: LeaseId) -> Result<Vec<WorkspaceProcess>> {
        let lease = self.lease_info(id)?;
        let Ok(root) = fs::canonicalize(&lease.worktree_path) else {
            return Ok(Vec::new());
        };
        processes_using(&root)
    }

    /// Report whether the reservation's workspace has staged, modified or untracked
    /// files, using one bounded, read-only `git status` tuned for large repositories.
    ///
    /// # Errors
    /// Rejects unknown or stale lease IDs and reports Git failures.
    pub fn workspace_changes(&self, id: LeaseId, timeout: Duration) -> Result<WorkspaceGitStatus> {
        let lease = self.lease_info(id)?;
        self.git_status(&lease.slot_id, &lease.worktree_path, timeout)
    }

    /// Whether `id` is a currently active (protected) reservation.
    ///
    /// # Errors
    /// Reports malformed protection records and filesystem errors.
    pub fn is_reserved(&self, id: LeaseId) -> Result<bool> {
        Ok(self
            .protected_leases()?
            .iter()
            .any(|lease| lease.lease_id == id))
    }

    /// Fail unless the workspace looks safe to hand back: no other processes using it
    /// and, when `require_saved`, no unsaved changes. Inactive reservations (for example
    /// a retried release) are not checked.
    ///
    /// # Errors
    /// [`ErrorCode::WorkspaceInUse`] lists the processes; [`ErrorCode::UnsavedChanges`]
    /// carries the observed changes. Both are overridable by the caller.
    pub fn ensure_quiescent(
        &self,
        id: LeaseId,
        require_saved: bool,
        timeout: Duration,
    ) -> Result<()> {
        if !self.is_reserved(id)? {
            return Ok(());
        }
        let processes = self.workspace_processes(id)?;
        if !processes.is_empty() {
            let summary = processes
                .iter()
                .map(|p| format!("{} ({})", p.pid, p.command))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::new(
                ErrorCode::WorkspaceInUse,
                format!("Processes are still using this workspace: {summary}; stop them, or pass --force"),
            )
            .with_details(|d| d.processes = processes));
        }
        if require_saved {
            let changes = self.workspace_changes(id, timeout)?;
            if changes.has_changes() {
                return Err(Error::new(
                    ErrorCode::UnsavedChanges,
                    "The workspace has unsaved changes; run finish to snapshot them, or pass --force to discard them",
                )
                .with_details(|d| d.changes = Some(changes)));
            }
        }
        Ok(())
    }
}

/// This process and every ancestor up to init, so the invoking shell is not reported.
fn self_and_ancestors() -> BTreeSet<u32> {
    let mut pids = BTreeSet::new();
    let mut pid = std::process::id();
    while pid > 1 && pids.insert(pid) {
        match parent_of(pid) {
            Some(parent) => pid = parent,
            None => break,
        }
    }
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ancestors_include_self_and_parent() {
        let pids = self_and_ancestors();
        assert!(pids.contains(&std::process::id()));
        assert!(parent_of(std::process::id())
            .is_some_and(|parent| pids.contains(&parent) || parent <= 1));
    }

    #[test]
    fn a_child_inside_a_directory_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&root)
            .spawn()
            .unwrap();
        let found = processes_using(&root).unwrap();
        let _ = child.kill();
        let _ = child.wait();
        assert!(found
            .iter()
            .any(|p| p.pid == child.id() && p.uses == ProcessUse::WorkingDirectory));
    }
}
