use crate::{
    git::Git,
    locking::SlotGuard,
    repository::{self, Repository},
    state::{self, Config},
    CleanupPolicy, Error, RecoveryOutcome, Result,
};
use std::{
    ffi::OsStr,
    fs,
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    time::Duration,
};

pub(crate) fn validate_config(git: &Git, repo: &Repository, oid: &crate::CommitOid) -> Result<()> {
    if git.has_gitlinks(&repo.common, oid.as_str())? {
        return Err(Error::new(
            crate::ErrorCode::UnsupportedConfiguration,
            "Submodules are not supported",
        ));
    }
    // Checkout filters execute arbitrary programs and may outlive Git. Reject configured
    // filters instead of silently producing a different baseline (including Git LFS).
    let config = git.run(&repo.common, &["config", "--null", "--list"], &[])?;
    for entry in config.split(|b| *b == 0) {
        let mut parts = entry.splitn(2, |b| *b == b'\n');
        let key = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or_default();
        if key.starts_with(b"filter.")
            || key == b"core.worktree"
            || key == b"extensions.worktreeconfig"
            || ((key == b"core.sparsecheckout" || key == b"core.sparsecheckoutcone")
                && value != b"false"
                && value != b"0")
        {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Filters, worktree-specific configuration and sparse checkout are not supported",
            ));
        }
    }
    Ok(())
}

pub(crate) fn prepare(
    repo: &Repository,
    config: &Config,
    slot: &crate::SlotId,
    guard: &SlotGuard,
    oid: &crate::CommitOid,
    timeout: Duration,
) -> Result<RecoveryOutcome> {
    let git = Git::new(timeout);
    let path = config.worktree_root.join(slot.as_str());
    validate_slot_path(repo, config, slot)?;
    let fd = guard.file.as_raw_fd();
    let exists = path.exists();
    if exists && registration(repo, &path).is_ok() && !interrupted(repo, &path)? {
        state::checkpoint("before-normalization");
        if normalize(&git, &path, oid, config.cleanup_policy, &[fd]).is_ok()
            && verify(&git, repo, &path, oid, config.cleanup_policy).is_ok()
        {
            state::checkpoint("verified");
            return Ok(RecoveryOutcome::Sanitized);
        }
    }
    let admin = SlotGuard::wait(
        &repo.admin_lock_path(),
        Some(config.admin_identity),
        timeout,
    )?;
    // Git registration mutations inherit BOTH locks, so SIGKILL of Groove cannot
    // expose a still-mutating registration or slot to another cooperating process.
    let fds = [fd, admin.file.as_raw_fd()];
    validate_slot_path(repo, config, slot)?;
    remove_owned(repo, &path)?;
    state::checkpoint("old-worktree-removed");
    let path_text = repository::utf8(&path)?;
    git.run(
        &repo.common,
        &[
            "worktree",
            "add",
            "--detach",
            "--lock",
            "--reason",
            "Groove managed slot",
            path_text,
            oid.as_str(),
        ],
        &fds,
    )?;
    state::checkpoint("worktree-created");
    normalize(&git, &path, oid, config.cleanup_policy, &fds)?;
    verify(&git, repo, &path, oid, config.cleanup_policy)?;
    state::checkpoint("verified");
    Ok(if exists {
        RecoveryOutcome::Rebuilt
    } else {
        RecoveryOutcome::Created
    })
}
fn normalize(
    git: &Git,
    path: &Path,
    oid: &crate::CommitOid,
    policy: CleanupPolicy,
    fds: &[i32],
) -> Result<()> {
    // Discard index flags and stat-cache assumptions too. A cached "clean" status
    // must not hide contamination from a previous lease.
    let admin = registration_from_file(path)?;
    let index = admin.join("index");
    repository::no_symlinks(&index)?;
    match fs::remove_file(index) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    git.run(
        path,
        &["checkout", "--detach", "--force", oid.as_str(), "--"],
        fds,
    )?;
    git.run(
        path,
        &[
            "clean",
            if policy == CleanupPolicy::Pristine {
                "-ffdx"
            } else {
                "-ffd"
            },
            "--",
        ],
        fds,
    )?;
    Ok(())
}
fn verify(
    git: &Git,
    repo: &Repository,
    path: &Path,
    oid: &crate::CommitOid,
    policy: CleanupPolicy,
) -> Result<()> {
    registration(repo, path)?;
    if interrupted(repo, path)? {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Interrupted Git operation remains",
        ));
    }
    let head = git.run(path, &["rev-parse", "--verify", "HEAD"], &[])?;
    if trim(&head) != oid.as_str().as_bytes() {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Unexpected worktree HEAD",
        ));
    }
    let head_file = fs::read(registration(repo, path)?.join("HEAD"))?;
    if trim(&head_file) != oid.as_str().as_bytes() {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Worktree HEAD is not detached",
        ));
    }
    git.run(
        path,
        &[
            "diff",
            "--quiet",
            "--no-ext-diff",
            "--no-textconv",
            "--cached",
            oid.as_str(),
            "--",
        ],
        &[],
    )?;
    git.run(
        path,
        &["diff", "--quiet", "--no-ext-diff", "--no-textconv", "--"],
        &[],
    )?;
    let mut args = vec!["status", "--porcelain=v1", "-z", "--untracked-files=all"];
    if policy == CleanupPolicy::Pristine {
        args.push("--ignored");
    }
    if !git.run(path, &args, &[])?.is_empty() {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Worktree is not at a clean baseline",
        ));
    }
    Ok(())
}
pub(crate) fn validate_slot_path(
    repo: &Repository,
    config: &Config,
    slot: &crate::SlotId,
) -> Result<()> {
    if !config.slots.contains_key(slot) {
        return Err(Error::new(
            crate::ErrorCode::InvalidArgument,
            "Unknown slot",
        ));
    }
    repository::owned_dir(&repo.root)?;
    repository::owned_dir(&config.worktree_root)?;
    let path = config.worktree_root.join(slot.as_str());
    repository::no_symlinks(&path)?;
    if let Ok(m) = fs::symlink_metadata(&path) {
        if !m.is_dir() {
            return Err(Error::new(
                crate::ErrorCode::RecoveryFailed,
                "Slot path is not a directory",
            ));
        }
    }
    Ok(())
}
fn trim(bytes: &[u8]) -> &[u8] {
    bytes.strip_suffix(b"\n").unwrap_or(bytes)
}
fn registration_from_file(path: &Path) -> Result<PathBuf> {
    let dotgit = path.join(".git");
    repository::no_symlinks(&dotgit)?;
    let m = fs::metadata(&dotgit)?;
    if !m.is_file() || m.len() > 16384 {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Invalid .git pointer",
        ));
    }
    let bytes = fs::read(&dotgit)?;
    let pointer = trim(&bytes)
        .strip_prefix(b"gitdir: ")
        .ok_or_else(|| Error::new(crate::ErrorCode::RecoveryFailed, "Invalid .git pointer"))?;
    use std::os::unix::ffi::OsStrExt;
    let raw = PathBuf::from(OsStr::from_bytes(pointer));
    let admin = if raw.is_absolute() {
        raw
    } else {
        path.join(raw)
    };
    repository::no_symlinks(&admin)?;
    Ok(admin)
}
fn registration(repo: &Repository, path: &Path) -> Result<PathBuf> {
    let admin = registration_from_file(path)?;
    if admin.parent() != Some(repo.common.join("worktrees").as_path()) {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Worktree registration is outside the repository",
        ));
    }
    validate_admin_tree(&admin)?;
    repository::no_symlinks(&admin.join("gitdir"))?;
    let backlink = fs::read(admin.join("gitdir"))?;
    use std::os::unix::ffi::OsStrExt;
    if trim(&backlink) != path.join(".git").as_os_str().as_bytes() {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Worktree registration backlink does not match slot",
        ));
    }
    repository::no_symlinks(&admin.join("commondir"))?;
    let common = fs::read(admin.join("commondir"))?;
    let common_path = admin.join(OsStr::from_bytes(trim(&common)));
    if fs::canonicalize(common_path)? != repo.common {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Registration common directory mismatch",
        ));
    }
    Ok(admin)
}
fn validate_admin_tree(path: &Path) -> Result<()> {
    repository::no_symlinks(path)?;
    for item in fs::read_dir(path)? {
        let item = item?;
        let kind = item.file_type()?;
        if kind.is_symlink() || (!kind.is_dir() && !kind.is_file()) {
            return Err(Error::new(
                crate::ErrorCode::RecoveryFailed,
                "Unsupported file in worktree administration",
            ));
        }
        if kind.is_dir() {
            validate_admin_tree(&item.path())?;
        }
    }
    Ok(())
}
fn interrupted(repo: &Repository, path: &Path) -> Result<bool> {
    let admin = registration(repo, path)?;
    repository::no_symlinks(&admin)?;
    let names = [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "REBASE_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "BISECT_LOG",
        "index.lock",
        "HEAD.lock",
    ];
    Ok(names
        .iter()
        .any(|name| fs::symlink_metadata(admin.join(name)).is_ok()))
}
fn remove_owned(repo: &Repository, path: &Path) -> Result<()> {
    // Find registrations by their exact backlink, never via an untrusted .git pointer
    // and never with a repository-wide prune. Corrupt unrelated entries are left alone.
    let registrations = repo.common.join("worktrees");
    repository::no_symlinks(&registrations)?;
    if registrations.exists() {
        for item in fs::read_dir(&registrations)? {
            let item = item?;
            if !item.file_type()?.is_dir() {
                continue;
            }
            let backlink = item.path().join("gitdir");
            if repository::no_symlinks(&backlink).is_err() {
                continue;
            }
            if let Ok(raw) = fs::read(backlink) {
                use std::os::unix::ffi::OsStrExt;
                if trim(&raw) == path.join(".git").as_os_str().as_bytes() {
                    repository::no_symlinks(&item.path())?;
                    fs::remove_dir_all(item.path())?;
                }
            }
        }
        state::sync_dir(&registrations)?;
    }
    repository::no_symlinks(path)?;
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    if let Some(parent) = path.parent() {
        state::sync_dir(parent)?;
    }
    Ok(())
}
