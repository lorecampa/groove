//! Linux SOCK_SEQPACKET + SCM_RIGHTS lease transport, shared by the CLI and Rust clients.
use crate::{Error, Result};
use serde_json::Value;
use std::{
    mem::{self, MaybeUninit},
    os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
};

/// Maximum encoded JSON size in one lease transport packet (64 KiB).
pub const MAX_RECORD: usize = 65536;
fn protocol(message: &str) -> Error {
    Error::new(crate::ErrorCode::ProtocolError, message)
}

/// Create a connected pair of close-on-exec Unix sequence-packet sockets.
///
/// # Errors
/// Returns an OS error if socket creation fails.
pub fn channel() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1; 2];
    // SAFETY: fds is writable storage for two descriptors; on success we own both.
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
            0,
            fds.as_mut_ptr(),
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a successful socketpair returned two new, uniquely owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}
/// Check that a descriptor identifies a connected Unix sequence-packet socket.
///
/// # Errors
/// Returns a protocol error for an incompatible or disconnected socket.
pub fn validate_channel(fd: BorrowedFd<'_>) -> Result<()> {
    let mut kind: libc::c_int = 0;
    let mut size = mem::size_of_val(&kind) as libc::socklen_t;
    // SAFETY: kind and size are writable values of the types expected by getsockopt.
    if unsafe {
        libc::getsockopt(
            fd.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut libc::c_int).cast(),
            &mut size,
        )
    } != 0
        || kind != libc::SOCK_SEQPACKET
    {
        return Err(protocol(
            "Lease channel must be a connected Unix SOCK_SEQPACKET socket",
        ));
    }
    let mut address = MaybeUninit::<libc::sockaddr_storage>::zeroed();
    let mut size = mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
    // SAFETY: address and size are valid writable storage; success initializes the address.
    if unsafe { libc::getpeername(fd.as_raw_fd(), address.as_mut_ptr().cast(), &mut size) } != 0 {
        return Err(protocol("Lease channel is not connected"));
    }
    // SAFETY: successful getpeername initialized address including ss_family.
    if unsafe { address.assume_init() }.ss_family as i32 != libc::AF_UNIX {
        return Err(protocol("Lease channel must use AF_UNIX"));
    }
    Ok(())
}
/// Atomically send one JSON record and one descriptor without blocking.
///
/// # Errors
/// Rejects incompatible channels, oversized records, encoding failures, and failed
/// sends (including a full socket buffer). The caller retains its descriptor.
pub fn send_record(
    channel: BorrowedFd<'_>,
    record: &Value,
    descriptor: BorrowedFd<'_>,
) -> Result<()> {
    validate_channel(channel)?;
    let data = serde_json::to_vec(record)?;
    if data.len() > MAX_RECORD {
        return Err(protocol("Lease record exceeds 64 KiB"));
    }
    // Ancillary storage must be aligned for cmsghdr; usize alignment suffices on Linux.
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    // SAFETY: an all-zero msghdr is valid before its pointers and lengths are populated.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    // SAFETY: CMSG_SPACE computes storage requirements for one integer payload.
    msg.msg_controllen = unsafe { libc::CMSG_SPACE(mem::size_of::<i32>() as u32) } as usize;
    // SAFETY: control is aligned and large enough for the header and one descriptor.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&msg);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len = libc::CMSG_LEN(mem::size_of::<i32>() as u32) as usize;
        std::ptr::write_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>(), descriptor.as_raw_fd());
    }
    loop {
        // SAFETY: all msg buffers are valid for this synchronous sendmsg call.
        let sent = unsafe {
            libc::sendmsg(
                channel.as_raw_fd(),
                &msg,
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if sent == data.len() as isize {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if sent < 0 && e.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(protocol("Unable to transfer complete lease record")
            .details(serde_json::json!({"cause": e.to_string()})));
    }
}
/// Receives exactly one response and descriptor. Every installed descriptor is
/// RAII-owned before parsing; malformed/truncated messages cannot leak locks.
/// This blocks unless the caller configures a socket timeout or nonblocking mode.
///
/// # Errors
/// Rejects malformed responses, mismatched request IDs, invalid descriptor counts,
/// EOF and socket I/O failures. Received descriptors are closed on every error.
pub fn receive_record(
    channel: BorrowedFd<'_>,
    request_id: Option<&str>,
) -> Result<(Value, OwnedFd)> {
    validate_channel(channel)?;
    let mut data = vec![0u8; MAX_RECORD];
    let mut control = [0usize; 8];
    let mut iov = libc::iovec {
        iov_base: data.as_mut_ptr().cast(),
        iov_len: data.len(),
    };
    // SAFETY: a zero msghdr is valid; buffers are populated before use.
    let mut msg: libc::msghdr = unsafe { mem::zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = control.as_mut_ptr().cast();
    msg.msg_controllen = mem::size_of_val(&control);
    let n = loop {
        // SAFETY: all buffers are writable and live throughout recvmsg. CLOEXEC is atomic.
        let n = unsafe { libc::recvmsg(channel.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n >= 0 {
            break n as usize;
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e.into());
        }
    };
    let mut fds = Vec::new();
    // SAFETY: recvmsg populated kernel-validated headers within control. CMSG_NXTHDR
    // bounds each traversal; SCM_RIGHTS descriptors are newly owned by this receiver.
    unsafe {
        let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
        while !cmsg.is_null() {
            if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                let header = libc::CMSG_LEN(0) as usize;
                let count = (*cmsg).cmsg_len.saturating_sub(header) / mem::size_of::<i32>();
                for i in 0..count {
                    let fd = std::ptr::read_unaligned(libc::CMSG_DATA(cmsg).cast::<i32>().add(i));
                    fds.push(OwnedFd::from_raw_fd(fd));
                }
            }
            cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
        }
    }
    if n == 0 || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0 || fds.len() != 1 {
        return Err(protocol(
            "Missing, extra, or truncated lease descriptors/record",
        ));
    }
    let record: Value = serde_json::from_slice(&data[..n])?;
    if record["api_version"] != 1
        || record["ok"] != true
        || record["request_id"].as_str() != request_id
        || (request_id.is_none() && !record["request_id"].is_null())
    {
        return Err(protocol(
            "Lease response version, success flag or request ID mismatch",
        ));
    }
    let info: crate::LeaseInfo = serde_json::from_value(record["result"].clone())?;
    if info.descriptor_count != 1
        || info.lease_transport != crate::LeaseTransport::ScmRights
        || info.head_mode != crate::HeadMode::Detached
        || !info.worktree_path.is_absolute()
    {
        return Err(protocol("Invalid acquisition result"));
    }
    Ok((record, fds.pop().expect("exactly one descriptor")))
}
/// Arrange inheritance for a specific subprocess; the parent's descriptor keeps
/// close-on-exec. The descriptor must outlive command spawning.
///
/// # Safety
/// `fd` must remain open and identify the same resource until `command.spawn()`
/// returns. Do not reuse the command after closing that descriptor.
pub unsafe fn inherit_fd(command: &mut std::process::Command, fd: BorrowedFd<'_>) {
    use std::os::unix::process::CommandExt;
    let raw = fd.as_raw_fd();
    // SAFETY: the caller guarantees fd validity at spawn; only fcntl runs after fork.
    unsafe {
        command.pre_exec(move || {
            let flags = libc::fcntl(raw, libc::F_GETFD);
            if flags < 0 || libc::fcntl(raw, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}
