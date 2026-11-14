//! Repository-wide namespace discovery and coordination. The original admin lock
//! stays permanent and serializes Git registrations and checkout-root claims.
use crate::{
    lifecycle,
    locking::{self, SlotGuard},
    pool::InitIntent,
    repository::{self, Repository},
    state::{self, Config},
    CleanupPolicy, Error, ErrorCode, Pool, PoolId, PoolName, PoolPhase, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

/// Nontransactional observation of a pool, including interrupted and deleted pools.
/// Names and metadata never confer ownership.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolSummary {
    /// Namespace selected by `--pool`.
    pub name: PoolName,
    /// Current generation, absent before assignment and in legacy pools.
    pub pool_id: Option<PoolId>,
    /// Current durable lifecycle phase.
    pub phase: PoolPhase,
    /// Pool-specific state directory.
    pub state_dir: PathBuf,
    /// Configured capacity, absent if initialization has not published its intent.
    pub capacity: Option<usize>,
    /// Cleanup policy of the ready pool or pending initialization.
    pub cleanup_policy: Option<CleanupPolicy>,
    /// Checkout root claimed by this namespace.
    pub worktree_root: Option<PathBuf>,
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn has_records(repo: &Repository) -> Result<bool> {
    for name in ["config.json", "initializing.json", "lifecycle.json"] {
        if exists(&repo.root.join(name))? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn repositories(repo: &Repository) -> Result<Vec<Repository>> {
    let root = repo.namespace_root();
    if !exists(&root)? {
        return Ok(Vec::new());
    }
    repository::owned_dir(&root)?;
    let default = repo.named(PoolName::default());
    let mut pools = Vec::new();
    if has_records(&default)? {
        pools.push(default);
    }
    let names = root.join("pools");
    if exists(&names)? {
        repository::owned_dir(&names)?;
        for item in fs::read_dir(names)? {
            let item = item?;
            let Some(name) = item
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<PoolName>().ok())
            else {
                continue;
            };
            if name.is_default() {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    "The default pool must use the original state directory",
                ));
            }
            let selected = repo.named(name);
            repository::owned_dir(&selected.root)?;
            if has_records(&selected)? {
                pools.push(selected);
            }
        }
    }
    pools.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(pools)
}

pub(crate) fn config(repo: &Repository) -> Result<Option<Config>> {
    if !exists(&repo.config_path())? {
        if lifecycle::read(repo)?.is_some_and(|r| r.phase != PoolPhase::Initializing) {
            return Err(Error::new(ErrorCode::ConfigConflict, "Recorded pool manifest is missing; restore it before changing repository root claims")
                .with_details(|d| d.path = Some(repo.config_path())));
        }
        return Ok(None);
    }
    let config: Config = state::read(&repo.config_path())?;
    config.validate()?;
    if !config.worktree_root.is_absolute()
        || crate::pool::overlaps_administration(repo, &config.worktree_root)
    {
        return Err(Error::new(
            ErrorCode::ConfigConflict,
            "Unsafe pool checkout root",
        ));
    }
    Ok(Some(config))
}

pub(crate) fn intent(repo: &Repository) -> Result<Option<InitIntent>> {
    let path = repo.root.join("initializing.json");
    if !exists(&path)? {
        return Ok(None);
    }
    let intent: InitIntent = state::read(&path)?;
    if intent.schema_version != state::SCHEMA_VERSION
        || !(1..=crate::MAX_CAPACITY).contains(&intent.capacity)
        || !intent.worktree_root.is_absolute()
        || crate::pool::overlaps_administration(repo, &intent.worktree_root)
    {
        return Err(Error::new(
            ErrorCode::UnsupportedConfiguration,
            "Invalid pool initialization intent",
        ));
    }
    Ok(Some(intent))
}

pub(crate) fn summary(repo: &Repository) -> Result<PoolSummary> {
    let lifecycle = lifecycle::read(repo)?;
    let config = config(repo)?;
    let intent = intent(repo)?;
    let pending = intent.is_some()
        && lifecycle
            .as_ref()
            .is_none_or(|r| matches!(r.phase, PoolPhase::Initializing | PoolPhase::Deleted));
    let phase = if pending {
        PoolPhase::Initializing
    } else {
        lifecycle.as_ref().map_or(PoolPhase::Ready, |r| r.phase)
    };
    let (capacity, cleanup, root) = if pending {
        intent.map(|i| (i.capacity, i.cleanup_policy, i.worktree_root))
    } else {
        config.map(|c| (c.capacity, c.cleanup_policy, c.worktree_root))
    }
    .map_or((None, None, None), |(c, p, r)| (Some(c), Some(p), Some(r)));
    Ok(PoolSummary {
        name: repo.name.clone(),
        pool_id: lifecycle
            .as_ref()
            .filter(|r| !(pending && r.phase == PoolPhase::Deleted))
            .map(|r| r.pool_id),
        phase,
        state_dir: repo.root.clone(),
        capacity,
        cleanup_policy: cleanup,
        worktree_root: root,
    })
}

/// Open the shared lock without ever recreating one recorded by any pool.
pub(crate) fn admin(repo: &Repository, timeout: Duration) -> Result<SlotGuard> {
    repository::mkdir(&repo.namespace_root())?;
    repository::owned_dir(&repo.namespace_root())?;
    repository::mkdir(&repo.namespace_root().join("locks"))?;
    repository::owned_dir(&repo.namespace_root().join("locks"))?;
    // Even an unpublished initialization intent proves the administrative lock
    // was created and synced before it. Missing published infrastructure is corruption.
    let create = repositories(repo)?.is_empty();
    let path = repo.admin_lock_path();
    let file = locking::open(&path, create)?;
    file.sync_all()?;
    state::sync_dir(path.parent().expect("administrative lock has a parent"))?;
    let identity = locking::identity(&file)?;
    let guard = SlotGuard::wait(&path, Some(identity), timeout)?;
    // Re-read after locking: another namespace may have published during the wait.
    for pool in repositories(repo)? {
        if config(&pool)?.is_some_and(|c| c.admin_identity != identity) {
            return Err(Error::new(
                ErrorCode::ConfigConflict,
                "Repository administrative lock identity changed",
            ));
        }
    }
    Ok(guard)
}

/// Under repository admin exclusion, check both published and pending roots,
/// including retained deleted pools. Never authorize overlapping ownership.
pub(crate) fn validate_root(repo: &Repository, root: &Path) -> Result<()> {
    validate_external_claim(repo, root)?;
    for other in repositories(repo)?
        .into_iter()
        .filter(|other| other.name != repo.name)
    {
        let mut roots = Vec::new();
        if let Some(config) = config(&other)? {
            roots.push(config.worktree_root);
        }
        if let Some(intent) = intent(&other)? {
            roots.push(intent.worktree_root);
        }
        for claimed in roots {
            if overlaps(root, &claimed)? {
                return Err(Error::new(
                    ErrorCode::ConfigConflict,
                    format!("Checkout root overlaps pool '{}'", other.name),
                )
                .with_details(|d| d.path = Some(claimed)));
            }
        }
    }
    Ok(())
}

const ROOT_CLAIM: &str = ".groove-owner";

fn validate_external_claim(repo: &Repository, root: &Path) -> Result<()> {
    for ancestor in root.ancestors() {
        let marker = ancestor.join(ROOT_CLAIM);
        let conflict = || {
            Error::new(
                ErrorCode::ConfigConflict,
                "Checkout root is claimed by another repository or has an invalid owner marker",
            )
            .with_details(|details| details.path = Some(marker.clone()))
        };
        match fs::read_link(&marker) {
            Ok(owner) if owner == repo.namespace_root() => (),
            Ok(_) => return Err(conflict()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(conflict().with_source(error)),
        }
    }
    Ok(())
}

/// Atomically claim an external root across independent Git repositories. The
/// immutable symlink stores the canonical repository state path without following it. It
/// coordinates namespace allocation only and never grants lease ownership.
pub(crate) fn claim_root(repo: &Repository, root: &Path) -> Result<()> {
    match std::os::unix::fs::symlink(repo.namespace_root(), root.join(ROOT_CLAIM)) {
        Ok(()) => state::sync_dir(root),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            validate_external_claim(repo, root)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn root_has_contents(repo: &Repository, root: &Path) -> Result<bool> {
    validate_external_claim(repo, root)?;
    for entry in fs::read_dir(root)? {
        if entry?.file_name() != ROOT_CLAIM {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(crate) fn managed_pool(
    repo: &Repository,
    checkout: &Path,
    timeout: Duration,
) -> Result<Option<Pool>> {
    for candidate in repositories(repo)? {
        if let Some(config) = config(&candidate)? {
            if checkout.parent() == Some(config.worktree_root.as_path())
                && checkout
                    .file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.parse::<crate::SlotId>().ok())
                    .is_some_and(|slot| config.slots.contains_key(&slot))
            {
                return Pool::load(candidate, timeout).map(Some);
            }
        }
    }
    Ok(None)
}

pub(crate) fn recovery_destination(repo: &Repository, destination: &Path) -> Result<PathBuf> {
    // Resolve existing parent aliases (notably /tmp on macOS) before comparing
    // claims. Pass this same resolved path to Git, including for relative ../ paths.
    let mut ancestor = destination;
    let mut suffix = Vec::new();
    while !exists(ancestor)? {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::InvalidArgument,
                        "Recovery parent cannot be resolved",
                    )
                })?
                .to_owned(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| Error::new(ErrorCode::InvalidArgument, "Recovery path has no parent"))?;
    }
    let mut destination = fs::canonicalize(ancestor)?;
    for part in suffix.into_iter().rev() {
        destination.push(part);
    }
    repository::no_symlinks(&destination)?;
    if destination.starts_with(&repo.common) || repo.common.starts_with(&destination) {
        return Err(Error::new(
            ErrorCode::InvalidArgument,
            "Recovery destination overlaps Git administration",
        ));
    }
    for other in repositories(repo)? {
        let mut roots = Vec::new();
        if let Some(config) = config(&other)? {
            roots.push(config.worktree_root);
        }
        if let Some(intent) = intent(&other)? {
            roots.push(intent.worktree_root);
        }
        for root in roots {
            if overlaps(&destination, &root)? {
                return Err(Error::new(
                    ErrorCode::InvalidArgument,
                    format!("Recovery destination overlaps pool '{}'", other.name),
                ));
            }
        }
    }
    Ok(destination)
}

fn overlaps(a: &Path, b: &Path) -> Result<bool> {
    use std::os::unix::fs::MetadataExt;
    if a.starts_with(b) || b.starts_with(a) {
        return Ok(true);
    }
    // Case-insensitive filesystems can resolve different spellings to the same
    // directory. Check actual endpoint identities against the other's ancestors.
    for (endpoint, candidate) in [(a, b), (b, a)] {
        let endpoint = match fs::metadata(endpoint) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for path in candidate.ancestors() {
            match fs::metadata(path) {
                Ok(m) if m.dev() == endpoint.dev() && m.ino() == endpoint.ino() => return Ok(true),
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(false)
}

impl Pool {
    /// List all namespaces, including interrupted initialization/deletion and retained
    /// deleted pools. This observation never proves ownership or permits cleanup.
    /// # Errors
    /// Reports invalid metadata, unsafe paths, discovery and I/O failures.
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// let pools = groove_core::Pool::list_pools("/path/to/repo", groove_core::DEFAULT_GIT_TIMEOUT)?;
    /// # let _ = pools;
    /// # Ok(()) }
    /// ```
    pub fn list_pools(repo: impl AsRef<Path>, timeout: Duration) -> Result<Vec<PoolSummary>> {
        if timeout.is_zero() {
            return Err(Error::new(
                ErrorCode::InvalidArgument,
                "Git timeout must be positive",
            ));
        }
        let repo = Repository::discover(repo.as_ref(), timeout)?;
        repositories(&repo)?.iter().map(summary).collect()
    }
}
