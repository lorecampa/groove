use crate::{Error, ErrorCode, Result};
#[cfg(target_os = "linux")]
use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};
use std::{
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    time::Duration,
};

const STOP_REQUEST: &[u8; 5] = b"stop\n";
const STOP_ACCEPTED: &[u8; 3] = b"ok\n";

#[cfg(target_os = "linux")]
fn same_user(stream: &UnixStream) -> Result<bool> {
    // SAFETY: getsockopt initializes this POD credential structure and its length.
    let mut credentials: libc::ucred = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&credentials) as libc::socklen_t;
    // SAFETY: stream is live; output pointers and the supplied size match SO_PEERCRED.
    if unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: geteuid has no preconditions.
    Ok(length as usize == std::mem::size_of_val(&credentials)
        && credentials.uid == unsafe { libc::geteuid() })
}

#[cfg(target_os = "macos")]
fn same_user(stream: &UnixStream) -> Result<bool> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: stream is connected and both credential outputs are writable.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: geteuid has no preconditions.
    Ok(uid == unsafe { libc::geteuid() })
}

/// A supervisor's local Unix control socket. Only same-user peers that send
/// [`STOP_REQUEST`] can cancel; no PID is ever signalled from metadata.
pub(crate) struct StopListener {
    listener: UnixListener,
    #[cfg(target_os = "macos")]
    _endpoint: SocketPath,
}
impl StopListener {
    pub fn bind(name: &str) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let listener = UnixListener::bind_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
        #[cfg(target_os = "macos")]
        let (listener, endpoint) = SocketPath::bind(name)?;
        let result = Self {
            listener,
            #[cfg(target_os = "macos")]
            _endpoint: endpoint,
        };
        result.listener.set_nonblocking(true)?;
        Ok(result)
    }
    /// Accept at most one pending connection and report whether it asked to stop.
    pub fn stop_requested(&self) -> Result<bool> {
        let Ok((mut stream, _)) = self.listener.accept() else {
            return Ok(false);
        };
        // Accepted streams inherit O_NONBLOCK on Darwin. The request read must
        // instead wait up to its bounded timeout, including when accept wins
        // the race with the client's first write.
        if stream.set_nonblocking(false).is_err() {
            return Ok(false);
        }
        match same_user(&stream) {
            Ok(true) => (),
            Ok(false) => return Ok(false),
            Err(error) => {
                // A client can disappear before accept/authentication. Reject
                // that connection without disrupting the supervised worker.
                tracing::debug!(%error, "Rejected unverifiable stop client");
                return Ok(false);
            }
        }
        if stream
            .set_read_timeout(Some(Duration::from_millis(100)))
            .is_err()
        {
            return Ok(false);
        }
        let mut request = [0; STOP_REQUEST.len()];
        if stream.read_exact(&mut request).is_err() || request != *STOP_REQUEST {
            return Ok(false);
        }
        if stream
            .set_write_timeout(Some(Duration::from_millis(100)))
            .is_err()
        {
            return Ok(true);
        }
        // The request is authenticated and accepted even if its caller exits
        // before reading the acknowledgement. Never undo cancellation on EPIPE.
        let _ = stream.write_all(STOP_ACCEPTED);
        Ok(true)
    }
}

/// Ask the live supervisor listening on `name` to stop. `unavailable` explains
/// what the operator should do when no supervisor answers.
pub(crate) fn send_stop(name: &str, unavailable: &'static str) -> Result<()> {
    let mut stream = connect(name)
        .map_err(|e| Error::new(ErrorCode::InvalidLease, unavailable).with_source(e))?;
    if !same_user(&stream)? {
        return Err(Error::new(
            ErrorCode::InvalidLease,
            "Supervisor has a different user identity",
        ));
    }
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    stream.write_all(STOP_REQUEST)?;
    // Keep the peer connected until the supervisor authenticates it. Darwin
    // can no longer retrieve credentials after a queued client disconnects.
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    let mut accepted = [0; STOP_ACCEPTED.len()];
    stream
        .read_exact(&mut accepted)
        .map_err(|error| Error::new(ErrorCode::InvalidLease, unavailable).with_source(error))?;
    if accepted != *STOP_ACCEPTED {
        return Err(Error::new(
            ErrorCode::InvalidLease,
            "Invalid supervisor acknowledgement",
        ));
    }
    Ok(())
}

fn connect(name: &str) -> Result<UnixStream> {
    #[cfg(target_os = "linux")]
    let stream = UnixStream::connect_addr(&SocketAddr::from_abstract_name(name.as_bytes())?)?;
    #[cfg(target_os = "macos")]
    let stream = UnixStream::connect(endpoint_path(name)?)?;
    Ok(stream)
}

#[cfg(target_os = "macos")]
fn endpoint_path(name: &str) -> Result<std::path::PathBuf> {
    use sha2::{Digest, Sha256};
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, MetadataExt},
        path::PathBuf,
    };
    // A fixed, short path avoids Darwin's small sun_path limit even when the
    // repository or TMPDIR has a long name. /tmp itself is a symlink on macOS.
    // SAFETY: geteuid has no preconditions.
    let directory = PathBuf::from(format!("/private/tmp/groove-{}", unsafe {
        libc::geteuid()
    }));
    match fs::DirBuilder::new().mode(0o700).create(&directory) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    crate::repository::owned_dir(&directory)?;
    if fs::symlink_metadata(&directory)?.mode() & 0o077 != 0 {
        return Err(Error::new(
            ErrorCode::UnsupportedConfiguration,
            "The Groove control socket directory must be private (mode 0700)",
        ));
    }
    let digest = Sha256::digest(name.as_bytes());
    let short: String = digest[..16].iter().map(|b| format!("{b:02x}")).collect();
    Ok(directory.join(format!("{short}.sock")))
}

#[cfg(target_os = "macos")]
struct SocketPath {
    path: std::path::PathBuf,
    dev: u64,
    ino: u64,
}

#[cfg(target_os = "macos")]
impl SocketPath {
    fn bind(name: &str) -> Result<(UnixListener, Self)> {
        use std::os::unix::fs::MetadataExt;
        let path = endpoint_path(name)?;
        // Never remove an existing endpoint to make bind succeed. Each run has
        // a fresh UUID; an occupied path must fail before the worker is spawned.
        let listener = UnixListener::bind(&path)?;
        let metadata = std::fs::symlink_metadata(&path)?;
        let endpoint = Self {
            path,
            dev: metadata.dev(),
            ino: metadata.ino(),
        };
        Ok((listener, endpoint))
    }
}

#[cfg(target_os = "macos")]
impl Drop for SocketPath {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        if let Ok(metadata) = std::fs::symlink_metadata(&self.path) {
            if metadata.file_type().is_socket()
                && metadata.dev() == self.dev
                && metadata.ino() == self.ino
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Called only under execution exclusion after validating an orphan's record,
/// or during an explicit setup retry after obtaining the same exclusion.
pub(crate) fn cleanup_endpoint(name: &str) -> Result<()> {
    #[cfg(target_os = "linux")]
    let _ = name; // Abstract endpoints have no filesystem entry to clean up.
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::{FileTypeExt, MetadataExt};
        let path = endpoint_path(name)?;
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
            Ok(metadata) => {
                // SAFETY: geteuid has no preconditions.
                let uid = unsafe { libc::geteuid() };
                if !metadata.file_type().is_socket() || metadata.uid() != uid {
                    return Err(Error::new(
                        ErrorCode::UnsupportedConfiguration,
                        "Refusing to remove an unexpected control endpoint",
                    ));
                }
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stop(listener: &StopListener, name: &str) {
        let name = name.to_owned();
        let caller = std::thread::spawn(move || send_stop(&name, "unavailable"));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !listener.stop_requested().unwrap() {
            assert!(
                std::time::Instant::now() < deadline,
                "stop request did not arrive"
            );
            std::thread::yield_now();
        }
        caller.join().unwrap().unwrap();
    }

    #[test]
    fn stop_authenticates_both_peers_and_requires_the_stop_message() {
        // Other unit tests fork: isolate the assertion about last descriptor
        // closure instead of assuming CLOEXEC has already run in every child.
        if !crate::test_support::in_subprocess(
            "platform::control::tests::stop_authenticates_both_peers_and_requires_the_stop_message",
        ) {
            return;
        }
        let name = format!("groove-test-{}", uuid::Uuid::new_v4());
        let listener = StopListener::bind(&name).unwrap();
        assert!(!listener.stop_requested().unwrap());
        let mut invalid = connect(&name).unwrap();
        assert!(same_user(&invalid).unwrap());
        invalid.write_all(b"nope\n").unwrap();
        assert!(!listener.stop_requested().unwrap());
        drop(invalid);
        stop(&listener, &name);
        drop(listener);
        assert!(send_stop(&name, "unavailable").is_err());
    }

    #[test]
    fn occupied_endpoint_is_preserved_and_can_still_receive_stop() {
        let name = format!("groove-test-{}", uuid::Uuid::new_v4());
        let listener = StopListener::bind(&name).unwrap();
        assert!(StopListener::bind(&name).is_err());
        stop(&listener, &name);
    }

    #[test]
    fn disconnected_client_cannot_interrupt_supervision_or_disable_later_stop() {
        let name = format!("groove-test-{}", uuid::Uuid::new_v4());
        let listener = StopListener::bind(&name).unwrap();
        let mut client = connect(&name).unwrap();
        client.write_all(b"nope\n").unwrap();
        drop(client);
        assert!(!listener.stop_requested().unwrap());
        stop(&listener, &name);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn long_control_names_fit_and_drop_preserves_replaced_files() {
        let name = format!("groove-{}-{}", uuid::Uuid::new_v4(), "x".repeat(300));
        let listener = StopListener::bind(&name).unwrap();
        stop(&listener, &name);
        let path = endpoint_path(&name).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"keep").unwrap();
        drop(listener);
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        assert_eq!(
            cleanup_endpoint(&name).unwrap_err().code,
            ErrorCode::UnsupportedConfiguration
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        std::fs::remove_file(path).unwrap();
    }
}
