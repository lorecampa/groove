//! Stash capture through a private repository facade. Never apply a stash to the
//! user's checkout: Groove restoration has exact-state, not merge semantics.
use super::{invalid, paths, SnapshotLock, Snapshots};
use crate::{git::Git, repository, Error, ErrorCode, Result};
use git2::{Config, Index, IndexEntryExtendedFlag, Oid, Repository, Signature, StashFlags};
use std::{fs, os::unix::fs::symlink, path::Path, time::Instant};

fn failure(error: git2::Error) -> Error {
    Error::new(
        ErrorCode::GitError,
        format!("Snapshot stash capture failed: {error}"),
    )
    .with_source(error)
}

impl Snapshots {
    /// Replace only the private working index. The caller validates paths and
    /// retains the snapshot/admission descriptors through capture and publication.
    pub(super) fn capture_stash(
        &self,
        index: &Path,
        index_tree: &str,
        head: Option<&str>,
        untracked: &[u8],
        lock: &SnapshotLock,
    ) -> Result<()> {
        let directory = index
            .parent()
            .expect("capture index is inside its scratch directory");
        let private = directory.join("stash.git");
        let format = if index_tree.len() == 64 {
            "sha256"
        } else {
            "sha1"
        };
        // Native Git chooses the object format without opening the source with
        // libgit2, which cannot open a repository using the reftable ref backend.
        Git::new(self.timeout).run(
            &self.root,
            &[
                "-c",
                "init.defaultRefFormat=files",
                "init",
                "--bare",
                "--template=",
                "--quiet",
                &format!("--object-format={format}"),
                repository::utf8(&private)?,
            ],
            &lock.descriptors(),
        )?;
        // Only remove the freshly initialized scratch object's empty directories.
        // Recursive scratch cleanup does not follow the resulting objects symlink.
        fs::remove_dir(private.join("objects/info"))?;
        fs::remove_dir(private.join("objects/pack"))?;
        fs::remove_dir(private.join("objects"))?;
        symlink(self.common.join("objects"), private.join("objects"))?;
        fs::create_dir_all(private.join("info"))?;
        for name in ["attributes", "exclude"] {
            let source = self.string(&[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &format!("info/{name}"),
            ])?;
            match fs::read(&source) {
                Ok(bytes) => fs::write(private.join("info").join(name), bytes)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        let mut config = Config::open(&private.join("config")).map_err(failure)?;
        for (key, value) in [
            ("core.bare", false),
            ("core.autocrlf", false),
            ("core.filemode", true),
            ("core.symlinks", true),
            ("core.fsyncObjectFiles", true),
        ] {
            config.set_bool(key, value).map_err(failure)?;
        }
        config.set_str("core.eol", "lf").map_err(failure)?;
        for key in [
            "core.ignorecase",
            "core.precomposeunicode",
            "core.sparsecheckout",
        ] {
            match self.string(&["config", "--get", key]) {
                Ok(value) => {
                    config.set_str(key, &value).map_err(failure)?;
                }
                Err(error) if error.git_exit_code() == Some(1) => (),
                Err(error) => return Err(error),
            }
        }
        for (key, default_name) in [
            ("core.attributesfile", "attributes"),
            ("core.excludesfile", "ignore"),
        ] {
            let value = match self.string(&["config", "--path", "--get", key]) {
                Ok(value) => self.root.join(value),
                Err(error) if error.git_exit_code() == Some(1) => {
                    std::env::var_os("XDG_CONFIG_HOME")
                        .filter(|value| !value.is_empty())
                        .map(std::path::PathBuf::from)
                        .or_else(|| {
                            std::env::var_os("HOME")
                                .map(|home| std::path::PathBuf::from(home).join(".config"))
                        })
                        .map(|home| home.join("git").join(default_name))
                        .unwrap_or_else(|| "/dev/null".into())
                }
                Err(error) => return Err(error),
            };
            config
                .set_str(key, repository::utf8(&value)?)
                .map_err(failure)?;
        }
        // libgit2 cannot read split or sparse indexes. Expand only the disposable copy.
        self.run(
            &[
                "-c",
                "core.splitIndex=false",
                "-c",
                "index.sparse=false",
                "update-index",
                "--no-split-index",
            ],
            Some(index),
            None,
            Some(lock),
        )?;
        fs::copy(index, private.join("index"))?;
        let mut repo = Repository::open_bare(&private).map_err(failure)?;
        repo.set_config(&config).map_err(failure)?;
        repo.set_workdir(&self.root, false).map_err(failure)?;
        let signature =
            Signature::now("Groove Snapshot", "snapshot@groove.invalid").map_err(failure)?;
        let baseline = match head {
            Some(head) => Oid::from_str_ext(head, repo.object_format()).map_err(failure)?,
            None => {
                let oid = Index::new_ext(repo.object_format())
                    .map_err(failure)?
                    .write_tree_to(&repo)
                    .map_err(failure)?;
                let tree = repo.find_tree(oid).map_err(failure)?;
                repo.commit(
                    None,
                    &signature,
                    &signature,
                    "Private unborn snapshot baseline",
                    &tree,
                    &[],
                )
                .map_err(failure)?
            }
        };
        repo.set_head_detached(baseline).map_err(failure)?;
        // libgit2's stash tree treats absent sparse paths as deletions. Retain
        // the validated skip-worktree entries before the stash refreshes its index.
        let sparse: Vec<_> = repo
            .index()
            .map_err(failure)?
            .iter()
            .filter(|entry| {
                entry.flags_extended & IndexEntryExtendedFlag::SKIP_WORKTREE.bits() != 0
            })
            .collect();
        crate::state::checkpoint("snapshot-stash-before-save");
        let started = Instant::now();
        let result = repo.stash_save(
            &signature,
            "Groove private capture",
            Some(StashFlags::KEEP_ALL | StashFlags::INCLUDE_UNTRACKED),
        );
        // The stash API has no cancellation hook. Reject late results, but this
        // cannot interrupt a running C call; hard cancellation remains a draft blocker.
        if started.elapsed() > self.timeout {
            return Err(
                Error::new(ErrorCode::GitError, "Snapshot stash capture timed out")
                    .with_git_timeout(true),
            );
        }
        let oid = match result {
            Ok(oid) => oid,
            Err(error)
                if error.code() == git2::ErrorCode::NotFound
                    && untracked.is_empty()
                    && repo
                        .find_commit(baseline)
                        .map_err(failure)?
                        .tree_id()
                        .to_string()
                        == index_tree =>
            {
                // Native diff-files can report stale stat entries even when
                // bytes equal the index (e.g. after interrupted restoration).
                // Independently verify that case before accepting an empty stash.
                // Refresh only our scratch copy; changed content still fails.
                self.run(
                    &["update-index", "--refresh"],
                    Some(index),
                    None,
                    Some(lock),
                )?;
                self.run(&["diff-files", "--quiet"], Some(index), None, Some(lock))?;
                return Ok(());
            }
            Err(error) => return Err(failure(error)),
        };
        crate::state::checkpoint("snapshot-stash-saved");
        let commit = repo.find_commit(oid).map_err(failure)?;
        if commit.parent(1).map_err(failure)?.tree_id().to_string() != index_tree {
            return Err(invalid("Stash capture changed the captured index tree"));
        }
        let mut working = Index::new_ext(repo.object_format()).map_err(failure)?;
        working
            .read_tree(&commit.tree().map_err(failure)?)
            .map_err(failure)?;
        for entry in sparse {
            use std::os::unix::ffi::OsStrExt;
            let path = self.root.join(std::ffi::OsStr::from_bytes(&entry.path));
            match fs::symlink_metadata(path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    working.add(&entry).map_err(failure)?
                }
                Err(error) => return Err(error.into()),
                Ok(_) => (),
            }
        }
        let mut added = Index::new_ext(repo.object_format()).map_err(failure)?;
        if commit.parent_count() == 3 {
            added
                .read_tree(&commit.parent(2).map_err(failure)?.tree().map_err(failure)?)
                .map_err(failure)?;
        }
        let expected: std::collections::BTreeSet<_> =
            paths(untracked).map(|path| path.to_vec()).collect();
        let actual: std::collections::BTreeSet<_> = added.iter().map(|entry| entry.path).collect();
        if expected != actual {
            return Err(invalid(
                "Stash capture disagrees with the validated untracked paths",
            ));
        }
        for entry in added.iter() {
            working.add(&entry).map_err(failure)?;
        }
        let tree = working.write_tree_to(&repo).map_err(failure)?;
        self.run(
            &["read-tree", &tree.to_string()],
            Some(index),
            None,
            Some(lock),
        )?;
        tracing::debug!(
            event = "snapshot.stash_captured",
            elapsed_ms = started.elapsed().as_millis(),
            "Captured snapshot through private stash"
        );
        Ok(())
    }
}
