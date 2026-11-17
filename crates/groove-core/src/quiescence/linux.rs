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
    let started = std::time::Instant::now();
    let executable = fs::metadata("/proc/self/exe")?;
    loop {
        let (mut found, readers) = scan_processes(root, skip_ancestors)?;
        if !found.is_empty() {
            crate::state::checkpoint("workspace-processes-scanned");
        }
        if found.is_empty() && super::retry_observed_readers(&readers, started) {
            continue;
        }
        // Discovery can start Git in the caller's workspace before Groove knows
        // which pool/slot admission to take. The caller already excludes admitted
        // inspectors. Briefly wait only for these pre-admission Groove children;
        // never exempt them or delay reporting unrelated workspace users.
        if !skip_ancestors
            || found.is_empty()
            || started.elapsed() >= std::time::Duration::from_millis(500)
            || !found.iter().all(|process| {
                super::parent_of(process.pid).is_some_and(|parent| {
                    fs::metadata(format!("/proc/{parent}/exe")).is_ok_and(|metadata| {
                        metadata.dev() == executable.dev() && metadata.ino() == executable.ino()
                    })
                })
            })
        {
            super::retain_live_observations(&mut found);
            return Ok(found);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn scan_processes(root: &Path, skip_ancestors: bool) -> Result<(Vec<WorkspaceProcess>, Vec<u32>)> {
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
    let mut observed_readers = Vec::new();
    for (pid, path) in candidates {
        let mut observed_reader = false;
        if let Some(uses) = process_use(&path, root, readers.contains(&pid), &mut observed_reader) {
            let command = fs::read_to_string(path.join("comm"))
                .map(|c| c.trim_end().to_owned())
                .unwrap_or_default();
            found.push(WorkspaceProcess { pid, command, uses });
        } else if observed_reader {
            observed_readers.push(pid);
        }
    }
    found.sort_by_key(|p| p.pid);
    Ok((found, observed_readers))
}

/// How the process at `/proc/PID` uses `root`, if at all. Unreadable entries
/// (other users, exited processes) are treated as not using it.
fn process_use(
    proc_dir: &Path,
    root: &Path,
    pipeline: bool,
    observed_reader: &mut bool,
) -> Option<ProcessUse> {
    let inside = |link: PathBuf| fs::read_link(link).is_ok_and(|target| target.starts_with(root));
    if !pipeline && inside(proc_dir.join("cwd")) {
        return Some(ProcessUse::WorkingDirectory);
    }
    if inside(proc_dir.join("exe")) {
        return Some(ProcessUse::Executable);
    }
    let outside = ["cwd", "exe"]
        .into_iter()
        .all(|link| fs::read_link(proc_dir.join(link)).is_ok_and(|path| !path.starts_with(root)));
    if let Ok(descriptors) = fs::read_dir(proc_dir.join("fd")) {
        for fd in descriptors.flatten().filter(|fd| inside(fd.path())) {
            let flags = fs::read_to_string(proc_dir.join("fdinfo").join(fd.file_name()))
                .ok()
                .and_then(|info| {
                    info.lines().find_map(|line| {
                        line.strip_prefix("flags:")
                            .and_then(|value| u32::from_str_radix(value.trim(), 8).ok())
                    })
                });
            if outside
                && flags
                    .is_some_and(|flags| flags & libc::O_ACCMODE as u32 == libc::O_RDONLY as u32)
            {
                *observed_reader = true;
            } else {
                return Some(ProcessUse::OpenFile);
            }
        }
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

/// An unreaped child still answers kill(pid, 0), but no longer owns its files.
pub(super) fn has_exited(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .map(|(_, fields)| fields.trim_start().starts_with("Z "))
        })
        .unwrap_or(false)
}
