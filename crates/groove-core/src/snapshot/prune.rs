//! Explicit, conservative snapshot retention under repository snapshot exclusion.
use super::{invalid, Snapshot, SnapshotLock, Snapshots, PINS};
use crate::{state, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Explicit retention policy. There is no automatic expiry and no pin override.
#[derive(Debug, Clone)]
pub struct PruneOptions {
    /// Select only snapshots strictly older than this age.
    pub older_than: Duration,
    /// Retain this many newest snapshots per originating Git directory, including pins.
    pub keep_last: usize,
    /// Delete selected references; false only previews the selection.
    pub apply: bool,
}
/// Why an individual snapshot survived retention selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PruneRetentionReason {
    /// A durable pin protects this checkpoint, including unfinished recovery.
    Pinned,
    /// Its source workspace has durable reuse protection.
    ProtectedWorkspace,
    /// It belongs to its workspace's newest retained window.
    KeepLast,
    /// It is at or newer than the strict age cutoff, including future timestamps.
    TooRecent,
}
/// A retained snapshot and its first applicable protection reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruneRetained {
    /// Snapshot identifier.
    pub id: String,
    /// Retention reason, prioritized pin, protection, newest window, then age.
    pub reason: PruneRetentionReason,
}
/// Retention preview or completed application, ordered newest first.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruneResult {
    /// Whether deletion was requested.
    pub applied: bool,
    /// IDs selected by the policy, also populated for previews.
    pub selected: Vec<String>,
    /// IDs actually deleted; empty for previews.
    pub deleted: Vec<String>,
    /// Every retained snapshot and its reason.
    pub retained: Vec<PruneRetained>,
}

impl Snapshots {
    /// Preview or apply explicit retention across all originating workspaces.
    ///
    /// One snapshot lock spans inventory, selection and transactional deletion.
    /// Pins are never overridden. Protected sources are conservatively retained
    /// to cover a saved release between snapshot publication and pin creation.
    /// The workspace key is its original Git directory (a reused slot is still
    /// the same workspace, regardless of task/session labels). Deletion removes
    /// references only; Git garbage collection controls object reclamation.
    ///
    /// # Errors
    /// Fails closed on corrupt inventories, protection records, changed refs or
    /// pins, lock contention and Git/filesystem failures. Application may delete
    /// a prefix before a later failure; retry recomputes the remaining selection.
    ///
    /// ```no_run
    /// # fn main() -> groove_core::Result<()> {
    /// use groove_core::snapshot::{Snapshots, PruneOptions};
    /// use std::time::Duration;
    /// let snapshots = Snapshots::open(".", Duration::from_secs(30))?;
    /// let preview = snapshots.prune(PruneOptions {
    ///     older_than: Duration::from_secs(30 * 24 * 60 * 60),
    ///     keep_last: 20, apply: false,
    /// })?;
    /// # let _ = preview;
    /// # Ok(()) }
    /// ```
    pub fn prune(&self, options: PruneOptions) -> Result<PruneResult> {
        let lock = self.lock()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| invalid("System clock precedes the Unix epoch"))?
            .as_nanos();
        // The shared inventory rejects Git diagnostics: a broken reference must
        // never silently reduce a retention window.
        let snapshots = self.list_inner(&lock)?;
        let protected = self.prune_protected_roots()?;
        let mut counts = BTreeMap::<String, usize>::new();
        let mut result = PruneResult {
            applied: options.apply,
            selected: Vec::new(),
            deleted: Vec::new(),
            retained: Vec::new(),
        };
        // Capture every selected identity before exposing the selection checkpoint.
        let mut selected = Vec::new();
        for snapshot in snapshots {
            let (loaded, oid) = self.load_at(&snapshot.id, &lock)?;
            if serde_json::to_value(&loaded)? != serde_json::to_value(&snapshot)? {
                return Err(invalid(
                    "Snapshot changed during retention selection; retry",
                ));
            }
            let count = counts.entry(snapshot.workspace.clone()).or_default();
            let in_window = *count < options.keep_last;
            *count = count.saturating_add(1);
            let reason = if snapshot.pinned {
                Some(PruneRetentionReason::Pinned)
            } else if self.prune_source_protected(&snapshot, &protected)? {
                Some(PruneRetentionReason::ProtectedWorkspace)
            } else if in_window {
                Some(PruneRetentionReason::KeepLast)
            } else if !strictly_older(snapshot.created_at_ns, now, options.older_than) {
                Some(PruneRetentionReason::TooRecent)
            } else {
                None
            };
            if let Some(reason) = reason {
                result.retained.push(PruneRetained {
                    id: snapshot.id,
                    reason,
                });
            } else {
                result.selected.push(snapshot.id.clone());
                selected.push((snapshot.id, oid));
            }
        }
        crate::state::checkpoint("snapshot-prune-selected");
        if options.apply {
            for (id, oid) in selected {
                self.prune_delete_expected(&id, &oid, &lock)?;
                result.deleted.push(id);
                crate::state::checkpoint("snapshot-prune-deleted");
            }
        }
        Ok(result)
    }

    fn prune_protected_roots(&self) -> Result<BTreeSet<PathBuf>> {
        let mut roots = BTreeSet::new();
        for repo in crate::namespace::repositories(&self.repository)? {
            let Some(config) = crate::namespace::config(&repo)? else {
                continue;
            };
            for slot in config.slots.keys() {
                let path = repo.protection_path(slot);
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.is_file() => (),
                    Ok(_) => {
                        return Err(invalid("Invalid reuse protection during snapshot pruning"))
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                }
                let protection: state::ReuseProtection = state::read(&path)?;
                if protection.schema_version != state::SCHEMA_VERSION
                    || protection.lease.slot_id != *slot
                    || protection.lease.worktree_path != config.worktree_root.join(slot.as_str())
                {
                    return Err(invalid("Invalid reuse protection during snapshot pruning"));
                }
                roots.insert(protection.lease.worktree_path);
            }
        }
        Ok(roots)
    }

    fn prune_source_protected(
        &self,
        snapshot: &Snapshot,
        roots: &BTreeSet<PathBuf>,
    ) -> Result<bool> {
        if roots.is_empty() || std::path::Path::new(&snapshot.workspace) == self.common {
            return Ok(false);
        }
        // A missing historical registration cannot establish which protected
        // source it belonged to. Retain conservatively until protection clears.
        let admin = PathBuf::from(&snapshot.workspace);
        match fs::read(admin.join("gitdir")) {
            Ok(backlink) => Ok(roots
                .iter()
                .any(|root| crate::recovery::backlink_matches(&admin, &backlink, root))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(error.into()),
        }
    }

    fn prune_delete_expected(&self, id: &str, oid: &str, lock: &SnapshotLock) -> Result<()> {
        // Never reload and delete a replacement object under the selected ID.
        // Both selected object identity and pin absence are checked atomically.
        let reference = Self::reference(id)?;
        crate::state::checkpoint("snapshot-before-delete");
        let transaction = format!(
            "start\ndelete {reference} {oid}\nverify {PINS}{id} {}\nprepare\n",
            "0".repeat(oid.len())
        );
        self.run_env(
            &["update-ref", "--no-deref", "--stdin"],
            None,
            Some(transaction.as_bytes()),
            Some(lock),
            &[],
            Some(Box::new(|timeout| {
                self.validate_prepared_pin(id, None, lock, timeout)
            })),
        )?;
        Ok(())
    }
}

fn strictly_older(created_at_ns: u128, now_ns: u128, age: Duration) -> bool {
    now_ns
        .checked_sub(age.as_nanos())
        .is_some_and(|cutoff| created_at_ns < cutoff)
}

#[cfg(test)]
mod tests {
    use super::strictly_older;
    use std::time::Duration;
    #[test]
    fn cutoff_is_strict_at_nanosecond_precision_and_never_expires_future_saves() {
        let now = 10_000_000_000;
        let age = Duration::from_secs(1);
        assert!(strictly_older(8_999_999_999, now, age));
        assert!(!strictly_older(9_000_000_000, now, age));
        assert!(!strictly_older(now + 1, now, age));
        assert!(!strictly_older(0, now, Duration::MAX));
        assert!(!strictly_older(now, now, Duration::ZERO));
    }
}
