use crate::{Error, ErrorCode, Result};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::{ffi::OsStrExt, fs::OpenOptionsExt},
    },
    path::Path,
};

pub(crate) fn local_fs(path: &Path) -> Result<()> {
    let c = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| Error::new(crate::ErrorCode::InvalidArgument, "NUL in path").with_source(e))?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: c is NUL-terminated and stat is writable storage of the required size.
    if unsafe { libc::statfs(c.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: successful statfs initialized stat.
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "linux")]
    let supported = ![
        0x6969, 0xff534d42, 0xfe534d42, 0x65735546, 0x73757245, 0x564c,
    ]
    .contains(&(stat.f_type as u64));
    #[cfg(target_os = "macos")]
    let supported = {
        let name: Vec<u8> = stat
            .f_fstypename
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        stat.f_flags & libc::MNT_LOCAL as u32 != 0 && name == b"apfs"
    };
    if !supported {
        return Err(Error::new(
            ErrorCode::UnsupportedConfiguration,
            "Unsupported filesystem: Groove requires local storage (APFS on macOS); network and FUSE filesystems are unsupported",
        ));
    }
    Ok(())
}

/// Create an independent copy-on-write artifact, preserving strict reflink mode:
/// unsupported filesystems fail instead of silently falling back to byte copies.
pub(crate) fn clone_file(input: &File, destination: &Path) -> std::io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        let output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(destination)?;
        // SAFETY: both descriptors own regular files with the required access
        // modes and remain open throughout the FICLONE ioctl.
        if unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(output)
    }
    #[cfg(target_os = "macos")]
    {
        let parent = destination.parent().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Missing clone parent")
        })?;
        let name = destination.file_name().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "Missing clone filename")
        })?;
        let name = CString::new(name.as_bytes())?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(parent)?;
        // SAFETY: input owns a regular file, directory owns the destination
        // parent, and name is one NUL-terminated component. fclonefileat creates
        // a new destination and fails if it exists; it never replaces a path.
        if unsafe { libc::fclonefileat(input.as_raw_fd(), directory.as_raw_fd(), name.as_ptr(), 0) }
            != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // A clone inherits read-only modes/flags. Open it for reading first so
        // its owner can make this private destination writable without touching
        // the source, then use a writable descriptor for xattr removal and sync.
        let reader = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(destination)?;
        // SAFETY: reader owns the newly created clone; zero clears its BSD flags.
        if unsafe { libc::fchflags(reader.as_raw_fd(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        use std::os::unix::fs::PermissionsExt;
        reader.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let output = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(destination)?;
        strip_clone_xattrs(&output)?;
        Ok(output)
    }
}

#[cfg(target_os = "macos")]
fn strip_clone_xattrs(file: &File) -> std::io::Result<()> {
    // fclonefileat copies BSD flags and xattrs as well as bytes. Cache artifacts
    // promise only bytes/mode/mtime, just like Linux FICLONE. CLONE_ACL is absent:
    // the clone inherits destination-directory ACLs instead of source ACLs.
    // SAFETY: a null buffer requests the byte count of attribute names.
    let count = unsafe { libc::flistxattr(file.as_raw_fd(), std::ptr::null_mut(), 0, 0) };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut names = vec![0_u8; count as usize];
    if names.is_empty() {
        return Ok(());
    }
    // SAFETY: names is writable storage of exactly the supplied size.
    let count =
        unsafe { libc::flistxattr(file.as_raw_fd(), names.as_mut_ptr().cast(), names.len(), 0) };
    if count < 0 {
        return Err(std::io::Error::last_os_error());
    }
    names.truncate(count as usize);
    for name in names.split_inclusive(|&b| b == 0) {
        let name = std::ffi::CStr::from_bytes_with_nul(name)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        // SAFETY: file is owned and name is a NUL-terminated attribute name
        // returned for this private clone. The source is never modified.
        if unsafe { libc::fremovexattr(file.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn apfs_clones_are_independent_preserve_read_only_sources_and_exclude_xattrs() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        fs::write(&source, b"original").unwrap();
        let input = File::open(&source).unwrap();
        let name = c"com.groove.fixture";
        let value = b"source metadata";
        // SAFETY: input is open, name is NUL-terminated, and value has the supplied size.
        assert_eq!(
            // SAFETY: input is open, name is NUL-terminated, and value has the supplied size.
            unsafe {
                libc::fsetxattr(
                    input.as_raw_fd(),
                    name.as_ptr(),
                    value.as_ptr().cast(),
                    value.len(),
                    0,
                    0,
                )
            },
            0
        );
        input
            .set_permissions(fs::Permissions::from_mode(0o444))
            .unwrap();
        let destination = temp.path().join("clone");
        let output = clone_file(&input, &destination).unwrap();
        output.sync_all().unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        // SAFETY: output is open and a null buffer requests the attribute size.
        assert_eq!(
            // SAFETY: output is open and a null buffer requests the attribute size.
            unsafe {
                libc::fgetxattr(
                    output.as_raw_fd(),
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                )
            },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENOATTR)
        );
        // SAFETY: input is open and a null buffer requests the attribute size.
        assert_eq!(
            // SAFETY: input is open and a null buffer requests the attribute size.
            unsafe {
                libc::fgetxattr(
                    input.as_raw_fd(),
                    name.as_ptr(),
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                )
            },
            value.len() as isize
        );
        output
            .set_permissions(fs::Permissions::from_mode(0o600))
            .unwrap();
        fs::write(&destination, b"private").unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"original");
        assert_eq!(
            fs::metadata(&source).unwrap().permissions().mode() & 0o777,
            0o444
        );
        assert!(clone_file(&input, &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"private");
    }
}
