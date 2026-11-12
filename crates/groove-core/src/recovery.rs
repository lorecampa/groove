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
        match normalize(&git, &path, oid, config.cleanup_policy, &[fd])
            .inspect(|()| state::checkpoint("normalized-in-place"))
            .and_then(|()| verify(&git, repo, &path, oid, config.cleanup_policy))
        {
            Ok(()) => {
                state::checkpoint("verified");
                return Ok(RecoveryOutcome::Sanitized);
            }
            Err(error) => {
                tracing::warn!(slot = %slot, path = %path.display(), %error, "In-place preparation failed; rebuilding the owned checkout")
            }
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
    register(&git, repo, &path, oid, &fds)?;
    state::checkpoint("worktree-created");
    // Registration is complete and natively locked. Only this slot's
    // descriptor is needed for checkout/index mutation and verification;
    // other slots can register and prepare concurrently.
    drop(admin);
    normalize(&git, &path, oid, config.cleanup_policy, &[fd])?;
    verify(&git, repo, &path, oid, config.cleanup_policy)?;
    state::checkpoint("verified");
    Ok(if exists {
        RecoveryOutcome::Rebuilt
    } else {
        RecoveryOutcome::Created
    })
}

pub(crate) fn register(
    git: &Git,
    repo: &Repository,
    path: &Path,
    oid: &crate::CommitOid,
    fds: &[i32],
) -> Result<()> {
    let path_text = repository::utf8(path)?;
    git.run(
        &repo.common,
        &[
            // Groove validates registrations by their absolute paths; never let a
            // repository's relative-path preference rewrite its own slots.
            "-c",
            "worktree.useRelativePaths=false",
            "worktree",
            "add",
            "--detach",
            "--no-checkout",
            "--lock",
            "--reason",
            "Groove managed slot",
            path_text,
            oid.as_str(),
        ],
        fds,
    )?;
    Ok(())
}
pub(crate) fn normalize(
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
pub(crate) fn verify(
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
    // Ask Git rather than reading the HEAD file: reftable repositories keep a
    // placeholder there and store the real per-worktree HEAD in their ref table.
    match git.run(path, &["symbolic-ref", "--quiet", "HEAD"], &[]) {
        Err(e) if e.code == crate::ErrorCode::GitError && e.git_exit_code() == Some(1) => (),
        Err(e) => return Err(e),
        Ok(_) => {
            return Err(Error::new(
                crate::ErrorCode::RecoveryFailed,
                "Worktree HEAD is not detached",
            ))
        }
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
/// Detach HEAD in a released checkout so a branch the task created or switched
/// to is no longer "checked out" there, freeing it for `git switch` or deletion
/// elsewhere. Only HEAD changes: files, index and the branch itself are kept.
///
/// Best effort: a missing, foreign or unborn checkout is left alone, because the
/// next acquisition normalizes or rebuilds it anyway.
pub(crate) fn detach_head(
    repo: &Repository,
    config: &Config,
    slot: &crate::SlotId,
    timeout: Duration,
    fds: &[i32],
) -> Result<()> {
    let path = config.worktree_root.join(slot.as_str());
    if validate_slot_path(repo, config, slot).is_err()
        || !path.exists()
        || registration(repo, &path).is_err()
    {
        return Ok(());
    }
    let git = Git::new(timeout);
    match git.run(&path, &["symbolic-ref", "--quiet", "HEAD"], &[]) {
        // Already detached.
        Err(e) if e.code == crate::ErrorCode::GitError && e.git_exit_code() == Some(1) => {
            return Ok(())
        }
        Err(e) => return Err(e),
        Ok(_) => (),
    }
    let Ok(oid) = git.run(
        &path,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        &[],
    ) else {
        // An unborn branch has no commit to detach at.
        return Ok(());
    };
    let oid = String::from_utf8_lossy(trim(&oid)).into_owned();
    git.run(
        &path,
        &[
            "update-ref",
            "--no-deref",
            "-m",
            "groove: detach HEAD on release",
            "HEAD",
            &oid,
        ],
        fds,
    )?;
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
    let admin = resolve_link(path, OsStr::from_bytes(pointer))?;
    repository::no_symlinks(&admin)?;
    Ok(admin)
}
pub(crate) fn registration(repo: &Repository, path: &Path) -> Result<PathBuf> {
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
    if !backlink_matches(&admin, &backlink, path) {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Worktree registration backlink does not match slot",
        ));
    }
    repository::no_symlinks(&admin.join("commondir"))?;
    let common = fs::read(admin.join("commondir"))?;
    use std::os::unix::ffi::OsStrExt;
    let common_path = admin.join(OsStr::from_bytes(trim(&common)));
    if fs::canonicalize(common_path)? != repo.common {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Registration common directory mismatch",
        ));
    }
    Ok(admin)
}
/// Resolve a worktree link that Git may record relative to `base`
/// (`worktree.useRelativePaths`). `..` is resolved lexically, and the result
/// must name the same file the kernel would reach, so a symlink cannot redirect it.
fn resolve_link(base: &Path, raw: &OsStr) -> Result<PathBuf> {
    let raw = Path::new(raw);
    if raw.is_absolute() {
        return Ok(raw.to_path_buf());
    }
    let resolved = lexical(&base.join(raw));
    if fs::canonicalize(base.join(raw)).ok() != fs::canonicalize(&resolved).ok() {
        return Err(Error::new(
            crate::ErrorCode::RecoveryFailed,
            "Relative worktree link does not resolve consistently",
        ));
    }
    Ok(resolved)
}
/// Remove `.` and resolve `..` components without consulting the filesystem.
fn lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => (),
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}
/// Whether a registration's `gitdir` backlink (absolute, or relative to its
/// administrative directory) names `path/.git`.
fn backlink_matches(admin: &Path, raw: &[u8], path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let expected = path.join(".git");
    let raw = OsStr::from_bytes(trim(raw));
    if Path::new(raw).is_absolute() {
        return raw.as_bytes() == expected.as_os_str().as_bytes();
    }
    // A relative link whose target is gone (a deleted slot) still matches lexically,
    // so stale registrations of an owned path can be removed.
    let resolved = lexical(&admin.join(raw));
    resolved == expected
        && match (
            fs::canonicalize(admin.join(raw)),
            fs::canonicalize(&resolved),
        ) {
            (Ok(a), Ok(b)) => a == b,
            (Err(_), Err(_)) => true,
            _ => false,
        }
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
pub(crate) fn interrupted(repo: &Repository, path: &Path) -> Result<bool> {
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
pub(crate) fn remove_owned(repo: &Repository, path: &Path) -> Result<()> {
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
                if backlink_matches(&item.path(), &raw, path) {
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
