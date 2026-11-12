//! Best-effort, bounded per-invocation logs. Never part of lease correctness.
use crate::preferences::Logging;
use crate::terminal::Terminal;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tracing::{
    field::{Field, Visit},
    Event, Subscriber,
};
use tracing_subscriber::{
    layer::{Context, SubscriberExt},
    Layer,
};

struct Output {
    file: File,
    bytes: u64,
}
struct Sink {
    output: Mutex<Option<Output>>,
    directory: Option<PathBuf>,
    invocation: String,
    terminal: Terminal,
    settings: Logging,
    request_id: Option<String>,
    command: String,
}
/// Closing the file releases its advisory lock before final retention cleanup.
pub struct Guard(Arc<Sink>);
impl Drop for Guard {
    fn drop(&mut self) {
        if let Ok(mut output) = self.0.output.lock() {
            output.take();
        }
        if let Some(dir) = &self.0.directory {
            let _ = prune(dir, &self.0.settings);
        }
    }
}
fn lock(file: &File) -> bool {
    // SAFETY: flock borrows a valid descriptor and uses nonblocking advisory locking.
    unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) == 0 }
}
fn directory() -> Option<PathBuf> {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
        .map(|p| p.join("groove/logs"))
}
fn validate_directory(dir: &Path) -> io::Result<()> {
    // Match managed-state policy: never traverse a symlink into a log destination.
    // Same-user concurrent path replacement is outside Groove's trust boundary.
    for path in dir.ancestors() {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(io::Error::other("Symlink in log directory path"));
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
fn owned_directory(dir: &Path) -> io::Result<()> {
    validate_directory(dir)?;
    let meta = fs::symlink_metadata(dir)?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(io::Error::other(
            "Unsafe log directory ownership or permissions",
        ));
    }
    Ok(())
}
fn regular_file(path: &Path, write: bool, create: bool) -> io::Result<File> {
    // O_NONBLOCK also applies before fstat: opening an attacker-supplied FIFO
    // must never wait for a reader/writer before logging can be disabled.
    let file = OpenOptions::new()
        .read(!write)
        .write(write)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    // SAFETY: geteuid has no preconditions.
    let uid = unsafe { libc::geteuid() };
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != uid || meta.mode() & 0o022 != 0 {
        return Err(io::Error::other(
            "Unsafe log file type, ownership or permissions",
        ));
    }
    Ok(file)
}
fn open_log(dir: &PathBuf, id: &str, settings: &Logging) -> io::Result<Output> {
    validate_directory(dir)?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    owned_directory(dir)?;
    let _ = prune(dir, settings);
    // Serialize publication with cleanup so a new file cannot be pruned before
    // its lifetime lock has been acquired. Contention only disables file logging.
    let publication = regular_file(&dir.join(".cleanup.lock"), true, true)?;
    if !lock(&publication) {
        return Err(io::Error::other("Log cleanup in progress"));
    }
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dir.join(format!("groove-{id}.jsonl")))?;
    if !lock(&file) {
        return Err(io::Error::other("Unable to lock log file"));
    }
    Ok(Output { file, bytes: 0 })
}
/// Only inactive files are eligible. Active files add at most max_file_mib each
/// above the retained-directory budget. Cleanup never waits for another process.
fn prune(dir: &PathBuf, settings: &Logging) -> io::Result<()> {
    owned_directory(dir)?;
    let guard = regular_file(&dir.join(".cleanup.lock"), true, true)?;
    if !lock(&guard) {
        return Ok(());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("groove-")
            || !name.ends_with(".jsonl")
            || !entry.file_type()?.is_file()
        {
            continue;
        }
        let meta = entry.metadata()?;
        files.push((meta.modified()?, meta.len(), entry.path()));
    }
    files.sort();
    let mut count = files.len();
    let mut bytes: u64 = files.iter().map(|f| f.1).sum();
    for (modified, size, path) in files {
        let expired = modified.elapsed().unwrap_or_default()
            > Duration::from_secs(settings.max_age_days * 86400);
        if !expired && count <= settings.max_files && bytes <= settings.max_total_mib * 1024 * 1024
        {
            continue;
        }
        let Ok(file) = regular_file(&path, false, false) else {
            continue;
        };
        if lock(&file) && fs::remove_file(&path).is_ok() {
            count -= 1;
            bytes = bytes.saturating_sub(size);
        }
    }
    Ok(())
}
pub fn init(
    terminal: Terminal,
    settings: Logging,
    request_id: Option<String>,
    command: &str,
) -> Guard {
    let id = uuid::Uuid::new_v4().to_string();
    let dir = if !settings.enabled {
        None
    } else {
        settings.directory.clone().or_else(directory)
    };
    let output = dir
        .as_ref()
        .and_then(|dir| open_log(dir, &id, &settings).ok());
    if terminal.verbosity.allows(tracing::Level::DEBUG) && output.is_some() {
        if let Some(dir) = &dir {
            terminal.write(
                format!(
                    "{} DEBUG Log file: {:?}\n",
                    timestamp(),
                    dir.join(format!("groove-{id}.jsonl"))
                )
                .as_bytes(),
            );
        }
    }
    if output.is_none() && settings.enabled && terminal.verbosity.allows(tracing::Level::WARN) {
        terminal.write(
            format!(
                "{} WARN File logging unavailable; continuing without a log file\n",
                timestamp()
            )
            .as_bytes(),
        );
    }
    let directory = if output.is_some() { dir } else { None };
    let sink = Arc::new(Sink {
        output: Mutex::new(output),
        directory,
        invocation: id,
        terminal,
        settings,
        request_id: request_id.map(|s| s.chars().take(2048).collect()),
        command: command.into(),
    });
    let subscriber = tracing_subscriber::registry().with(LogLayer(sink.clone()));
    let _ = tracing::subscriber::set_global_default(subscriber);
    Guard(sink)
}
struct Fields(serde_json::Map<String, serde_json::Value>);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        let mut value = value.to_owned();
        if value.len() > 2048 {
            let mut end = 2048;
            while !value.is_char_boundary(end) {
                end -= 1;
            }
            value.truncate(end);
            value.push_str(" [truncated]");
        }
        self.0.insert(field.name().into(), value.into());
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), value.into());
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), serde_json::json!(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
}
fn timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

struct LogLayer(Arc<Sink>);
impl<S: Subscriber> Layer<S> for LogLayer {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _: Context<'_, S>) -> bool {
        metadata.target().starts_with("groove")
            && (self.0.terminal.verbosity.allows(*metadata.level())
                || (self.0.settings.enabled && self.0.settings.level.allows(*metadata.level())))
    }
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        let mut fields = Fields(Default::default());
        event.record(&mut fields);
        let metadata = event.metadata();
        let message = fields.0.remove("message").unwrap_or_default();
        let event_name = fields
            .0
            .remove("event")
            .unwrap_or_else(|| "diagnostic".into());
        // The error renderer owns the terminal presentation; retain this event
        // in the diagnostic file without printing a second copy.
        let file_only = fields
            .0
            .remove("file_only")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timestamp = timestamp();
        let value = serde_json::json!({
            "log_version": 1,
            "message": message, "event": event_name,
            "request_id": self.0.request_id, "command": self.0.command,
            "pid": std::process::id(),
            "timestamp": timestamp,
            "invocation_id": self.0.invocation,
            "level": metadata.level().as_str(), "target": metadata.target(), "fields": fields.0
        });
        if !file_only && self.0.terminal.verbosity.allows(*metadata.level()) {
            let message = message
                .as_str()
                .unwrap_or_default()
                .escape_debug()
                .to_string();
            let fields = fields
                .0
                .iter()
                .map(|(k, v)| format!(" {k}={v}"))
                .collect::<String>();
            self.0.terminal.write(
                format!("{} {} {}{}\n", timestamp, metadata.level(), message, fields).as_bytes(),
            );
        }
        if !self.0.settings.level.allows(*metadata.level()) {
            return;
        }
        if let Ok(mut output) = self.0.output.lock() {
            if let Some(out) = output.as_mut() {
                if let Ok(mut record) = serde_json::to_vec(&value) {
                    record.push(b'\n');
                    if out.bytes + record.len() as u64 <= self.0.settings.max_file_mib * 1024 * 1024
                    {
                        if out.file.write_all(&record).is_ok() {
                            out.bytes += record.len() as u64;
                        } else {
                            output.take();
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::SystemTime};
    // Git fork/exec in the failed-sink test can briefly inherit another test's
    // flock descriptors before CLOEXEC runs. Keep these in-process fixtures
    // separate; cross-process contention is covered by the integration test.
    static TEST_LOCK: Mutex<()> = Mutex::new(());
    const FILE_LIMIT: u64 = 5 * 1024 * 1024;
    const TOTAL_LIMIT: u64 = 100 * 1024 * 1024;
    const COUNT_LIMIT: usize = 100;
    const MAX_AGE: Duration = Duration::from_secs(7 * 86400);
    fn log_directory() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        dir
    }
    fn inactive_log(path: &Path, contents: &[u8]) -> File {
        let mut file = regular_file(path, true, true).unwrap();
        file.write_all(contents).unwrap();
        file
    }
    #[test]
    fn write_failure_disables_file_sink_without_interrupting_lease_operations() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        // A read-only descriptor rejects writes on every supported platform.
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut full = File::open(file.path()).unwrap();
        assert_eq!(
            full.write_all(b"probe").unwrap_err().raw_os_error(),
            Some(libc::EBADF)
        );
        let sink = Arc::new(Sink {
            output: Mutex::new(Some(Output {
                file: full,
                bytes: 0,
            })),
            directory: None,
            invocation: "disk-full".into(),
            terminal: Terminal::new(crate::terminal::Verbosity {
                verbose: 0,
                quiet: true,
            }),
            settings: Logging::default(),
            request_id: None,
            command: "test".into(),
        });
        let repo = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("-q")
            .arg(repo.path())
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "-c",
                "core.hooksPath=/dev/null",
                "commit",
                "--allow-empty",
                "-qm",
                "baseline",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        let subscriber = tracing_subscriber::registry().with(LogLayer(sink.clone()));
        tracing::subscriber::with_default(subscriber, || {
            let pool = groove_core::Pool::init(
                repo.path(),
                groove_core::PoolOptions {
                    capacity: 1,
                    ..Default::default()
                },
            )
            .unwrap();
            let pool = groove_core::Pool::open(&pool.common_git_dir).unwrap();
            let info = pool
                .acquire(Default::default())
                .unwrap()
                .into_protected()
                .unwrap();
            assert!(
                sink.output.lock().unwrap().is_none(),
                "write failure must disable the sink"
            );
            assert_eq!(pool.protected_leases().unwrap().len(), 1);
            pool.release_protected(&info.slot_id, info.lease_id)
                .unwrap();
            let mut next = pool.acquire(Default::default()).unwrap();
            next.release().unwrap();
        });
    }
    #[test]
    fn retention_skips_active_files_and_preserves_unrelated_files() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = log_directory();
        let path = dir.path().to_path_buf();
        let active = open_log(&path, "active", &Logging::default()).unwrap();
        active.file.set_len(TOTAL_LIMIT + 1).unwrap();
        fs::write(path.join("unrelated.jsonl"), "keep").unwrap();
        inactive_log(&path.join("groove-old.jsonl"), b"old");
        prune(&path, &Logging::default()).unwrap();
        assert!(path.join("groove-active.jsonl").exists());
        assert!(path.join("unrelated.jsonl").exists());
        assert!(!path.join("groove-old.jsonl").exists());
        drop(active);
        prune(&path, &Logging::default()).unwrap();
        assert!(!path.join("groove-active.jsonl").exists());
    }
    #[test]
    fn retention_enforces_count_and_age() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = log_directory();
        let path = dir.path().to_path_buf();
        for n in 0..105 {
            inactive_log(&path.join(format!("groove-{n:03}.jsonl")), b"{}\n");
        }
        let old = path.join("groove-expired.jsonl");
        let file = inactive_log(&old, b"");
        file.set_modified(SystemTime::now() - MAX_AGE - Duration::from_secs(1))
            .unwrap();
        prune(&path, &Logging::default()).unwrap();
        assert!(!old.exists());
        assert_eq!(
            fs::read_dir(&path)
                .unwrap()
                .flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
                .count(),
            COUNT_LIMIT
        );
    }
    #[test]
    fn file_logging_is_bounded_and_json_lines_remain_valid() {
        let _test_lock = TEST_LOCK.lock().unwrap();
        let dir = log_directory();
        let id = "bounded";
        let sink = Arc::new(Sink {
            output: Mutex::new(Some(
                open_log(&dir.path().to_path_buf(), id, &Logging::default()).unwrap(),
            )),
            directory: None,
            invocation: id.into(),
            terminal: Terminal::new(crate::terminal::Verbosity {
                verbose: 0,
                quiet: true,
            }),
            settings: Logging::default(),
            request_id: None,
            command: "test".into(),
        });
        let subscriber = tracing_subscriber::registry().with(LogLayer(sink.clone()));
        tracing::subscriber::with_default(subscriber, || {
            tracing::debug!(
                event = "test.typed",
                count = 42u64,
                success = true,
                "Typed event"
            );
            for _ in 0..4000 {
                tracing::debug!(payload = "x".repeat(2000), "event");
            }
        });
        let bytes = fs::read(dir.path().join("groove-bounded.jsonl")).unwrap();
        let first: serde_json::Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap()).unwrap();
        assert_eq!(first["fields"]["count"], 42);
        assert_eq!(first["fields"]["success"], true);
        assert_eq!(first["message"], "Typed event");
        assert!(bytes.len() as u64 <= FILE_LIMIT);
        assert!(bytes.len() as u64 > FILE_LIMIT - 4096);
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            serde_json::from_slice::<serde_json::Value>(line).unwrap();
        }
    }
}
