use crate::{
    error::PathContext, locking::Identity, repository, CleanupPolicy, Error, LeaseInfo, Result,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

/// Version written into, and required from, every durable Groove record.
///
/// Groove has not been released, so there is exactly one on-disk format. A future
/// format change must bump this and add an explicit, tested migration.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// Ready pool manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_lock: Option<Identity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<crate::ReadySettings>,
    pub schema_version: u32,
    pub capacity: usize,
    pub cleanup_policy: CleanupPolicy,
    pub worktree_root: PathBuf,
    pub admin_identity: Identity,
    /// Permanent slot ownership lock identities.
    pub slots: BTreeMap<crate::SlotId, Identity>,
    /// Permanent legacy execution lock identities, one per slot.
    pub execution_locks: BTreeMap<crate::SlotId, Identity>,
}

impl Config {
    /// Reject manifests this release cannot interpret safely.
    pub fn validate(&self) -> Result<()> {
        if let Some(ready) = &self.ready {
            ready.validate(self.capacity)?;
            if self.ready_lock.is_none() {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Missing ready worker lock identity",
                ));
            }
        }
        if self.schema_version != SCHEMA_VERSION {
            return Err(Error::new(
                crate::ErrorCode::UnsupportedConfiguration,
                "Unsupported pool manifest schema",
            ));
        }
        if !(1..=crate::MAX_CAPACITY).contains(&self.capacity)
            || self.slots.len() != self.capacity
            || self.execution_locks.keys().ne(self.slots.keys())
        {
            return Err(Error::new(
                crate::ErrorCode::ConfigConflict,
                "Invalid manifest slot set",
            ));
        }
        for i in 1..=self.capacity {
            if !self.slots.contains_key(&crate::SlotId::new(i)?) {
                return Err(Error::new(
                    crate::ErrorCode::ConfigConflict,
                    "Invalid manifest slot set",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SlotPhase {
    Available,
    Leased,
    Released,
    Quarantined,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SlotRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_head: Option<crate::CommitOid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_since_unix_ms: Option<u64>,
    pub schema_version: u32,
    pub phase: SlotPhase,
    pub lease: Option<LeaseInfo>,
    pub failure: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReuseProtection {
    pub schema_version: u32,
    pub lease: LeaseInfo,
}
impl SlotRecord {
    pub fn empty() -> Self {
        Self {
            released_head: None,
            idle_since_unix_ms: None,
            schema_version: SCHEMA_VERSION,
            phase: SlotPhase::Available,
            lease: None,
            failure: None,
        }
    }
}
pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    read_bounded(path, 1024 * 1024)
}
pub(crate) fn read_bounded<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    read_unlabelled(path, limit).at(path)
}
fn read_unlabelled<T: serde::de::DeserializeOwned>(path: &Path, limit: u64) -> Result<T> {
    repository::no_symlinks(path)?;
    let mut opts = fs::OpenOptions::new();
    let file = opts
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
        return Err(Error::new(
            crate::ErrorCode::ProtocolError,
            "Invalid or oversized metadata file",
        ));
    }
    // Limit the reader as well as the initial metadata check against growth.
    serde_json::from_reader(std::io::BufReader::new(std::io::Read::take(
        file,
        limit + 1,
    )))
    .map_err(Error::from)
}
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    write_unlabelled(path, value).at(path)
}
fn write_unlabelled<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    repository::no_symlinks(path)?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new(crate::ErrorCode::IoError, "Missing metadata parent"))?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut tmp, value)?;
    tmp.write_all(b"\n")?;
    tmp.as_file().sync_all()?;
    checkpoint("metadata-synced");
    tmp.persist(path).map_err(|e| Error::from(e.error))?;
    checkpoint("metadata-renamed");
    File::open(parent)?.sync_all()?;
    Ok(())
}
pub(crate) fn sync_dir(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

// Compile-time opt-in only, never enabled in production builds.
#[cfg(feature = "test-hooks")]
pub(crate) fn checkpoint(name: &str) {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    if std::env::var("GROOVE_TEST_CHECKPOINT").as_deref() != Ok(name) {
        return;
    }
    if let Ok(path) = std::env::var("GROOVE_TEST_SOCKET") {
        let mut stream = UnixStream::connect(path).expect("test checkpoint connection");
        stream
            .write_all(name.as_bytes())
            .expect("test checkpoint notification");
        let mut byte = [0];
        let _ = stream.read_exact(&mut byte);
    }
}
#[cfg(not(feature = "test-hooks"))]
pub(crate) fn checkpoint(_: &str) {}

/// Lowercase hexadecimal for deterministic namespace and request hashes.
pub(crate) fn hex(bytes: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut result = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        write!(result, "{byte:02x}").expect("writing to a String cannot fail");
    }
    result
}
