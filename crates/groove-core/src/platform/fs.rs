use crate::{Error, ErrorCode, Result};
use std::{ffi::CString, os::unix::ffi::OsStrExt, path::Path};

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
