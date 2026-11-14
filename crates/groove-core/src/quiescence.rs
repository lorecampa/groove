//! Advisory checks for unsaved work and processes using a workspace.
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
use os::parent_of;

use crate::{Error, ErrorCode, LeaseId, Pool, Result, WorkspaceGitStatus};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, fs, time::Duration};

fn processes_using(root: &std::path::Path) -> Result<Vec<WorkspaceProcess>> {
    os::processes_using(root, true)
}

pub(crate) fn all_processes_using(root: &std::path::Path) -> Result<Vec<WorkspaceProcess>> {
    os::processes_using(root, false)
}

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
    /// shell that ran this command), plus its process group (pipeline peers),
    /// are excluded. Only processes this user may
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

    /// Reject observed workspace users and, when `require_saved`, unsaved work.
    ///
    /// The saved-work check covers staged, modified and non-ignored untracked files,
    /// plus HEAD commits unreachable from any ref. Inactive reservations (for example
    /// a retried release) are not checked. These observations do not stop writers or
    /// acquire ownership; callers must stop writers before release even if this succeeds.
    ///
    /// # Errors
    /// [`ErrorCode::WorkspaceInUse`] lists the processes; [`ErrorCode::UnsavedChanges`]
    /// carries observed file changes or the path of an unpreserved HEAD. Also reports
    /// lease lookup, missing-checkout, process-enumeration and Git failures.
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
                format!("Processes are still using this workspace: {summary}; stop all workspace writers before retrying"),
            )
            .retryable(true)
            .with_details(|d| d.processes = processes));
        }
        if require_saved {
            let lease = self.lease_info(id)?;
            missing_checkout(&lease.worktree_path)?;
            let changes = self.workspace_changes(id, timeout)?;
            if changes.has_changes() {
                return Err(Error::new(
                    ErrorCode::UnsavedChanges,
                    "The workspace has unsaved changes; run release --save to snapshot them, or pass --force to discard them",
                )
                .with_details(|d| d.changes = Some(changes)));
            }
            if let Some(path) = crate::workspace_files::first_special(&lease.worktree_path)? {
                return Err(Error::new(ErrorCode::UnsavedChanges,
                    "The workspace contains a special file Git cannot save; export or remove it, or release with --force to discard it")
                    .with_details(|details| details.path = Some(path)));
            }
            require_saved_head(&lease.worktree_path, timeout)?;
        }
        Ok(())
    }
}

/// A clean index does not preserve detached commits. Reflogs can expire, so
/// require a ref that keeps HEAD reachable before normal release or destruction.
pub(crate) fn require_saved_head(path: &std::path::Path, timeout: Duration) -> Result<()> {
    let git = crate::git::Git::new(timeout);
    let head = git.run(path, &["rev-parse", "--verify", "HEAD^{commit}"], &[])?;
    let head = std::str::from_utf8(&head)
        .map_err(|e| Error::new(ErrorCode::GitError, "Invalid HEAD OID").with_source(e))?
        .trim_end_matches('\n');
    let common = git.run(
        path,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        &[],
    )?;
    let common = std::str::from_utf8(&common)
        .map_err(|error| {
            Error::new(ErrorCode::GitError, "Invalid common Git directory").with_source(error)
        })?
        .trim_end_matches('\n');
    if git
        .run_with(
            std::path::Path::new(common),
            &[
                "for-each-ref",
                "--count=1",
                "--contains",
                head,
                "--format=%(refname)",
            ],
            crate::git::Invocation {
                reject_stderr: true,
                ..Default::default()
            },
        )?
        .is_empty()
    {
        return Err(Error::new(
            ErrorCode::UnsavedChanges,
            "Workspace HEAD is not preserved by a shared ref; create a branch, tag, stash or snapshot, or explicitly discard the work",
        ).with_details(|d| d.path = Some(path.into())));
    }
    Ok(())
}

/// Reject a reservation whose checkout was removed outside Groove (for example
/// `rm -rf` or `git worktree remove --force`). Nothing remains to inspect or save,
/// so the caller must explicitly acknowledge the loss.
pub(crate) fn missing_checkout(path: &std::path::Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::new(
            ErrorCode::RecoveryFailed,
            format!(
                "The reserved checkout {} no longer exists, so its work cannot be inspected or saved; release with --force to discard the reservation",
                path.display()
            ),
        )
        .with_details(|d| d.worktree_path = Some(path.to_owned()))),
        _ => Ok(()),
    }
}

/// Pipeline peers belong to the invocation, rather than independent workspace users.
fn same_process_group(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: these calls only observe process-group IDs. A vanished or
    // inaccessible PID returns -1 and cannot match this process's group.
    unsafe { libc::getpgid(pid) == libc::getpgrp() }
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
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&root)
            .process_group(0)
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
