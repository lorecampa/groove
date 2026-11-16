use super::{cargo, process, release_notes};
use crate::{locking, repository, state, Error, ErrorCode, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const DAY_MS: u64 = 24 * 60 * 60 * 1000;
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Cache {
    checked_at_ms: u64,
    latest: Option<String>,
    notified_at_ms: u64,
}
/// A cached update notice. It does not imply that an upgrade has been authorized.
#[derive(Debug, Clone)]
pub struct UpdateNotice {
    /// Installed version compiled into the calling CLI.
    pub current_version: String,
    /// Most recent non-yanked stable crates.io version.
    pub latest_version: String,
    /// Whether the target may contain breaking changes.
    pub breaking_change: bool,
    /// Release notes for the target.
    pub release_notes: String,
}
/// An opportunistic update check that never waits for the network at completion.
///
/// Dropping the handle kills and reaps its child. No daemon or orphan updater is
/// left behind. Fast commands may finish before discovery and show no notice.
/// Attempts, including failures/cancellation, are throttled across processes.
pub struct NoticeCheck {
    running: Option<process::Running>,
    directory: PathBuf,
    _guard: Option<locking::SlotGuard>,
    cache: Cache,
}
impl std::fmt::Debug for NoticeCheck {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoticeCheck")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}
fn directory(variable: &str, fallback: &str) -> Result<PathBuf> {
    let root = std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(fallback))
        })
        .ok_or_else(|| {
            Error::new(
                ErrorCode::UnsupportedConfiguration,
                format!("Set HOME or {variable} for upgrade coordination"),
            )
        })?;
    let directory = root.join("groove");
    repository::mkdir(&directory)?;
    repository::owned_dir(&directory)?;
    Ok(directory)
}
pub(super) fn upgrade_lock() -> Result<locking::SlotGuard> {
    let path = directory("XDG_STATE_HOME", ".local/state")?.join("upgrade.lock");
    drop(locking::open(&path, true)?);
    locking::SlotGuard::try_lock(&path, None)?.ok_or_else(|| {
        Error::new(
            ErrorCode::ConfigConflict,
            "Another Groove upgrade is running; wait for its installers to finish",
        )
        .retryable(true)
    })
}
fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}
impl NoticeCheck {
    /// Start a daily, best-effort check for a verified crates.io installation.
    ///
    /// The CLI must apply interactive/CI/quiet/JSON and preference gating first.
    /// Errors only concern optional discovery and must not fail the user's command.
    /// Returns `None` for unmanaged installations. No repository state is opened.
    pub fn start() -> Result<Option<Self>> {
        if cargo::Installation::detect()?.is_none() {
            return Ok(None);
        }
        let directory = directory("XDG_CACHE_HOME", ".cache")?;
        let path = self::directory("XDG_STATE_HOME", ".local/state")?.join("notice.lock");
        drop(locking::open(&path, true)?);
        let Some(guard) = locking::SlotGuard::try_lock(&path, None)? else {
            return Ok(None);
        };
        let cache_path = directory.join("update.json");
        let mut cache: Cache = state::read(&cache_path).unwrap_or_default();
        let now = now_ms();
        // Future timestamps are not an authority to suppress checks forever.
        if cache.checked_at_ms <= now && now - cache.checked_at_ms < DAY_MS {
            return Ok(Some(Self {
                running: None,
                directory,
                _guard: Some(guard),
                cache,
            }));
        }
        cache.checked_at_ms = now;
        state::write(&cache_path, &cache)?;
        let running = process::index_request(Duration::from_secs(2))?;
        Ok(Some(Self {
            running: Some(running),
            directory,
            _guard: Some(guard),
            cache,
        }))
    }
    /// Consume any already-completed discovery and return a cached notice.
    ///
    /// Never waits for an HTTP request. An unfinished request is killed/reaped
    /// on drop; failed discovery leaves the last successful release available.
    /// Errors can be ignored because this is advisory, never an upgrade preflight.
    pub fn finish(mut self) -> Result<Option<UpdateNotice>> {
        if let Some(running) = &mut self.running {
            if let Ok(Some(bytes)) = running.poll() {
                if let Ok(version) = cargo::select(&bytes, None) {
                    self.cache.latest = Some(version.to_string());
                    state::write(&self.directory.join("update.json"), &self.cache)?;
                }
            }
        }
        let Some(latest) = self
            .cache
            .latest
            .as_deref()
            .and_then(|v| Version::parse(v).ok())
        else {
            return Ok(None);
        };
        let current = Version::parse(env!("CARGO_PKG_VERSION")).map_err(|e| {
            Error::new(ErrorCode::ProtocolError, "Invalid build version").with_source(e)
        })?;
        let now = now_ms();
        if self.cache.notified_at_ms <= now && now - self.cache.notified_at_ms < DAY_MS {
            return Ok(None);
        }
        if latest > current {
            self.cache.notified_at_ms = now;
            state::write(&self.directory.join("update.json"), &self.cache)?;
        }
        Ok((latest > current).then(|| UpdateNotice {
            current_version: current.to_string(),
            latest_version: latest.to_string(),
            breaking_change: super::compatibility_boundary(&current, &latest),
            release_notes: release_notes(&latest),
        }))
    }
}
