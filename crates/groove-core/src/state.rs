use crate::{locking::Identity, repository, CleanupPolicy, Error, LeaseInfo, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
    pub schema_version: u32,
    pub capacity: usize,
    pub cleanup_policy: CleanupPolicy,
    pub worktree_root: PathBuf,
    pub admin_identity: Identity,
    pub slots: BTreeMap<crate::SlotId, Identity>,
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
            schema_version: 1,
            phase: SlotPhase::Available,
            lease: None,
            failure: None,
        }
    }
}
pub(crate) fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    repository::no_symlinks(path)?;
    let mut opts = fs::OpenOptions::new();
    let file = opts
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 1024 * 1024 {
        return Err(Error::new(
            crate::ErrorCode::ProtocolError,
            "Invalid or oversized metadata file",
        ));
    }
    serde_json::from_reader(file).map_err(Error::from)
}
pub(crate) fn write<T: Serialize>(path: &Path, value: &T) -> Result<()> {
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
