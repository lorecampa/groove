use crate::Result;

/// Whether `child` has exited, without reaping it.
///
/// The leader stays a zombie, so its PID and process-group ID cannot be reused
/// by an unrelated process while the caller still signals the group.
pub(crate) fn exited(child: &std::process::Child) -> Result<bool> {
    // Keep the leader unreaped until signalling is complete, preventing PID/PGID reuse.
    // SAFETY: info is valid output storage for waitid and child is our unreaped child.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: child.id identifies our child; WNOWAIT leaves its identity reserved.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            child.id(),
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result < 0 {
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(e.into());
    }
    #[cfg(target_os = "linux")]
    // SAFETY: waitid initialized info; zero si_pid means no exited child.
    let pid = unsafe { info.si_pid() };
    #[cfg(target_os = "macos")]
    let pid = info.si_pid;
    Ok(pid != 0)
}
