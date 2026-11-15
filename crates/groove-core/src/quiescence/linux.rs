use super::{same_process_group, self_and_ancestors, ProcessUse, WorkspaceProcess};
use crate::Result;
use std::{
    collections::HashSet,
    fs,
    os::unix::fs::MetadataExt,
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
    let mut candidates = Vec::new();
    let executable = fs::metadata("/proc/self/exe")?;
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
        if skip_ancestors
            && fs::metadata(entry.path().join("exe")).is_ok_and(|metadata| {
                metadata.dev() == executable.dev() && metadata.ino() == executable.ino()
            })
        {
            continue;
        }
        candidates.push((pid, entry.path()));
    }
    let readers = if skip_ancestors {
        pipeline_readers(&candidates)
    } else {
        HashSet::new()
    };
    let mut found = Vec::new();
    for (pid, path) in candidates {
        if let Some(uses) = process_use(&path, root, readers.contains(&pid)) {
            let command = fs::read_to_string(path.join("comm"))
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

fn pipeline_readers(candidates: &[(u32, PathBuf)]) -> HashSet<u32> {
    fn pipe(path: impl AsRef<Path>) -> Option<PathBuf> {
        fs::read_link(path)
            .ok()
            .filter(|target| target.as_os_str().to_string_lossy().starts_with("pipe:["))
    }
    let mut pipes: HashSet<_> = ["/proc/self/fd/1", "/proc/self/fd/2"]
        .into_iter()
        .filter_map(pipe)
        .collect();
    let mut readers = HashSet::new();
    loop {
        let before = readers.len();
        for (pid, path) in candidates {
            if !readers.contains(pid)
                && same_process_group(*pid)
                && pipe(path.join("fd/0")).is_some_and(|input| pipes.contains(&input))
            {
                readers.insert(*pid);
                pipes.extend(
                    ["fd/1", "fd/2"]
                        .into_iter()
                        .filter_map(|fd| pipe(path.join(fd))),
                );
            }
        }
        if readers.len() == before {
            return readers;
        }
    }
}

/// Parent PID from `/proc/PID/stat`; the command name may contain spaces and parens.
pub(super) fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = &stat[stat.rfind(')')? + 1..];
    after_name.split_whitespace().nth(1)?.parse().ok()
}
