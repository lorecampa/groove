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
