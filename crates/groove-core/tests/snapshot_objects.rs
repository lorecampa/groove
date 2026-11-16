//! Snapshot storage contracts: the exact Git objects each kind of save writes.
//!
//! A snapshot is a commit whose tree points at the captured index tree, working
//! tree and untracked-path listing. Saves must reuse every existing object and
//! write only what changed: new file contents once, the trees along changed
//! paths, the small wrapper tree and one commit. These tests compare complete
//! object sets before and after each operation, so any extra blob, duplicated
//! tree or regression in reuse fails here, deterministically and without the
//! benchmark lab. Expected objects are derived independently from Git.
mod support;

use groove_core::snapshot::{SaveOptions, Snapshot, Snapshots};
use std::{collections::BTreeSet, fs, time::Duration};
use support::Repo;

const FORMATS: [&str; 2] = ["sha1", "sha256"];

fn service(repo: &Repo) -> Snapshots {
    Snapshots::open(&repo.path, Duration::from_secs(30)).unwrap()
}
fn save(snapshots: &Snapshots) -> Snapshot {
    snapshots.save(SaveOptions::default()).unwrap()
}

/// Every object in the repository, including loose and packed objects.
fn objects(repo: &Repo) -> BTreeSet<String> {
    repo.git(&[
        "cat-file",
        "--batch-all-objects",
        "--batch-check=%(objecttype) %(objectname)",
    ])
    .lines()
    .map(str::to_owned)
    .collect()
}
/// Objects written since `before`, grouped by type.
#[derive(Debug, Default, PartialEq)]
struct Written {
    blobs: BTreeSet<String>,
    trees: BTreeSet<String>,
    commits: BTreeSet<String>,
}
fn written(repo: &Repo, before: &BTreeSet<String>) -> Written {
    let mut written = Written::default();
    for entry in objects(repo).difference(before) {
        let (kind, oid) = entry.split_once(' ').unwrap();
        let set = match kind {
            "blob" => &mut written.blobs,
            "tree" => &mut written.trees,
            "commit" => &mut written.commits,
            other => panic!("save wrote an unexpected {other} object {oid}"),
        };
        set.insert(oid.to_owned());
    }
    written
}
fn set<const N: usize>(oids: [&str; N]) -> BTreeSet<String> {
    oids.into_iter().map(str::to_owned).collect()
}
/// The published commit and its wrapper tree, resolved through the snapshot ref.
fn metadata(repo: &Repo, snapshot: &Snapshot) -> (String, String) {
    let reference = format!("refs/groove/snapshots/{}", snapshot.id);
    (
        repo.git(&["rev-parse", &format!("{reference}^{{commit}}")]),
        repo.git(&["rev-parse", &format!("{reference}^{{tree}}")]),
    )
}
/// A tree and the subtrees on the way to `directory` ("" is the root only).
fn path_trees(repo: &Repo, tree: &str, directory: &str) -> Vec<String> {
    let mut trees = vec![tree.to_owned()];
    let mut prefix = String::new();
    for part in directory.split('/').filter(|p| !p.is_empty()) {
        prefix = if prefix.is_empty() {
            part.to_owned()
        } else {
            format!("{prefix}/{part}")
        };
        trees.push(repo.git(&["rev-parse", &format!("{tree}:{prefix}")]));
    }
    trees
}
/// Blob ID of `contents` without writing it, so expectations never add objects.
fn blob_id(repo: &Repo, path: &str) -> String {
    repo.git(&["hash-object", "--no-filters", "--", path])
}

/// A small but structured tree: a deep edited path, siblings at every level
/// and a wide unrelated directory whose trees must never be rewritten.
fn fixture(format: &str) -> Repo {
    let repo = Repo::format(format);
    for (path, contents) in [
        ("src/core/deep.txt", "deep baseline\n"),
        ("src/core/sibling.txt", "sibling\n"),
        ("src/peer.txt", "peer\n"),
        ("docs/guide.txt", "guide\n"),
    ] {
        let path = repo.path.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fs::create_dir(repo.path.join("wide")).unwrap();
    for i in 0..50 {
        fs::write(repo.path.join(format!("wide/{i:02}.txt")), format!("{i}\n")).unwrap();
    }
    repo.git(&["add", "."]);
    repo.git(&["commit", "-qm", "structured tree"]);
    repo
}

#[test]
fn clean_and_unchanged_saves_reuse_every_tree_and_write_only_metadata() {
    for format in FORMATS {
        let repo = fixture(format);
        let snapshots = service(&repo);
        let head_tree = repo.git(&["rev-parse", "HEAD^{tree}"]);

        let before = objects(&repo);
        let first = save(&snapshots);
        let (commit, wrapper) = metadata(&repo, &first);
        assert_eq!(
            (&first.index_tree, &first.work_tree),
            (&head_tree, &head_tree)
        );
        // The untracked listing blob is shared by every save without untracked
        // files, so only the first such save may write it.
        let mut listing = set([first.untracked_blob.as_str()]);
        listing.retain(|oid| !before.contains(&format!("blob {oid}")));
        assert_eq!(
            written(&repo, &before),
            Written {
                blobs: listing,
                trees: set([wrapper.as_str()]),
                commits: set([commit.as_str()]),
            },
            "{format}: a clean save may write only its wrapper tree, commit and listing"
        );

        let before = objects(&repo);
        let unchanged = save(&snapshots);
        let (commit, unchanged_wrapper) = metadata(&repo, &unchanged);
        assert_eq!(
            unchanged_wrapper, wrapper,
            "{format}: identical state reuses the wrapper tree"
        );
        assert_eq!(
            written(&repo, &before),
            Written {
                commits: set([commit.as_str()]),
                ..Default::default()
            },
            "{format}: an unchanged save writes exactly one commit"
        );
    }
}

#[test]
fn an_edit_writes_one_blob_and_only_the_trees_on_its_path() {
    for format in FORMATS {
        let repo = fixture(format);
        let snapshots = service(&repo);
        let clean = save(&snapshots);
        let (_, clean_wrapper) = metadata(&repo, &clean);
        let head_tree = repo.git(&["rev-parse", "HEAD^{tree}"]);

        fs::write(repo.path.join("src/core/deep.txt"), "deep edit\n").unwrap();
        let blob = blob_id(&repo, "src/core/deep.txt");
        let before = objects(&repo);
        let edited = save(&snapshots);
        let (commit, wrapper) = metadata(&repo, &edited);
        assert_eq!(
            edited.index_tree, head_tree,
            "{format}: unstaged edits leave the index tree"
        );
        let mut trees: BTreeSet<_> = path_trees(&repo, &edited.work_tree, "src/core")
            .into_iter()
            .collect();
        trees.insert(wrapper);
        assert_eq!(trees.len(), 4, "{format}: root, src, src/core and wrapper");
        assert_eq!(
            written(&repo, &before),
            Written {
                blobs: set([blob.as_str()]),
                trees,
                commits: set([commit.as_str()]),
            },
            "{format}: unrelated directories (wide/, docs/) must not be rewritten"
        );

        // Saving the same edit again stores nothing new but its commit.
        let before = objects(&repo);
        let again = save(&snapshots);
        assert_eq!(
            written(&repo, &before),
            Written {
                commits: set([metadata(&repo, &again).0.as_str()]),
                ..Default::default()
            },
            "{format}: an unchanged edited state writes exactly one commit"
        );

        // Returning to a saved state reuses that state's trees and wrapper.
        repo.git(&["checkout", "--", "src/core/deep.txt"]);
        let before = objects(&repo);
        let reverted = save(&snapshots);
        let (commit, wrapper) = metadata(&repo, &reverted);
        assert_eq!(
            wrapper, clean_wrapper,
            "{format}: reverted state matches the clean save"
        );
        assert_eq!(
            written(&repo, &before),
            Written {
                commits: set([commit.as_str()]),
                ..Default::default()
            },
            "{format}: reverting an edit writes exactly one commit"
        );

        // A deletion has no contents to store: only the trees on its path.
        fs::remove_file(repo.path.join("src/peer.txt")).unwrap();
        let before = objects(&repo);
        let deleted = save(&snapshots);
        let (commit, wrapper) = metadata(&repo, &deleted);
        let mut trees: BTreeSet<_> = path_trees(&repo, &deleted.work_tree, "src")
            .into_iter()
            .collect();
        trees.insert(wrapper);
        assert_eq!(
            written(&repo, &before),
            Written {
                trees,
                commits: set([commit.as_str()]),
                ..Default::default()
            },
            "{format}: a deletion writes no blobs"
        );
    }
}

#[test]
fn staged_and_working_versions_are_each_stored_once() {
    for format in FORMATS {
        let repo = fixture(format);
        let snapshots = service(&repo);
        save(&snapshots);

        fs::write(repo.path.join("src/core/deep.txt"), "staged version\n").unwrap();
        repo.git(&["add", "--", "src/core/deep.txt"]);
        let staged = blob_id(&repo, "src/core/deep.txt");
        fs::write(repo.path.join("src/core/deep.txt"), "working version\n").unwrap();
        let working = blob_id(&repo, "src/core/deep.txt");

        let before = objects(&repo);
        assert!(
            before.contains(&format!("blob {staged}")),
            "git add stored the staged blob"
        );
        let saved = save(&snapshots);
        let (commit, wrapper) = metadata(&repo, &saved);
        let mut trees: BTreeSet<_> = path_trees(&repo, &saved.index_tree, "src/core")
            .into_iter()
            .chain(path_trees(&repo, &saved.work_tree, "src/core"))
            .collect();
        trees.insert(wrapper);
        assert_eq!(
            trees.len(),
            7,
            "{format}: three index trees, three work trees, wrapper"
        );
        assert_eq!(
            written(&repo, &before),
            Written {
                blobs: set([working.as_str()]),
                trees,
                commits: set([commit.as_str()]),
            },
            "{format}: the already-staged blob is reused, the working version stored once"
        );
    }
}

#[test]
fn untracked_files_store_their_contents_and_a_listing_but_never_ignored_files() {
    for format in FORMATS {
        let repo = fixture(format);
        let snapshots = service(&repo);
        let clean = save(&snapshots);

        fs::write(repo.path.join("docs/notes.txt"), "untracked notes\n").unwrap();
        let contents = blob_id(&repo, "docs/notes.txt");
        // The fixture ignores ignored/; its bytes must never enter the object store.
        fs::create_dir(repo.path.join("ignored")).unwrap();
        fs::write(
            repo.path.join("ignored/cache.bin"),
            b"ignored build output\0",
        )
        .unwrap();
        let ignored = blob_id(&repo, "ignored/cache.bin");

        let before = objects(&repo);
        let saved = save(&snapshots);
        let (commit, wrapper) = metadata(&repo, &saved);
        assert_ne!(saved.untracked_blob, clean.untracked_blob);
        let mut trees: BTreeSet<_> = path_trees(&repo, &saved.work_tree, "docs")
            .into_iter()
            .collect();
        trees.insert(wrapper);
        let written = written(&repo, &before);
        assert!(
            !written.blobs.contains(&ignored),
            "{format}: ignored file was stored"
        );
        assert_eq!(
            written,
            Written {
                blobs: set([contents.as_str(), saved.untracked_blob.as_str()]),
                trees,
                commits: set([commit.as_str()]),
            },
            "{format}: an untracked file adds its contents, its listing and docs/ trees"
        );
    }
}

#[test]
fn in_place_restore_writes_only_its_safety_snapshot() {
    for format in FORMATS {
        let repo = fixture(format);
        let snapshots = service(&repo);
        let clean = save(&snapshots);

        fs::write(repo.path.join("src/core/deep.txt"), "work to protect\n").unwrap();
        let blob = blob_id(&repo, "src/core/deep.txt");
        let before = objects(&repo);
        let restored = snapshots.restore(&clean.id, None).unwrap();
        let safety_id = restored
            .safety_snapshot_id
            .expect("in-place restore saves first");
        let safety = snapshots
            .list()
            .unwrap()
            .into_iter()
            .find(|s| s.id == safety_id)
            .unwrap();
        let (commit, wrapper) = metadata(&repo, &safety);
        let mut trees: BTreeSet<_> = path_trees(&repo, &safety.work_tree, "src/core")
            .into_iter()
            .collect();
        trees.insert(wrapper);
        assert_eq!(
            written(&repo, &before),
            Written {
                blobs: set([blob.as_str()]),
                trees,
                commits: set([commit.as_str()]),
            },
            "{format}: applying the restore itself must write no objects"
        );
        assert_eq!(
            fs::read(repo.path.join("src/core/deep.txt")).unwrap(),
            b"deep baseline\n"
        );
    }
}
