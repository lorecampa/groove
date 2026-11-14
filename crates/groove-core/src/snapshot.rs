//! Local checkpoints stored as reachable Git objects, independent of pool initialization.
use crate::{
    git::{Git, Invocation},
    locking, repository, Error, ErrorCode, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs,
    io::{Seek, Write},
    os::{
        fd::{AsFd, AsRawFd},
        unix::ffi::OsStrExt,
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const PREFIX: &str = "refs/groove/snapshots/";
mod pins;

use pins::PINS;
/// Tracked paths whose working files differ from the index.
const DIFF_FILES_ARGS: &[&str] = &[
    "diff-files",
    "--name-only",
    "--no-ext-diff",
    "--no-textconv",
    "--no-renames",
    "-z",
    "--",
];
/// Untracked, non-ignored paths.
const UNTRACKED_ARGS: &[&str] = &["ls-files", "--others", "--exclude-standard", "-z"];

/// File identity used to detect writers racing a capture: size, mtime (s, ns) and mode.
type Stamp = (u64, i64, i64, u32);

/// Workspace state recorded at the start of a capture.
struct Observation {
    head: Option<String>,
    index: Option<Vec<u8>>,
    changed: Vec<u8>,
    untracked: Vec<u8>,
    stamps: Vec<(PathBuf, Option<Stamp>)>,
}
fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidArgument, message)
}
fn unsupported(message: impl Into<String>) -> Error {
    Error::new(ErrorCode::UnsupportedConfiguration, message)
}
fn text(bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes)
        .map(|s| s.trim_end_matches('\n').to_owned())
        .map_err(|_| unsupported("Git metadata must be UTF-8"))
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn paths(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split(|b| *b == 0).filter(|p| !p.is_empty())
}
fn checked_id(id: &str) -> Result<String> {
    let uuid = Uuid::parse_str(id).map_err(|_| invalid("Snapshot ID must be a UUID"))?;
    Ok(uuid.to_string())
}

/// A saved checkpoint. File paths in Git trees retain their original bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Manifest version; unknown versions are rejected.
    pub version: u32,
    /// Globally unique snapshot identifier.
    pub id: String,
    /// Creation time in Unix seconds.
    pub created_at: u64,
    /// Nanosecond creation order, used when multiple saves occur within a second.
    pub created_at_ns: u128,
    /// User description.
    pub description: String,
    /// Original worktree Git directory.
    pub workspace: String,
    /// Original branch, or null for detached HEAD.
    pub branch: Option<String>,
    /// Original commit, or null before the first commit.
    pub head: Option<String>,
    /// Saved staging tree.
    pub index_tree: String,
    /// Saved working files including eligible untracked files.
    pub work_tree: String,
    /// Blob containing NUL-delimited untracked paths.
    pub untracked_blob: String,
    /// Optional caller task or session identifier.
    pub session: Option<String>,
    /// Whether `release --save` pinned this snapshot; deleting it then requires `force`.
    #[serde(default)]
    pub pinned: bool,
}
/// Capture settings. A zero size limit explicitly permits arbitrary file sizes.
#[derive(Debug, Clone)]
pub struct SaveOptions {
    /// Description shown by list/show.
    pub description: String,
    /// Optional session association.
    pub session: Option<String>,
    /// Maximum size of each changed file; defaults to 100 MiB.
    pub max_file_bytes: u64,
}

/// Where a snapshot was restored.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreResult {
    /// Directory containing the restored files.
    pub path: PathBuf,
    /// Restored snapshot.
    pub snapshot_id: String,
    /// Safety snapshot taken before an in-place restore.
    pub safety_snapshot_id: Option<String>,
}

impl Default for SaveOptions {
    fn default() -> Self {
        Self {
            description: String::new(),
            session: None,
            max_file_bytes: 100 * 1024 * 1024,
        }
    }
}
/// Repository-scoped service. Operations serialize through an advisory lock.
/// External writers must be quiescent for reliable capture/restore.
#[derive(Debug)]
pub struct Snapshots {
    root: PathBuf,
    common: PathBuf,
    git_dir: PathBuf,
    state: PathBuf,
    timeout: Duration,
    pool: Option<crate::Pool>,
    repository: repository::Repository,
}

struct SnapshotLock {
    file: fs::File,
    admission: Option<fs::File>,
    admin: Option<crate::locking::SlotGuard>,
}
impl AsRawFd for SnapshotLock {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.file.as_raw_fd()
    }
}
impl SnapshotLock {
    fn descriptors(&self) -> Vec<std::os::fd::RawFd> {
        std::iter::once(self.file.as_raw_fd())
            .chain(self.admission.as_ref().map(AsRawFd::as_raw_fd))
            .chain(self.admin.as_ref().map(|g| g.file.as_raw_fd()))
            .collect()
    }
}
impl Snapshots {
    /// Read a saved-release snapshot without depending on a surviving checkout.
    /// The caller retains pool lifecycle admission; refs belong to the common repo.
    pub(crate) fn saved_release_snapshot(
        repository: &repository::Repository,
        timeout: Duration,
        id: &str,
    ) -> Result<Snapshot> {
        let service = Self {
            root: repository.common.clone(),
            common: repository.common.clone(),
            git_dir: repository.common.clone(),
            state: repository.common.join("groove-snapshots"),
            timeout,
            pool: None,
            repository: repository.clone(),
        };
        let snapshot = service.show(id)?;
        if !snapshot.pinned {
            return Err(invalid("Saved release snapshot is no longer pinned"));
        }
        Ok(snapshot)
    }

    /// Discover a non-bare Git worktree, without initializing a Groove pool.
    /// The timeout bounds each Git subprocess and each wait for the shared snapshot lock.
    ///
    /// # Errors
    /// Reports repository discovery, filesystem and Git failures, including bare
    /// repositories and unsupported repository layouts. No pool is initialized.
    pub fn open(path: impl AsRef<Path>, timeout: Duration) -> Result<Self> {
        let git = Git::new(timeout);
        let repo = repository::Repository::discover(path.as_ref(), timeout)?;
        let root = PathBuf::from(text(git.run(
            path.as_ref(),
            &["rev-parse", "--show-toplevel"],
            &[],
        )?)?);
        let git_dir = PathBuf::from(text(git.run(
            &root,
            &["rev-parse", "--absolute-git-dir"],
            &[],
        )?)?);
        let state = repo.common.join("groove-snapshots");
        let pool = if git_dir != repo.common {
            crate::namespace::managed_pool(&repo, &root, timeout)?
        } else {
            None
        };
        repository::mkdir(&state)?;
        let _ = locking::open(&state.join("lock"), true)?;
        Ok(Self {
            root,
            common: repo.common.clone(),
            git_dir,
            state,
            timeout,
            pool,
            repository: repo,
        })
    }
    fn lock(&self) -> Result<SnapshotLock> {
        let admission = self.pool.as_ref().map(crate::Pool::operation).transpose()?;
        let file = locking::open(&self.state.join("lock"), false)?;
        let started = std::time::Instant::now();
        let mut first_contention = true;
        while !locking::flock(file.as_fd(), libc::LOCK_EX | libc::LOCK_NB)? {
            if first_contention {
                crate::state::checkpoint("snapshot-lock-contended");
                first_contention = false;
            }
            if started.elapsed() >= self.timeout {
                let mut error =
                    invalid("Another snapshot operation is running; retry when it finishes");
                error.retryable = true;
                return Err(error);
            }
            // Other tasks and surviving mutation children retain this permanent lock.
            // Honor the caller's wait budget without treating age as ownership.
            std::thread::sleep(
                Duration::from_millis(2).min(self.timeout.saturating_sub(started.elapsed())),
            );
        }
        // Only the lock owner can have active scratch data. Mutation children retain it.
        for entry in fs::read_dir(&self.state)? {
            let entry = entry?;
            if entry.file_name().as_bytes().starts_with(b"scratch-") {
                if entry.file_type()?.is_dir() {
                    fs::remove_dir_all(entry.path())?;
                } else {
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(SnapshotLock {
            file,
            admission,
            admin: None,
        })
    }
    fn run(
        &self,
        args: &[&str],
        index: Option<&Path>,
        input: Option<&[u8]>,
        lock: Option<&SnapshotLock>,
    ) -> Result<Vec<u8>> {
        self.run_env(args, index, input, lock, &[], None)
    }
    fn run_env(
        &self,
        args: &[&str],
        index: Option<&Path>,
        input: Option<&[u8]>,
        lock: Option<&SnapshotLock>,
        extra: &[(&str, &OsStr)],
        prepared: Option<Box<dyn FnOnce(Duration) -> Result<()> + '_>>,
    ) -> Result<Vec<u8>> {
        let mut env = vec![
            ("GIT_AUTHOR_NAME", OsStr::new("Groove Snapshot")),
            ("GIT_AUTHOR_EMAIL", OsStr::new("snapshot@groove.invalid")),
            ("GIT_COMMITTER_NAME", OsStr::new("Groove Snapshot")),
            ("GIT_COMMITTER_EMAIL", OsStr::new("snapshot@groove.invalid")),
            ("GIT_LITERAL_PATHSPECS", OsStr::new("1")),
        ];
        if let Some(index) = index {
            env.push(("GIT_INDEX_FILE", index.as_os_str()));
        }
        env.extend_from_slice(extra);
        let input = input
            .map(|bytes| -> Result<fs::File> {
                let mut f = tempfile::tempfile()?;
                f.write_all(bytes)?;
                f.rewind()?;
                Ok(f)
            })
            .transpose()?;
        let fds = lock.map(SnapshotLock::descriptors).unwrap_or_default();
        let mut durable_args = vec![
            "-c",
            "core.fsync=loose-object,reference",
            "-c",
            "core.fsyncMethod=fsync",
            // Snapshots record physical working files, independently of checkout
            // preferences. Keep user ignore/attribute rules, but never normalize
            // bytes, conceal chmod changes, or materialize symlinks as text files.
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.filemode=true",
            "-c",
            "core.symlinks=true",
        ];
        durable_args.extend_from_slice(args);
        let invocation = Invocation {
            inherit: &fds,
            env: &env,
            stdin: input,
            prepared,
            // Git can exit successfully after skipping unreadable directories.
            // An incomplete untracked inventory must never authorize publication.
            reject_stderr: args == UNTRACKED_ARGS,
            ..Invocation::snapshot()
        };
        Git::new(self.timeout).run_with(&self.root, &durable_args, invocation)
    }

    fn string(&self, args: &[&str]) -> Result<String> {
        text(self.run(args, None, None, None)?)
    }
    fn head(&self) -> Result<Option<String>> {
        let value = self.string(&["rev-parse", "--verify", "--quiet", "HEAD"]);
        match value {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.git_exit_code() == Some(1) => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn reference(id: &str) -> Result<String> {
        Ok(format!("{PREFIX}{}", checked_id(id)?))
    }
    fn load(&self, id: &str, lock: &SnapshotLock) -> Result<Snapshot> {
        Ok(self.load_at(id, lock)?.0)
    }

    fn load_at(&self, id: &str, lock: &SnapshotLock) -> Result<(Snapshot, String)> {
        let reference = Self::reference(id)?;
        let oid = self.string(&["rev-parse", "--verify", "--end-of-options", &reference])?;
        let _: crate::CommitOid = oid.parse()?;
        let commit_oid = &oid;
        let commit = self.run(&["cat-file", "commit", &oid], None, None, None)?;
        let message = commit
            .windows(2)
            .position(|w| w == b"\n\n")
            .ok_or_else(|| invalid("Malformed snapshot commit"))?
            + 2;
        let mut snapshot: Snapshot = serde_json::from_slice(&commit[message..])?;
        if snapshot.version != 1 || snapshot.id != checked_id(id)? {
            return Err(invalid("Unsupported or inconsistent snapshot manifest"));
        }
        // Validate object types and relationships before using stored metadata in commands.
        for (name, oid, kind) in [
            ("index", &snapshot.index_tree, "tree"),
            ("work", &snapshot.work_tree, "tree"),
            ("untracked", &snapshot.untracked_blob, "blob"),
        ] {
            if !oid.bytes().all(|b| b.is_ascii_hexdigit()) || ![40, 64].contains(&oid.len()) {
                return Err(invalid("Invalid snapshot object ID"));
            }
            if self.string(&["rev-parse", &format!("{commit_oid}:{name}")])? != *oid
                || self.string(&["cat-file", "-t", oid])? != kind
            {
                return Err(invalid("Snapshot object graph does not match its manifest"));
            }
        }
        if let Some(head) = &snapshot.head {
            if self.string(&["rev-parse", &format!("{commit_oid}^1")])? != *head {
                return Err(invalid("Snapshot base does not match its parent"));
            }
        }
        snapshot.pinned = self.pinned(&snapshot.id, &oid, lock)?;
        Ok((snapshot, oid))
    }
    /// Read and validate one snapshot.
    ///
    /// # Errors
    /// Reports invalid IDs, missing or malformed snapshots, inconsistent pins,
    /// lock contention, and Git or filesystem failures.
    pub fn show(&self, id: &str) -> Result<Snapshot> {
        let lock = self.lock()?;
        self.load(id, &lock)
    }
    fn list_inner(&self, lock: &SnapshotLock) -> Result<Vec<Snapshot>> {
        let refs = self.string(&[
            "for-each-ref",
            "--format=%(refname) %(objectname) %(contents:subject)",
            PREFIX,
        ])?;
        let pins = self.pin_inventory(PINS, lock, self.timeout)?;
        let mut absent = Vec::new();
        let mut result = Vec::new();
        for line in refs.lines() {
            let (reference, rest) = line
                .split_once(' ')
                .ok_or_else(|| invalid("Invalid snapshot reference metadata"))?;
            let (oid, message) = rest
                .split_once(' ')
                .ok_or_else(|| invalid("Invalid snapshot reference metadata"))?;
            let _: crate::CommitOid = oid.parse()?;
            let mut snapshot: Snapshot = serde_json::from_str(message)?;
            if snapshot.version != 1 || reference != Self::reference(&snapshot.id)? {
                return Err(invalid("Unsupported or inconsistent snapshot manifest"));
            }
            let pin = format!("{PINS}{}", snapshot.id);
            snapshot.pinned = match pins.get(&pin) {
                Some(pin_oid) if pin_oid == oid => true,
                Some(_) => {
                    return Err(Error::new(
                        ErrorCode::GitError,
                        "Snapshot pin does not match its snapshot commit",
                    ))
                }
                None => {
                    absent.push((pin, oid.len()));
                    false
                }
            };
            result.push(snapshot);
        }
        self.verify_absent_pins(&absent, lock)?;
        result.sort_by(|a, b| {
            b.created_at_ns
                .cmp(&a.created_at_ns)
                .then_with(|| b.id.cmp(&a.id))
        });
        Ok(result)
    }
    /// List checkpoints across all worktrees, newest first.
    ///
    /// # Errors
    /// Reports invalid snapshot manifests, unreadable or inconsistent pins,
    /// lock contention, and Git or filesystem failures. An incomplete pin
    /// inventory is an error rather than evidence that snapshots are unpinned.
    pub fn list(&self) -> Result<Vec<Snapshot>> {
        let lock = self.lock()?;
        self.list_inner(&lock)
    }
    fn validate(&self) -> Result<()> {
        // A cleanly merged index still has unfinished history. The snapshot
        // format does not record MERGE_HEAD, so saving it would lose a parent.
        if self.git_dir.join("MERGE_HEAD").try_exists()? {
            return Err(unsupported(
                "Complete or abort the merge before taking a snapshot",
            ));
        }
        // One index enumeration supplies conflict, mode, flag and attribute-path checks.
        let sparse = match self.string(&["config", "--type=bool", "--get", "core.sparseCheckout"]) {
            Ok(value) => value == "true",
            Err(error) if error.git_exit_code() == Some(1) => false,
            Err(error) => return Err(error),
        };
        let empty_blob = self.string(&["hash-object", "--stdin"])?;
        let entries = self.run(&["ls-files", "-v", "--stage", "-z"], None, None, None)?;
        let mut names = Vec::new();
        let mut may_have_intent = false;
        for record in paths(&entries) {
            let tab = record
                .iter()
                .position(|b| *b == b'\t')
                .ok_or_else(|| invalid("Malformed Git index entry"))?;
            let header = &record[..tab];
            if header.last() != Some(&b'0') {
                return Err(unsupported(
                    "Resolve merge conflicts before taking a snapshot",
                ));
            }
            if record.get(2..9) == Some(b"160000 ") {
                return Err(unsupported("Submodules are not yet supported by snapshots"));
            }
            if record.first().is_some_and(u8::is_ascii_lowercase) {
                return Err(unsupported(
                    "Clear assume-unchanged index flags before saving",
                ));
            }
            if record.starts_with(b"S ") && !sparse {
                return Err(unsupported(
                    "Clear manual skip-worktree flags before saving",
                ));
            }
            may_have_intent |= header
                .split(|b| *b == b' ')
                .any(|part| part == empty_blob.as_bytes());
            names.extend_from_slice(&record[tab + 1..]);
            names.push(0);
        }
        drop(entries);
        names.extend_from_slice(&self.run(UNTRACKED_ARGS, None, None, None)?);
        self.validate_attributes(&names, None, false, None)?;
        if may_have_intent
            && self.run(
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "-z",
                    "--ita-visible-in-index",
                    "--",
                ],
                None,
                None,
                None,
            )? != self.run(
                &[
                    "diff",
                    "--cached",
                    "--name-only",
                    "-z",
                    "--ita-invisible-in-index",
                    "--",
                ],
                None,
                None,
                None,
            )?
        {
            return Err(unsupported(
                "Stage intent-to-add entries before saving a snapshot",
            ));
        }
        Ok(())
    }

    /// Reject active conversion rules, rather than harmless installed filter definitions.
    /// A scratch index lets restore check the saved attributes before checkout runs them.
    fn validate_attributes(
        &self,
        names: &[u8],
        index: Option<&Path>,
        cached: bool,
        lock: Option<&SnapshotLock>,
    ) -> Result<()> {
        let mut args = vec!["check-attr", "--all", "-z", "--stdin"];
        if cached {
            args.push("--cached");
        }
        let attrs = self.run(&args, index, Some(names), lock)?;
        // Unlike path lists, attribute triples can contain empty values (diff=).
        // Drop only the final record terminator, never an empty field.
        if attrs.is_empty() {
            return Ok(());
        }
        let attrs = attrs
            .strip_suffix(&[0])
            .ok_or_else(|| unsupported("Malformed Git attribute response"))?;
        let fields: Vec<_> = attrs.split(|b| *b == 0).collect();
        let (triples, remainder) = fields.as_chunks::<3>();
        if !remainder.is_empty() {
            return Err(unsupported("Malformed Git attribute response"));
        }
        for attr in triples {
            if [
                b"text".as_slice(),
                b"crlf",
                b"eol",
                b"filter",
                b"working-tree-encoding",
                b"ident",
            ]
            .contains(&attr[1])
                && attr[2] != b"unset"
            {
                return Err(unsupported(
                    "Content-converting Git attributes are not yet supported by snapshots",
                ));
            }
        }
        Ok(())
    }

    fn validate_restore_attributes(&self, snapshot: &Snapshot, lock: &SnapshotLock) -> Result<()> {
        let temp = tempfile::Builder::new()
            .prefix("scratch-")
            .tempdir_in(&self.state)?;
        let index = temp.path().join("index");
        self.run(
            &["read-tree", &snapshot.work_tree],
            Some(&index),
            None,
            Some(lock),
        )?;
        let names = self.run(&["ls-files", "-z"], Some(&index), None, Some(lock))?;
        self.validate_attributes(&names, Some(&index), true, Some(lock))
    }
    fn index_bytes(&self) -> Result<Option<Vec<u8>>> {
        match fs::read(self.git_dir.join("index")) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    /// Stage every changed and untracked path into a scratch `index`, recording a
    /// stamp per path so a concurrent writer can be detected afterwards.
    fn stage_changes(
        &self,
        index: &Path,
        selected: &BTreeSet<Vec<u8>>,
        options: &SaveOptions,
        lock: &SnapshotLock,
    ) -> Result<Vec<(PathBuf, Option<Stamp>)>> {
        let mut pathspec = Vec::new();
        let mut removals = Vec::new();
        let mut stamps = Vec::new();
        for p in selected {
            let path = self.root.join(OsStr::from_bytes(p));
            let stamp = stamp(&path)?;
            if let Some((size, _, _, _)) = stamp {
                if options.max_file_bytes != 0 && size > options.max_file_bytes {
                    return Err(invalid(format!("Changed file {} exceeds {} bytes; raise --max-file-bytes or use 0 explicitly", path.display(), options.max_file_bytes)));
                }
                if fs::symlink_metadata(&path)?.is_dir()
                    && (path.join(".git").exists() || p.ends_with(b"/"))
                {
                    return Err(unsupported("Nested repositories are not supported"));
                }
            }
            let remove = stamp.is_none() || fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
            stamps.push((path, stamp));
            let list = if remove { &mut removals } else { &mut pathspec };
            list.extend_from_slice(p);
            list.push(0);
        }
        if !removals.is_empty() {
            self.run(
                &["update-index", "--force-remove", "-z", "--stdin"],
                Some(index),
                Some(&removals),
                Some(lock),
            )?;
        }
        if !pathspec.is_empty() {
            self.run(
                &["update-index", "--add", "--remove", "-z", "--stdin"],
                Some(index),
                Some(&pathspec),
                Some(lock),
            )?;
        }
        Ok(stamps)
    }
    /// Fail if HEAD, the real index or any captured path changed while capturing.
    fn ensure_stable(&self, before: &Observation, lock: &SnapshotLock) -> Result<()> {
        let changed = before.head != self.head()?
            || before.index != self.index_bytes()?
            || before.changed != self.run(DIFF_FILES_ARGS, None, None, Some(lock))?
            || before
                .stamps
                .iter()
                .any(|(p, s)| stamp(p).ok().as_ref() != Some(s))
            || before.untracked != self.run(UNTRACKED_ARGS, None, None, Some(lock))?;
        if changed {
            return Err(invalid(
                "Workspace changed during capture; stop writers and retry",
            ));
        }
        Ok(())
    }
    /// Store `snapshot` as a commit and publish its ref with compare-and-create.
    fn publish(&self, snapshot: &Snapshot, lock: &SnapshotLock) -> Result<()> {
        let tree = format!(
            "040000 tree {}\tindex\n040000 tree {}\twork\n100644 blob {}\tuntracked\n",
            snapshot.index_tree, snapshot.work_tree, snapshot.untracked_blob
        );
        let tree = text(self.run(&["mktree"], None, Some(tree.as_bytes()), Some(lock))?)?;
        let mut args = vec![
            "-c",
            "core.fsync=loose-object,reference",
            "commit-tree",
            &tree,
        ];
        if let Some(head) = &snapshot.head {
            args.extend(["-p", head]);
        }
        let commit = text(self.run(
            &args,
            None,
            Some(&serde_json::to_vec(snapshot)?),
            Some(lock),
        )?)?;
        crate::state::checkpoint("snapshot-before-publish");
        let zero = "0".repeat(commit.len());
        self.run(
            &[
                "-c",
                "core.fsync=loose-object,reference",
                "update-ref",
                &Self::reference(&snapshot.id)?,
                &commit,
                &zero,
            ],
            None,
            None,
            Some(lock),
        )?;
        crate::state::checkpoint("snapshot-published");
        Ok(())
    }
    fn capture(
        &self,
        options: &SaveOptions,
        lock: &SnapshotLock,
        publish: bool,
    ) -> Result<Snapshot> {
        self.validate()?;
        let head = self.head()?;
        let original = self.index_bytes()?;
        let temp = tempfile::Builder::new()
            .prefix("scratch-")
            .tempdir_in(&self.state)?;
        let index = temp.path().join("index");
        if let Some(bytes) = &original {
            fs::write(&index, bytes)?;
        } else {
            self.run(&["read-tree", "--empty"], Some(&index), None, Some(lock))?;
        }
        let index_tree = text(self.run(&["write-tree"], Some(&index), None, Some(lock))?)?;
        let untracked = self.run(UNTRACKED_ARGS, Some(&index), None, Some(lock))?;
        let changed = self.run(DIFF_FILES_ARGS, Some(&index), None, Some(lock))?;
        let selected: BTreeSet<Vec<u8>> = paths(&changed)
            .chain(paths(&untracked))
            .map(<[u8]>::to_vec)
            .collect();
        let stamps = self.stage_changes(&index, &selected, options, lock)?;
        let work_tree = text(self.run(&["write-tree"], Some(&index), None, Some(lock))?)?;
        crate::state::checkpoint("snapshot-captured");
        let observation = Observation {
            head,
            index: original,
            changed,
            untracked,
            stamps,
        };
        self.ensure_stable(&observation, lock)?;
        let untracked_blob = text(self.run(
            &["hash-object", "-w", "--stdin"],
            None,
            Some(&observation.untracked),
            Some(lock),
        )?)?;
        let snapshot = Snapshot {
            version: 1,
            id: Uuid::new_v4().to_string(),
            created_at: now(),
            created_at_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
            description: options.description.clone(),
            workspace: self.git_dir.to_string_lossy().into_owned(),
            branch: self.string(&["symbolic-ref", "-q", "HEAD"]).ok(),
            head: observation.head,
            index_tree,
            work_tree,
            untracked_blob,
            session: options.session.clone(),
            pinned: false,
        };
        if publish {
            self.publish(&snapshot, lock)?;
        }
        Ok(snapshot)
    }
    /// Capture without changing HEAD, working files or the real index.
    /// Preserves physical bytes, executable bits and symlinks independently of
    /// Git checkout preferences. Active content-converting attributes are rejected.
    ///
    /// ```no_run
    /// use groove_core::{snapshot::{SaveOptions, Snapshots}, DEFAULT_GIT_TIMEOUT};
    /// let snapshots = Snapshots::open("/path/to/repo", DEFAULT_GIT_TIMEOUT)?;
    /// let checkpoint = snapshots.save(SaveOptions {
    ///     description: "before refactor".into(),
    ///     ..Default::default()
    /// })?;
    /// println!("Saved {}", checkpoint.id);
    /// # Ok::<(), groove_core::Error>(())
    /// ```
    ///
    /// # Errors
    /// Rejects unsupported index states or attributes, oversized changed files,
    /// and workspace changes detected during capture. Also reports lock, Git
    /// and filesystem failures. Publication follows validation and the workspace
    /// stability checks; a lost response can still leave a published snapshot.
    pub fn save(&self, options: SaveOptions) -> Result<Snapshot> {
        let lock = self.lock()?;
        self.capture(&options, &lock, true)
    }
    /// Delete references only; pinned snapshots require explicit override.
    ///
    /// # Errors
    /// Reports invalid or missing snapshots, corrupt pins, and pinned snapshots
    /// when `force` is false. Reference comparison or pin validation failures
    /// abort the deletion transaction. Also reports lock, Git and filesystem
    /// failures. `force` does not bypass validation or reference comparisons.
    pub fn delete(&self, id: &str, force: bool) -> Result<()> {
        let lock = self.lock()?;
        self.delete_inner(id, force, &lock)
    }
    /// Produce a Git patch between checkpoints, or against current working files.
    ///
    /// # Errors
    /// Reports invalid or unreadable snapshots and pins, lock contention, Git
    /// failures, and non-UTF-8 patches. Comparing against working files also has
    /// the capture restrictions of [`Self::save`], without publishing a snapshot.
    pub fn diff(&self, id: &str, other: Option<&str>) -> Result<String> {
        let lock = self.lock()?;
        let a = self.load(id, &lock)?;
        let b = match other {
            Some(id) => self.load(id, &lock)?.work_tree,
            None => {
                let s = self.capture(
                    &SaveOptions {
                        description: "temporary diff".into(),
                        ..Default::default()
                    },
                    &lock,
                    false,
                )?;
                s.work_tree
            }
        };
        // Export an applicable patch independently of user display preferences.
        // Keep the trailing newlines: metadata's string helper would trim bytes
        // needed by git apply, including when the patch is delivered as JSON.
        String::from_utf8(self.run(
            &[
                "diff-tree",
                "-r",
                "-p",
                "--binary",
                "--full-index",
                "--no-commit-id",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--no-relative",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--unified=3",
                &a.work_tree,
                &b,
                "--",
            ],
            None,
            None,
            Some(&lock),
        )?)
        .map_err(|_| unsupported("Git patch must be UTF-8"))
    }

    pub(crate) fn matches_saved_files(&self, id: &str) -> Result<bool> {
        let lock = self.lock()?;
        let saved = self.load(id, &lock)?;
        if !saved.pinned {
            return Ok(false);
        }
        let current = self.capture(&SaveOptions::default(), &lock, false)?;
        Ok(saved.head == current.head
            && saved.index_tree == current.index_tree
            && saved.work_tree == current.work_tree
            && saved.untracked_blob == current.untracked_blob)
    }
    /// Restore in place, after a safety snapshot, or into a new checkout.
    /// In-place restore requires the original HEAD and stopped external writers.
    /// Saved bytes and file types do not depend on current Git checkout preferences;
    /// active conversion attributes are checked before materializing the result.
    ///
    /// # Errors
    /// Rejects unsafe or existing recovery destinations, unsupported attributes,
    /// ignored-file collisions, and in-place restores whose HEAD differs from
    /// the saved base. Also reports snapshot validation, lock, Git and filesystem
    /// failures. In-place restore errors after the safety snapshot is captured
    /// include its ID in the error details; a failed restore can leave partial
    /// working-file changes, so retain that snapshot for recovery.
    pub fn restore(&self, id: &str, destination: Option<&Path>) -> Result<RestoreResult> {
        let mut lock = self.lock()?;
        // A main-checkout recovery must also exclude root/settings replacement
        // between checking its destination and finishing the new worktree. Pass
        // that admission to mutation children even though the source is outside
        // the pool. Independent snapshot reads/saves remain pool-independent.
        if destination.is_some() {
            lock.admin = Some(crate::namespace::admin(&self.repository, self.timeout)?);
        }
        let s = self.load(id, &lock)?;
        if let Some(destination) = destination {
            let destination = if destination.is_absolute() {
                destination.to_path_buf()
            } else {
                std::env::current_dir()?.join(destination)
            };
            let destination =
                crate::namespace::recovery_destination(&self.repository, &destination)?;
            if destination.exists() {
                return Err(invalid("Recovery destination must not exist"));
            }
            // Check the saved tree plus effective shared/user attributes before
            // creating a recovery worktree or allowing checkout conversion/filtering.
            self.validate_restore_attributes(&s, &lock)?;
            let destination_text = repository::utf8(&destination)?;
            if let Some(head) = &s.head {
                self.run(
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        "--no-checkout",
                        destination_text,
                        head,
                    ],
                    None,
                    None,
                    Some(&lock),
                )?;
            } else {
                // An unborn source has no base for worktree add. Create an independent recovery repository.
                let format = self.string(&["rev-parse", "--show-object-format"])?;
                let mut init = vec!["init".to_owned(), format!("--object-format={format}")];
                if let Some(branch) = s.branch.as_deref() {
                    let branch = branch
                        .strip_prefix("refs/heads/")
                        .ok_or_else(|| invalid("Invalid unborn snapshot branch"))?;
                    // Git validates the saved name; an option value cannot become an argument.
                    init.push(format!("--initial-branch={branch}"));
                }
                init.push(destination_text.to_owned());
                let args: Vec<_> = init.iter().map(String::as_str).collect();
                self.run(&args, None, None, Some(&lock))?;
                Git::new(self.timeout).run(
                    &destination,
                    &[
                        "fetch",
                        "--no-tags",
                        repository::utf8(&self.common)?,
                        &Self::reference(id)?,
                    ],
                    &lock.descriptors(),
                )?;
            }
            let target = Self::open(&destination, self.timeout)?;
            target.apply(&s, None, &lock)?;
            return Ok(RestoreResult {
                path: destination.to_owned(),
                snapshot_id: id.to_owned(),
                safety_snapshot_id: None,
            });
        }
        if self.head()? != s.head {
            return Err(invalid(
                "HEAD differs from snapshot base; use restore --to <directory>",
            ));
        }
        self.validate()?;
        // Capture first so every deletion during apply has a recovery object.
        let safety = self.capture(
            &SaveOptions {
                description: format!("before restoring {id}"),
                ..Default::default()
            },
            &lock,
            true,
        )?;
        if safety.head != s.head {
            return Err(
                invalid("HEAD changed before restoration; no files were restored")
                    .with_details(|d| d.safety_snapshot_id = Some(safety.id.clone())),
            );
        }
        self.apply(&s, Some(&safety), &lock)
            .map_err(|e| e.with_details(|d| d.safety_snapshot_id = Some(safety.id.clone())))?;
        Ok(RestoreResult {
            path: self.root.clone(),
            snapshot_id: id.to_owned(),
            safety_snapshot_id: Some(safety.id),
        })
    }
    fn apply(&self, s: &Snapshot, current: Option<&Snapshot>, lock: &SnapshotLock) -> Result<()> {
        self.validate_restore_attributes(s, lock)?;
        // Git can overwrite ignored files during checkout. Reject collisions explicitly first.
        let ignored = self.run(
            &[
                "ls-files",
                "--others",
                "--ignored",
                "--exclude-standard",
                "-z",
            ],
            None,
            None,
            Some(lock),
        )?;
        let target = self.run(
            &["ls-tree", "-r", "--name-only", "-z", &s.work_tree],
            None,
            None,
            Some(lock),
        )?;
        // Existing working attributes may also affect paths absent from the
        // current index, so check every path about to be restored.
        self.validate_attributes(&target, None, false, Some(lock))?;
        let target: BTreeSet<Vec<u8>> = paths(&target).map(Vec::from).collect();
        for ignored in paths(&ignored) {
            if path_collision(&target, ignored) {
                return Err(invalid(
                    "Restore would overwrite an ignored file; move it before retrying",
                ));
            }
        }
        // Temporary index represents captured files; refresh stats before merging trees.
        let temp = tempfile::Builder::new()
            .prefix("scratch-")
            .tempdir_in(&self.state)?;
        let index = temp.path().join("index");
        if let Some(current) = current {
            self.run(
                &["read-tree", &current.work_tree],
                Some(&index),
                None,
                Some(lock),
            )?;
        } else {
            self.run(&["read-tree", "--empty"], Some(&index), None, Some(lock))?;
        }
        self.run(
            &["update-index", "--refresh"],
            Some(&index),
            None,
            Some(lock),
        )?;
        self.run(
            &["read-tree", "-m", "-u", &s.work_tree],
            Some(&index),
            None,
            Some(lock),
        )?;
        crate::state::checkpoint("snapshot-restored-files");
        self.run(&["read-tree", &s.index_tree], None, None, Some(lock))?;
        // Verify working contents using the materialized work tree, without mutating the real index.
        self.run(&["diff-files", "--quiet"], Some(&index), None, Some(lock))?;
        if self.string(&["write-tree"])? != s.index_tree {
            return Err(invalid("Restored index verification failed"));
        }
        Ok(())
    }
}

fn path_collision(target: &BTreeSet<Vec<u8>>, candidate: &[u8]) -> bool {
    if target.contains(candidate) {
        return true;
    }
    let mut prefix = candidate.to_vec();
    prefix.push(b'/');
    if target
        .range(prefix.clone()..)
        .next()
        .is_some_and(|p| p.starts_with(&prefix))
    {
        return true;
    }
    candidate
        .iter()
        .enumerate()
        .any(|(i, b)| *b == b'/' && target.contains(&candidate[..i]))
}
fn stamp(path: &Path) -> Result<Option<Stamp>> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(m) => Ok(Some((m.len(), m.mtime(), m.mtime_nsec(), m.mode()))),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn collisions_respect_path_boundaries() {
        let target: BTreeSet<Vec<u8>> = [b"a/b".to_vec(), b"file".to_vec()].into_iter().collect();
        for path in [b"a".as_slice(), b"a/b", b"file", b"file/child"] {
            assert!(path_collision(&target, path));
        }
        for path in [b"ab".as_slice(), b"a/bb", b"files", b"unrelated/child"] {
            assert!(!path_collision(&target, path));
        }
    }
    #[test]
    fn identifiers_cannot_escape_namespace() {
        for id in ["../heads/main", "HEAD", "--all", ""] {
            assert!(checked_id(id).is_err());
        }
        assert!(checked_id(&Uuid::new_v4().to_string()).is_ok());
    }
    #[test]
    fn nul_paths_preserve_bytes() {
        assert_eq!(
            paths(b"a\nb\0\xff\0").collect::<Vec<_>>(),
            vec![b"a\nb".as_slice(), b"\xff".as_slice()]
        );
    }
}
