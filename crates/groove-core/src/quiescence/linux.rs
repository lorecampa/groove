use super::{same_process_group, self_and_ancestors, ProcessUse, WorkspaceProcess};
use crate::Result;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// Scan `/proc` for processes using `root`. Exclude this process and its ancestors
/// only when `skip_ancestors` is set; destructive cleanup also checks the caller.
pub(super) fn processes_using(root: &Path, skip_ancestors: bool) -> Result<Vec<WorkspaceProcess>> {
    let skip = if skip_ancestors {
        self_and_ancestors()
    } else {
        Default::default()
    };
    let entries = fs::read_dir("/proc")?;
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if skip.contains(&pid) || (skip_ancestors && same_process_group(pid)) {
            continue;
        }
        if let Some(uses) = process_use(&entry.path(), root) {
            let command = fs::read_to_string(entry.path().join("comm"))
                .map(|c| c.trim_end().to_owned())
                .unwrap_or_default();
            found.push(WorkspaceProcess { pid, command, uses });
        }
    }
    found.sort_by_key(|p| p.pid);
    Ok(found)
}

/// How the process at `/proc/PID` uses `root`, if at all. Unreadable entries
/// (other users, exited processes) are treated as not using it.
fn process_use(proc_dir: &Path, root: &Path) -> Option<ProcessUse> {
    let inside = |link: PathBuf| fs::read_link(link).is_ok_and(|target| target.starts_with(root));
    if inside(proc_dir.join("cwd")) {
        return Some(ProcessUse::WorkingDirectory);
    }
    if inside(proc_dir.join("exe")) {
        return Some(ProcessUse::Executable);
    }
    let descriptors = fs::read_dir(proc_dir.join("fd")).ok()?;
    descriptors
        .flatten()
        .any(|fd| inside(fd.path()))
        .then_some(ProcessUse::OpenFile)
}

/// Parent PID from `/proc/PID/stat`; the command name may contain spaces and parens.
pub(super) fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}
