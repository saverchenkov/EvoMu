//! Automated client exception reporting, native V24 `F12` In-Game Bug Reporter (`bugreporter`)
//! HTTP backend (`/bugs/api/*`), and diagnostic bundle generator.
//!
//! Stages rich incident bundles (plus `F12` screenshot/`igbr.zip` attachments) into `.evomu-bugs/queue/`
//! for deduplicated batch sync to GitHub via `tools/sync_bugs_to_github.py`.

#![forbid(unsafe_code)]

use crate::flight_recorder::FlightRecorderEntry;
use crate::log_ring_buffer::global_log_ring;
use crate::router::context::ServerContext;
use crate::sso::HttpResponse;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info, warn};
use uuid::Uuid;

pub const DEFAULT_BUG_QUEUE_DIR: &str = ".evomu-bugs/queue";
pub const DEFAULT_TRAILING_CAPTURE_DURATION: Duration = Duration::from_millis(1500);
/// Maximum request body accepted for `POST /bugs/api/issue` (2 MiB).
pub const MAX_BUG_ISSUE_BODY_BYTES: usize = 2 * 1024 * 1024;
/// Maximum raw upload body accepted for `POST /bugs/api/attachment/{issueID}` (8 MiB).
pub const MAX_BUG_ATTACHMENT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum number of bug report bundles retained in memory/disk before oldest-first eviction.
pub const MAX_STORED_BUG_REPORTS: usize = 256;
/// Maximum total bytes retained across all queued bug report bundles and attachments (64 MiB).
pub const MAX_BUG_QUEUE_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum uncompressed size extracted per text entry from `igbr.zip` (4 MiB zip-bomb guard).
const MAX_ZIP_TEXT_ENTRY_BYTES: usize = 4 * 1024 * 1024;
/// Maximum lines kept from `logs.txt` inside the JSON summary (`igbr.zip` still retains full raw logs).
const MAX_CLIENT_LOG_TAIL_LINES: usize = 60;
/// Maximum number of attachments accepted for a single `EBR-*` report (V24 uploads <= 5: 3 screenshots + `logs.txt` + `igbr.zip`).
const MAX_ATTACHMENTS_PER_REPORT: usize = 16;

/// Per-`ServerContext` bug reporting state (avoids process-global env-var races in parallel tests
/// and maps active `EBR-*` report IDs to their staged JSON paths with bounded FIFO eviction).
#[derive(Debug)]
pub struct BugReportingRuntime {
    queue_dir_override: RwLock<Option<PathBuf>>,
    report_paths: Mutex<HashMap<String, PathBuf>>,
    report_order: Mutex<VecDeque<String>>,
    next_report_seq: AtomicU64,
    max_reports: AtomicUsize,
    max_total_bytes: AtomicU64,
}

impl Default for BugReportingRuntime {
    fn default() -> Self {
        Self {
            queue_dir_override: RwLock::new(None),
            report_paths: Mutex::new(HashMap::new()),
            report_order: Mutex::new(VecDeque::new()),
            next_report_seq: AtomicU64::new(0),
            max_reports: AtomicUsize::new(MAX_STORED_BUG_REPORTS),
            max_total_bytes: AtomicU64::new(MAX_BUG_QUEUE_TOTAL_BYTES),
        }
    }
}

impl BugReportingRuntime {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_storage_limits(&self, max_reports: usize, max_total_bytes: u64) {
        self.max_reports.store(max_reports.max(1), Ordering::Relaxed);
        self.max_total_bytes
            .store(max_total_bytes.max(1024), Ordering::Relaxed);
    }

    pub fn stored_report_count(&self) -> usize {
        self.report_paths.lock().map(|g| g.len()).unwrap_or(0)
    }

    pub fn set_queue_dir_override(&self, dir: Option<PathBuf>) {
        if let Ok(mut guard) = self.queue_dir_override.write() {
            *guard = dir;
        }
    }

    pub fn effective_queue_dir(&self) -> PathBuf {
        if let Ok(guard) = self.queue_dir_override.read() {
            if let Some(dir) = guard.as_ref() {
                return dir.clone();
            }
        }
        std::env::var("EVOMU_BUG_QUEUE_DIR")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_BUG_QUEUE_DIR))
    }

    pub fn register_report_path(&self, report_key: impl Into<String>, path: PathBuf) {
        let key: String = report_key.into();
        let max_reports = self.max_reports.load(Ordering::Relaxed);
        let max_bytes = self.max_total_bytes.load(Ordering::Relaxed);
        let mut evicted_paths = Vec::new();
        if let (Ok(mut paths), Ok(mut order)) = (self.report_paths.lock(), self.report_order.lock()) {
            if !paths.contains_key(&key) {
                order.push_back(key.clone());
            }
            paths.insert(key, path.clone());
            while order.len() > max_reports {
                if let Some(old_key) = order.pop_front() {
                    if let Some(old_path) = paths.remove(&old_key) {
                        evicted_paths.push((old_key, old_path));
                    }
                }
            }
        }
        let qdir = self.effective_queue_dir();
        for (old_key, old_path) in evicted_paths {
            remove_report_files(&qdir, &old_key, &old_path);
        }
        self.enforce_disk_limits(&qdir, max_reports, max_bytes);
    }

    pub fn enforce_disk_limits(&self, qdir: &Path, max_reports: usize, max_total_bytes: u64) {
        let Ok(entries) = std::fs::read_dir(qdir) else {
            return;
        };
        let mut bundles: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let modified = meta.modified().unwrap_or(UNIX_EPOCH);
            bundles.push((modified, p, meta.len()));
        }
        bundles.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let attachments_root = qdir.join("attachments");
        let mut total_bytes = directory_tree_bytes(qdir);
        let mut remaining = bundles.len();
        for (_, path, file_len) in bundles {
            if remaining <= max_reports && total_bytes <= max_total_bytes {
                break;
            }
            let report_key = extract_report_key_from_path(&path);
            let mut removed_bytes = file_len;
            if std::fs::remove_file(&path).is_err() {
                removed_bytes = 0;
            }
            if let Some(ref key) = report_key {
                let attach_dir = attachments_root.join(key);
                if attach_dir.exists() {
                    removed_bytes = removed_bytes.saturating_add(directory_tree_bytes(&attach_dir));
                    let _ = std::fs::remove_dir_all(&attach_dir);
                }
                if let Ok(mut paths) = self.report_paths.lock() {
                    paths.remove(key);
                }
                if let Ok(mut order) = self.report_order.lock() {
                    order.retain(|k| k != key);
                }
            }
            total_bytes = total_bytes.saturating_sub(removed_bytes);
            remaining = remaining.saturating_sub(1);
        }
    }

    pub fn lookup_report_path(&self, report_key: &str) -> Option<PathBuf> {
        if let Ok(guard) = self.report_paths.lock() {
            if let Some(p) = guard.get(report_key) {
                return Some(p.clone());
            }
        }
        let qdir = self.effective_queue_dir();
        let entries = std::fs::read_dir(&qdir).ok()?;
        let suffix = format!("_{report_key}.json");
        for entry in entries.flatten() {
            let p = entry.path();
            if p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
            {
                return Some(p);
            }
        }
        None
    }

    pub fn next_seq(&self) -> u64 {
        self.next_report_seq.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// Builds the MachoNet `config_vals` dictionary advertised to V24 clients during handshake
/// (`InitClientGlobalConfig`) and via `machoNet.GetGlobalConfig`, enabling the `F12 -> Report a Bug`
/// button (`bugReporting_ShowButton = "1"`) and pointing `BugReporter` (`bugReporting_BugReportServer`)
/// at this server's `/bugs/api` endpoint on the exact host IP the client connected to.
pub fn build_client_global_config(
    ctx: &ServerContext,
    local_host_hint: Option<&str>,
) -> HashMap<String, PyRep> {
    let mut cfg = HashMap::new();

    let explicit_url = std::env::var("EVOMU_BUG_SERVER_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty());

    let esi_port = ctx.esi_port();
    let xmpp_port = ctx.xmpp_port();
    let bug_server_url = if let Some(url) = explicit_url {
        Some(url)
    } else if esi_port > 0 {
        let host = match local_host_hint.map(str::trim).filter(|h| !h.is_empty()) {
            Some("0.0.0.0")
            | Some("::")
            | Some("[::]")
            | Some("::1")
            | Some("[::1]")
            | Some("0:0:0:0:0:0:0:1")
            | Some("[0:0:0:0:0:0:0:1]")
            | None => "127.0.0.1".to_string(),
            Some(h) if h.starts_with("::ffff:") || h.starts_with("[::ffff:") => h
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim_start_matches("::ffff:")
                .to_string(),
            Some(h) if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
            Some(h) => h.to_string(),
        };
        Some(format!("http://{host}:{esi_port}/bugs/api"))
    } else {
        None
    };

    if let Some(url) = bug_server_url {
        cfg.insert("bugReporting_ShowButton".to_string(), PyRep::ascii_bytes("1"));
        cfg.insert(
            "bugReporting_BugReportServer".to_string(),
            PyRep::ascii_bytes(url.as_bytes()),
        );
    }

    let host = match local_host_hint.map(str::trim).filter(|h| !h.is_empty()) {
        Some("0.0.0.0") | Some("::") | Some("[::]") | None => "127.0.0.1".to_string(),
        Some(h) if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
        Some(h) => h.to_string(),
    };
    cfg.insert(
        "ejabberdAddress".to_string(),
        PyRep::ascii_bytes(host.as_bytes()),
    );
    cfg.insert(
        "ejabberd_client_address".to_string(),
        PyRep::ascii_bytes(host.as_bytes()),
    );
    cfg.insert(
        "ejabberdPort".to_string(),
        PyRep::Integer(i64::from(xmpp_port)),
    );
    cfg.insert(
        "ejabberd_client_port".to_string(),
        PyRep::Integer(i64::from(xmpp_port)),
    );
    cfg.insert(
        "chatserver_disable_tls".to_string(),
        PyRep::ascii_bytes(b"1"),
    );

    if let Ok(extra) = ctx.global_config_values.read() {
        for (k, v) in extra.iter() {
            cfg.insert(k.clone(), v.clone());
        }
    }

    cfg
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinatesSnapshot {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BugSessionSnapshot {
    pub session_id: u64,
    pub user_id: Option<u64>,
    pub character_id: Option<i64>,
    pub character_name: Option<String>,
    pub corporation_id: Option<i64>,
    pub corporation_name: Option<String>,
    pub alliance_id: Option<i64>,
    pub alliance_name: Option<String>,
    pub solar_system_id: Option<u32>,
    pub solar_system_name: Option<String>,
    pub security_status: Option<f64>,
    pub station_id: Option<i64>,
    pub structure_id: Option<i64>,
    pub ship_type_id: Option<i64>,
    pub ship_type_name: Option<String>,
    pub coordinates: Option<CoordinatesSnapshot>,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManualBugReportDetails {
    pub report_key: String,
    pub category_id: String,
    pub category_name: String,
    pub title: String,
    pub description: String,
    pub reproduction_steps: String,
    pub computer_info: String,
    pub server_name: String,
    pub client_build: Option<i64>,
    pub labels: Vec<String>,
    pub attachments: Vec<String>,
    pub ship_fitting_eft: Option<String>,
    pub client_logs_tail: Vec<String>,
}

fn default_report_source() -> String {
    "exception".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BugIncidentBundle {
    pub incident_id: String,
    pub timestamp: String,
    pub timestamp_millis: u64,
    pub stack_hash: i64,
    pub stack_key: String,
    pub trace: String,
    #[serde(default = "default_report_source")]
    pub report_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_report: Option<ManualBugReportDetails>,
    pub session: BugSessionSnapshot,
    pub flight_recorder: Vec<FlightRecorderEntry>,
    pub server_logs_before: Vec<String>,
    pub server_logs_after: Vec<String>,
}

fn enrich_session_snapshot(
    ctx: &ServerContext,
    sess_snap: &mut BugSessionSnapshot,
    explicit_ship_id: Option<i64>,
) {
    let mut active_ship_id = explicit_ship_id;

    if let Some(cid) = sess_snap.character_id {
        ctx.db.with_snapshot(|snap| {
            if let Some(c) = snap.characters.get(&cid) {
                sess_snap.character_name = Some(c.name.clone());
                sess_snap.corporation_id = Some(c.corporation_id);
                sess_snap.alliance_id = c.alliance_id;
                sess_snap.security_status = Some(c.security_status);
                if sess_snap.user_id.is_none() {
                    sess_snap.user_id = Some(c.user_id);
                }
                if sess_snap.solar_system_id.is_none() && c.solar_system_id > 0 {
                    sess_snap.solar_system_id = Some(c.solar_system_id);
                }
                if sess_snap.station_id.is_none() && sess_snap.structure_id.is_none() {
                    sess_snap.station_id = c.station_id;
                    sess_snap.structure_id = c.structure_id;
                }
                if active_ship_id.is_none() && c.active_ship_id > 0 {
                    active_ship_id = Some(c.active_ship_id);
                }
            }
            if let Some(corp_id) = sess_snap.corporation_id {
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    sess_snap.corporation_name = Some(corp.name.clone());
                }
            }
            if let Some(ally_id) = sess_snap.alliance_id {
                if let Some(ally) = snap.alliances.get(&ally_id) {
                    sess_snap.alliance_name = Some(ally.name.clone());
                }
            }
        });
    }

    if let Some(sys_id) = sess_snap.solar_system_id {
        if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
            sess_snap.solar_system_name = Some(sys.name.clone());
            if sess_snap.security_status.is_none() {
                sess_snap.security_status = Some(sys.security_status as f64);
            }
        }
    }

    if let Some(ship_item_id) = active_ship_id {
        ctx.db.with_snapshot(|snap| {
            if let Some(item) = snap.items.get(&ship_item_id) {
                sess_snap.ship_type_id = Some(item.type_id as i64);
                if let Some(t) = ctx.sde.types.get(&item.type_id) {
                    sess_snap.ship_type_name = Some(t.name.clone());
                }
            }
        });

        if sess_snap.station_id.is_none() && sess_snap.structure_id.is_none() {
            if let Some(sys_id) = sess_snap.solar_system_id {
                if let Some(pos) = ctx.systems.get_ball_position_sync(sys_id, ship_item_id) {
                    sess_snap.coordinates = Some(CoordinatesSnapshot {
                        x: pos.x,
                        y: pos.y,
                        z: pos.z,
                    });
                }
            }
            if sess_snap.coordinates.is_none() {
                ctx.db.with_snapshot(|snap| {
                    if let Some(&[x, y, z]) = snap.item_positions.get(&ship_item_id) {
                        sess_snap.coordinates = Some(CoordinatesSnapshot { x, y, z });
                    }
                });
            }
        }
    }
}

impl BugIncidentBundle {
    /// Builds an initial incident bundle snapshot from current session and server state.
    pub fn build_snapshot(
        ctx: &ServerContext,
        session: &SessionState,
        stack_hash: i64,
        stack_key: impl Into<String>,
        trace: impl Into<String>,
    ) -> Self {
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let iso_time = crate::esi::sample_routes::format_iso8601_utc(now_millis / 1000);
        let incident_id = Uuid::new_v4().to_string();

        let mut sess_snap = BugSessionSnapshot {
            session_id: session.session_id,
            user_id: session.user_id,
            character_id: session.character_id,
            corporation_id: session.corporation_id,
            alliance_id: session.alliance_id,
            solar_system_id: session.solar_system_id,
            station_id: session.station_id,
            structure_id: session.structure_id,
            ship_type_id: None,
            address: session.address.clone(),
            ..Default::default()
        };
        enrich_session_snapshot(ctx, &mut sess_snap, session.ship_id);

        let flight_recorder = ctx
            .flight_recorder
            .get_history(session.session_id, now_millis, 20);
        let server_logs_before = global_log_ring().snapshot_recent(40);

        let trace_str: String = trace.into();
        let stack_key_str: String = stack_key.into();
        let effective_stack_hash = disambiguate_uthread_stack_hash(stack_hash, &stack_key_str, &trace_str);

        Self {
            incident_id,
            timestamp: iso_time,
            timestamp_millis: now_millis,
            stack_hash: effective_stack_hash,
            stack_key: stack_key_str,
            trace: trace_str,
            report_source: "exception".to_string(),
            manual_report: None,
            session: sess_snap,
            flight_recorder,
            server_logs_before,
            server_logs_after: Vec::new(),
        }
    }

    /// Saves the incident bundle as formatted JSON into `queue_dir`.
    pub fn save_to_queue(&self, queue_dir: impl AsRef<Path>) -> std::io::Result<PathBuf> {
        let dir = queue_dir.as_ref();
        std::fs::create_dir_all(dir)?;

        let filename = if let Some(manual) = &self.manual_report {
            format!(
                "{}_{:016x}_{}.json",
                self.timestamp_millis,
                (self.stack_hash as u64),
                manual.report_key
            )
        } else {
            format!(
                "{}_{:016x}_{}.json",
                self.timestamp_millis,
                (self.stack_hash as u64),
                &self.incident_id[..8]
            )
        };
        let path = dir.join(filename);
        self.write_to_path(&path)?;
        Ok(path)
    }

    /// Atomically writes the JSON bundle to `path`.
    pub fn write_to_path(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json_bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        let tmp_path = path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json_bytes)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }
}

/// Dispatches asynchronous incident capture with a trailing log collection window.
pub fn schedule_incident_capture(
    mut bundle: BugIncidentBundle,
    queue_dir: Option<PathBuf>,
    trailing_duration: Option<Duration>,
) {
    let qdir = queue_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_BUG_QUEUE_DIR));
    let trailing = trailing_duration.unwrap_or(DEFAULT_TRAILING_CAPTURE_DURATION);
    let incident_millis = bundle.timestamp_millis;

    tokio::spawn(async move {
        tokio::time::sleep(trailing).await;
        bundle.server_logs_after = global_log_ring().snapshot_since(incident_millis, 25);

        match bundle.save_to_queue(&qdir) {
            Ok(path) => {
                info!(
                    incident_id = %bundle.incident_id,
                    stack_hash = bundle.stack_hash,
                    path = ?path,
                    "Queued client bug incident report for GitHub sync"
                );
            }
            Err(e) => {
                error!(
                    incident_id = %bundle.incident_id,
                    error = %e,
                    "Failed to save client bug incident to queue directory"
                );
            }
        }
    });
}

// ============================================================================
// Native V24 `F12 -> Report a Bug` (`bugreporter`) HTTP API (`/bugs/api/*`)
// ============================================================================

const BUG_CATEGORIES: &[(&str, &str)] = &[
    ("Gameplay", "Gameplay & Mechanics"),
    ("UI", "User Interface & Windows"),
    ("Combat", "Combat, Fitting & Dogma"),
    ("Space", "Navigation, Space & Structures"),
    ("Economy", "Market, Industry & Contracts"),
    ("PvE", "Missions, NPCs & Exploration"),
    ("Graphic", "Graphics, Audio & Performance"),
    ("Localization", "Localization & Text"),
];

fn category_display_name(category_id: &str) -> String {
    BUG_CATEGORIES
        .iter()
        .find(|(id, _)| id.eq_ignore_ascii_case(category_id))
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| category_id.to_string())
}

fn fnv1a_i64(input: &str) -> i64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash as i64
}

/// Disambiguates generic `uthread2` / `MollycoddledUthread` wrapper stack hashes (such as `-1040548311`)
/// by mixing in the `Formatted exception info:` summary line and leaf `Thrown at:` frame so unrelated
/// tasklet exceptions do not collide on the same GitHub issue.
fn disambiguate_uthread_stack_hash(stack_hash: i64, stack_key: &str, trace: &str) -> i64 {
    if !stack_key.contains("MollycoddledUthread") && !trace.contains("MollycoddledUthread") {
        return stack_hash;
    }
    let formatted_line = trace
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("Formatted exception info:"))
        .unwrap_or("");
    let thrown_leaf = trace
        .lines()
        .map(str::trim)
        .skip_while(|l| !l.starts_with("Thrown at:"))
        .take_while(|l| !l.starts_with("Formatted exception info:") && !l.starts_with("EXCEPTION END"))
        .filter(|l| l.starts_with("<pre>"))
        .last()
        .unwrap_or("");
    if formatted_line.is_empty() && thrown_leaf.is_empty() {
        return stack_hash;
    }
    fnv1a_i64(&format!("{stack_hash}:{thrown_leaf}:{formatted_line}"))
}

fn parse_char_id_from_session_info(desc: &str) -> Option<i64> {
    // V24 `_GetSessionInfo` (`bugReporting.pyj:579`): `Character: {charname} ({charid})`
    let marker = "Character:";
    let idx = desc.find(marker)?;
    let line = desc[idx + marker.len()..].lines().next()?;
    let open = line.rfind('(')?;
    let close = line[open + 1..].find(')')?;
    line[open + 1..open + 1 + close].trim().parse::<i64>().ok()
}

fn is_safe_report_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn sanitize_attachment_filename(raw_name: &str) -> String {
    let base = raw_name
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("attachment.bin");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .take(96)
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "attachment.bin".to_string()
    } else {
        cleaned
    }
}

/// Handles the 4 REST endpoints called by V24 `bugreporter.BugReporter` (`bugreporter/__init__.pyj`):
/// - `GET  /bugs/api/status` -> `{"isOnline": true}`
/// - `GET  /bugs/api/category` (and `/categories`) -> `{"categories": [{"id": ..., "value": ...}]}`
/// - `POST /bugs/api/issue` -> stages `BugIncidentBundle` and returns `201 {"id": key, "key": key}`
/// - `POST /bugs/api/attachment/{issueID}` -> saves multipart screenshots / `igbr.zip` and extracts
///   `fitting.txt` + `logs.txt` into the staged JSON bundle.
pub fn handle_bugs_http_request(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> HttpResponse {
    let sub = path.strip_prefix("/bugs/api").unwrap_or("");
    let sub = sub.trim_end_matches('/');

    match (method, sub) {
        ("GET", "/status") => HttpResponse::json(200, "OK", json!({ "isOnline": true })),

        ("GET", "/category") | ("GET", "/categories") => {
            let categories: Vec<serde_json::Value> = BUG_CATEGORIES
                .iter()
                .map(|(id, val)| json!({ "id": id, "value": val }))
                .collect();
            HttpResponse::json(200, "OK", json!({ "categories": categories }))
        }

        ("POST", "/issue") => handle_post_bug_issue(ctx, headers, body),

        ("POST", rest) if rest.starts_with("/attachment/") => {
            let issue_id = rest.trim_start_matches("/attachment/");
            handle_post_bug_attachment(ctx, issue_id, headers, body)
        }

        _ => HttpResponse::json(
            404,
            "Not Found",
            json!({
                "error": "not_found",
                "error_description": format!("Unknown bug reporting endpoint: {method} {path}")
            }),
        ),
    }
}

fn directory_tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| directory_tree_bytes(&entry.path()))
        .sum()
}

fn extract_report_key_from_path(path: &Path) -> Option<String> {
    let fname = path.file_name()?.to_str()?;
    let stem = fname.strip_suffix(".json")?;
    let (_, key) = stem.split_once('_')?;
    Some(key.to_string())
}

fn remove_report_files(qdir: &Path, report_key: &str, report_path: &Path) {
    let _ = std::fs::remove_file(report_path);
    let attach_dir = qdir.join("attachments").join(report_key);
    if attach_dir.exists() {
        let _ = std::fs::remove_dir_all(&attach_dir);
    }
}

fn verify_optional_bug_reporter_auth(
    ctx: &ServerContext,
    headers: &HashMap<String, String>,
) -> Result<Option<(u64, Option<i64>)>, HttpResponse> {
    let auth_entry = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .map(|(_, v)| v.clone());
    let Some(auth_val) = auth_entry else {
        return Ok(None);
    };
    let mut normalized = HashMap::new();
    normalized.insert("authorization".to_string(), auth_val);
    let claims = crate::sso::verify_esi_bearer_token(ctx, &normalized, None)?;
    Ok(Some((
        claims.user_id,
        (claims.character_id > 0).then_some(claims.character_id),
    )))
}

fn handle_post_bug_issue(
    ctx: &ServerContext,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> HttpResponse {
    if body.len() > MAX_BUG_ISSUE_BODY_BYTES {
        return HttpResponse::json(
            413,
            "Payload Too Large",
            json!({
                "error": "payload_too_large",
                "error_description": format!("Bug report body exceeds {} bytes", MAX_BUG_ISSUE_BODY_BYTES)
            }),
        );
    }
    let verified_caller = match verify_optional_bug_reporter_auth(ctx, headers) {
        Ok(caller) => caller,
        Err(resp) => return resp,
    };

    let payload: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return HttpResponse::json(
                400,
                "Bad Request",
                json!({ "error": "invalid_json", "error_description": e.to_string() }),
            );
        }
    };

    let category_id = payload
        .get("categoryID")
        .and_then(|v| v.as_str())
        .unwrap_or("Gameplay")
        .trim()
        .to_string();
    let title = payload
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("Untitled Bug Report")
        .trim()
        .to_string();
    let raw_description = payload
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let reproduction_steps = payload
        .get("reproductionSteps")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let computer_info = payload
        .get("computerInfo")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let _spoofed_reporter_uid = payload
        .get("originalReporterId")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .or_else(|| v.as_u64())
        });
    let server_name = payload
        .get("Server")
        .and_then(|v| v.as_str())
        .unwrap_or("EvoMu")
        .to_string();
    let client_build = payload.get("Build").and_then(|v| {
        v.as_i64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok()))
    });
    let labels: Vec<String> = payload
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    let (clean_desc, _) = raw_description
        .split_once("\n\nSession Info:\n")
        .map(|(d, s)| (d.trim().to_string(), s))
        .unwrap_or_else(|| (raw_description.trim().to_string(), ""));

    let now_millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let iso_time = crate::esi::sample_routes::format_iso8601_utc(now_millis / 1000);
    let incident_id = Uuid::new_v4().to_string();
    let seq = ctx.bug_reporting.next_seq();
    let report_key = format!("EBR-{seq:04}-{}", &incident_id[..6].to_ascii_uppercase());

    // Bind live session / FlightRecorder snapshots strictly to an authenticated caller session.
    // Unauthenticated requests must never resolve arbitrary client-supplied user/character IDs.
    let (sess_snap, flight_recorder, inferred_cid) = if let Some((auth_uid, auth_cid)) = verified_caller {
        let online_sess = ctx.db.get_online_session(auth_uid);
        let session_id = online_sess.as_ref().map(|s| s.session_id).unwrap_or(0);
        let caller_cid = online_sess
            .as_ref()
            .and_then(|s| s.character_id)
            .or(auth_cid)
            .or_else(|| {
                parse_char_id_from_session_info(&raw_description).filter(|cid| {
                    ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .get(cid)
                            .is_some_and(|c| c.user_id == auth_uid)
                    })
                })
            })
            .or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.characters
                        .values()
                        .find(|c| c.user_id == auth_uid)
                        .map(|c| c.character_id)
                })
            });
        let mut snap = BugSessionSnapshot {
            session_id,
            user_id: Some(auth_uid),
            character_id: caller_cid,
            address: String::new(),
            ..Default::default()
        };
        enrich_session_snapshot(ctx, &mut snap, None);
        let fr = if session_id > 0 {
            ctx.flight_recorder.get_history(session_id, now_millis, 20)
        } else {
            Vec::new()
        };
        (snap, fr, caller_cid)
    } else {
        (BugSessionSnapshot::default(), Vec::new(), None)
    };
    let server_logs_before = global_log_ring().snapshot_recent(40);

    let stack_key = format!("f12:{}:{}", category_id.to_ascii_lowercase(), title);
    let stack_hash = fnv1a_i64(&format!(
        "f12:{}:{}",
        category_id.to_ascii_lowercase(),
        title.to_ascii_lowercase()
    ));

    let bundle = BugIncidentBundle {
        incident_id: incident_id.clone(),
        timestamp: iso_time,
        timestamp_millis: now_millis,
        stack_hash,
        stack_key,
        trace: String::new(),
        report_source: "f12_manual".to_string(),
        manual_report: Some(ManualBugReportDetails {
            report_key: report_key.clone(),
            category_id: category_id.clone(),
            category_name: category_display_name(&category_id),
            title: title.clone(),
            description: clean_desc,
            reproduction_steps,
            computer_info,
            server_name,
            client_build,
            labels,
            attachments: Vec::new(),
            ship_fitting_eft: None,
            client_logs_tail: Vec::new(),
        }),
        session: sess_snap,
        flight_recorder,
        server_logs_before,
        server_logs_after: Vec::new(),
    };

    let qdir = ctx.bug_reporting.effective_queue_dir();
    let saved_path = match bundle.save_to_queue(&qdir) {
        Ok(p) => p,
        Err(e) => {
            error!(report_key = %report_key, error = %e, "Failed to persist F12 bug report bundle");
            return HttpResponse::json(
                500,
                "Internal Server Error",
                json!({ "error": "storage_error", "error_description": e.to_string() }),
            );
        }
    };
    ctx.bug_reporting
        .register_report_path(report_key.clone(), saved_path.clone());

    info!(
        report_key = %report_key,
        category = %category_id,
        title = %title,
        character_id = ?inferred_cid,
        path = ?saved_path,
        "Received F12 in-game bug report"
    );

    // Asynchronously append trailing +1.5s server log window without overwriting attachment updates
    let path_for_trailing = saved_path;
    tokio::spawn(async move {
        tokio::time::sleep(DEFAULT_TRAILING_CAPTURE_DURATION).await;
        let trailing_logs = global_log_ring().snapshot_since(now_millis, 25);
        if trailing_logs.is_empty() {
            return;
        }
        if let Ok(raw) = std::fs::read_to_string(&path_for_trailing) {
            if let Ok(mut latest) = serde_json::from_str::<BugIncidentBundle>(&raw) {
                latest.server_logs_after = trailing_logs;
                let _ = latest.write_to_path(&path_for_trailing);
            }
        }
    });

    // V24 `BugReporter.SendBugReport` reads `response['id']` and `DoClickSend` reads `result['key']`
    HttpResponse::json(
        201,
        "Created",
        json!({
            "id": report_key,
            "key": report_key,
        }),
    )
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn extract_multipart_file<'a>(
    headers: &HashMap<String, String>,
    body: &'a [u8],
) -> Option<(String, &'a [u8])> {
    let ct = headers.get("content-type")?;
    let boundary = ct
        .split(';')
        .map(str::trim)
        .find_map(|p| p.strip_prefix("boundary="))
        .map(|b| b.trim_matches('"'))
        .filter(|b| !b.is_empty())?;

    let open_delim = format!("--{boundary}");
    let start_idx = find_bytes(body, open_delim.as_bytes())?;
    let after_open = &body[start_idx + open_delim.len()..];
    let after_open = after_open
        .strip_prefix(b"\r\n")
        .or_else(|| after_open.strip_prefix(b"\n"))?;

    let hdr_end_rel = find_bytes(after_open, b"\r\n\r\n")?;
    let header_str = String::from_utf8_lossy(&after_open[..hdr_end_rel]);
    let mut filename = "attachment.bin".to_string();
    for line in header_str.lines() {
        if line.to_ascii_lowercase().starts_with("content-disposition:") {
            for seg in line.split(';') {
                let seg = seg.trim();
                if let Some(fname) = seg.strip_prefix("filename=") {
                    filename = fname.trim_matches('"').to_string();
                }
            }
        }
    }

    let payload_rest = &after_open[hdr_end_rel + 4..];
    let close_delim = format!("\r\n--{boundary}");
    let file_bytes = if let Some(end_idx) = find_bytes(payload_rest, close_delim.as_bytes()) {
        &payload_rest[..end_idx]
    } else {
        payload_rest
    };

    Some((sanitize_attachment_filename(&filename), file_bytes))
}

/// Extracts UTF-8 text files (`fitting.txt`, `logs.txt`, etc.) from a standard 32-bit PKZIP archive
/// (`igbr.zip`) using Central Directory headers and `miniz_oxide` raw DEFLATE decompression.
pub fn extract_text_files_from_zip(zip_bytes: &[u8], wanted_names: &[&str]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if zip_bytes.len() < 22 {
        return out;
    }

    // Locate End of Central Directory (EOCD) signature `0x06054b50` scanning backwards
    let min_eocd = zip_bytes.len().saturating_sub(65_557);
    let mut eocd_pos = None;
    for pos in (min_eocd..=zip_bytes.len() - 22).rev() {
        if zip_bytes[pos..pos + 4] == [0x50, 0x4b, 0x05, 0x06] {
            eocd_pos = Some(pos);
            break;
        }
    }
    let Some(eocd) = eocd_pos else {
        return out;
    };

    let total_entries = u16::from_le_bytes([zip_bytes[eocd + 10], zip_bytes[eocd + 11]]) as usize;
    let cd_offset = u32::from_le_bytes([
        zip_bytes[eocd + 16],
        zip_bytes[eocd + 17],
        zip_bytes[eocd + 18],
        zip_bytes[eocd + 19],
    ]) as usize;

    let mut cursor = cd_offset;
    for _ in 0..total_entries {
        if cursor + 46 > zip_bytes.len() || zip_bytes[cursor..cursor + 4] != [0x50, 0x4b, 0x01, 0x02]
        {
            break;
        }
        let method = u16::from_le_bytes([zip_bytes[cursor + 10], zip_bytes[cursor + 11]]);
        let comp_size = u32::from_le_bytes([
            zip_bytes[cursor + 20],
            zip_bytes[cursor + 21],
            zip_bytes[cursor + 22],
            zip_bytes[cursor + 23],
        ]) as usize;
        let uncomp_size = u32::from_le_bytes([
            zip_bytes[cursor + 24],
            zip_bytes[cursor + 25],
            zip_bytes[cursor + 26],
            zip_bytes[cursor + 27],
        ]) as usize;
        let name_len = u16::from_le_bytes([zip_bytes[cursor + 28], zip_bytes[cursor + 29]]) as usize;
        let extra_len =
            u16::from_le_bytes([zip_bytes[cursor + 30], zip_bytes[cursor + 31]]) as usize;
        let comment_len =
            u16::from_le_bytes([zip_bytes[cursor + 32], zip_bytes[cursor + 33]]) as usize;
        let local_hdr_off = u32::from_le_bytes([
            zip_bytes[cursor + 42],
            zip_bytes[cursor + 43],
            zip_bytes[cursor + 44],
            zip_bytes[cursor + 45],
        ]) as usize;

        if cursor + 46 + name_len > zip_bytes.len() {
            break;
        }
        let entry_name =
            String::from_utf8_lossy(&zip_bytes[cursor + 46..cursor + 46 + name_len]).into_owned();
        cursor = cursor.saturating_add(46 + name_len + extra_len + comment_len);

        let base_name = entry_name.rsplit(['/', '\\']).next().unwrap_or(&entry_name);
        if !wanted_names
            .iter()
            .any(|w| w.eq_ignore_ascii_case(base_name))
        {
            continue;
        }
        if uncomp_size > MAX_ZIP_TEXT_ENTRY_BYTES || comp_size > MAX_ZIP_TEXT_ENTRY_BYTES {
            continue;
        }
        if local_hdr_off + 30 > zip_bytes.len()
            || zip_bytes[local_hdr_off..local_hdr_off + 4] != [0x50, 0x4b, 0x03, 0x04]
        {
            continue;
        }
        let local_name_len = u16::from_le_bytes([
            zip_bytes[local_hdr_off + 26],
            zip_bytes[local_hdr_off + 27],
        ]) as usize;
        let local_extra_len = u16::from_le_bytes([
            zip_bytes[local_hdr_off + 28],
            zip_bytes[local_hdr_off + 29],
        ]) as usize;
        let data_start = local_hdr_off + 30 + local_name_len + local_extra_len;
        let Some(comp_slice) = zip_bytes.get(data_start..data_start.saturating_add(comp_size)) else {
            continue;
        };

        let raw_text_bytes = match method {
            0 => Some(comp_slice.to_vec()),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(
                comp_slice,
                MAX_ZIP_TEXT_ENTRY_BYTES,
            )
            .ok(),
            _ => None,
        };
        if let Some(bytes) = raw_text_bytes {
            out.insert(
                base_name.to_ascii_lowercase(),
                String::from_utf8_lossy(&bytes).into_owned(),
            );
        }
    }

    out
}

fn summarize_client_logs(raw_logs: &str) -> Vec<String> {
    let non_empty: Vec<&str> = raw_logs
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let start = non_empty.len().saturating_sub(MAX_CLIENT_LOG_TAIL_LINES);
    non_empty[start..].iter().map(|s| (*s).to_string()).collect()
}

fn handle_post_bug_attachment(
    ctx: &ServerContext,
    issue_id: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> HttpResponse {
    if !is_safe_report_key(issue_id) {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({ "error": "invalid_issue_id", "error_description": "Invalid bug report key" }),
        );
    }
    if body.len() > MAX_BUG_ATTACHMENT_BYTES {
        return HttpResponse::json(
            413,
            "Payload Too Large",
            json!({
                "error": "payload_too_large",
                "error_description": format!("Attachment exceeds {} bytes", MAX_BUG_ATTACHMENT_BYTES)
            }),
        );
    }

    let Some(report_json_path) = ctx.bug_reporting.lookup_report_path(issue_id) else {
        return HttpResponse::json(
            404,
            "Not Found",
            json!({ "error": "issue_not_found", "error_description": format!("Unknown bug report {issue_id}") }),
        );
    };

    let Some((safe_filename, file_bytes)) = extract_multipart_file(headers, body) else {
        return HttpResponse::json(
            400,
            "Bad Request",
            json!({ "error": "invalid_multipart", "error_description": "Expected multipart/form-data file upload" }),
        );
    };

    let qdir = ctx.bug_reporting.effective_queue_dir();
    let attach_dir = qdir.join("attachments").join(issue_id);
    if let Ok(existing) = std::fs::read_dir(&attach_dir) {
        if existing.count() >= MAX_ATTACHMENTS_PER_REPORT {
            return HttpResponse::json(
                429,
                "Too Many Requests",
                json!({
                    "error": "attachment_limit_exceeded",
                    "error_description": format!("Maximum {MAX_ATTACHMENTS_PER_REPORT} attachments per bug report")
                }),
            );
        }
    }
    if let Err(e) = std::fs::create_dir_all(&attach_dir) {
        warn!(issue_id = %issue_id, error = %e, "Failed to create bug attachment directory");
        return HttpResponse::json(
            500,
            "Internal Server Error",
            json!({ "error": "storage_error", "error_description": e.to_string() }),
        );
    }

    let file_path = attach_dir.join(&safe_filename);
    if let Err(e) = std::fs::write(&file_path, file_bytes) {
        warn!(issue_id = %issue_id, filename = %safe_filename, error = %e, "Failed to write bug attachment");
        return HttpResponse::json(
            500,
            "Internal Server Error",
            json!({ "error": "storage_error", "error_description": e.to_string() }),
        );
    }
    ctx.bug_reporting.enforce_disk_limits(
        &qdir,
        ctx.bug_reporting.max_reports.load(Ordering::Relaxed),
        ctx.bug_reporting.max_total_bytes.load(Ordering::Relaxed),
    );

    let rel_attachment = format!("attachments/{issue_id}/{safe_filename}");
    let mut extracted_fitting: Option<String> = None;
    let mut extracted_logs: Vec<String> = Vec::new();

    if safe_filename.eq_ignore_ascii_case("igbr.zip") {
        let extracted =
            extract_text_files_from_zip(file_bytes, &["fitting.txt", "logs.txt", "lastcrashes.txt"]);
        if let Some(fit) = extracted.get("fitting.txt") {
            let trimmed = fit.trim();
            if !trimmed.is_empty() {
                extracted_fitting = Some(trimmed.to_string());
            }
        }
        if let Some(logs) = extracted.get("logs.txt") {
            extracted_logs = summarize_client_logs(logs);
        }
    } else if safe_filename.eq_ignore_ascii_case("logs.txt") {
        let logs = String::from_utf8_lossy(file_bytes);
        extracted_logs = summarize_client_logs(&logs);
    } else if safe_filename.eq_ignore_ascii_case("fitting.txt") {
        let fit = String::from_utf8_lossy(file_bytes);
        if !fit.trim().is_empty() {
            extracted_fitting = Some(fit.trim().to_string());
        }
    }

    if let Ok(raw_json) = std::fs::read_to_string(&report_json_path) {
        if let Ok(mut bundle) = serde_json::from_str::<BugIncidentBundle>(&raw_json) {
            if let Some(manual) = bundle.manual_report.as_mut() {
                if !manual.attachments.contains(&rel_attachment) {
                    manual.attachments.push(rel_attachment.clone());
                }
                if let Some(fit) = extracted_fitting {
                    manual.ship_fitting_eft = Some(fit);
                }
                if !extracted_logs.is_empty() {
                    manual.client_logs_tail = extracted_logs;
                }
            }
            let _ = bundle.write_to_path(&report_json_path);
        }
    }

    info!(
        issue_id = %issue_id,
        filename = %safe_filename,
        bytes = file_bytes.len(),
        "Saved F12 bug report attachment"
    );

    HttpResponse::json(
        201,
        "Created",
        json!({
            "ok": true,
            "file": safe_filename,
            "bytes": file_bytes.len(),
        }),
    )
}

#[cfg(test)]
#[path = "bug_reporting_tests.rs"]
mod bug_reporting_tests;
#[cfg(test)] #[path = "bug_reporting_extra_tests.rs"] mod bug_reporting_extra_tests;
#[cfg(test)] #[path = "bug_reporting_edges_tests.rs"] mod bug_reporting_edges_tests;

