use crate::Result;
#[cfg(target_os = "linux")]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

/// Block cancellation signals before starting other threads in a managed CLI invocation.
/// The calling thread must retain this guard until execution completes.
/// Embeddings must arrange for other threads to block these signals too, and must
/// not concurrently consume SIGINT/SIGTERM through their own signal waiters.
pub struct ExecutionSignals {
    previous: libc::sigset_t,
    #[cfg(target_os = "linux")]
    fd: OwnedFd,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl std::fmt::Debug for ExecutionSignals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionSignals").finish_non_exhaustive()
    }
}
impl ExecutionSignals {
    /// Enable Linux child-subreaper behavior in a dedicated supervisor executable.
    /// Do not enable this implicitly in embedding applications; it changes process-wide adoption.
    /// On macOS this leaves orphan adoption to the system reaper. Both platforms
    /// require the execution's process group to disappear before clearing protection.
    ///
    /// # Errors
    /// Reports prctl failure on Linux.
    pub fn enable_subreaper() -> Result<()> {
        #[cfg(target_os = "linux")]
        // SAFETY: PR_SET_CHILD_SUBREAPER accepts a boolean and has no pointer arguments.
        if unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    /// Block SIGINT/SIGTERM for cancellation without installing signal handlers.
    /// Linux uses signalfd; macOS consumes pending signals using sigwait.
    ///
    /// # Errors
    /// Reports signal-mask or signalfd failures. Restore occurs on the same thread at drop.
    pub fn block() -> Result<Self> {
        let mask = cancellation_mask();
        // SAFETY: previous is writable output for pthread_sigmask.
        let mut previous: libc::sigset_t = unsafe { std::mem::zeroed() };
        // SAFETY: both masks point to valid storage for this calling thread.
        let error = unsafe { libc::pthread_sigmask(libc::SIG_BLOCK, &mask, &mut previous) };
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error).into());
        }
        #[cfg(target_os = "linux")]
        let fd = {
            // SAFETY: mask is initialized; -1 requests a new descriptor.
            let fd = unsafe { libc::signalfd(-1, &mask, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC) };
            if fd < 0 {
                let error = std::io::Error::last_os_error();
                // SAFETY: restore the mask captured from this same thread.
                unsafe {
                    libc::pthread_sigmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
                }
                return Err(error.into());
            }
            // SAFETY: signalfd returned a new owned descriptor.
            unsafe { OwnedFd::from_raw_fd(fd) }
        };
        Ok(Self {
            previous,
            #[cfg(target_os = "linux")]
            fd,
            _thread: std::marker::PhantomData,
        })
    }
    #[cfg(target_os = "linux")]
    pub(crate) fn poll(&self) -> Result<Option<i32>> {
        // SAFETY: signalfd initializes this POD structure on a successful full read.
        let mut info: libc::signalfd_siginfo = unsafe { std::mem::zeroed() };
        // SAFETY: fd is owned and the output region has the exact required size.
        let n = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                (&mut info as *mut libc::signalfd_siginfo).cast(),
                std::mem::size_of_val(&info),
            )
        };
        if n >= 0 {
            return Ok((n > 0).then_some(info.ssi_signo as i32));
        }
        let error = std::io::Error::last_os_error();
        if matches!(
            error.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
        ) {
            Ok(None)
        } else {
            Err(error.into())
        }
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn poll(&self) -> Result<Option<i32>> {
        // Setup control and command supervision can poll on different threads.
        // Serialize the pending check and consumption so neither can steal the
        // observed signal and leave the other's sigwait blocked. No handlers or
        // additional threads are installed. Callers must not use competing waiters.
        static POLL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _poll = POLL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut pending = 0;
        // SAFETY: pending is writable sigset_t storage on Darwin.
        if unsafe { libc::sigpending(&mut pending) } != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        for signal in [libc::SIGINT, libc::SIGTERM] {
            // SAFETY: pending is initialized and signal is valid.
            if unsafe { libc::sigismember(&pending, signal) } == 1 {
                let mut mask = 0;
                // SAFETY: mask is writable sigset_t storage, and signal is valid.
                unsafe {
                    libc::sigemptyset(&mut mask);
                    libc::sigaddset(&mut mask, signal);
                }
                let mut received = 0;
                // SAFETY: this guard blocks signal on this thread. It is pending,
                // and the mutex excludes other Groove consumers until sigwait
                // removes it, including with nested guards on the same thread.
                let error = unsafe { libc::sigwait(&mask, &mut received) };
                if error != 0 {
                    return Err(std::io::Error::from_raw_os_error(error).into());
                }
                return Ok(Some(received));
            }
        }
        Ok(None)
    }
}
impl Drop for ExecutionSignals {
    fn drop(&mut self) {
        // SAFETY: this non-Send guard restores the mask on its creating thread.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.previous, std::ptr::null_mut());
        }
    }
}

/// SIGINT and SIGTERM, the signals Groove forwards as cancellation.
pub(crate) fn cancellation_mask() -> libc::sigset_t {
    // SAFETY: a zeroed sigset_t is valid storage, initialized by sigemptyset below.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: mask is valid, writable sigset_t storage.
    unsafe {
        libc::sigemptyset(&mut mask);
        libc::sigaddset(&mut mask, libc::SIGINT);
        libc::sigaddset(&mut mask, libc::SIGTERM);
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocked(signal: i32) -> bool {
        // SAFETY: zeroed sigset_t is valid output storage for pthread_sigmask.
        let mut mask = unsafe { std::mem::zeroed() };
        // SAFETY: a null input queries this thread's mask into writable storage.
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask) };
        assert_eq!(result, 0);
        // SAFETY: mask is initialized and signal is valid.
        unsafe { libc::sigismember(&mask, signal) == 1 }
    }

    #[test]
    fn nested_guards_consume_cancellation_and_restore_the_calling_thread_mask() {
        let previous = [blocked(libc::SIGINT), blocked(libc::SIGTERM)];
        let outer = ExecutionSignals::block().unwrap();
        {
            let inner = ExecutionSignals::block().unwrap();
            for signal in [libc::SIGINT, libc::SIGTERM] {
                assert!(blocked(signal));
                assert_eq!(inner.poll().unwrap(), None);
                // SAFETY: pthread_self is this live thread; the guard blocks
                // signal. Targeting this thread leaves libtest's runner alone.
                let result = unsafe { libc::pthread_kill(libc::pthread_self(), signal) };
                assert_eq!(result, 0);
                assert_eq!(inner.poll().unwrap(), Some(signal));
                assert_eq!(inner.poll().unwrap(), None);
            }
        }
        assert!(blocked(libc::SIGINT));
        assert!(blocked(libc::SIGTERM));
        assert_eq!(outer.poll().unwrap(), None);
        drop(outer);
        assert_eq!([blocked(libc::SIGINT), blocked(libc::SIGTERM)], previous);
    }
}
