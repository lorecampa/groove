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
        if skip.contains(&pid) {
            continue;
        }
        let pipeline = skip_ancestors && same_process_group(pid) && pipeline_reader(&entry.path());
        if let Some(uses) = process_use(&entry.path(), root, pipeline) {
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
fn process_use(proc_dir: &Path, root: &Path, pipeline: bool) -> Option<ProcessUse> {
    let inside = |link: PathBuf| fs::read_link(link).is_ok_and(|target| target.starts_with(root));
    if !pipeline && inside(proc_dir.join("cwd")) {
        return Some(ProcessUse::WorkingDirectory);
    }
    if inside(proc_dir.join("exe")) {
        return Some(ProcessUse::Executable);
    }
    if fs::read_dir(proc_dir.join("fd"))
        .is_ok_and(|descriptors| descriptors.flatten().any(|fd| inside(fd.path())))
    {
        return Some(ProcessUse::OpenFile);
    }
    // Closing the descriptor does not revoke a shared writable mapping.
    use std::os::unix::ffi::OsStrExt;
    let maps = fs::read(proc_dir.join("maps")).ok()?;
    for line in maps.split(|byte| *byte == b'\n') {
        let mut fields = line.splitn(6, |byte| byte.is_ascii_whitespace());
        let _address = fields.next();
        let permissions = fields.next().unwrap_or_default();
        let _offset = fields.next();
        let _device = fields.next();
        let _inode = fields.next();
        let pathname = fields.next().unwrap_or_default();
        let pathname = pathname.trim_ascii_start();
        if permissions.get(1) == Some(&b'w')
            && permissions.get(3) == Some(&b's')
            && Path::new(std::ffi::OsStr::from_bytes(pathname)).starts_with(root)
        {
            return Some(ProcessUse::OpenFile);
        }
    }
    None
}

fn pipeline_reader(proc_dir: &Path) -> bool {
    let Ok(input) = fs::read_link(proc_dir.join("fd/0")) else {
        return false;
    };
    input.as_os_str().to_string_lossy().starts_with("pipe:[")
        && ["/proc/self/fd/1", "/proc/self/fd/2"]
            .iter()
            .any(|output| fs::read_link(output).is_ok_and(|output| output == input))
}

/// Parent PID from `/proc/PID/stat`; the command name may contain spaces and parens.
pub(super) fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}
