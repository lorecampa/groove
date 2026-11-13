//! Immutable approved artifact snapshots. Cache locks are permanent and remain
//! outside every replaceable data directory. Cached artifacts confer no ownership.
use crate::{
    error::PathContext,
    git::Git,
    locking,
    project::{invalid, relative},
    repository, state, CacheMode, CacheProfile, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    path: PathBuf,
    kind: Kind,
    mode: u32,
    bytes: u64,
    hash: String,
    modified_ns: u128,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Kind {
    Directory,
    File,
    Link,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    key: String,
    profile: String,
    entries: Vec<Entry>,
}

struct Scan {
    entries: Vec<Entry>,
    bytes: u64,
    device: u64,
    hash_contents: bool,
}

pub(crate) struct Artifacts<'a> {
    root: PathBuf,
    common: PathBuf,
    key: String,
    name: &'a str,
    profile: &'a CacheProfile,
    started: Instant,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
    _guard: locking::SlotGuard,
}
impl<'a> Artifacts<'a> {
    pub(crate) fn open(
        pool: &crate::Pool,
        name: &'a str,
        profile: &'a CacheProfile,
        key: String,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self> {
        let root = pool.repo.root.join("artifacts");
        repository::mkdir(&root)?;
        repository::owned_dir(&root).at(&root)?;
        let lock = root.join(format!("{key}.lock"));
        let file = locking::open(&lock, true)?;
        file.sync_all()?;
        state::sync_dir(&root)?;
        let identity = locking::identity(&file)?;
        let waiting = Instant::now();
        let guard = loop {
            if cancelled.load(Ordering::Acquire) {
                return Err(invalid("Artifact lock wait cancelled"));
            }
            if let Some(guard) = locking::SlotGuard::try_lock(&lock, Some(identity))? {
                break guard;
            }
            if waiting.elapsed() >= timeout {
                return Err(invalid("Artifact lock wait timed out"));
            }
            std::thread::sleep(
                Duration::from_millis(5).min(timeout.saturating_sub(waiting.elapsed())),
            );
        };
        Ok(Self {
            root,
            common: pool.repo.common.clone(),
            key,
            name,
            profile,
            started: Instant::now(),
            timeout,
            cancelled,
            _guard: guard,
        })
    }
    fn deadline(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(invalid(
                "Artifact operation cancelled; the reservation remains protected",
            ));
        }
        if self.started.elapsed() >= self.timeout {
            return Err(invalid("Artifact snapshot operation timed out"));
        }
        Ok(())
    }
    fn path(&self) -> PathBuf {
        self.root.join(&self.key)
    }

    pub(crate) fn seed(&mut self, checkout: &Path) -> Result<Option<(u64, u64)>> {
        self.started = Instant::now();
        let path = self.path();
        repository::no_symlinks(&path)?;
        if !path.try_exists()? {
            self.validate_ignored(checkout)?;
            self.clean_roots(checkout)?;
            return Ok(None);
        }
        repository::owned_dir(&path).at(&path)?;
        let manifest: Manifest =
            state::read_bounded(&path.join("manifest.json"), 128 * 1024 * 1024)?;
        if manifest.schema_version != state::SCHEMA_VERSION
            || manifest.key != self.key
            || manifest.profile != self.name
        {
            return Err(invalid(
                "Artifact cache metadata is incompatible; use a new explicit cache key",
            ));
        }
        let source = path.join("data");
        let entries = self.scan(&source)?;
        if entries != manifest.entries {
            return Err(invalid(
                "Artifact cache contents changed; use a new explicit cache key",
            ));
        }
        self.validate_ignored(checkout)?;
        self.clean_roots(checkout)?;
        self.copy(&source, checkout, &entries)?;
        state::checkpoint("cache-seeded");
        Ok(Some(totals(&entries)))
    }

    fn clean_roots(&self, checkout: &Path) -> Result<()> {
        // Inspect ALL roots before the first removal, including on a miss or
        // retry. Cleanup needs structure, not hashes of bytes it will discard.
        self.inspect(checkout, false)?;
        // This explicitly requested setup discards only the allowlisted ignored
        // paths, never broad patterns or any tracked path. Descendant symlinks
        // are removed as links by remove_dir_all, never followed.
        for relative in &self.profile.paths {
            self.deadline()?;
            let target = checkout.join(relative);
            repository::no_symlinks(
                target
                    .parent()
                    .ok_or_else(|| invalid("Missing cache parent"))?,
            )
            .at(&target)?;
            match fs::symlink_metadata(&target) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    return Err(invalid("Cache roots cannot be symlinks"))
                }
                Ok(meta) if meta.is_dir() => fs::remove_dir_all(&target)?,
                Ok(meta) if meta.is_file() => fs::remove_file(&target)?,
                Ok(_) => return Err(invalid("Unsupported cache root")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn import(&mut self, source: &Path, checkout: &Path) -> Result<()> {
        self.started = Instant::now();
        if self.path().try_exists()? {
            return Ok(());
        }
        if fingerprint(source, self.profile)? != fingerprint(checkout, self.profile)? {
            return Err(invalid(
                "Artifact source has different compatibility inputs/toolchain key",
            ));
        }
        self.validate_ignored(source)?;
        // An imported checkout can have a different registration inventory from
        // this pool. Observe it without repairing or modifying the source.
        self.check_registered_roots(source, source)?;
        self.publish(source)
    }

    pub(crate) fn publish(&mut self, checkout: &Path) -> Result<()> {
        self.started = Instant::now();
        if self.path().try_exists()? {
            return Ok(());
        }
        self.deadline()?;
        let before = fingerprint(checkout, self.profile)?;
        let entries = self.scan(checkout)?;
        if !self
            .profile
            .paths
            .iter()
            .all(|path| entries.iter().any(|entry| entry.path == *path))
        {
            return Err(invalid(
                "Every approved cache root must exist after successful setup",
            ));
        }
        let stage = tempfile::Builder::new()
            .prefix(".cache-stage-")
            .tempdir_in(&self.root)?;
        fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
        repository::mkdir(&stage.path().join("data"))?;
        self.copy(checkout, &stage.path().join("data"), &entries)?;
        if entries != self.scan(checkout)? || before != fingerprint(checkout, self.profile)? {
            return Err(invalid(
                "Artifact source changed while publishing; stop writers and explicitly retry setup",
            ));
        }
        let manifest = Manifest {
            schema_version: state::SCHEMA_VERSION,
            key: self.key.clone(),
            profile: self.name.into(),
            entries,
        };
        if serde_json::to_vec(&manifest)?.len() > 128 * 1024 * 1024 {
            return Err(invalid("Artifact manifest exceeded 128 MiB"));
        }
        state::write(&stage.path().join("manifest.json"), &manifest)?;
        state::sync_dir(stage.path())?;
        state::checkpoint("before-cache-publish");
        fs::rename(stage.path(), self.path())?;
        state::sync_dir(&self.root)?;
        state::checkpoint("cache-published");
        Ok(())
    }
    pub(crate) fn validate_ignored(&self, checkout: &Path) -> Result<()> {
        for path in &self.profile.paths {
            relative(path)?;
            repository::no_symlinks(&checkout.join(path)).at(&checkout.join(path))?;
            let text = repository::utf8(path)?;
            if !Git::new(self.timeout)
                .run(
                    checkout,
                    &["--literal-pathspecs", "ls-files", "-z", "--", text],
                    &[],
                )?
                .is_empty()
            {
                return Err(invalid(
                    "Approved artifact paths must contain no tracked files",
                ));
            }
            let ignored = format!("{}/", text.trim_end_matches('/'));
            Git::new(self.timeout)
                .run(
                    checkout,
                    &["check-ignore", "--no-index", "--", text, &ignored],
                    &[],
                )
                .map_err(|error| {
                    invalid("Every artifact root must be ignored by Git before setup")
                        .with_source(error)
                })?;
        }
        Ok(())
    }
    fn scan(&self, root: &Path) -> Result<Vec<Entry>> {
        self.inspect(root, true)
    }
    fn inspect(&self, root: &Path, hash_contents: bool) -> Result<Vec<Entry>> {
        self.deadline()?;
        repository::no_symlinks(root)?;
        self.check_registered_roots(root, &self.common)?;
        let mut scan = Scan {
            entries: Vec::new(),
            bytes: 0,
            device: fs::symlink_metadata(root).at(root)?.dev(),
            hash_contents,
        };
        for path in &self.profile.paths {
            self.walk(root, path, path, &mut scan)
                .at(&root.join(path))?;
        }
        if hash_contents {
            scan.entries.sort_by(|a, b| a.path.cmp(&b.path));
        }
        self.deadline()?;
        Ok(scan.entries)
    }
    fn check_registered_roots(&self, root: &Path, git_cwd: &Path) -> Result<()> {
        self.deadline()?;
        let root = fs::canonicalize(root).at(root)?;
        // NUL records preserve spaces, newlines and Git's unquoted path bytes.
        // The ordinary Git capture bound and the remaining operation budget
        // bound this read. Registrations only cause refusal, never confer ownership.
        let inventory = Git::new(self.timeout.saturating_sub(self.started.elapsed())).run(
            git_cwd,
            &["worktree", "list", "--porcelain", "-z"],
            &[],
        )?;
        self.deadline()?;
        for record in inventory.split(|byte| *byte == 0) {
            self.deadline()?;
            if let Some(path) = record.strip_prefix(b"worktree ") {
                use std::os::unix::ffi::OsStrExt;
                let registered = Path::new(std::ffi::OsStr::from_bytes(path));
                for approved in &self.profile.paths {
                    if registered.starts_with(root.join(approved)) {
                        return Err(invalid(
                            "Artifact roots cannot contain registered Git checkouts; move the checkout safely before retrying setup",
                        )).at(registered);
                    }
                }
            }
        }
        Ok(())
    }
    fn reject_git_store(&self, directory: &Path) -> Result<()> {
        // Recognize ordinary and reftable bare stores by their administrative
        // shape. Inspect links themselves, never their targets or Git config.
        let kind = |name: &str| -> Result<Option<fs::FileType>> {
            self.deadline()?;
            let path = directory.join(name);
            match fs::symlink_metadata(&path) {
                Ok(meta) => Ok(Some(meta.file_type())),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(error).at(&path),
            }
        };
        if kind("HEAD")?.is_some_and(|kind| kind.is_file() || kind.is_symlink())
            && kind("objects")?.is_some_and(|kind| kind.is_dir() || kind.is_symlink())
            && (kind("refs")?.is_some_and(|kind| kind.is_dir() || kind.is_symlink())
                || kind("reftable")?.is_some_and(|kind| kind.is_dir() || kind.is_symlink()))
        {
            return Err(invalid(
                "Artifact roots cannot contain bare Git stores; move the repository safely before retrying setup",
            )).at(directory);
        }
        Ok(())
    }
    fn walk(&self, root: &Path, path: &Path, artifact: &Path, scan: &mut Scan) -> Result<()> {
        self.deadline()?;
        relative(path)?;
        if path.components().count() > 128
            || path.to_str().is_none()
            || scan.entries.len() as u64 >= self.profile.max_files
        {
            return Err(invalid(
                "Artifact path depth, encoding or entry limit exceeded",
            ));
        }
        let full = root.join(path);
        repository::no_symlinks(
            full.parent()
                .ok_or_else(|| invalid("Missing artifact parent"))?,
        )?;
        let meta = match fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && path == artifact => {
                return Ok(())
            }
            Err(error) => return Err(error.into()),
        };
        if meta.dev() != scan.device {
            return Err(invalid("Artifact paths cannot cross filesystem boundaries"));
        }
        let mode = meta.mode() & 0o777;
        let entry = if meta.file_type().is_symlink() {
            self.link_entry(path, &full, artifact, mode)?
        } else if meta.is_dir() {
            self.reject_git_store(&full)?;
            Entry {
                path: path.into(),
                kind: Kind::Directory,
                mode,
                bytes: 0,
                hash: String::new(),
                modified_ns: 0,
            }
        } else if meta.is_file() {
            self.file_entry(path, &full, &meta, mode, scan)?
        } else {
            return Err(invalid(
                "Artifact snapshots reject sockets, devices and FIFOs",
            ));
        };
        let directory = entry.kind == Kind::Directory;
        scan.entries.push(entry);
        if directory {
            let mut children = Vec::new();
            for child in fs::read_dir(full)? {
                self.deadline()?;
                if children.len() as u64 + scan.entries.len() as u64 >= self.profile.max_files {
                    return Err(invalid("Artifact entry limit exceeded"));
                }
                children.push(child?);
            }
            children.sort_by_key(|entry| entry.file_name());
            for child in children {
                let path = path.join(child.file_name());
                self.walk(root, &path, artifact, scan)
                    .at(&root.join(path))?;
            }
        }
        Ok(())
    }
    fn link_entry(&self, path: &Path, full: &Path, artifact: &Path, mode: u32) -> Result<Entry> {
        if path == artifact {
            return Err(invalid("Artifact roots cannot be symlinks"));
        }
        let target = fs::read_link(full)?;
        let mut normalized = path
            .parent()
            .ok_or_else(|| invalid("Missing symlink parent"))?
            .to_path_buf();
        for part in target.components() {
            match part {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => (),
                Component::ParentDir if normalized.pop() => (),
                _ => {
                    return Err(invalid(
                        "Artifact symlinks must remain within their approved root",
                    ))
                }
            }
        }
        if !normalized.starts_with(artifact) {
            return Err(invalid("Artifact symlink escapes its approved root"));
        }
        Ok(Entry {
            path: path.into(),
            kind: Kind::Link,
            mode,
            bytes: 0,
            hash: repository::utf8(&target)?.into(),
            modified_ns: 0,
        })
    }

    fn file_entry(
        &self,
        path: &Path,
        full: &Path,
        meta: &fs::Metadata,
        mode: u32,
        scan: &mut Scan,
    ) -> Result<Entry> {
        // Cleanup preflight must not read or hash soon-to-be-deleted files.
        // Publication and seeding retain their byte limits and verification.
        if !scan.hash_contents {
            return Ok(Entry {
                path: path.into(),
                kind: Kind::File,
                mode,
                bytes: 0,
                hash: String::new(),
                modified_ns: 0,
            });
        }
        scan.bytes = scan
            .bytes
            .checked_add(meta.len())
            .ok_or_else(|| invalid("Artifact byte limit overflow"))?;
        if scan.bytes > self.profile.max_bytes {
            return Err(invalid("Artifact byte limit exceeded"));
        }
        let hash = self.hash_file(full)?;
        let after = fs::symlink_metadata(full)?;
        if (
            meta.dev(),
            meta.ino(),
            meta.len(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec(),
        ) != (
            after.dev(),
            after.ino(),
            after.len(),
            after.mtime(),
            after.mtime_nsec(),
            after.ctime(),
            after.ctime_nsec(),
        ) {
            return Err(invalid("Artifact file changed while reading"));
        }
        let modified_ns = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| invalid("Artifact timestamp predates Unix epoch").with_source(error))?
            .as_nanos();
        Ok(Entry {
            path: path.into(),
            kind: Kind::File,
            mode,
            bytes: meta.len(),
            hash,
            modified_ns,
        })
    }

    fn hash_file(&self, path: &Path) -> Result<String> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid("Artifact inputs must remain regular files"));
        }
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 65536];
        loop {
            self.deadline()?;
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        Ok(crate::project::hex(hash.finalize()))
    }
    fn copy(&self, source: &Path, destination: &Path, entries: &[Entry]) -> Result<()> {
        for entry in entries {
            self.deadline()?;
            let from = source.join(&entry.path);
            let to = destination.join(&entry.path);
            repository::no_symlinks(
                to.parent()
                    .ok_or_else(|| invalid("Missing artifact destination parent"))?,
            )?;
            if let Some(parent) = to.parent() {
                repository::mkdir(parent)?;
            }
            match entry.kind {
                Kind::Directory => repository::mkdir(&to)?,
                Kind::Link => symlink(&entry.hash, &to)?,
                Kind::File => {
                    let mut input = OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(&from)?;
                    if !input.metadata()?.is_file() {
                        return Err(invalid("Artifact source is no longer regular"));
                    }
                    let mut output = if self.profile.mode == CacheMode::Reflink {
                        crate::platform::fs::clone_file(&input, &to)?
                    } else {
                        OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .custom_flags(libc::O_NOFOLLOW)
                            .open(&to)?
                    };
                    if self.profile.mode == CacheMode::Copy {
                        let mut copied = 0_u64;
                        let mut buffer = [0_u8; 65536];
                        loop {
                            self.deadline()?;
                            let n = input.read(&mut buffer)?;
                            if n == 0 {
                                break;
                            }
                            copied += n as u64;
                            if copied > entry.bytes {
                                return Err(invalid("Artifact grew during copy"));
                            }
                            output.write_all(&buffer[..n])?;
                        }
                    }
                    if output.metadata()?.len() != entry.bytes || self.hash_file(&to)? != entry.hash
                    {
                        return Err(invalid("Artifact contents changed during copy"));
                    }
                    output.set_permissions(fs::Permissions::from_mode(entry.mode))?;
                    let nanos =
                        u64::try_from(entry.modified_ns % 1_000_000_000).map_err(|error| {
                            invalid("Invalid artifact timestamp").with_source(error)
                        })?;
                    let seconds =
                        u64::try_from(entry.modified_ns / 1_000_000_000).map_err(|error| {
                            invalid("Invalid artifact timestamp").with_source(error)
                        })?;
                    output.set_times(fs::FileTimes::new().set_modified(
                        std::time::UNIX_EPOCH + Duration::new(seconds, nanos as u32),
                    ))?;
                    output.sync_all()?;
                }
            }
        }
        // Apply restrictive directory permissions after populating descendants.
        for entry in entries
            .iter()
            .rev()
            .filter(|entry| entry.kind == Kind::Directory)
        {
            let directory = destination.join(&entry.path);
            fs::set_permissions(&directory, fs::Permissions::from_mode(entry.mode))?;
            File::open(directory)?.sync_all()?;
        }
        state::sync_dir(destination)?;
        Ok(())
    }
}
fn totals(entries: &[Entry]) -> (u64, u64) {
    (
        entries.iter().map(|entry| entry.bytes).sum(),
        entries
            .iter()
            .filter(|entry| entry.kind != Kind::Directory)
            .count() as u64,
    )
}

pub(crate) fn fingerprint(root: &Path, profile: &CacheProfile) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(serde_json::to_vec(profile)?);
    hash.update(std::env::consts::OS);
    hash.update(std::env::consts::ARCH);
    for path in &profile.inputs {
        relative(path)?;
        let full = root.join(path);
        repository::no_symlinks(&full)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&full)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > 16 * 1024 * 1024 {
            return Err(invalid(
                "Cache compatibility inputs must be regular files of at most 16 MiB",
            ));
        }
        hash.update(repository::utf8(path)?.as_bytes());
        let mut digest = Sha256::new();
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(invalid("Cache input grew beyond 16 MiB"));
        }
        digest.update(bytes);
        hash.update(digest.finalize());
    }
    Ok(crate::project::hex(hash.finalize()))
}
