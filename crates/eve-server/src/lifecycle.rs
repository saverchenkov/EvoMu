//! Synchronous process lifecycle logger (`lifecycle.jsonl`) with hourly rotation,
//! resource snapshots, signal recording, and uncaught-exception / panic observer (`#1087`).
//!
//! All file I/O is synchronous and lock-order-safe so panic hooks can flush a
//! record immediately without touching the Tokio runtime or server locks.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Instant, UNIX_EPOCH};

/// Default directory holding `lifecycle.jsonl` and hourly archives.
pub const DEFAULT_LIFECYCLE_LOG_DIR: &str = "logs";
/// Live synchronous lifecycle log filename.
pub const LIFECYCLE_LOG_FILENAME: &str = "lifecycle.jsonl";
/// Environment variable overriding the lifecycle log directory (used by binaries and tests).
pub const LIFECYCLE_LOG_DIR_ENV: &str = "EVOMU_LIFECYCLE_LOG_DIR";

/// One JSONL record written to `lifecycle.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleRecord {
    pub ts_unix: u64,
    pub ts_utc: String,
    pub pid: u32,
    pub uptime_s: u64,
    pub rss_bytes: u64,
    pub open_sessions: usize,
    pub pending_wal_records: u64,
    pub last_tick_lateness_ms: i64,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Process-wide atomic resource gauges read lock-free during lifecycle/panic logging.
#[derive(Debug)]
pub struct LifecycleGauges {
    pub open_sessions: AtomicUsize,
    pub pending_wal_records: AtomicU64,
    pub last_tick_lateness_ms: AtomicI64,
}

impl Default for LifecycleGauges {
    fn default() -> Self {
        Self {
            open_sessions: AtomicUsize::new(0),
            pending_wal_records: AtomicU64::new(0),
            last_tick_lateness_ms: AtomicI64::new(0),
        }
    }
}

impl LifecycleGauges {
    pub fn snapshot(&self) -> (usize, u64, i64) {
        (
            self.open_sessions.load(Ordering::Relaxed),
            self.pending_wal_records.load(Ordering::Relaxed),
            self.last_tick_lateness_ms.load(Ordering::Relaxed),
        )
    }
}

static GLOBAL_GAUGES: OnceLock<LifecycleGauges> = OnceLock::new();

pub fn global_lifecycle_gauges() -> &'static LifecycleGauges {
    GLOBAL_GAUGES.get_or_init(LifecycleGauges::default)
}

pub fn set_open_sessions(count: usize) {
    global_lifecycle_gauges()
        .open_sessions
        .store(count, Ordering::Relaxed);
}

pub fn set_pending_wal_records(count: u64) {
    global_lifecycle_gauges()
        .pending_wal_records
        .store(count, Ordering::Relaxed);
}

pub fn set_last_tick_lateness_ms(lateness_ms: i64) {
    global_lifecycle_gauges()
        .last_tick_lateness_ms
        .store(lateness_ms, Ordering::Relaxed);
}

/// Formats `unix_secs` as an ISO-8601 UTC timestamp (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_utc_iso8601(unix_secs: u64) -> String {
    crate::esi::sample_routes::format_iso8601_utc(unix_secs)
}

/// Formats the hourly archive filename (`lifecycle-YYYYMMDD-HH.jsonl`) for `hour_bucket = unix_secs / 3600`.
pub fn hourly_archive_filename(hour_bucket: u64) -> String {
    let unix_secs = hour_bucket.saturating_mul(3600);
    let (year, month, day, hh, _mm, _ss) = crate::esi::sample_routes::civil_from_unix(unix_secs);
    format!("lifecycle-{year:04}{month:02}{day:02}-{hh:02}.jsonl")
}

/// Reads current process resident set size (`rss_bytes`) without `unsafe`.
pub fn read_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        if let Ok(status) = fs::read_to_string("/proc/self/status") {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    if let Some(kb) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|tok| tok.parse::<u64>().ok())
                    {
                        return kb.saturating_mul(1024);
                    }
                }
            }
        }
        if let Ok(statm) = fs::read_to_string("/proc/self/statm") {
            if let Some(pages) = statm
                .split_whitespace()
                .nth(1)
                .and_then(|tok| tok.parse::<u64>().ok())
            {
                return pages.saturating_mul(4096);
            }
        }
    }

    #[cfg(unix)]
    {
        let pid = std::process::id().to_string();
        if let Ok(output) = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &pid])
            .output()
        {
            if output.status.success() {
                if let Ok(text) = std::str::from_utf8(&output.stdout) {
                    if let Some(kb) = text
                        .split_whitespace()
                        .next()
                        .and_then(|tok| tok.parse::<u64>().ok())
                    {
                        return kb.saturating_mul(1024);
                    }
                }
            }
        }
    }

    0
}

#[derive(Debug)]
struct LifecycleWriterState {
    dir: PathBuf,
    current_hour_bucket: Option<u64>,
}

/// Synchronous, hourly-rotating `lifecycle.jsonl` writer safe for use inside panic hooks.
#[derive(Debug)]
pub struct LifecycleLogger {
    boot_instant: Instant,
    state: Mutex<LifecycleWriterState>,
}

impl LifecycleLogger {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            boot_instant: Instant::now(),
            state: Mutex::new(LifecycleWriterState {
                dir: dir.into(),
                current_hour_bucket: None,
            }),
        }
    }

    pub fn from_env() -> Self {
        let dir = std::env::var_os(LIFECYCLE_LOG_DIR_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_LIFECYCLE_LOG_DIR));
        Self::new(dir)
    }

    pub fn log_dir(&self) -> PathBuf {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dir
            .clone()
    }

    pub fn live_log_path(&self) -> PathBuf {
        self.log_dir().join(LIFECYCLE_LOG_FILENAME)
    }

    /// Builds a [`LifecycleRecord`] at `now_unix` using current process gauges.
    pub fn build_record_at(
        &self,
        now_unix: u64,
        reason: impl Into<String>,
        detail: Option<String>,
    ) -> LifecycleRecord {
        let (open_sessions, pending_wal_records, last_tick_lateness_ms) =
            global_lifecycle_gauges().snapshot();
        LifecycleRecord {
            ts_unix: now_unix,
            ts_utc: format_utc_iso8601(now_unix),
            pid: std::process::id(),
            uptime_s: self.boot_instant.elapsed().as_secs(),
            rss_bytes: read_rss_bytes(),
            open_sessions,
            pending_wal_records,
            last_tick_lateness_ms,
            reason: reason.into(),
            detail,
        }
    }

    /// Synchronously writes a lifecycle record at `now_unix`, rotating prior-hour `lifecycle.jsonl`
    /// into `lifecycle-YYYYMMDD-HH.jsonl` before appending.
    pub fn write_record_at(
        &self,
        now_unix: u64,
        reason: impl Into<String>,
        detail: Option<String>,
    ) -> std::io::Result<LifecycleRecord> {
        thread_local! {
            static IN_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
        }
        struct ResetFlag;
        impl Drop for ResetFlag {
            fn drop(&mut self) {
                IN_WRITE.with(|flag| flag.set(false));
            }
        }
        let entered = IN_WRITE.with(|flag| {
            if flag.get() {
                false
            } else {
                flag.set(true);
                true
            }
        });
        if !entered {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "reentrant lifecycle log write skipped",
            ));
        }
        let _reset = ResetFlag;

        let record = self.build_record_at(now_unix, reason, detail);
        let mut line = serde_json::to_string(&record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');

        let hour_bucket = now_unix / 3600;
        let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let state = &mut *guard;
        fs::create_dir_all(&state.dir)?;
        let live_path = state.dir.join(LIFECYCLE_LOG_FILENAME);

        rotate_if_needed(
            &state.dir,
            &live_path,
            &mut state.current_hour_bucket,
            hour_bucket,
        )?;

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&live_path)?;
        file.write_all(line.as_bytes())?;
        file.flush()?;
        state.current_hour_bucket = Some(hour_bucket);

        Ok(record)
    }

    /// Synchronously writes a lifecycle record using wall-clock UNIX time.
    pub fn record_sync(
        &self,
        reason: impl Into<String>,
        detail: Option<String>,
    ) -> std::io::Result<LifecycleRecord> {
        let now_unix = eve_db::WallUnixMs::system_now().as_secs();
        self.write_record_at(now_unix, reason, detail)
    }

    /// Uncaught-exception-style observer: runs `f`, and if it unwinds/panics, synchronously logs
    /// a `reason = "panic"` (or custom reason) snapshot and immediately re-raises via `resume_unwind`.
    pub fn observe_and_reraise<F, R>(&self, reason: &str, f: F) -> R
    where
        F: FnOnce() -> R,
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
            Ok(v) => v,
            Err(payload) => {
                let msg = panic_payload_to_string(&*payload);
                let _ = self.record_sync(reason, Some(msg));
                std::panic::resume_unwind(payload);
            }
        }
    }
}

fn rotate_if_needed(
    dir: &Path,
    live_path: &Path,
    current_hour_bucket: &mut Option<u64>,
    new_hour_bucket: u64,
) -> std::io::Result<()> {
    let existing_bucket = match *current_hour_bucket {
        Some(b) => Some(b),
        None => live_path
            .metadata()
            .ok()
            .filter(|m| m.len() > 0)
            .and_then(|m| m.modified().ok())
            .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() / 3600),
    };

    if let Some(prev_bucket) = existing_bucket {
        if prev_bucket != new_hour_bucket {
            let meta = match fs::metadata(live_path) {
                Ok(m) => Some(m),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            if meta.is_some_and(|m| m.len() > 0) {
                let archive_path = dir.join(hourly_archive_filename(prev_bucket));
                if archive_path.exists() {
                    let contents = fs::read(live_path)?;
                    let mut arch = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&archive_path)?;
                    arch.write_all(&contents)?;
                    arch.flush()?;
                    OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(live_path)?;
                } else {
                    fs::rename(live_path, &archive_path)?;
                }
            }
        }
    }
    *current_hour_bucket = Some(new_hour_bucket);
    Ok(())
}

fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

static GLOBAL_LIFECYCLE_LOGGER: OnceLock<Arc<LifecycleLogger>> = OnceLock::new();
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Returns the process-wide [`LifecycleLogger`].
pub fn global_lifecycle_logger() -> &'static Arc<LifecycleLogger> {
    GLOBAL_LIFECYCLE_LOGGER.get_or_init(|| Arc::new(LifecycleLogger::from_env()))
}

/// Synchronously records a lifecycle event to the global `lifecycle.jsonl`.
pub fn record_global_lifecycle_event(
    reason: impl Into<String>,
    detail: Option<String>,
) -> std::io::Result<LifecycleRecord> {
    global_lifecycle_logger().record_sync(reason, detail)
}

/// Installs an `uncaught-exception`-style panic observer that synchronously appends a
/// `reason = "panic"` snapshot to `lifecycle.jsonl` and delegates to the previous panic hook
/// without ever swallowing the panic.
pub fn install_panic_observer(logger: Arc<LifecycleLogger>) {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload_msg = panic_payload_to_string(info.payload());
        let detail = match info.location() {
            Some(loc) => format!("{}:{}:{}: {payload_msg}", loc.file(), loc.line(), loc.column()),
            None => payload_msg,
        };
        let _ = logger.record_sync("panic", Some(detail));
        prev_hook(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(label: &str) -> PathBuf {
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "evomu_lifecycle_{label}_{}_{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn lifecycle_record_writes_required_fields_and_rotates_hourly() {
        let dir = unique_temp_dir("rotate");
        let logger = LifecycleLogger::new(&dir);
        set_open_sessions(42);
        set_pending_wal_records(7);
        set_last_tick_lateness_ms(19);

        let hour1 = 1_700_002_800_u64; // aligned within an hour
        let rec1 = logger
            .write_record_at(hour1, "startup_check", Some("unit".into()))
            .expect("write hour1");
        assert_eq!(rec1.pid, std::process::id());
        assert_eq!(rec1.open_sessions, 42);
        assert_eq!(rec1.pending_wal_records, 7);
        assert_eq!(rec1.last_tick_lateness_ms, 19);
        assert_eq!(rec1.reason, "startup_check");

        let hour2 = hour1 + 3_600;
        let _rec2 = logger
            .write_record_at(hour2, "clean_exit", None)
            .expect("write hour2");

        let archive = dir.join(hourly_archive_filename(hour1 / 3600));
        assert!(archive.exists(), "previous hour archive must exist: {archive:?}");
        let archive_body = fs::read_to_string(&archive).expect("read archive");
        let archived_json: serde_json::Value =
            serde_json::from_str(archive_body.lines().next().expect("line")).expect("json");
        for field in [
            "ts_unix",
            "ts_utc",
            "pid",
            "uptime_s",
            "rss_bytes",
            "open_sessions",
            "pending_wal_records",
            "last_tick_lateness_ms",
            "reason",
        ] {
            assert!(archived_json.get(field).is_some(), "missing field {field}");
        }
        assert_eq!(archived_json["reason"], "startup_check");

        let live_body = fs::read_to_string(logger.live_log_path()).expect("read live");
        let live_lines: Vec<&str> = live_body.lines().collect();
        assert_eq!(live_lines.len(), 1);
        let live_json: serde_json::Value = serde_json::from_str(live_lines[0]).expect("json");
        assert_eq!(live_json["reason"], "clean_exit");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn observe_and_reraise_logs_synchronously_and_never_swallows_panic() {
        let dir = unique_temp_dir("observer");
        let logger = LifecycleLogger::new(&dir);
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            logger.observe_and_reraise("panic", || {
                panic!("observer_boom_1087");
            });
        }));
        assert!(res.is_err(), "observer must re-raise instead of swallowing panic");

        let body = fs::read_to_string(logger.live_log_path()).expect("read lifecycle.jsonl");
        let rec: LifecycleRecord =
            serde_json::from_str(body.lines().next().expect("line")).expect("parse json");
        assert_eq!(rec.reason, "panic");
        assert!(
            rec.detail
                .as_deref()
                .is_some_and(|d| d.contains("observer_boom_1087")),
            "expected panic payload in detail, got {:?}",
            rec.detail
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_appends_to_existing_archive_and_handles_non_string_panics() {
        let dir = unique_temp_dir("append_archive");
        let hour1 = 1_700_002_800_u64;
        let archive = dir.join(hourly_archive_filename(hour1 / 3600));
        fs::write(&archive, "{\"reason\":\"prior\"}\n").expect("seed prior archive");

        let logger = LifecycleLogger::new(&dir);
        logger
            .write_record_at(hour1, "first_in_hour", None)
            .expect("write hour1");
        logger
            .write_record_at(hour1 + 3_600, "hour2_event", None)
            .expect("write hour2 triggers append rotation");

        let archive_body = fs::read_to_string(&archive).expect("read combined archive");
        let lines: Vec<&str> = archive_body.lines().collect();
        assert_eq!(lines.len(), 2, "must append rather than clobber existing archive");
        assert!(lines[0].contains("prior"));
        assert!(lines[1].contains("first_in_hour"));

        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            logger.observe_and_reraise("panic", || {
                std::panic::panic_any(42_u64);
            });
        }));
        assert!(res.is_err());
        let live_body = fs::read_to_string(logger.live_log_path()).expect("read live log");
        let last_line = live_body.lines().last().expect("last line");
        let rec: LifecycleRecord = serde_json::from_str(last_line).expect("json");
        assert_eq!(rec.detail.as_deref(), Some("non-string panic payload"));

        #[cfg(unix)]
        assert!(
            read_rss_bytes() > 0,
            "Unix RSS snapshot should return a positive byte count"
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
