use crate::Result;

/// Observe the group only after all signalling is complete. No metadata PID
/// authorizes signalling, and enumeration failures never authorize reuse.
pub(crate) fn group_exists(pid: u32) -> Result<bool> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: signal zero probes existence without delivering a signal.
        let result = unsafe { libc::kill(-(pid as i32), 0) };
        Ok(result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH))
    }
    #[cfg(target_os = "macos")]
    {
        let mut member: libc::pid_t = 0;
        // Darwin killpg(0) may skip zombie members. libproc enumerates both live
        // and zombie members; one output element suffices to prove non-emptiness.
        // Its wrapper returns zero for both an empty list and a syscall failure,
        // so clear and check this thread's errno to distinguish them.
        // SAFETY: __error returns this thread's errno storage. member is writable
        // pid_t storage of the supplied size; the group is only being observed.
        let (count, error) = unsafe {
            *libc::__error() = 0;
            let count = libc::proc_listpgrppids(
                pid as i32,
                (&mut member as *mut libc::pid_t).cast(),
                std::mem::size_of_val(&member) as i32,
            );
            (count, *libc::__error())
        };
        if count == 0 && error != 0 {
            return Err(std::io::Error::from_raw_os_error(error).into());
        }
        Ok(count > 0)
    }
}

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
