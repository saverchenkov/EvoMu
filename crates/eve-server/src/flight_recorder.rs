//! In-memory rolling ring buffer of recent MachoNet RPC calls per session ("Flight Recorder").
//!
//! Provides the diagnostic causality chain attached to client exception alerts.

#![forbid(unsafe_code)]

use eve_proto::PyRep;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_HISTORY_PER_SESSION: usize = 30;
pub const MAX_OUTBOUND_LEDGER_PER_SESSION: usize = 500;
pub const MAX_DESTINY_STAMPS_PER_SESSION: usize = 500;
pub const PACKET_PERF_WARN_THRESHOLD_MS: u64 = 100;
const MAX_PACKET_PERF_WARNINGS: usize = 200;
const MAX_HISTOGRAM_SAMPLES_PER_METHOD: usize = 2048;
const MAX_GLOBAL_HISTOGRAM_SAMPLES: usize = 4096;
const MAX_TRACKED_SESSIONS: usize = 512;
const MAX_ARGS_REPR_LEN: usize = 160;

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FlightRecorderEntry {
    pub timestamp_millis: u64,
    #[serde(default)]
    pub offset_ms: i64,
    pub service: String,
    pub method: String,
    pub bound_id: Option<u64>,
    pub args_summary: String,
    pub duration_micros: u64,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboundDisposition {
    Sent,
    Dropped,
    Coalesced,
}

impl std::fmt::Display for OutboundDisposition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Sent => f.write_str("sent"),
            Self::Dropped => f.write_str("dropped"),
            Self::Coalesced => f.write_str("coalesced"),
        }
    }
}

/// Bounded per-session outbound packet ledger entry (`#1077 §A`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutboundLedgerEntry {
    pub timestamp_millis: u64,
    pub disposition: OutboundDisposition,
    pub packet_kind: String,
    pub byte_size: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stamp: Option<u32>,
    #[serde(default)]
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_since_request_micros: Option<u64>,
    #[serde(default)]
    pub bootstrap: bool,
}

/// Server-side destiny stamp triple recorded for every `DoDestinyUpdate` (`#1077 §A`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestinyStampRecord {
    pub timestamp_millis: u64,
    pub session_id: u64,
    pub stamp: u32,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entity_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_current_tick: Option<u64>,
}

/// Slow RPC handler warning (`> 100 ms`) captured for `[PacketPerf]` inspection (`#1077 §A`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PacketPerfWarning {
    pub timestamp_millis: u64,
    pub session_id: u64,
    pub service: String,
    pub method: String,
    pub arg_size: usize,
    pub duration_micros: u64,
    #[serde(default)]
    pub bootstrap: bool,
}

#[derive(Debug, Clone, Default)]
struct MethodLatencyBucket {
    count: u64,
    total_micros: u64,
    min_micros: u64,
    max_micros: u64,
    samples_micros: VecDeque<u64>,
}

impl MethodLatencyBucket {
    fn record(&mut self, micros: u64) {
        if self.count == 0 {
            self.min_micros = micros;
            self.max_micros = micros;
        } else {
            self.min_micros = self.min_micros.min(micros);
            self.max_micros = self.max_micros.max(micros);
        }
        self.count = self.count.saturating_add(1);
        self.total_micros = self.total_micros.saturating_add(micros);
        if self.samples_micros.len() >= MAX_HISTOGRAM_SAMPLES_PER_METHOD {
            self.samples_micros.pop_front();
        }
        self.samples_micros.push_back(micros);
    }

    fn summarize(&self, service: &str, method: &str) -> LatencyHistogramSummary {
        let mut sorted: Vec<u64> = self.samples_micros.iter().copied().collect();
        sorted.sort_unstable();
        let avg_micros = if self.count == 0 {
            0.0
        } else {
            self.total_micros as f64 / self.count as f64
        };
        LatencyHistogramSummary {
            service: service.to_string(),
            method: method.to_string(),
            count: self.count,
            min_micros: self.min_micros,
            p50_micros: percentile_sorted(&sorted, 50, 100),
            p95_micros: percentile_sorted(&sorted, 95, 100),
            p99_micros: percentile_sorted(&sorted, 99, 100),
            max_micros: self.max_micros,
            avg_micros,
        }
    }
}

#[derive(Debug, Default)]
struct LatencyHistogramState {
    total_calls: u64,
    global_samples_micros: VecDeque<u64>,
    per_method: HashMap<(String, String), MethodLatencyBucket>,
}

/// Summary for one `(service, method)` RPC latency histogram (`#1077 §A`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LatencyHistogramSummary {
    pub service: String,
    pub method: String,
    pub count: u64,
    pub min_micros: u64,
    pub p50_micros: u64,
    pub p95_micros: u64,
    pub p99_micros: u64,
    pub max_micros: u64,
    pub avg_micros: f64,
}

/// Aggregated RPC handler latency histogram report (`#1077 §A`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LatencyHistogramReport {
    pub total_calls: u64,
    pub overall_p99_micros: u64,
    pub overall_p99_ms: f64,
    pub methods: Vec<LatencyHistogramSummary>,
}

fn percentile_sorted(sorted: &[u64], pct_num: usize, pct_den: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (sorted.len() * pct_num)
        .div_ceil(pct_den)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[idx]
}

pub struct FlightRecorder {
    sessions: RwLock<HashMap<u64, VecDeque<FlightRecorderEntry>>>,
    outbound_ledgers: RwLock<HashMap<u64, VecDeque<OutboundLedgerEntry>>>,
    destiny_stamps: RwLock<HashMap<u64, VecDeque<DestinyStampRecord>>>,
    latency_histograms: RwLock<LatencyHistogramState>,
    packet_perf_warnings: RwLock<VecDeque<PacketPerfWarning>>,
}

impl Default for FlightRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl FlightRecorder {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            outbound_ledgers: RwLock::new(HashMap::new()),
            destiny_stamps: RwLock::new(HashMap::new()),
            latency_histograms: RwLock::new(LatencyHistogramState::default()),
            packet_perf_warnings: RwLock::new(VecDeque::new()),
        }
    }

    /// Records an RPC execution event for `session_id`.
    pub fn record(
        &self,
        session_id: u64,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
        duration: Duration,
        status: &str,
    ) {
        let now_millis = now_unix_millis();

        let args_summary = summarize_args(service, method, args, kwargs);

        let entry = FlightRecorderEntry {
            timestamp_millis: now_millis,
            offset_ms: 0,
            service: service.to_string(),
            method: method.to_string(),
            bound_id,
            args_summary,
            duration_micros: duration.as_micros() as u64,
            status: status.to_string(),
        };

        if let Ok(mut lock) = self.sessions.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&session_id) {
                // Prune arbitrary oldest session
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            let queue = lock.entry(session_id).or_default();
            if queue.len() >= MAX_HISTORY_PER_SESSION {
                queue.pop_front();
            }
            queue.push_back(entry);
        }
    }

    /// Retrieves the recent RPC history for `session_id`, computing `offset_ms` relative to `ref_timestamp_millis`.
    pub fn get_history(
        &self,
        session_id: u64,
        ref_timestamp_millis: u64,
        limit: usize,
    ) -> Vec<FlightRecorderEntry> {
        let lock = match self.sessions.read() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };

        let queue = match lock.get(&session_id) {
            Some(q) => q,
            None => return Vec::new(),
        };

        let count = queue.len().min(limit);
        queue
            .iter()
            .rev()
            .take(count)
            .rev()
            .map(|e| {
                let mut item = e.clone();
                item.offset_ms = (item.timestamp_millis as i64) - (ref_timestamp_millis as i64);
                item
            })
            .collect()
    }

    /// Records a bounded per-session outbound packet event (`sent | dropped | coalesced`) (`#1077 §A`).
    #[allow(clippy::too_many_arguments)]
    pub fn record_outbound(
        &self,
        session_id: u64,
        disposition: OutboundDisposition,
        packet_kind: &str,
        byte_size: usize,
        stamp: Option<u32>,
        reason: &str,
        elapsed_since_request_micros: Option<u64>,
        bootstrap: bool,
    ) {
        let entry = OutboundLedgerEntry {
            timestamp_millis: now_unix_millis(),
            disposition,
            packet_kind: packet_kind.to_string(),
            byte_size,
            stamp,
            reason: reason.to_string(),
            elapsed_since_request_micros,
            bootstrap,
        };
        if let Ok(mut lock) = self.outbound_ledgers.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&session_id) {
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            let queue = lock.entry(session_id).or_default();
            if queue.len() >= MAX_OUTBOUND_LEDGER_PER_SESSION {
                queue.pop_front();
            }
            queue.push_back(entry);
        }
    }

    /// Returns up to `limit` most recent outbound ledger entries for `session_id` in chronological order.
    pub fn get_outbound_ledger(&self, session_id: u64, limit: usize) -> Vec<OutboundLedgerEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(lock) = self.outbound_ledgers.read() else {
            return Vec::new();
        };
        let Some(queue) = lock.get(&session_id) else {
            return Vec::new();
        };
        let count = queue.len().min(limit);
        queue.iter().rev().take(count).rev().cloned().collect()
    }

    /// Extracts and records `(session, stamp, action, entity, client_current_tick)` records for a
    /// `DoDestinyUpdate` frame (`#1077 §A`).
    pub fn record_destiny_update_stamps(
        &self,
        session_id: u64,
        default_stamp: u32,
        updates: &PyRep,
        client_current_tick: Option<u64>,
    ) {
        let triples = extract_destiny_stamp_triples(default_stamp, updates);
        let now_millis = now_unix_millis();
        if let Ok(mut lock) = self.destiny_stamps.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&session_id) {
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            let queue = lock.entry(session_id).or_default();
            for (stamp, action, entity_id) in triples {
                if queue.len() >= MAX_DESTINY_STAMPS_PER_SESSION {
                    queue.pop_front();
                }
                queue.push_back(DestinyStampRecord {
                    timestamp_millis: now_millis,
                    session_id,
                    stamp,
                    action,
                    entity_id,
                    client_current_tick,
                });
            }
        }
    }

    /// Returns up to `limit` most recent destiny stamp triple records for `session_id`.
    pub fn get_destiny_stamps(&self, session_id: u64, limit: usize) -> Vec<DestinyStampRecord> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(lock) = self.destiny_stamps.read() else {
            return Vec::new();
        };
        let Some(queue) = lock.get(&session_id) else {
            return Vec::new();
        };
        let count = queue.len().min(limit);
        queue.iter().rev().take(count).rev().cloned().collect()
    }

    /// Records handler latency in microseconds for `(service, method)` (`#1077 §A`).
    pub fn record_rpc_latency(&self, service: &str, method: &str, duration: Duration) {
        let micros = duration.as_micros().min(u64::MAX as u128) as u64;
        if let Ok(mut lock) = self.latency_histograms.write() {
            lock.total_calls = lock.total_calls.saturating_add(1);
            if lock.global_samples_micros.len() >= MAX_GLOBAL_HISTOGRAM_SAMPLES {
                lock.global_samples_micros.pop_front();
            }
            lock.global_samples_micros.push_back(micros);
            let key = (service.to_string(), method.to_string());
            lock.per_method.entry(key).or_default().record(micros);
        }
    }

    /// Computes a deterministic per-`(service, method)` latency histogram report (`#1077 §A`).
    pub fn latency_histogram_report(&self) -> LatencyHistogramReport {
        let Ok(lock) = self.latency_histograms.read() else {
            return LatencyHistogramReport {
                total_calls: 0,
                overall_p99_micros: 0,
                overall_p99_ms: 0.0,
                methods: Vec::new(),
            };
        };
        let mut global_sorted: Vec<u64> = lock.global_samples_micros.iter().copied().collect();
        global_sorted.sort_unstable();
        let overall_p99_micros = percentile_sorted(&global_sorted, 99, 100);
        let mut methods: Vec<LatencyHistogramSummary> = lock
            .per_method
            .iter()
            .map(|((svc, meth), bucket)| bucket.summarize(svc, meth))
            .collect();
        methods.sort_by(|a, b| {
            b.p99_micros
                .cmp(&a.p99_micros)
                .then_with(|| b.count.cmp(&a.count))
                .then_with(|| a.service.cmp(&b.service))
                .then_with(|| a.method.cmp(&b.method))
        });
        LatencyHistogramReport {
            total_calls: lock.total_calls,
            overall_p99_micros,
            overall_p99_ms: overall_p99_micros as f64 / 1000.0,
            methods,
        }
    }

    /// Exports the current handler latency histogram report as pretty JSON to `path` (`#1077 §A`).
    pub fn export_latency_histogram_json(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let report = self.latency_histogram_report();
        let json = serde_json::to_string_pretty(&report)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
    }

    /// Records a slow handler `[PacketPerf]` warning (`> 100 ms`) (`#1077 §A`).
    pub fn record_packet_perf_warning(
        &self,
        session_id: u64,
        service: &str,
        method: &str,
        arg_size: usize,
        duration: Duration,
        bootstrap: bool,
    ) {
        let entry = PacketPerfWarning {
            timestamp_millis: now_unix_millis(),
            session_id,
            service: service.to_string(),
            method: method.to_string(),
            arg_size,
            duration_micros: duration.as_micros().min(u64::MAX as u128) as u64,
            bootstrap,
        };
        if let Ok(mut lock) = self.packet_perf_warnings.write() {
            if lock.len() >= MAX_PACKET_PERF_WARNINGS {
                lock.pop_front();
            }
            lock.push_back(entry);
        }
    }

    /// Returns up to `limit` most recent `[PacketPerf]` warnings in chronological order.
    pub fn recent_packet_perf_warnings(&self, limit: usize) -> Vec<PacketPerfWarning> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(lock) = self.packet_perf_warnings.read() else {
            return Vec::new();
        };
        let count = lock.len().min(limit);
        lock.iter().rev().take(count).rev().cloned().collect()
    }
}

/// Computes the marshaled wire byte size of RPC `args` and `kwargs` for `[PacketPerf]` logs.
pub fn encoded_args_byte_size(args: &[PyRep], kwargs: &HashMap<String, PyRep>) -> usize {
    let args_rep = PyRep::Tuple(args.to_vec());
    let mut total = eve_proto::marshal::encode(&args_rep).len();
    if !kwargs.is_empty() {
        let kw_rep = PyRep::Dict(
            kwargs
                .iter()
                .map(|(k, v)| (PyRep::String(k.clone()), v.clone()))
                .collect(),
        );
        total = total.saturating_add(eve_proto::marshal::encode(&kw_rep).len());
    }
    total
}

fn extract_entity_from_destiny_action(action: &str, args: &PyRep) -> Option<i64> {
    if action == "SetState" {
        if let Some(id) = args.dict_get("ego").and_then(PyRep::as_i64) {
            return Some(id);
        }
        if let PyRep::Tuple(items) | PyRep::List(items) = args {
            for item in items {
                if let Some(id) = item.dict_get("ego").and_then(PyRep::as_i64) {
                    return Some(id);
                }
            }
        }
        return None;
    }
    match args {
        PyRep::Tuple(items) | PyRep::List(items) => {
            if let Some(id) = items.first().and_then(PyRep::as_i64) {
                return Some(id);
            }
            if action == "AddBalls2" {
                if let Some(PyRep::Tuple(bundle) | PyRep::List(bundle)) = items.first() {
                    if let Some(PyRep::List(slims) | PyRep::Tuple(slims)) = bundle.get(1) {
                        if let Some(id) = slims
                            .first()
                            .and_then(|s| s.dict_get("itemID").and_then(PyRep::as_i64))
                        {
                            return Some(id);
                        }
                    }
                    if let Some(PyRep::Tuple(ball_hdr) | PyRep::List(ball_hdr)) = bundle.first() {
                        if let Some(id) = ball_hdr.first().and_then(PyRep::as_i64) {
                            return Some(id);
                        }
                    }
                }
            }
            None
        }
        PyRep::Integer(id) => Some(*id),
        _ => None,
    }
}

/// Extracts `(stamp, action, entity_id)` triples from a `DoDestinyUpdate` `updates` `PyRep`.
pub fn extract_destiny_stamp_triples(
    default_stamp: u32,
    updates: &PyRep,
) -> Vec<(u32, String, Option<i64>)> {
    let items = match updates {
        PyRep::List(items) | PyRep::Tuple(items) => items,
        _ => return Vec::new(),
    };
    let mut out = Vec::with_capacity(items.len());
    for entry in items {
        let outer = match entry {
            PyRep::Tuple(v) | PyRep::List(v) if v.len() >= 2 => v,
            _ => continue,
        };
        let stamp = outer[0]
            .as_i64()
            .and_then(|s| u32::try_from(s).ok())
            .unwrap_or(default_stamp);
        let action_pair = match &outer[1] {
            PyRep::Tuple(v) | PyRep::List(v) if !v.is_empty() => v,
            _ => continue,
        };
        let Some(action) = action_pair[0].as_str() else {
            continue;
        };
        let entity_id = action_pair
            .get(1)
            .and_then(|args| extract_entity_from_destiny_action(action, args));
        out.push((stamp, action.to_string(), entity_id));
    }
    out
}

/// Summarizes arguments while redacting sensitive fields (passwords, tokens, credentials).
fn summarize_args(
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> String {
    // Redact password or token methods
    let method_lc = method.to_ascii_lowercase();
    if service == "client_login"
        || service == "authentication"
        || method_lc.contains("login")
        || method_lc.contains("auth")
        || method_lc.contains("password")
        || method_lc.contains("token")
        || method_lc.contains("secret")
    {
        return "(***redacted credentials***)".to_string();
    }

    let mut parts = Vec::new();
    for a in args.iter().take(4) {
        parts.push(sanitize_pyrep(a));
    }
    if args.len() > 4 {
        parts.push(format!("... (+{} args)", args.len() - 4));
    }
    for (k, v) in kwargs.iter().take(2) {
        parts.push(format!("{k}={}", sanitize_pyrep(v)));
    }
    if kwargs.len() > 2 {
        parts.push(format!("... (+{} kwargs)", kwargs.len() - 2));
    }

    let summary = format!("({})", parts.join(", "));
    if summary.chars().count() > MAX_ARGS_REPR_LEN {
        let prefix: String = summary
            .chars()
            .take(MAX_ARGS_REPR_LEN.saturating_sub(4))
            .collect();
        format!("{prefix}...)")
    } else {
        summary
    }
}

fn sanitize_pyrep(rep: &PyRep) -> String {
    match rep {
        PyRep::None => "None".to_string(),
        PyRep::Bool(b) => b.to_string(),
        PyRep::Integer(i) => i.to_string(),
        PyRep::Real(f) => format!("{f:.2}"),
        PyRep::String(s) => {
            if s.chars().count() > 32 {
                let prefix: String = s.chars().take(32).collect();
                format!("\"{prefix}...\"")
            } else {
                format!("\"{s}\"")
            }
        }
        PyRep::List(items) => format!("[{} items]", items.len()),
        PyRep::Tuple(items) => format!("({} items)", items.len()),
        PyRep::Dict(entries) => format!("{{{} entries}}", entries.len()),
        PyRep::Object { type_name, .. } => format!("<obj {type_name}>"),
        _ => "<pyrep>".to_string(),
    }
}

#[cfg(test)]
#[path = "flight_recorder_tests.rs"]
mod flight_recorder_tests;

#[cfg(test)]
#[path = "flight_recorder_extra_tests.rs"]
mod flight_recorder_extra_tests;
