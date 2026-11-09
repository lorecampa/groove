use groove_core::{transport, Error, Result};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

pub fn duplicate(raw: i32) -> Result<OwnedFd> {
    // SAFETY: fcntl checks validity before constructing a borrowed descriptor;
    // this process is single-threaded until after arguments are duplicated.
    if unsafe { libc::fcntl(raw, libc::F_GETFD) } < 0 {
        return Err(Error::new(
            groove_core::ErrorCode::ProtocolError,
            "Inherited descriptor is not open",
        ));
    }
    // SAFETY: the descriptor was validated and remains open throughout duplication.
    let fd = unsafe { BorrowedFd::borrow_raw(raw) };
    // SAFETY: raw is validated above and remains owned by the single-threaded CLI.
    // Mark the original close-on-exec too, so Git cannot inherit the transport.
    if unsafe { libc::fcntl(raw, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(fd.try_clone_to_owned()?)
}
pub fn channel(raw: i32) -> Result<OwnedFd> {
    let fd = duplicate(raw)?;
    transport::validate_channel(fd.as_fd())?;
    Ok(fd)
}
