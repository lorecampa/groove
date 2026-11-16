//! Repository-wide object storage and optional logical saved-blob inventory.
use super::*;
use std::io::BufReader;

/// Local Git storage excluding alternates; not snapshot-attributable or reclaimable bytes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectStorage {
    /// Number of loose objects in the repository.
    pub loose_objects: u64,
    /// Loose object storage, Git's rounded KiB measurement converted to bytes.
    pub loose_bytes: u64,
    /// Number of packed objects in the repository.
    pub packed_objects: u64,
    /// Pack and pack-index storage, Git's rounded KiB measurement converted to bytes.
    pub packed_bytes: u64,
    /// Number of packs.
    pub packs: u64,
}

/// One unique blob, with one deterministic representative saved occurrence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageBlob {
    /// Git blob object ID; shared content appears only once.
    pub oid: String,
    /// Uncompressed logical size, not allocated or reclaimable storage.
    pub logical_bytes: u64,
    /// Representative snapshot ID (lexicographically first occurrence).
    pub snapshot_id: String,
    /// Saved tree containing the representative occurrence: `index` or `work`.
    pub tree: String,
    /// Original path displayed as UTF-8 with replacement for invalid byte sequences.
    pub path: String,
}

/// Snapshot inventory and repository-wide storage observed under the snapshot lock.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageReport {
    /// Total snapshots across all worktrees and pools.
    pub snapshots: usize,
    /// Snapshots protected by a saved-release pin.
    pub pinned: usize,
    /// Oldest creation time in Unix seconds, or null when empty.
    pub oldest_created_at: Option<u64>,
    /// Newest creation time in Unix seconds, or null when empty.
    pub newest_created_at: Option<u64>,
    /// Whole Git object store; includes ordinary history and non-snapshot objects.
    pub repository_objects: ObjectStorage,
    /// Requested maximum number of unique blobs; zero skips tree enumeration.
    pub largest_limit: usize,
    /// Unique blobs ordered by descending logical size, then ascending object ID.
    pub largest_blobs: Vec<StorageBlob>,
}

impl Snapshots {
    /// Report snapshot counts and the whole repository's Git object storage.
    ///
    /// `largest == 0` skips saved-tree enumeration. Otherwise streams index and
    /// working tree inventories through disk-backed temporary files and retains
    /// only the requested top unique blobs. Blob bytes are never read. Shared
    /// blobs have one representative occurrence, not exhaustive provenance.
    /// Concurrent ordinary Git maintenance may change repository totals.
    ///
    /// # Errors
    /// Reports malformed manifests, pins or Git inventories, lock contention,
    /// and Git/filesystem failures. No refs or working files are changed.
    ///
    /// # Examples
    /// ```no_run
    /// # use groove_core::snapshot::Snapshots;
    /// # use std::time::Duration;
    /// let snapshots = Snapshots::open(".", Duration::from_secs(120))?;
    /// let report = snapshots.storage(0)?;
    /// println!("{} snapshots", report.snapshots);
    /// # Ok::<(), groove_core::Error>(())
    /// ```
    pub fn storage(&self, largest: usize) -> Result<StorageReport> {
        let lock = self.lock()?;
        let mut snapshots = self.list_inner(&lock)?;
        let counts = text(self.run(&["count-objects", "-v"], None, None, Some(&lock))?)?;
        let value = |key: &str| -> Result<u64> {
            counts
                .lines()
                .find_map(|line| line.strip_prefix(key))
                .ok_or_else(|| invalid("Missing Git object storage measurement"))?
                .parse()
                .map_err(|error| {
                    invalid("Invalid Git object storage measurement").with_source(error)
                })
        };
        let bytes = |key| -> Result<u64> {
            value(key)?
                .checked_mul(1024)
                .ok_or_else(|| invalid("Git object storage measurement overflow"))
        };
        let mut report = StorageReport {
            snapshots: snapshots.len(),
            pinned: snapshots.iter().filter(|s| s.pinned).count(),
            oldest_created_at: snapshots.iter().map(|s| s.created_at).min(),
            newest_created_at: snapshots.iter().map(|s| s.created_at).max(),
            repository_objects: ObjectStorage {
                loose_objects: value("count: ")?,
                loose_bytes: bytes("size: ")?,
                packed_objects: value("in-pack: ")?,
                packed_bytes: bytes("size-pack: ")?,
                packs: value("packs: ")?,
            },
            largest_limit: largest,
            largest_blobs: Vec::new(),
        };
        if largest == 0 {
            return Ok(report);
        }
        snapshots.sort_by(|a, b| a.id.cmp(&b.id));
        for snapshot in snapshots {
            // Validate saved object IDs and graph before using manifest trees as argv.
            let snapshot = self.load(&snapshot.id, &lock)?;
            for (tree, oid) in [
                ("index", &snapshot.index_tree),
                ("work", &snapshot.work_tree),
            ] {
                let file = self.spool_names(&["ls-tree", "-r", "-l", "-z", oid], None, &lock)?;
                path_batches(BufReader::new(file), |batch| {
                    for record in paths(batch) {
                        let tab = record
                            .iter()
                            .position(|b| *b == b'\t')
                            .ok_or_else(|| invalid("Malformed Git tree inventory"))?;
                        let header = std::str::from_utf8(&record[..tab]).map_err(|error| {
                            invalid("Invalid Git tree header").with_source(error)
                        })?;
                        let fields: Vec<_> = header.split_ascii_whitespace().collect();
                        if fields.len() != 4 || fields[1] != "blob" {
                            return Err(invalid("Expected blob in saved tree inventory"));
                        }
                        let logical_bytes = fields[3]
                            .parse::<u64>()
                            .map_err(|error| invalid("Invalid Git blob size").with_source(error))?;
                        let blobs = &mut report.largest_blobs;
                        if blobs.iter().any(|b| b.oid == fields[2]) {
                            continue;
                        }
                        if blobs.len() == largest
                            && blobs.last().is_some_and(|b| {
                                (logical_bytes, std::cmp::Reverse(fields[2]))
                                    <= (b.logical_bytes, std::cmp::Reverse(b.oid.as_str()))
                            })
                        {
                            continue;
                        }
                        blobs.push(StorageBlob {
                            oid: fields[2].into(),
                            logical_bytes,
                            snapshot_id: snapshot.id.clone(),
                            tree: tree.into(),
                            path: String::from_utf8_lossy(&record[tab + 1..]).into_owned(),
                        });
                        blobs.sort_by(|a, b| {
                            b.logical_bytes
                                .cmp(&a.logical_bytes)
                                .then_with(|| a.oid.cmp(&b.oid))
                        });
                        blobs.truncate(largest);
                    }
                    Ok(())
                })?;
            }
        }
        Ok(report)
    }
}
