mod support;
use groove_core::snapshot::Snapshots;
use std::{fs, os::unix::ffi::OsStrExt, time::Duration};
use support::{in_subprocess, Repo};

fn service(repo: &Repo) -> Snapshots {
    Snapshots::open(&repo.path, Duration::from_secs(30)).unwrap()
}

#[test]
fn storage_empty_catalog_reports_repository_history_without_scanning() {
    if !in_subprocess("storage_empty_catalog_reports_repository_history_without_scanning") {
        return;
    }
    let repo = Repo::new();
    let report = service(&repo).storage(0).unwrap();
    assert_eq!(report.snapshots, 0);
    assert_eq!(report.pinned, 0);
    assert_eq!(report.oldest_created_at, None);
    assert_eq!(report.newest_created_at, None);
    assert!(report.largest_blobs.is_empty());
    assert!(report.repository_objects.loose_objects > 0);
    assert_eq!(report.repository_objects.packs, 0);
}

#[test]
fn storage_unique_logical_blobs_survive_packing_with_byte_path_provenance() {
    if !in_subprocess("storage_unique_logical_blobs_survive_packing_with_byte_path_provenance") {
        return;
    }
    let repo = Repo::new();
    let snapshots = service(&repo);
    // APFS requires valid Unicode names; Linux also exercises invalid UTF-8.
    let path = if cfg!(target_os = "linux") {
        std::ffi::OsStr::from_bytes(b"a\t\n\xff")
    } else {
        std::ffi::OsStr::new("a\t\nè")
    };
    let content = vec![0xff; 8193];
    fs::write(repo.path.join(path), &content).unwrap();
    fs::write(repo.path.join("same-blob"), &content).unwrap();
    repo.git(&["add", "."]);
    let first = snapshots.save(Default::default()).unwrap();
    fs::write(repo.path.join("work-only"), vec![0; 4097]).unwrap();
    fs::write(repo.path.join("work-only-tied"), vec![1; 4097]).unwrap();
    let second = snapshots.save(Default::default()).unwrap();
    let report = snapshots.storage(3).unwrap();
    assert_eq!(report.snapshots, 2);
    assert_eq!(report.oldest_created_at, Some(first.created_at));
    assert_eq!(report.newest_created_at, Some(second.created_at));
    assert_eq!(report.largest_blobs.len(), 3);
    let largest = &report.largest_blobs[0];
    assert_eq!(largest.logical_bytes, 8193);
    assert_eq!(largest.path, path.to_string_lossy());
    assert_eq!(largest.tree, "index");
    assert_eq!(largest.snapshot_id, first.id.min(second.id));
    assert_eq!(repo.git(&["cat-file", "-s", &largest.oid]), "8193");
    assert_eq!(report.largest_blobs[1].logical_bytes, 4097);
    assert_eq!(report.largest_blobs[1].tree, "work");
    assert_eq!(report.largest_blobs[2].logical_bytes, 4097);
    assert!(report.largest_blobs[1].oid < report.largest_blobs[2].oid);
    let one = snapshots.storage(1).unwrap();
    assert_eq!(one.largest_blobs.len(), 1);
    assert_eq!(one.largest_blobs[0].oid, largest.oid);
    repo.git(&["repack", "-ad"]);
    let packed = snapshots.storage(3).unwrap();
    assert!(packed.repository_objects.packs > 0);
    assert!(packed.repository_objects.packed_objects > 0);
    assert!(packed.repository_objects.packed_bytes > 0);
    assert_eq!(
        serde_json::to_value(&packed.largest_blobs).unwrap(),
        serde_json::to_value(&report.largest_blobs).unwrap()
    );
    let oracle = repo.git(&["count-objects", "-v"]);
    let count = |key: &str| -> u64 {
        oracle
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .unwrap()
            .parse()
            .unwrap()
    };
    assert_eq!(packed.repository_objects.loose_objects, count("count: "));
    assert_eq!(
        packed.repository_objects.loose_bytes,
        count("size: ") * 1024
    );
    assert_eq!(packed.repository_objects.packed_objects, count("in-pack: "));
    assert_eq!(
        packed.repository_objects.packed_bytes,
        count("size-pack: ") * 1024
    );
    assert_eq!(packed.repository_objects.packs, count("packs: "));
}

#[test]
fn storage_unborn_empty_repository_has_no_objects_or_saved_blobs() {
    if !in_subprocess("storage_unborn_empty_repository_has_no_objects_or_saved_blobs") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    support::git(temp.path(), &["init", "-q"]);
    let snapshots = Snapshots::open(temp.path(), Duration::from_secs(30)).unwrap();
    let report = snapshots.storage(10).unwrap();
    assert_eq!(report.snapshots, 0);
    assert_eq!(report.pinned, 0);
    assert_eq!(report.oldest_created_at, None);
    assert_eq!(report.newest_created_at, None);
    assert_eq!(report.repository_objects.loose_objects, 0);
    assert_eq!(report.repository_objects.loose_bytes, 0);
    assert_eq!(report.repository_objects.packed_objects, 0);
    assert_eq!(report.repository_objects.packed_bytes, 0);
    assert_eq!(report.repository_objects.packs, 0);
    assert!(report.largest_blobs.is_empty());
}
