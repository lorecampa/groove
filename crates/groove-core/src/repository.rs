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
    pub name: crate::PoolName,
    /// Directory the pool was discovered from. Revisions such as `HEAD` resolve
    /// here, so a linked worktree's caller gets its own HEAD, not the main one's.
    pub origin: PathBuf,
}
impl Repository {
    pub fn discover(repo: &Path, timeout: Duration) -> Result<Self> {
        // A shell left inside a checkout that was later rebuilt has a deleted
        // working directory; say so instead of surfacing Git's failure.
        if repo.is_relative() {
            if let Err(e) = std::env::current_dir() {
                return Err(Error::new(
                    crate::ErrorCode::InvalidArgument,
                    "The current directory no longer exists (a released checkout may have been rebuilt); change to the repository or pass its path explicitly",
                )
                .with_source(e));
            }
        }
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
        let origin = fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
        Ok(Self {
            root: common.join("groove"),
            name: crate::PoolName::default(),
            common,
            origin,
        })
    }
    pub fn discover_named(repo: &Path, name: &crate::PoolName, timeout: Duration) -> Result<Self> {
        Ok(Self::discover(repo, timeout)?.named(name.clone()))
    }
    pub fn named(&self, name: crate::PoolName) -> Self {
        let mut repo = self.clone();
        repo.root = if name.is_default() {
            self.namespace_root()
        } else {
            self.namespace_root().join("pools").join(name.as_str())
        };
        repo.name = name;
        repo
    }
    pub fn namespace_root(&self) -> PathBuf {
        self.common.join("groove")
    }
    pub fn recorded_default_common<'a>(&self, root: &'a Path) -> Option<&'a Path> {
        let relative = self.root.strip_prefix(&self.common).ok()?.join("worktrees");
        root.ends_with(&relative)
            .then(|| root.ancestors().nth(relative.components().count()))
            .flatten()
    }
    pub fn admin_lock_path(&self) -> PathBuf {
        self.namespace_root().join("locks/admin.lock")
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
    /// Original checkout root, still accepted for existing pools and explicit overrides.
    pub fn legacy_worktree_root(&self) -> PathBuf {
        self.root.join("worktrees")
    }
    /// Name external checkouts by canonical common Git directory, independent of
    /// the calling worktree, remote URL or mutable branch names.
    pub fn default_worktree_root(&self, base: Option<PathBuf>) -> Result<PathBuf> {
        use sha2::{Digest, Sha256};
        let base = base
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".worktrees")))
            .filter(|path| path.is_absolute())
            .ok_or_else(|| {
                Error::new(
                    crate::ErrorCode::InvalidArgument,
                    "Set an absolute HOME, configure worktrees.root, or pass --worktree-root",
                )
            })?;
        // User-selected parents may be aliases (for example a symlinked HOME).
        // Resolve the existing prefix before creating Groove-owned descendants;
        // those descendants still undergo the strict managed-path checks below.
        let mut ancestor = base.as_path();
        let mut suffix = Vec::new();
        while !ancestor.try_exists()? {
            suffix.push(ancestor.file_name().ok_or_else(|| {
                Error::new(
                    crate::ErrorCode::InvalidArgument,
                    "Worktree base cannot be resolved",
                )
            })?);
            ancestor = ancestor.parent().ok_or_else(|| {
                Error::new(
                    crate::ErrorCode::InvalidArgument,
                    "Worktree base has no parent",
                )
            })?;
        }
        let mut resolved_base = fs::canonicalize(ancestor)?;
        for part in suffix.into_iter().rev() {
            resolved_base.push(part);
        }
        let project = if self.common.file_name().is_some_and(|name| name == ".git") {
            self.common.parent().and_then(Path::file_name)
        } else {
            self.common.file_stem()
        };
        // Keep components short and portable even for unusual repository names.
        let project: String = project
            .and_then(|name| name.to_str())
            .unwrap_or("repo")
            .chars()
            .take(48)
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let digest = Sha256::digest(utf8(&self.common)?.as_bytes());
        let id = crate::state::hex(&digest[..16]);
        let root = resolved_base
            .join(format!("{project}-{id}"))
            .join(self.name.as_str());
        no_symlinks(&root)?;
        utf8(&root)?;
        Ok(root)
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
