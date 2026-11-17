//! Best-effort Darwin process inspection through libproc, never ownership proof.
use super::{same_process_group, self_and_ancestors, ProcessUse, WorkspaceProcess};
use crate::Result;
use std::{
    collections::HashSet,
    ffi::OsString,
    fs,
    mem::{size_of, MaybeUninit},
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

// Darwin's public <sys/proc_info.h>. libc supplies the nested vnode structures,
// but not these two wrappers or PROC_PIDFDVNODEPATHINFO.
// https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/proc_info.h
#[repr(C)]
struct ProcFileInfo {
    open_flags: u32,
    status: u32,
    offset: libc::off_t,
    kind: i32,
    guard_flags: u32,
}

#[repr(C)]
struct VnodeFdInfoWithPath {
    file: ProcFileInfo,
    vnode: libc::vnode_info_path,
}
const PROC_PIDFDVNODEPATHINFO: i32 = 2;

#[repr(C)]
struct PipeInfo {
    stat: libc::vinfo_stat,
    handle: u64,
    peer: u64,
    status: i32,
    reserved: i32,
}
#[repr(C)]
struct PipeFdInfo {
    file: ProcFileInfo,
    pipe: PipeInfo,
}

fn pipe_info(pid: i32, fd: i32) -> Option<PipeFdInfo> {
    let mut info = MaybeUninit::<PipeFdInfo>::uninit();
    // SAFETY: the buffer matches pipe_fdinfo from Apple's proc_info.h and is
    // writable for exactly the provided size; flavor 6 is PROC_PIDFDPIPEINFO.
    let bytes = unsafe {
        libc::proc_pidfdinfo(
            pid,
            fd,
            6,
            info.as_mut_ptr().cast(),
            size_of::<PipeFdInfo>() as i32,
        )
    };
    if bytes as usize != size_of::<PipeFdInfo>() {
        return None;
    }
    // SAFETY: a complete successful response initialized every field.
    Some(unsafe { info.assume_init() })
}

fn pipeline_readers(pids: &[i32]) -> HashSet<i32> {
    let mut pipes: HashSet<_> = [1, 2]
        .into_iter()
        .filter_map(|fd| pipe_info(std::process::id() as i32, fd))
        .map(|info| info.pipe.handle)
        .filter(|handle| *handle != 0)
        .collect();
    let mut readers = HashSet::new();
    loop {
        let before = readers.len();
        for &pid in pids {
            if !readers.contains(&pid)
                && same_process_group(pid as u32)
                && pipe_info(pid, 0).is_some_and(|input| pipes.contains(&input.pipe.peer))
            {
                readers.insert(pid);
                pipes.extend(
                    [1, 2]
                        .into_iter()
                        .filter_map(|fd| pipe_info(pid, fd))
                        .map(|info| info.pipe.handle)
                        .filter(|handle| *handle != 0),
                );
            }
        }
        if before == readers.len() {
            return readers;
        }
    }
}

pub(super) fn processes_using(root: &Path, skip_ancestors: bool) -> Result<Vec<WorkspaceProcess>> {
    let started = std::time::Instant::now();
    let executable = fs::metadata(std::env::current_exe()?)?;
    loop {
        let (mut found, readers) = scan_processes(root, skip_ancestors, &executable)?;
        if !found.is_empty() {
            crate::state::checkpoint("workspace-processes-scanned");
        }
        if found.is_empty() && super::retry_observed_readers(&readers, started) {
            continue;
        }
        // Match Linux: admitted inspections hold shared execution locks. Git
        // discovery can briefly precede that admission, so wait for children of
        // this exact executable, without exempting any observed child writer.
        if !skip_ancestors
            || found.is_empty()
            || started.elapsed() >= std::time::Duration::from_millis(500)
            || !found.iter().all(|process| {
                let parent = parent_of(process.pid);
                let parent_executable = parent.and_then(|pid| executable_path(pid as i32));
                let groove_child = parent_executable
                    .as_ref()
                    .and_then(|path| fs::metadata(path).ok())
                    .is_some_and(|metadata| same_executable(&metadata, &executable));
                tracing::debug!(
                    pid = process.pid,
                    ?parent,
                    ?parent_executable,
                    groove_child,
                    "Classifying a pre-admission workspace process"
                );
                groove_child
            })
        {
            tracing::debug!(elapsed = ?started.elapsed(), count = found.len(),
                "Workspace process scan completed");
            super::retain_live_observations(&mut found);
            return Ok(found);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn same_executable(first: &fs::Metadata, second: &fs::Metadata) -> bool {
    first.dev() == second.dev() && first.ino() == second.ino()
}

fn scan_processes(
    root: &Path,
    skip_ancestors: bool,
    executable: &fs::Metadata,
) -> Result<(Vec<WorkspaceProcess>, Vec<u32>)> {
    let skip = if skip_ancestors {
        self_and_ancestors()
    } else {
        Default::default()
    };
    let candidates = pids()?;
    let readers = if skip_ancestors {
        pipeline_readers(&candidates)
    } else {
        HashSet::new()
    };
    let mut found = Vec::new();
    let mut observed_readers = Vec::new();
    for pid in candidates {
        if pid <= 1 || skip.contains(&(pid as u32)) {
            continue;
        }
        if skip_ancestors
            && executable_path(pid)
                .and_then(|path| fs::metadata(path).ok())
                .is_some_and(|metadata| same_executable(&metadata, executable))
        {
            continue;
        }
        let pipeline = readers.contains(&pid);
        let mut observed_reader = false;
        if let Some(uses) = process_use(pid, root, pipeline, &mut observed_reader) {
            let command = bsd_info(pid)
                .map(|info| {
                    let bytes: Vec<u8> = info
                        .pbi_comm
                        .iter()
                        .take_while(|&&c| c != 0)
                        .map(|&c| c as u8)
                        .collect();
                    String::from_utf8_lossy(&bytes).into_owned()
                })
                .unwrap_or_default();
            found.push(WorkspaceProcess {
                pid: pid as u32,
                command,
                uses,
            });
        } else if observed_reader {
            observed_readers.push(pid as u32);
        }
    }
    found.sort_by_key(|p| p.pid);
    Ok((found, observed_readers))
}

fn pids() -> std::io::Result<Vec<libc::pid_t>> {
    // SAFETY: a null buffer requests a count without writing memory.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut capacity = count as usize + 128;
    for _ in 0..3 {
        let mut pids = vec![0; capacity];
        // SAFETY: pids is initialized writable pid_t storage of the supplied
        // byte size. proc_listallpids returns an element count, not bytes.
        let count = unsafe {
            libc::proc_listallpids(
                pids.as_mut_ptr().cast(),
                std::mem::size_of_val(pids.as_slice()) as i32,
            )
        };
        if count <= 0 {
            return Err(std::io::Error::last_os_error());
        }
        if (count as usize) < pids.len() {
            pids.truncate(count as usize);
            return Ok(pids);
        }
        capacity *= 2;
    }
    Err(std::io::Error::other(
        "Process list kept growing during workspace inspection; retry after stopping writers",
    ))
}

fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    read_bsd_info(pid, false).ok()
}

// PROC_PIDTBSDINFO's nonzero argument includes unreaped children. Without it,
// XNU returns ESRCH for zombies even while kill(pid, 0) still succeeds.
// https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c
fn read_bsd_info(pid: i32, include_zombies: bool) -> std::io::Result<libc::proc_bsdinfo> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::uninit();
    // SAFETY: info has the exact layout/size required by PROC_PIDTBSDINFO.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            u64::from(include_zombies),
            info.as_mut_ptr().cast(),
            size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if bytes as usize != size_of::<libc::proc_bsdinfo>() {
        return Err(if bytes <= 0 {
            std::io::Error::last_os_error()
        } else {
            std::io::Error::other("Incomplete process information")
        });
    }
    // SAFETY: a complete successful response initialized every field.
    Ok(unsafe { info.assume_init() })
}

/// A zombie or OS-confirmed exited process no longer owns workspace resources.
pub(super) fn has_exited(pid: u32) -> bool {
    i32::try_from(pid).ok().is_some_and(|pid| {
        match read_bsd_info(pid, true) {
            Ok(info) => info.pbi_status == libc::SZOMB,
            // It may have been reaped since the preceding signal-zero probe.
            Err(error) => error.raw_os_error() == Some(libc::ESRCH),
        }
    })
}

pub(super) fn parent_of(pid: u32) -> Option<u32> {
    bsd_info(i32::try_from(pid).ok()?).map(|info| info.pbi_ppid)
}

fn vnode_path(info: &libc::vnode_info_path) -> PathBuf {
    let bytes: Vec<u8> = info
        .vip_path
        .as_flattened()
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    PathBuf::from(OsString::from_vec(bytes))
}

fn process_use(
    pid: i32,
    root: &Path,
    pipeline: bool,
    observed_reader: &mut bool,
) -> Option<ProcessUse> {
    let mut cwd_outside = false;
    let mut cwd = MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    // SAFETY: cwd has the exact layout/size required by PROC_PIDVNODEPATHINFO.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            cwd.as_mut_ptr().cast(),
            size_of::<libc::proc_vnodepathinfo>() as i32,
        )
    };
    if bytes as usize == size_of::<libc::proc_vnodepathinfo>() {
        // SAFETY: a complete successful response initialized both vnode paths.
        let cwd = unsafe { cwd.assume_init() };
        if !pipeline && vnode_path(&cwd.pvi_cdir).starts_with(root) {
            return Some(ProcessUse::WorkingDirectory);
        }
        let path = vnode_path(&cwd.pvi_cdir);
        cwd_outside = path.is_absolute() && !path.starts_with(root);
    }
    let executable = executable_path(pid);
    if executable
        .as_ref()
        .is_some_and(|path| path.starts_with(root))
    {
        return Some(ProcessUse::Executable);
    }
    let outside =
        cwd_outside && executable.is_some_and(|path| path.is_absolute() && !path.starts_with(root));
    open_file_use(pid, root, outside, observed_reader)
}

fn executable_path(pid: i32) -> Option<PathBuf> {
    let mut executable = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: executable is writable storage of the supplied byte size.
    let bytes =
        unsafe { libc::proc_pidpath(pid, executable.as_mut_ptr().cast(), executable.len() as u32) };
    if bytes > 0 {
        executable.truncate(
            executable
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(executable.len()),
        );
        return Some(PathBuf::from(OsString::from_vec(executable)));
    }
    None
}

fn open_file_use(
    pid: i32,
    root: &Path,
    outside: bool,
    observed_reader: &mut bool,
) -> Option<ProcessUse> {
    // SAFETY: a null buffer requests the number of bytes needed for descriptors.
    let bytes =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return None;
    }
    let count = bytes as usize / size_of::<libc::proc_fdinfo>() + 32;
    let mut fds: Vec<_> = (0..count)
        .map(|_| libc::proc_fdinfo {
            proc_fd: -1,
            proc_fdtype: 0,
        })
        .collect();
    // SAFETY: fds is initialized writable proc_fdinfo storage of the supplied size.
    let bytes = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr().cast(),
            std::mem::size_of_val(fds.as_slice()) as i32,
        )
    };
    if bytes <= 0 {
        return None;
    }
    let count = (bytes as usize / size_of::<libc::proc_fdinfo>()).min(fds.len());
    for fd in &fds[..count] {
        if fd.proc_fdtype != libc::PROX_FDTYPE_VNODE as u32 {
            continue;
        }
        let mut info = MaybeUninit::<VnodeFdInfoWithPath>::uninit();
        // SAFETY: info matches vnode_fdinfowithpath in Darwin's proc_info.h;
        // proc_pidfdinfo is given exactly its writable size. Exited processes,
        // closed descriptors and denied access are skipped as best-effort observations.
        let bytes = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                info.as_mut_ptr().cast(),
                size_of::<VnodeFdInfoWithPath>() as i32,
            )
        };
        if bytes as usize == size_of::<VnodeFdInfoWithPath>() {
            // SAFETY: a complete successful response initialized all fields.
            let info = unsafe { info.assume_init() };
            if vnode_path(&info.vnode).starts_with(root) {
                // proc_info.c exposes fg_flag, not POSIX O_ACCMODE. Darwin's
                // sys/fcntl.h defines the independently testable FWRITE bit as 2.
                const FWRITE: u32 = 2;
                if outside && info.file.open_flags & FWRITE == 0 {
                    *observed_reader = true;
                } else {
                    return Some(ProcessUse::OpenFile);
                }
            }
        }
    }
    None
}
