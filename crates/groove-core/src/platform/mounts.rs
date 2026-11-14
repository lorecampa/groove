//! Linux mount boundaries include bind mounts that retain the same device ID.
use crate::{Error, ErrorCode, Result};
use std::{os::unix::ffi::OsStrExt, path::Path};

pub(crate) fn reject_below(path: &Path) -> Result<()> {
    check_table(path, &std::fs::read("/proc/self/mountinfo")?)
}

fn check_table(path: &Path, table: &[u8]) -> Result<()> {
    for line in table
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let field = line
            .split(|byte| *byte == b' ')
            .nth(4)
            .ok_or_else(malformed)?;
        let mut decoded = Vec::with_capacity(field.len());
        let mut remaining = field;
        while let Some((&byte, tail)) = remaining.split_first() {
            if byte == b'\\' {
                let escape = tail.get(..3).ok_or_else(malformed)?;
                let value = match escape {
                    b"040" => b' ',
                    b"011" => b'\t',
                    b"012" => b'\n',
                    b"134" => b'\\',
                    _ => return Err(malformed()),
                };
                decoded.push(value);
                remaining = &tail[3..];
            } else {
                decoded.push(byte);
                remaining = tail;
            }
        }
        let mount = Path::new(std::ffi::OsStr::from_bytes(&decoded));
        if !mount.is_absolute() {
            return Err(malformed());
        }
        if mount.starts_with(path) {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "Mounted path in cleanup tree; unmount it before retrying",
            )
            .with_details(|d| d.path = Some(mount.into())));
        }
    }
    Ok(())
}

fn malformed() -> Error {
    Error::new(ErrorCode::ProtocolError, "Invalid Linux mount table")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_device_mounts_and_escaped_paths_block_cleanup() {
        let table = b"1 0 8:1 / / rw - ext4 /dev/sda rw\n2 1 8:1 /source /pool/slot/ignored/bind rw - ext4 /dev/sda rw\n3 1 8:1 /source /pool/odd\\040name/line\\012break rw - ext4 /dev/sda rw\n";
        for path in ["/pool/slot", "/pool/slot/ignored/bind", "/pool/odd name"] {
            assert_eq!(
                check_table(Path::new(path), table).unwrap_err().code,
                ErrorCode::ConfigConflict
            );
        }
        check_table(Path::new("/pool/slot-other"), table).unwrap();
        check_table(Path::new("/pool/slot/ordinary"), table).unwrap();
    }

    #[test]
    fn unreadable_mount_records_are_not_treated_as_no_mounts() {
        for table in [
            b"invalid\n".as_slice(),
            b"1 0 8:1 / relative rw\n",
            b"1 0 8:1 / /bad\\999 rw\n",
        ] {
            assert_eq!(
                check_table(Path::new("/pool"), table).unwrap_err().code,
                ErrorCode::ProtocolError
            );
        }
    }
}
