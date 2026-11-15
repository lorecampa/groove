//! Physical workspace entries which Git does not inventory.
use crate::{Error, ErrorCode, Result};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub(crate) struct DirectoryState {
    path: Vec<u8>,
    mode: u32,
    attributes: Vec<(Vec<u8>, Vec<u8>)>,
}

/// Observe directory metadata which Git checkout does not reset. File contents
/// and file metadata have their own normalization path.
pub(crate) fn directory_state(root: &Path) -> Result<Vec<DirectoryState>> {
    use std::os::unix::ffi::OsStrExt;
    fn visit(root: &Path, path: &Path, state: &mut Vec<DirectoryState>) -> Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.dev() != fs::metadata(root)?.dev() {
            return Err(Error::new(
                ErrorCode::RecoveryFailed,
                "Changed directory during metadata inventory",
            ));
        }
        state.push(DirectoryState {
            path: path
                .strip_prefix(root)
                .map_err(std::io::Error::other)?
                .as_os_str()
                .as_bytes()
                .to_vec(),
            mode: metadata.mode() & 0o7777,
            attributes: directory_attributes(path)?,
        });
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            if path == root && entry.file_name() == ".git" {
                continue;
            }
            if entry.file_type()?.is_dir() {
                visit(root, &entry.path(), state)?;
            }
        }
        Ok(())
    }
    let mut state = Vec::new();
    visit(root, root, &mut state)?;
    state.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(state)
}

pub(crate) fn directory_metadata_matches(root: &Path, saved: &[DirectoryState]) -> Result<bool> {
    use std::os::unix::ffi::OsStrExt;
    for entry in saved {
        let relative = Path::new(std::ffi::OsStr::from_bytes(&entry.path));
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Ok(false);
        }
        let path = root.join(relative);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_dir() {
            continue;
        }
        if metadata.mode() & 0o7777 != entry.mode
            || directory_attributes(&path)? != entry.attributes
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn directory_attributes(path: &Path) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
    use std::{ffi::CString, os::fd::AsRawFd};
    let file = fs::File::open(path)?;
    let fd = file.as_raw_fd();
    let list = |buffer: *mut libc::c_char, size| {
        // SAFETY: fd is owned by file, and each caller supplies either a null
        // size query or writable storage of exactly size bytes.
        unsafe {
            #[cfg(target_os = "linux")]
            {
                libc::flistxattr(fd, buffer, size)
            }
            #[cfg(target_os = "macos")]
            {
                libc::flistxattr(fd, buffer, size, 0)
            }
        }
    };
    let size = list(std::ptr::null_mut(), 0);
    if size < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            return Ok(Vec::new());
        }
        return Err(error.into());
    }
    let mut names = vec![0u8; size as usize];
    let size = list(names.as_mut_ptr().cast(), names.len());
    if size < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    names.truncate(size as usize);
    let mut attributes = Vec::new();
    for name in names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        let key = CString::new(name).map_err(std::io::Error::other)?;
        let get = |buffer: *mut libc::c_void, size| {
            // SAFETY: file owns fd, key is terminated, and the caller provides
            // a null query or a writable buffer covering exactly size bytes.
            unsafe {
                #[cfg(target_os = "linux")]
                {
                    libc::fgetxattr(fd, key.as_ptr(), buffer, size)
                }
                #[cfg(target_os = "macos")]
                {
                    libc::fgetxattr(fd, key.as_ptr(), buffer, size, 0, 0)
                }
            }
        };
        let size = get(std::ptr::null_mut(), 0);
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let mut value = vec![0u8; size as usize];
        let size = get(value.as_mut_ptr().cast(), value.len());
        if size < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        value.truncate(size as usize);
        attributes.push((name.to_vec(), value));
    }
    attributes.sort();
    Ok(attributes)
}

pub(crate) fn first_special(root: &Path, timeout: std::time::Duration) -> Result<Option<PathBuf>> {
    walk(
        root,
        root,
        fs::symlink_metadata(root)?.dev(),
        Operation::Inspect(timeout),
    )
}

pub(crate) fn remove_special(root: &Path) -> Result<()> {
    walk(
        root,
        root,
        fs::symlink_metadata(root)?.dev(),
        Operation::Prepare,
    )?;
    Ok(())
}

pub(crate) fn make_writable(root: &Path) -> Result<()> {
    walk(
        root,
        root,
        fs::symlink_metadata(root)?.dev(),
        Operation::RemoveTree,
    )?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Operation {
    Inspect(std::time::Duration),
    Prepare,
    RemoveTree,
}

fn walk(
    root: &Path,
    directory: &Path,
    device: u64,
    operation: Operation,
) -> Result<Option<PathBuf>> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.dev() != device {
        return Err(Error::new(
            ErrorCode::RecoveryFailed,
            "Mounted or changed directory in workspace inventory",
        )
        .with_details(|details| details.path = Some(directory.into())));
    }
    if !matches!(operation, Operation::Inspect(_)) && metadata.mode() & 0o700 != 0o700 {
        // SAFETY: geteuid has no preconditions and only observes this process.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(Error::new(
                ErrorCode::RecoveryFailed,
                "Cannot repair a directory owned by another user",
            )
            .with_details(|details| details.path = Some(directory.into())));
        }
        fs::set_permissions(
            directory,
            fs::Permissions::from_mode((metadata.mode() & 0o7777) | 0o700),
        )?;
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if directory == root
            && entry.file_name() == ".git"
            && !matches!(operation, Operation::RemoveTree)
        {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.dev() != device {
                return Err(Error::new(
                    ErrorCode::RecoveryFailed,
                    "Mounted directory in workspace inventory",
                )
                .with_details(|details| details.path = Some(path)));
            }
            if let Some(found) = walk(root, &path, device, operation)? {
                return Ok(Some(found));
            }
        } else if !kind.is_file() && !kind.is_symlink() {
            match operation {
                Operation::Prepare => fs::remove_file(&path)?,
                Operation::Inspect(timeout) => {
                    use std::{
                        io::{Seek, Write},
                        os::unix::ffi::OsStrExt,
                    };
                    let mut input = tempfile::tempfile()?;
                    input.write_all(
                        path.strip_prefix(root)
                            .map_err(std::io::Error::other)?
                            .as_os_str()
                            .as_bytes(),
                    )?;
                    input.write_all(&[0])?;
                    input.rewind()?;
                    let ignored = crate::git::Git::new(timeout).run_with(
                        root,
                        &["check-ignore", "--stdin", "-z"],
                        crate::git::Invocation {
                            stdin: Some(input),
                            reject_stderr: true,
                            ..crate::git::Invocation::snapshot()
                        },
                    );
                    match ignored {
                        Ok(_) => (),
                        Err(error) if error.git_exit_code() == Some(1) => return Ok(Some(path)),
                        Err(error) => return Err(error),
                    }
                }
                Operation::RemoveTree => (),
            }
        }
    }
    Ok(None)
}
