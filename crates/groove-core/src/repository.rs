pub(crate) use crate::platform::fs::local_fs;
use crate::{git::Git, state, Error, Result};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Component, Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone)]
pub(crate) struct Repository {
    pub common: PathBuf,
    pub root: PathBuf,
}
impl Repository {
    pub fn discover(repo: &Path, timeout: Duration) -> Result<Self> {
        let git = Git::new(timeout);
        let version = git.run(repo, &["--version"], &[])?;
        let version = String::from_utf8_lossy(&version);
        let numbers: Vec<u32> = version
            .trim()
            .strip_prefix("git version ")
            .unwrap_or_default()
            .split('.')
            .take(2)
            .filter_map(|part| part.parse().ok())
            .collect();
        if numbers.as_slice() < [2, 40].as_slice() {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Git 2.40 or newer is required",
            ));
        }
        let out = git.run(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            &[],
        )?;
        let raw = out.strip_suffix(b"\n").unwrap_or(&out);
        let text = std::str::from_utf8(raw).map_err(|e| {
            Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Repository root must be UTF-8",
            )
            .with_source(e)
        })?;
        let common = fs::canonicalize(text)?;
        utf8(&common)?;
        no_symlinks(&common)?;
        local_fs(&common)?;
        Ok(Self {
            root: common.join("groove"),
            common,
        })
    }
    pub fn admin_lock_path(&self) -> PathBuf {
        self.root.join("locks/admin.lock")
    }
    pub fn lock_path(&self, slot: &crate::SlotId) -> PathBuf {
        self.root.join("locks").join(format!("{slot}.lock"))
    }
    pub fn record_path(&self, slot: &crate::SlotId) -> PathBuf {
        self.root.join("slots").join(format!("{slot}.json"))
    }
    pub fn protection_path(&self, slot: &crate::SlotId) -> PathBuf {
        self.root
            .join("slots")
            .join(format!("{slot}.protected.json"))
    }
    /// Default checkout root beneath the Groove state directory.
    pub fn default_worktree_root(&self) -> PathBuf {
        self.root.join("worktrees")
    }
    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }
    pub fn execution_lock_path(&self, slot: &crate::SlotId) -> PathBuf {
        self.root
            .join("locks")
            .join(format!("{slot}.execution.lock"))
    }
    pub fn execution_path(&self, slot: &crate::SlotId) -> PathBuf {
        self.root
            .join("slots")
            .join(format!("{slot}.execution.json"))
    }
}
pub(crate) fn utf8(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| {
        Error::new(
            crate::ErrorCode::UnsupportedConfiguration,
            "Managed root paths must be UTF-8",
        )
    })
}
pub(crate) fn no_symlinks(path: &Path) -> Result<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if component == Component::ParentDir {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Parent path components are not allowed",
            ));
        }
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(Error::new(
                    crate::ErrorCode::UnsupportedConfiguration,
                    format!("Symlink in managed path: {}", prefix.display()),
                ))
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
pub(crate) fn mkdir(path: &Path) -> Result<()> {
    use crate::error::PathContext;
    mkdir_unlabelled(path).at(path)
}
fn mkdir_unlabelled(path: &Path) -> Result<()> {
    no_symlinks(path)?;
    if !path.exists() {
        if let Some(parent) = path.parent() {
            mkdir(parent)?;
        }
        match fs::DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        if let Some(parent) = path.parent() {
            state::sync_dir(parent)?;
        }
    }
    let m = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions.
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } {
        // System-owned ancestors are allowed; only newly managed dirs are checked by caller.
        if !m.is_dir() {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Managed path must be a directory",
            ));
        }
    }
    Ok(())
}
pub(crate) fn owned_dir(path: &Path) -> Result<()> {
    use crate::error::PathContext;
    owned_dir_unlabelled(path).at(path)
}
fn owned_dir_unlabelled(path: &Path) -> Result<()> {
    no_symlinks(path)?;
    let m = fs::metadata(path)?;
    // SAFETY: geteuid has no preconditions.
    if !m.is_dir() || m.uid() != unsafe { libc::geteuid() } || m.mode() & 0o022 != 0 {
        return Err(Error::new(crate::ErrorCode::UnsupportedConfiguration, "Managed directories must be owned by the current user and not writable by group or others"));
    }
    local_fs(path)
}
