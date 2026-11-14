//! Physical workspace entries which Git does not inventory.
use crate::{Error, ErrorCode, Result};
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(crate) fn first_special(root: &Path) -> Result<Option<PathBuf>> {
    walk(
        root,
        root,
        fs::symlink_metadata(root)?.dev(),
        Operation::Inspect,
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
    Inspect,
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
    if !matches!(operation, Operation::Inspect) && metadata.mode() & 0o700 != 0o700 {
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
                Operation::Inspect => return Ok(Some(path)),
                Operation::RemoveTree => (),
            }
        }
    }
    Ok(None)
}
