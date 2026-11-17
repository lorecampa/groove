//! Bounded parallel inventories of physical checkout entries.
use crate::{workspace_files::DirectoryState, Error, ErrorCode, Result};
use std::{
    fs,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Condvar, Mutex,
    },
};

#[derive(Clone, Copy, Default)]
pub(crate) struct Options {
    pub verified: Option<(i64, i64)>,
    pub attributes: bool,
    pub remove_specials: bool,
}

pub(crate) struct Inventory {
    /// Directory ctime changed since verification, including the root. Git status
    /// omits empty directories, so their residue needs a separate cleanup signal.
    pub directories_changed: bool,
    pub directories: Vec<DirectoryState>,
    pub touched: Vec<u8>,
    pub touched_complete: bool,
    pub specials: Vec<PathBuf>,
}

#[derive(Default)]
struct Partial {
    directories_changed: bool,
    directories: Vec<DirectoryState>,
    touched: Vec<u8>,
    specials: Vec<PathBuf>,
}

struct Shared {
    queue: Mutex<Vec<(PathBuf, bool)>>,
    wake: Condvar,
    pending: AtomicUsize,
    failed: AtomicBool,
    error: Mutex<Option<Error>>,
    touched_complete: AtomicBool,
    touched_bytes: AtomicUsize,
}
impl Shared {
    fn fail(&self, error: Error) {
        let _queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut first = self
            .error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        first.get_or_insert(error);
        self.failed.store(true, Ordering::Release);
        self.wake.notify_all();
    }
}

struct WakeOnPanic<'a>(&'a Shared);
impl Drop for WakeOnPanic<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            // A panicking worker must not leave peers waiting for its pending
            // item while the scoped parent waits to join those same peers.
            let _queue = self
                .0
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.0.failed.store(true, Ordering::Release);
            self.0.wake.notify_all();
        }
    }
}

pub(crate) fn inventory(root: &Path, options: Options) -> Result<Inventory> {
    let metadata = fs::symlink_metadata(root)?;
    let device = metadata.dev();
    let shared = Shared {
        queue: Mutex::new(vec![(root.into(), false)]),
        wake: Condvar::new(),
        pending: AtomicUsize::new(1),
        failed: AtomicBool::new(false),
        error: Mutex::new(None),
        touched_complete: AtomicBool::new(true),
        touched_bytes: AtomicUsize::new(0),
    };
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .clamp(1, 8);
    let partials = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for _ in 0..workers {
            let shared = &shared;
            match std::thread::Builder::new()
                .spawn_scoped(scope, move || worker(root, device, options, shared))
            {
                Ok(handle) => handles.push(handle),
                Err(error) => {
                    shared.fail(error.into());
                    break;
                }
            }
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
            .collect::<Vec<_>>()
    });
    if let Some(error) = shared
        .error
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        return Err(error);
    }
    let complete = shared.touched_complete.load(Ordering::Relaxed);
    let mut result = Inventory {
        directories_changed: false,
        directories: Vec::new(),
        touched: Vec::new(),
        touched_complete: complete,
        specials: Vec::new(),
    };
    for partial in partials {
        result.directories_changed |= partial.directories_changed;
        result.directories.extend(partial.directories);
        result.specials.extend(partial.specials);
        if complete {
            result.touched.extend(partial.touched);
        }
    }
    result.directories.sort_by(|a, b| a.path.cmp(&b.path));
    result.specials.sort();
    Ok(result)
}

fn worker(root: &Path, device: u64, options: Options, shared: &Shared) -> Partial {
    let _unwind = WakeOnPanic(shared);
    let mut partial = Partial::default();
    loop {
        let item = {
            let mut queue = shared
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            loop {
                if shared.failed.load(Ordering::Acquire) {
                    return partial;
                }
                if let Some(item) = queue.pop() {
                    break Some(item);
                }
                if shared.pending.load(Ordering::Relaxed) == 0 {
                    break None;
                }
                queue = shared
                    .wake
                    .wait(queue)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let Some((directory, ancestor_touched)) = item else {
            return partial;
        };
        let children = visit(
            root,
            &directory,
            device,
            ancestor_touched,
            options,
            shared,
            &mut partial,
        );
        let mut queue = shared
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match children {
            Ok(children) => {
                shared.pending.fetch_add(children.len(), Ordering::Relaxed);
                queue.extend(children);
            }
            Err(error) => {
                shared
                    .error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_or_insert(error);
                shared.failed.store(true, Ordering::Release);
            }
        }
        // Completion and notification share the queue mutex: otherwise a waiter
        // can observe pending work and miss the final wakeup before it sleeps.
        shared.pending.fetch_sub(1, Ordering::Relaxed);
        shared.wake.notify_all();
    }
}

fn visit(
    root: &Path,
    directory: &Path,
    device: u64,
    ancestor_touched: bool,
    options: Options,
    shared: &Shared,
    partial: &mut Partial,
) -> Result<Vec<(PathBuf, bool)>> {
    let metadata = fs::symlink_metadata(directory)?;
    if !metadata.is_dir() || metadata.dev() != device {
        return Err(Error::new(
            ErrorCode::RecoveryFailed,
            "Mounted or changed directory in workspace inventory",
        )
        .with_details(|details| details.path = Some(directory.into())));
    }
    if options.remove_specials {
        crate::workspace_files::make_directory_writable(directory, &metadata)?;
    }
    partial.directories_changed |= options
        .verified
        .is_none_or(|verified| (metadata.ctime(), metadata.ctime_nsec()) >= verified);
    let touched = ancestor_touched
        || (directory != root
            && options
                .verified
                .is_some_and(|verified| (metadata.ctime(), metadata.ctime_nsec()) >= verified));
    partial.directories.push(DirectoryState {
        path: directory
            .strip_prefix(root)
            .map_err(std::io::Error::other)?
            .as_os_str()
            .as_bytes()
            .to_vec(),
        mode: metadata.mode() & 0o7777,
        attributes: if options.attributes {
            crate::workspace_files::directory_attributes(directory)?
        } else {
            Vec::new()
        },
    });
    let mut children = Vec::new();
    for entry in fs::read_dir(directory)? {
        if shared.failed.load(Ordering::Acquire) {
            break;
        }
        let entry = entry?;
        if directory == root && entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            children.push((path, touched));
        } else if !kind.is_file() && !kind.is_symlink() {
            if options.remove_specials {
                fs::remove_file(path)?;
            } else {
                partial.specials.push(path);
            }
        } else if let Some(verified) = options
            .verified
            .filter(|_| shared.touched_complete.load(Ordering::Relaxed))
        {
            let metadata = fs::symlink_metadata(&path)?;
            if touched || (metadata.ctime(), metadata.ctime_nsec()) >= verified {
                let relative = path
                    .strip_prefix(root)
                    .map_err(std::io::Error::other)?
                    .as_os_str()
                    .as_bytes();
                let size = relative.len() + 1;
                if entry.file_name() == ".gitattributes"
                    || shared
                        .touched_bytes
                        .fetch_add(size, Ordering::Relaxed)
                        .saturating_add(size)
                        > 32 * 1024 * 1024
                {
                    shared.touched_complete.store(false, Ordering::Relaxed);
                } else {
                    partial.touched.extend_from_slice(relative);
                    partial.touched.push(0);
                }
            }
        }
    }
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    fn names(inventory: &Inventory) -> Vec<Vec<u8>> {
        let mut names = inventory
            .touched
            .split(|byte| *byte == 0)
            .filter(|name| !name.is_empty())
            .map(<[u8]>::to_vec)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn directories_are_sorted_preserve_modes_and_exclude_git() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["z", "a", ".git/hidden"] {
            fs::create_dir_all(temp.path().join(name)).unwrap();
        }
        fs::set_permissions(temp.path().join("a"), fs::Permissions::from_mode(0o750)).unwrap();
        let result = inventory(
            temp.path(),
            Options {
                attributes: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            result
                .directories
                .iter()
                .map(|directory| directory.path.as_slice())
                .collect::<Vec<_>>(),
            [b"".as_slice(), b"a", b"z"]
        );
        assert_eq!(result.directories[1].mode, 0o750);
    }

    #[test]
    fn touched_files_follow_non_root_ancestors_and_attributes_force_rewrite() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["a", "d"] {
            fs::create_dir(temp.path().join(name)).unwrap();
        }
        for name in ["a/one", "a/two", "d/three", "old-top"] {
            fs::write(temp.path().join(name), "before").unwrap();
        }
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        let verified = (
            i64::try_from(now.as_secs()).unwrap(),
            i64::from(now.subsec_nanos()),
        );
        let options = Options {
            verified: Some(verified),
            ..Default::default()
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            fs::write(temp.path().join("a/one"), "after").unwrap();
            let metadata = fs::metadata(temp.path().join("a/one")).unwrap();
            if (metadata.ctime(), metadata.ctime_nsec()) >= verified {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "filesystem timestamp did not advance"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(
            names(&inventory(temp.path(), options).unwrap()),
            [b"a/one".to_vec()]
        );
        fs::write(temp.path().join("d/four"), "new").unwrap();
        fs::write(temp.path().join("top"), "new").unwrap();
        assert_eq!(
            names(&inventory(temp.path(), options).unwrap()),
            [
                b"a/one".to_vec(),
                b"d/four".to_vec(),
                b"d/three".to_vec(),
                b"top".to_vec()
            ]
        );
        fs::write(temp.path().join(".gitattributes"), "*.txt text").unwrap();
        let result = inventory(temp.path(), options).unwrap();
        assert!(!result.touched_complete);
        assert!(result.touched.is_empty());
    }

    #[test]
    fn special_files_are_reported_or_removed_without_following_links() {
        let temp = tempfile::tempdir().unwrap();
        let fifo = temp.path().join("pipe");
        std::os::unix::fs::symlink("pipe", temp.path().join("link")).unwrap();
        let raw = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: raw is a live NUL-terminated path and mkfifo retains no pointer.
        assert_eq!(unsafe { libc::mkfifo(raw.as_ptr(), 0o600) }, 0);
        assert_eq!(
            inventory(temp.path(), Options::default())
                .unwrap()
                .specials
                .as_slice(),
            std::slice::from_ref(&fifo)
        );
        let result = inventory(
            temp.path(),
            Options {
                remove_specials: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(result.specials.is_empty());
        assert!(!fifo.exists());
        assert!(fs::symlink_metadata(temp.path().join("link"))
            .unwrap()
            .is_symlink());
    }

    #[test]
    fn missing_roots_fail_instead_of_returning_an_empty_inventory() {
        let temp = tempfile::tempdir().unwrap();
        assert_eq!(
            inventory(&temp.path().join("absent"), Options::default())
                .err()
                .unwrap()
                .code,
            ErrorCode::IoError
        );
    }
}
