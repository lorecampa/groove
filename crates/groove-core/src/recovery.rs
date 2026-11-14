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
    validate_user_filters(git, repo, oid)
}

fn validate_user_filters(git: &Git, repo: &Repository, oid: &crate::CommitOid) -> Result<()> {
    use crate::git::Invocation;
    use std::collections::BTreeSet;
    let config = git.run_with(
        &repo.origin,
        &["config", "--null", "--list"],
        Invocation::snapshot(),
    )?;
    let mut drivers = BTreeSet::new();
    for entry in config.split(|byte| *byte == 0) {
        let key = entry
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap_or_default();
        if let Some(key) = key.strip_prefix(b"filter.") {
            let driver = key
                .rsplitn(2, |byte| *byte == b'.')
                .nth(1)
                .unwrap_or_default();
            if driver.is_empty()
                || !driver
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(byte))
            {
                return Err(Error::new(
                    crate::ErrorCode::UnsupportedConfiguration,
                    format!(
                        "Unsupported filter driver name: {}",
                        String::from_utf8_lossy(driver)
                    ),
                ));
            }
            drivers.insert(
                std::str::from_utf8(driver)
                    .map_err(|error| {
                        Error::new(
                            crate::ErrorCode::UnsupportedConfiguration,
                            "Filter driver name must be ASCII",
                        )
                        .with_source(error)
                    })?
                    .to_owned(),
            );
        }
    }
    if drivers.is_empty() {
        return Ok(());
    }
    // Installing an unused global filter must not strand ordinary tasks. Check
    // the requested commit's attributes, not the caller's index or working files.
    // A large private index belongs on repository storage, not a potentially
    // memory-backed or quota-limited system temporary directory. Keep it outside
    // the scratch tree: a tracked directory named `index` is valid Git content.
    let scratch = tempfile::Builder::new()
        .prefix("groove-filter-")
        .tempdir_in(&repo.common)?;
    let index = scratch.path().join("index");
    let worktree = scratch.path().join("worktree");
    fs::create_dir(&worktree)?;
    let env = [
        ("GIT_INDEX_FILE", index.as_os_str()),
        ("GIT_DIR", repo.common.as_os_str()),
        ("GIT_WORK_TREE", worktree.as_os_str()),
    ];
    git.run_with(
        &worktree,
        &["read-tree", oid.as_str()],
        Invocation {
            env: &env,
            ..Default::default()
        },
    )?;
    // Materialize only attribute files, without user filter definitions. Attribute
    // pathspecs then see the requested tree at a stable root, including when the
    // caller is a subdirectory or a background worker runs from the common .git.
    let names = git.spool(
        &worktree,
        &["ls-files", "-z", "--", ":(glob)**/.gitattributes"],
        Invocation {
            env: &env,
            reject_stderr: true,
            ..Default::default()
        },
    )?;
    git.run_with(
        &worktree,
        &[
            "-c",
            "core.autocrlf=false",
            "checkout-index",
            "-z",
            "--stdin",
        ],
        Invocation {
            env: &env,
            stdin: Some(names),
            reject_stderr: true,
            ..Default::default()
        },
    )?;
    for driver in drivers {
        let selection = format!(":(attr:filter={driver})");
        let selected = git.spool(
            &worktree,
            &["ls-files", "-z", "--", &selection],
            Invocation {
                env: &env,
                reject_stderr: true,
                ..Invocation::snapshot()
            },
        )?;
        if selected.metadata()?.len() > 0 {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                format!("Content filter {driver} is selected by checkout attributes and is not supported"),
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
        // Slot ownership excludes cooperating registration changes for this path.
        // Read-only duplicate detection must not wait behind a sibling's init or
        // destroy: healthy checkout normalization needs only this slot's lock.
        matching_registration(repo, &path)?;
        state::checkpoint("before-normalization");
        match normalize(&git, &path, oid, config.cleanup_policy, &[fd])
            .inspect(|()| state::checkpoint("normalized-in-place"))
            .and_then(|()| verify(&git, repo, &path, oid, config.cleanup_policy))
        {
            Ok(()) => {
                mark_verified(&path)?;
                state::checkpoint("verified");
                return Ok(RecoveryOutcome::Sanitized);
            }
            Err(error) => {
                if error.is_git_timeout() {
                    return Err(error);
                }
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
    mark_verified(&path)?;
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
    let admin = registration_from_file(path)?;
    let index = admin.join("index");
    repository::no_symlinks(&index)?;
    let marker = admin.join("groove-verified");
    repository::no_symlinks(&marker)?;
    let verified = match fs::metadata(&marker) {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    match fs::remove_file(&marker) {
        Ok(()) => crate::state::sync_dir(&admin)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    state::checkpoint("reuse-marker-removed");
    if !reuse_untouched(git, path, oid, &index, verified.as_ref(), fds)? {
        // The full path discards flags and stat-cache assumptions and breaks every
        // tracked hardlink. A missing marker also forces this path after interruption.
        let had_index = index.exists();
        match fs::remove_file(&index) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
        if had_index {
            // Only a revision switch needs an old inventory. Ordinary same-commit
            // resets keep the original index-free checkout without an extra write.
            let changes_revision =
                match git.run(path, &["rev-parse", "--verify", "--quiet", "HEAD"], fds) {
                    Ok(previous) => trim(&previous) != oid.as_str().as_bytes(),
                    Err(error) if error.git_exit_code().is_some() => false,
                    Err(error) => return Err(error),
                };
            // Retain the previous tracked-path inventory so checkout removes files
            // the destination deletes and ignores. Leave stat entries empty: full
            // rewriting also breaks hardlinks and restores file creation metadata.
            // An orphan/unreadable HEAD has no trustworthy inventory; keep the old
            // index-free checkout behavior rather than rebuilding and losing caches.
            if changes_revision {
                if let Err(error) = git.run(path, &["read-tree", "HEAD"], fds) {
                    // Only retry after Git exited. Cancellation, execution and I/O
                    // failures retain their usual recovery/teardown behavior.
                    if error.git_exit_code().is_none() {
                        return Err(error);
                    }
                    tracing::debug!(%error, "Previous HEAD has no usable tracked-path inventory");
                    repository::no_symlinks(&index)?;
                    match fs::remove_file(&index) {
                        Ok(()) => (),
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
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

fn mark_verified(path: &Path) -> Result<()> {
    let admin = registration_from_file(path)?;
    let marker = admin.join("groove-verified");
    repository::no_symlinks(&marker)?;
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(marker)?;
    file.sync_all()?;
    state::sync_dir(&admin)
}

/// Reuse stat entries only for paths whose physical metadata predates the last
/// verified checkout. ctime cannot be backdated by an ordinary task; root/clock
/// changes are outside this cooperative local-filesystem trust assumption.
fn reuse_untouched(
    git: &Git,
    path: &Path,
    oid: &crate::CommitOid,
    index: &Path,
    verified: Option<&fs::Metadata>,
    fds: &[i32],
) -> Result<bool> {
    use std::{
        io::{Seek, Write},
        os::unix::fs::MetadataExt,
    };
    let Some(verified) = verified else {
        return Ok(false);
    };
    if !index.try_exists()? || verified.modified()? > std::time::SystemTime::now() {
        return Ok(false);
    }
    let audit = git.audit_index(
        path,
        crate::git::Invocation {
            inherit: fds,
            ..Default::default()
        },
    )?;
    if audit.assume_unchanged || audit.skip_worktree || audit.conflicts || audit.gitlinks {
        return Ok(false);
    }
    let previous = match git.run(path, &["rev-parse", "--verify", "--quiet", "HEAD"], fds) {
        Ok(previous) => previous,
        Err(error) if error.git_exit_code().is_some() => return Ok(false),
        Err(error) => return Err(error),
    };
    if trim(&previous) != oid.as_str().as_bytes() {
        match git.run(
            path,
            &[
                "diff",
                "--quiet",
                "--no-ext-diff",
                "--no-textconv",
                "HEAD",
                oid.as_str(),
                "--",
                ":(glob)**/.gitattributes",
            ],
            fds,
        ) {
            Ok(_) => (),
            Err(error) if error.git_exit_code() == Some(1) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    let verified_at = (verified.mtime(), verified.mtime_nsec());
    let mut touched = Vec::new();
    if !touched_paths(path, path, verified_at, false, &mut touched)? {
        return Ok(false);
    }
    if touched.is_empty() {
        return Ok(true);
    }
    let mut input = tempfile::tempfile()?;
    input.write_all(&touched)?;
    input.rewind()?;
    git.run_with(
        path,
        &["update-index", "--force-remove", "-z", "--stdin"],
        crate::git::Invocation {
            inherit: fds,
            stdin: Some(input),
            ..Default::default()
        },
    )?;
    // The path list is NUL-delimited and never passed through revision syntax.
    debug_assert!(touched.last() == Some(&0));
    Ok(true)
}

fn touched_paths(
    root: &Path,
    directory: &Path,
    verified: (i64, i64),
    ancestor_touched: bool,
    touched: &mut Vec<u8>,
) -> Result<bool> {
    use std::os::unix::{ffi::OsStrExt, fs::MetadataExt};
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if directory == root && entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        let changed = ancestor_touched || (metadata.ctime(), metadata.ctime_nsec()) >= verified;
        if metadata.is_dir() {
            // A directory swap changes the directory's ctime, not every child's.
            if !touched_paths(root, &path, verified, changed, touched)? {
                return Ok(false);
            }
        } else if changed {
            if entry.file_name() == ".gitattributes" {
                return Ok(false);
            }
            let relative = path.strip_prefix(root).map_err(|error| {
                Error::new(
                    crate::ErrorCode::RecoveryFailed,
                    "Touched path escaped its checkout",
                )
                .with_source(error)
            })?;
            let bytes = relative.as_os_str().as_bytes();
            if touched.len().saturating_add(bytes.len() + 1) > 32 * 1024 * 1024 {
                return Ok(false);
            }
            touched.extend_from_slice(bytes);
            touched.push(0);
        }
    }
    Ok(true)
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
    #[cfg(target_os = "linux")]
    crate::platform::mounts::reject_below(&path)?;
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
    let registration = validate_disposal(repo, path)?;
    // Find registrations by their exact backlink, never via an untrusted .git pointer
    // and never with a repository-wide prune. Corrupt unrelated entries are left alone.
    if let Some(registration) = registration {
        fs::remove_dir_all(registration)?;
        state::sync_dir(&repo.common.join("worktrees"))?;
    }
    repository::no_symlinks(path)?;
    state::checkpoint("owned-registrations-removed");
    if path.exists() {
        fs::remove_dir_all(path)?;
    }
    if let Some(parent) = path.parent() {
        if parent.try_exists()? {
            state::sync_dir(parent)?;
        }
    }
    Ok(())
}

/// Preflight disposal without changing the occupant or any registration. Callers
/// hold repository admin exclusion and either slot ownership or exclusive lifecycle
/// admission. Return the sole exact registration that removal may discard.
pub(crate) fn validate_disposal(repo: &Repository, path: &Path) -> Result<Option<PathBuf>> {
    let refusal = |reason: &str| disposal_refusal(path, reason);
    repository::no_symlinks(path)?;
    #[cfg(target_os = "linux")]
    crate::platform::mounts::reject_below(path)?;
    let registrations = repo.common.join("worktrees");
    let claim = matching_registration(repo, path)?;
    if !path.try_exists()? {
        return Ok(claim);
    }
    let dotgit = path.join(".git");
    repository::no_symlinks(&dotgit)?;
    if dotgit.is_dir() {
        return Err(refusal("A standalone repository occupies this slot"));
    }
    if let Ok(admin) = registration_from_file(path) {
        // A forged pointer to this repository's primary store is damaged slot
        // metadata, not authority over that store. Preserve bounded rebuilding
        // while only ever removing registrations found by their exact backlink.
        if admin != repo.common {
            let own_admin = admin.parent() == Some(registrations.as_path());
            let backlink = admin.join("gitdir");
            repository::no_symlinks(&backlink)?;
            match fs::read(&backlink) {
                Ok(raw) if !own_admin || !backlink_matches(&admin, &raw, path) => {
                    return Err(refusal("Another linked worktree occupies this slot"));
                }
                Ok(_) => {
                    // A matching backlink alone cannot adopt another common store.
                    let common = admin.join("commondir");
                    repository::no_symlinks(&common)?;
                    match fs::read(&common) {
                        Ok(raw) => {
                            use std::os::unix::ffi::OsStrExt;
                            let common = resolve_link(&admin, OsStr::from_bytes(trim(&raw)))?;
                            if common.try_exists()? && fs::canonicalize(common)? != repo.common {
                                return Err(refusal("Worktree belongs to another repository"));
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if !own_admin
                        && (recognizable_store(&admin)? || admin.join("commondir").try_exists()?)
                    {
                        return Err(refusal("A foreign Git store occupies this slot"));
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    // Only inspect the occupant itself: Git discovery here could walk into the
    // primary repository. Task-created nested repositories remain disposable.
    if !dotgit.try_exists()? && recognizable_store(path)? {
        return Err(refusal("A bare Git store occupies this slot"));
    }
    Ok(claim)
}

fn recognizable_store(path: &Path) -> Result<bool> {
    let head = path.join("HEAD");
    let objects = path.join("objects");
    repository::no_symlinks(&head)?;
    repository::no_symlinks(&objects)?;
    Ok(head.is_file() && objects.is_dir())
}

fn disposal_refusal(path: &Path, reason: &str) -> Error {
    Error::new(
        crate::ErrorCode::RecoveryFailed,
        format!(
            "{}: {reason}; safely move the occupant or restore its original location before retrying",
            path.display()
        ),
    )
    .with_details(|d| d.path = Some(path.into()))
}

/// Inventory exact backlinks, without following the occupant's pointer. Removal
/// callers hold admin exclusion; healthy normalization holds the slot lock and
/// tolerates unrelated registrations disappearing during this read-only scan.
fn matching_registration(repo: &Repository, path: &Path) -> Result<Option<PathBuf>> {
    let registrations = repo.common.join("worktrees");
    repository::no_symlinks(&registrations)?;
    let mut claim = None;
    if registrations.try_exists()? {
        for item in fs::read_dir(&registrations)? {
            let item = item?;
            let kind = match item.file_type() {
                Ok(kind) => kind,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if !kind.is_dir() {
                continue;
            }
            let backlink = item.path().join("gitdir");
            if repository::no_symlinks(&backlink).is_err() {
                continue;
            }
            if let Ok(raw) = fs::read(backlink) {
                if backlink_matches(&item.path(), &raw, path) {
                    if claim.is_some() {
                        return Err(disposal_refusal(
                            path,
                            "Multiple worktree registrations claim this slot",
                        ));
                    }
                    repository::no_symlinks(&item.path())?;
                    claim = Some(item.path());
                }
            }
        }
    }
    Ok(claim)
}
