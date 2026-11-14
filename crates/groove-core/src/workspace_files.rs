//! Physical workspace entries which Git does not inventory.
use crate::{Error, ErrorCode, Result};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(crate) fn first_special(root: &Path) -> Result<Option<PathBuf>> {
    special_entries(root, root, false)
}

pub(crate) fn remove_special(root: &Path) -> Result<()> {
    special_entries(root, root, true)?;
    Ok(())
}

fn special_entries(root: &Path, directory: &Path, remove: bool) -> Result<Option<PathBuf>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if directory == root && entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.dev() != fs::metadata(root)?.dev() {
                return Err(Error::new(
                    ErrorCode::RecoveryFailed,
                    "Mounted directory in workspace inventory",
                )
                .with_details(|details| details.path = Some(path)));
            }
            if let Some(found) = special_entries(root, &path, remove)? {
                return Ok(Some(found));
            }
        } else if !kind.is_file() && !kind.is_symlink() {
            if remove {
                fs::remove_file(&path)?;
            } else {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}
