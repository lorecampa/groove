use crate::{Error, ErrorCode, Result};
use std::{
    ffi::{CString, OsStr},
    os::unix::ffi::OsStrExt,
    path::Path,
};

pub(crate) fn local_fs(path: &Path) -> Result<()> {
    require_supported(
        recognized_fs(path)?,
        std::env::var_os("GROOVE_ALLOW_UNSUPPORTED_FILESYSTEM").as_deref(),
    )
}

/// Native filesystem qualification, without the operator's explicit override.
pub(crate) fn recognized_fs(path: &Path) -> Result<bool> {
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
    let supported = linux_fs_supported(stat.f_type as u64);
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
    Ok(supported)
}

fn require_supported(supported: bool, override_value: Option<&OsStr>) -> Result<()> {
    if !supported && override_value != Some(OsStr::new("1")) {
        return Err(Error::new(
            ErrorCode::UnsupportedConfiguration,
            "Unsupported filesystem: Groove requires a recognized local filesystem (APFS on macOS); set GROOVE_ALLOW_UNSUPPORTED_FILESYSTEM=1 only after verifying local flock, atomic rename and fsync support",
        ));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_fs_supported(magic: u64) -> bool {
    // Linux UAPI include/uapi/linux/magic.h; OpenZFS ZFS_SUPER_MAGIC.
    // A type check cannot certify the underlying storage or power-loss safety.
    matches!(
        // Some 32-bit statfs ABIs sign-extend f_type. Linux filesystem magic
        // numbers occupy 32 bits, including Btrfs/F2FS values with bit 31 set.
        magic as u32,
        0xef53 // ext2/3/4
        | 0x58465342 // XFS
        | 0x9123683e // Btrfs
        | 0xf2f52010 // F2FS
        | 0x2fc12fc1 // ZFS
        | 0x01021994 // tmpfs
        | 0x794c7630 // overlayfs (underlying storage must also be local)
    )
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::linux_fs_supported;

    #[test]
    fn signed_32_bit_filesystem_types_keep_their_unsigned_identity() {
        for magic in [0x9123683e_u32, 0xf2f52010_u32] {
            assert!(linux_fs_supported(magic as u64));
            assert!(linux_fs_supported(magic as i32 as i64 as u64));
        }
        assert!(!linux_fs_supported(0xffff_ffff_ff53_4d42)); // SMB
    }

    #[test]
    fn network_shared_and_unknown_filesystems_are_rejected() {
        for magic in [
            0x5346414f, 0x6b414653, // AFS implementations
            0x01021997, // 9p
            0x00c36400, // Ceph
            0x65735546, // FUSE, also used by virtiofs
            0x6969, 0xff534d42, 0xfe534d42, 0x517b, // NFS/SMB
            0x73757245, 0x7461636f, // Coda/OCFS2
            0xdeadbeef, // unknown types must not silently pass
        ] {
            assert!(!linux_fs_supported(magic), "{magic:x}");
        }
    }

    #[test]
    fn documented_local_types_remain_supported() {
        for magic in [
            0xef53, 0x58465342, 0x9123683e, 0xf2f52010, 0x2fc12fc1, 0x01021994, 0x794c7630,
        ] {
            assert!(linux_fs_supported(magic), "{magic:x}");
        }
    }
}

#[cfg(test)]
mod override_tests {
    use super::*;

    #[test]
    fn unsupported_filesystems_require_an_explicit_exact_override() {
        require_supported(false, Some(OsStr::new("1"))).unwrap();
        require_supported(true, None).unwrap();
        for value in [
            None,
            Some(OsStr::new("0")),
            Some(OsStr::new("true")),
            Some(OsStr::new("")),
        ] {
            let error = require_supported(false, value).unwrap_err();
            assert_eq!(error.code, ErrorCode::UnsupportedConfiguration);
            assert!(error
                .message
                .contains("GROOVE_ALLOW_UNSUPPORTED_FILESYSTEM=1"));
        }
    }
}
