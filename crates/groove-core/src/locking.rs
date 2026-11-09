use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::{AsRawFd, BorrowedFd},
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::Path,
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Identity {
    pub dev: u64,
    pub ino: u64,
}
pub(crate) fn identity(file: &File) -> Result<Identity> {
    let m = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !m.is_file() || m.nlink() != 1 || m.mode() & 0o022 != 0 || m.uid() != uid {
        return Err(Error::new(
            crate::ErrorCode::UnsupportedConfiguration,
            "Lock must be a regular, singly linked file owned by the current user",
        ));
    }
    Ok(Identity {
        dev: m.dev(),
        ino: m.ino(),
    })
}
pub(crate) fn open(path: &Path, create: bool) -> Result<File> {
    crate::repository::no_symlinks(path)?;
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    identity(&f)?;
    Ok(f)
}
pub(crate) fn flock(fd: BorrowedFd<'_>, op: i32) -> std::io::Result<bool> {
    loop {
        // SAFETY: fd is borrowed and valid for the syscall.
        if unsafe { libc::flock(fd.as_raw_fd(), op) } == 0 {
            return Ok(true);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if e.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(false);
        }
        return Err(e);
    }
}

#[derive(Debug)]
pub(crate) struct SlotGuard {
    pub file: File,
}
impl SlotGuard {
    pub fn try_lock(path: &Path, expected: Option<Identity>) -> Result<Option<Self>> {
        use std::os::fd::AsFd;
        let file = open(path, false)?;
        if expected.is_some_and(|e| identity(&file).ok() != Some(e)) {
            return Err(Error::new(
                crate::ErrorCode::IoError,
                "Permanent lock identity changed",
            ));
        }
        if !flock(file.as_fd(), libc::LOCK_EX | libc::LOCK_NB)? {
            return Ok(None);
        }
        let current = std::fs::symlink_metadata(path)?;
        let id = identity(&file)?;
        if current.dev() != id.dev || current.ino() != id.ino || !current.is_file() {
            return Err(Error::new(
                crate::ErrorCode::IoError,
                "Lock pathname changed during acquisition",
            ));
        }
        Ok(Some(Self { file }))
    }
    pub fn wait(path: &Path, expected: Option<Identity>, timeout: Duration) -> Result<Self> {
        let start = Instant::now();
        loop {
            if let Some(lock) = Self::try_lock(path, expected)? {
                return Ok(lock);
            }
            if start.elapsed() >= timeout {
                return Err(Error::new(
                    crate::ErrorCode::IoError,
                    "Administrative lock wait timed out",
                ));
            }
            std::thread::sleep(
                Duration::from_millis(5).min(timeout.saturating_sub(start.elapsed())),
            );
        }
    }
}
