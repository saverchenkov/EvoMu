//! In-memory rolling ring buffer of recent MachoNet RPC calls per session ("Flight Recorder").
//!
//! Provides the diagnostic causality chain attached to client exception alerts.

#![forbid(unsafe_code)]

use eve_proto::PyRep;
use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_HISTORY_PER_SESSION: usize = 30;
pub const MAX_OUTBOUND_LEDGER_PER_SESSION: usize = 500;
pub const MAX_DESTINY_STAMPS_PER_SESSION: usize = 500;
pub const MAX_INVARIANT_VIOLATIONS_PER_SESSION: usize = 200;
pub const MAX_PREVENTED_JOLTS_PER_SESSION: usize = 200;
pub const PACKET_PERF_WARN_THRESHOLD_MS: u64 = 100;
const MAX_PACKET_PERF_WARNINGS: usize = 200;
const MAX_HISTOGRAM_SAMPLES_PER_METHOD: usize = 2048;
const MAX_GLOBAL_HISTOGRAM_SAMPLES: usize = 4096;
pub const MAX_TRACKED_RPC_METHODS: usize = 2_048;
pub const MAX_RPC_LATENCY_KEY_LEN: usize = 128;
pub const UNKNOWN_RPC_LABEL: &str = "<unknown>";
const MAX_INDIVIDUAL_RPC_METHODS: usize = MAX_TRACKED_RPC_METHODS.saturating_sub(1);
const MAX_TRACKED_SESSIONS: usize = 512;
const MAX_ARGS_REPR_LEN: usize = 160;
const MAX_UNAUTH_WARN_KEYS: usize = 64;
const UNAUTH_WARN_RATE_LIMIT_SECS: u64 = 10;

fn is_valid_rpc_latency_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment.len() <= MAX_RPC_LATENCY_KEY_LEN
        && segment != UNKNOWN_RPC_LABEL
        && !segment.bytes().any(|b| b.is_ascii_control())
}

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

/// Bounded per-session invariant violation record captured by the outbound sequencer (`GH #1084`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SequencerInvariantViolationRecord {
    pub timestamp_millis: u64,
    pub session_id: u64,
    pub epoch: u32,
    pub stamp: u32,
    pub invariant: String,
    pub detail: String,
}

/// Bounded per-session prevented-jolt marker recorded when a backstepping stamp is rejected (`GH #1084`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PreventedJoltRecord {
    pub timestamp_millis: u64,
    pub session_id: u64,
    pub epoch: u32,
    pub batch_stamp: u32,
    pub trusted_stamp: u32,
    pub reason: String,
}

/// Per-session outbound sequencer counters exposed via `/session ledger` and `/ops` (`GH #1084`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionSequencerCounters {
    pub dropped_dependent_notifications: u64,
    pub dropped_dependent_by_name: HashMap<String, u64>,
    pub invariant_violations: u64,
    pub invariant_violations_by_name: HashMap<String, u64>,
    pub prevented_jolts: u64,
}

/// Cluster-wide sequencer gate/validator summary exposed via `--gm` `/ops` (`GH #1084`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SequencerObservabilitySummary {
    pub dropped_dependent_notifications_total: u64,
    pub dropped_dependent_by_name: HashMap<String, u64>,
    pub invariant_violations_total: u64,
    pub invariant_violations_by_name: HashMap<String, u64>,
    pub prevented_jolts_total: u64,
    pub sessions_with_counters: usize,
    pub recent_invariant_violations: Vec<SequencerInvariantViolationRecord>,
    pub recent_prevented_jolts: Vec<PreventedJoltRecord>,
}

/// Snapshot of a session's outbound `DestinyStream` / sequencer wire state (`#1088`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WireSessionSnapshot {
    pub session_id: u64,
    pub client_id: u64,
    pub materialized: bool,
    #[serde(default)]
    pub materialized_set_size: usize,
    pub destiny_state: String,
    pub epoch: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_state_stamp: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_stamp: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_client_tick: Option<u64>,
    pub in_space: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solar_system_id: Option<u32>,
}

fn push_bounded_session_queue<T>(
    map: &mut HashMap<u64, VecDeque<T>>,
    session_id: u64,
    cap: usize,
    entry: T,
) {
    if map.len() >= MAX_TRACKED_SESSIONS && !map.contains_key(&session_id) {
        if let Some(oldest) = map.keys().next().copied() {
            map.remove(&oldest);
        }
    }
    let queue = map.entry(session_id).or_default();
    if queue.len() >= cap {
        queue.pop_front();
    }
    queue.push_back(entry);
}

fn read_bounded_session_queue<T: Clone>(
    lock: &RwLock<HashMap<u64, VecDeque<T>>>,
    session_id: u64,
    limit: usize,
) -> Vec<T> {
    if limit == 0 {
        return Vec::new();
    }
    let Ok(guard) = lock.read() else {
        return Vec::new();
    };
    let Some(queue) = guard.get(&session_id) else {
        return Vec::new();
    };
    let count = queue.len().min(limit);
    queue.iter().rev().take(count).rev().cloned().collect()
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
    per_method: HashMap<String, HashMap<String, MethodLatencyBucket>>,
    tracked_method_count: usize,
    unknown_bucket: Option<MethodLatencyBucket>,
}

impl LatencyHistogramState {
    fn record_unknown(&mut self, micros: u64) {
        self.unknown_bucket
            .get_or_insert_with(MethodLatencyBucket::default)
            .record(micros);
    }

    fn record_call(&mut self, service: &str, method: &str, track_individual: bool, micros: u64) {
        self.total_calls = self.total_calls.saturating_add(1);
        if self.global_samples_micros.len() >= MAX_GLOBAL_HISTOGRAM_SAMPLES {
            self.global_samples_micros.pop_front();
        }
        self.global_samples_micros.push_back(micros);

        let key_valid = track_individual
            && is_valid_rpc_latency_segment(service)
            && is_valid_rpc_latency_segment(method);
        if !key_valid {
            self.record_unknown(micros);
            return;
        }

        if let Some(bucket) = self
            .per_method
            .get_mut(service)
            .and_then(|methods| methods.get_mut(method))
        {
            bucket.record(micros);
            return;
        }

        // Reserve one slot for the `<unknown>` bucket so tracked entries stay bounded at
        // `<= MAX_TRACKED_RPC_METHODS` regardless of call order.
        if self.tracked_method_count >= MAX_INDIVIDUAL_RPC_METHODS {
            self.record_unknown(micros);
            return;
        }

        let mut bucket = MethodLatencyBucket::default();
        bucket.record(micros);
        if let Some(methods) = self.per_method.get_mut(service) {
            methods.insert(method.to_string(), bucket);
        } else {
            let mut methods = HashMap::new();
            methods.insert(method.to_string(), bucket);
            self.per_method.insert(service.to_string(), methods);
        }
        self.tracked_method_count = self.tracked_method_count.saturating_add(1);
    }
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

/// Client/server MachoNet handshake boot-build metadata captured for incident triage (`BF-1344-03`).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HandshakeBuildMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_boot_build: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_boot_build: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_mismatch: Option<bool>,
}

/// Cumulative per-session RPC and handshake telemetry queried by bug-reporting bundles.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionRpcTelemetry {
    #[serde(default)]
    pub server_request_count: u64,
    #[serde(default)]
    pub gateway_request_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handshake: Option<HandshakeBuildMetadata>,
}

pub struct FlightRecorder {
    sessions: RwLock<HashMap<u64, VecDeque<FlightRecorderEntry>>>,
    outbound_ledgers: RwLock<HashMap<u64, VecDeque<OutboundLedgerEntry>>>,
    destiny_stamps: RwLock<HashMap<u64, VecDeque<DestinyStampRecord>>>,
    wire_states: RwLock<HashMap<u64, WireSessionSnapshot>>,
    latency_histograms: RwLock<LatencyHistogramState>,
    packet_perf_warnings: RwLock<VecDeque<PacketPerfWarning>>,
    sequencer_counters: RwLock<HashMap<u64, SessionSequencerCounters>>,
    invariant_violations: RwLock<HashMap<u64, VecDeque<SequencerInvariantViolationRecord>>>,
    prevented_jolts: RwLock<HashMap<u64, VecDeque<PreventedJoltRecord>>>,
    session_request_counts: RwLock<HashMap<u64, u64>>,
    session_gateway_counts: RwLock<HashMap<u64, u64>>,
    user_gateway_counts: RwLock<HashMap<u64, u64>>,
    session_handshakes: RwLock<HashMap<u64, HandshakeBuildMetadata>>,
    preauth_handshakes: RwLock<HashMap<u64, HandshakeBuildMetadata>>,
    user_handshakes: RwLock<HashMap<u64, HandshakeBuildMetadata>>,
    quasar_unauth_warn_times: RwLock<HashMap<(Option<IpAddr>, &'static str), u64>>,
    quasar_unauth_warn_fallback_unix: AtomicU64,
}

impl Default for FlightRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for FlightRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlightRecorder").finish_non_exhaustive()
    }
}

impl FlightRecorder {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
            outbound_ledgers: RwLock::new(HashMap::new()),
            destiny_stamps: RwLock::new(HashMap::new()),
            wire_states: RwLock::new(HashMap::new()),
            latency_histograms: RwLock::new(LatencyHistogramState::default()),
            packet_perf_warnings: RwLock::new(VecDeque::new()),
            sequencer_counters: RwLock::new(HashMap::new()),
            invariant_violations: RwLock::new(HashMap::new()),
            prevented_jolts: RwLock::new(HashMap::new()),
            session_request_counts: RwLock::new(HashMap::new()),
            session_gateway_counts: RwLock::new(HashMap::new()),
            user_gateway_counts: RwLock::new(HashMap::new()),
            session_handshakes: RwLock::new(HashMap::new()),
            preauth_handshakes: RwLock::new(HashMap::new()),
            user_handshakes: RwLock::new(HashMap::new()),
            quasar_unauth_warn_times: RwLock::new(HashMap::new()),
            quasar_unauth_warn_fallback_unix: AtomicU64::new(0),
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

        Self::increment_bounded_counter(&self.session_request_counts, session_id);
        if service == "publicGatewaySvc" {
            Self::increment_bounded_counter(&self.session_gateway_counts, session_id);
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

    /// Records handler latency in microseconds for `(service, method)` (`#1077 §A`), enforcing
    /// key-length and entry-count bounds directly inside `FlightRecorder`.
    pub fn record_rpc_latency(&self, service: &str, method: &str, duration: Duration) {
        self.record_rpc_latency_classified(service, method, true, duration);
    }

    /// Records handler latency in microseconds while collapsing unimplemented, rejected,
    /// invalid, oversized, or over-cap calls into static `("<unknown>", "<unknown>")`.
    pub fn record_rpc_latency_classified(
        &self,
        service: &str,
        method: &str,
        track_individual: bool,
        duration: Duration,
    ) {
        let micros = duration.as_micros().min(u64::MAX as u128) as u64;
        if let Ok(mut lock) = self.latency_histograms.write() {
            lock.record_call(service, method, track_individual, micros);
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
        let mut methods: Vec<LatencyHistogramSummary> =
            Vec::with_capacity(lock.tracked_method_count + usize::from(lock.unknown_bucket.is_some()));
        for (svc, svc_methods) in &lock.per_method {
            for (meth, bucket) in svc_methods {
                methods.push(bucket.summarize(svc, meth));
            }
        }
        if let Some(unknown) = &lock.unknown_bucket {
            methods.push(unknown.summarize(UNKNOWN_RPC_LABEL, UNKNOWN_RPC_LABEL));
        }
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

    fn update_session_counters(&self, session_id: u64, f: impl FnOnce(&mut SessionSequencerCounters)) {
        if let Ok(mut lock) = self.sequencer_counters.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&session_id) {
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            let entry = lock.entry(session_id).or_default();
            f(entry);
        }
    }

    /// Increments the per-session and per-name dropped dependent notification counter (`GH #1084`).
    pub fn record_dropped_dependent_notification(&self, session_id: u64, notification_name: &str) {
        self.update_session_counters(session_id, |c| {
            c.dropped_dependent_notifications = c.dropped_dependent_notifications.saturating_add(1);
            *c.dropped_dependent_by_name
                .entry(notification_name.to_string())
                .or_insert(0) += 1;
        });
    }

    /// Records a destiny group invariant violation for `session_id` (`GH #1084`).
    pub fn record_invariant_violation(
        &self,
        session_id: u64,
        epoch: u32,
        stamp: u32,
        invariant: &str,
        detail: &str,
    ) {
        self.update_session_counters(session_id, |c| {
            c.invariant_violations = c.invariant_violations.saturating_add(1);
            *c.invariant_violations_by_name
                .entry(invariant.to_string())
                .or_insert(0) += 1;
        });
        let entry = SequencerInvariantViolationRecord {
            timestamp_millis: now_unix_millis(),
            session_id,
            epoch,
            stamp,
            invariant: invariant.to_string(),
            detail: detail.to_string(),
        };
        if let Ok(mut lock) = self.invariant_violations.write() {
            push_bounded_session_queue(
                &mut lock,
                session_id,
                MAX_INVARIANT_VIOLATIONS_PER_SESSION,
                entry,
            );
        }
    }

    /// Records a prevented-jolt marker when a backstepping destiny batch is rejected (`GH #1084`).
    pub fn record_prevented_jolt(
        &self,
        session_id: u64,
        epoch: u32,
        batch_stamp: u32,
        trusted_stamp: u32,
        reason: &str,
    ) {
        self.update_session_counters(session_id, |c| {
            c.prevented_jolts = c.prevented_jolts.saturating_add(1);
        });
        let entry = PreventedJoltRecord {
            timestamp_millis: now_unix_millis(),
            session_id,
            epoch,
            batch_stamp,
            trusted_stamp,
            reason: reason.to_string(),
        };
        if let Ok(mut lock) = self.prevented_jolts.write() {
            push_bounded_session_queue(&mut lock, session_id, MAX_PREVENTED_JOLTS_PER_SESSION, entry);
        }
    }

    /// Returns the per-session gate, invariant violation, and prevented-jolt counters (`GH #1084`).
    pub fn session_sequencer_counters(&self, session_id: u64) -> SessionSequencerCounters {
        self.sequencer_counters
            .read()
            .ok()
            .and_then(|lock| lock.get(&session_id).cloned())
            .unwrap_or_default()
    }

    /// Returns up to `limit` most recent invariant violations for `session_id` in chronological order.
    pub fn get_session_invariant_violations(
        &self,
        session_id: u64,
        limit: usize,
    ) -> Vec<SequencerInvariantViolationRecord> {
        read_bounded_session_queue(&self.invariant_violations, session_id, limit)
    }

    /// Returns up to `limit` most recent prevented-jolt markers for `session_id` in chronological order.
    pub fn get_session_prevented_jolts(
        &self,
        session_id: u64,
        limit: usize,
    ) -> Vec<PreventedJoltRecord> {
        read_bounded_session_queue(&self.prevented_jolts, session_id, limit)
    }

    /// Returns the aggregated cluster-wide gate/validator/prevented-jolt summary for `/ops` (`GH #1084`).
    pub fn sequencer_observability_summary(&self, recent_limit: usize) -> SequencerObservabilitySummary {
        let mut summary = SequencerObservabilitySummary::default();
        if let Ok(lock) = self.sequencer_counters.read() {
            summary.sessions_with_counters = lock.len();
            for c in lock.values() {
                summary.dropped_dependent_notifications_total = summary
                    .dropped_dependent_notifications_total
                    .saturating_add(c.dropped_dependent_notifications);
                for (name, count) in &c.dropped_dependent_by_name {
                    *summary
                        .dropped_dependent_by_name
                        .entry(name.clone())
                        .or_insert(0) += *count;
                }
                summary.invariant_violations_total = summary
                    .invariant_violations_total
                    .saturating_add(c.invariant_violations);
                for (name, count) in &c.invariant_violations_by_name {
                    *summary
                        .invariant_violations_by_name
                        .entry(name.clone())
                        .or_insert(0) += *count;
                }
                summary.prevented_jolts_total =
                    summary.prevented_jolts_total.saturating_add(c.prevented_jolts);
            }
        }
        if recent_limit > 0 {
            if let Ok(lock) = self.invariant_violations.read() {
                let mut all: Vec<SequencerInvariantViolationRecord> = lock
                    .values()
                    .flat_map(|q| q.iter().cloned())
                    .collect();
                all.sort_by_key(|e| (e.timestamp_millis, e.session_id, e.stamp));
                let start = all.len().saturating_sub(recent_limit);
                summary.recent_invariant_violations = all.split_off(start);
            }
            if let Ok(lock) = self.prevented_jolts.read() {
                let mut all: Vec<PreventedJoltRecord> = lock
                    .values()
                    .flat_map(|q| q.iter().cloned())
                    .collect();
                all.sort_by_key(|e| (e.timestamp_millis, e.session_id, e.batch_stamp));
                let start = all.len().saturating_sub(recent_limit);
                summary.recent_prevented_jolts = all.split_off(start);
            }
        }
        summary
    }

    /// Snapshots a session's outbound `DestinyStream` / sequencer state for `/session` inspection (`#1088`).
    pub fn record_wire_state(&self, wire_state: &crate::outbound_sequencer::SessionWireState) {
        use crate::outbound_sequencer::DestinyStream;
        let (destiny_state, epoch, set_state_stamp, last_stamp) = match wire_state.destiny {
            DestinyStream::Detached => ("Detached".to_string(), 0, None, None),
            DestinyStream::AwaitingSetState { epoch } => {
                ("AwaitingSetState".to_string(), epoch, None, None)
            }
            DestinyStream::Live {
                epoch,
                set_state_stamp,
                last_stamp,
            } => (
                "Live".to_string(),
                epoch,
                Some(set_state_stamp),
                Some(last_stamp),
            ),
        };
        let snap = WireSessionSnapshot {
            session_id: wire_state.session_id,
            client_id: wire_state.client_id,
            materialized: wire_state.destiny.is_live(),
            materialized_set_size: wire_state.materialized.len(),
            destiny_state,
            epoch,
            set_state_stamp,
            last_stamp,
            last_client_tick: wire_state.last_client_tick,
            in_space: wire_state.in_space,
            solar_system_id: wire_state.solar_system_id,
        };
        if let Ok(mut lock) = self.wire_states.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&wire_state.session_id) {
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            lock.insert(wire_state.session_id, snap);
        }
    }

    /// Removes the cached wire state snapshot when a connection closes (`#1088`).
    pub fn remove_wire_state(&self, session_id: u64) {
        if let Ok(mut lock) = self.wire_states.write() {
            lock.remove(&session_id);
        }
    }

    /// Returns the latest cached wire state snapshot for `session_id` (`#1088`).
    pub fn get_wire_state(&self, session_id: u64) -> Option<WireSessionSnapshot> {
        self.wire_states
            .read()
            .ok()
            .and_then(|lock| lock.get(&session_id).cloned())
    }

    fn increment_bounded_counter(map: &RwLock<HashMap<u64, u64>>, key: u64) {
        if let Ok(mut lock) = map.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&key) {
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            *lock.entry(key).or_insert(0) += 1;
        }
    }

    fn upsert_bounded_handshake(
        map: &RwLock<HashMap<u64, HandshakeBuildMetadata>>,
        key: u64,
        client_boot_build: Option<u32>,
        server_boot_build: u32,
        build_mismatch: bool,
    ) {
        if let Ok(mut lock) = map.write() {
            if lock.len() >= MAX_TRACKED_SESSIONS && !lock.contains_key(&key) {
                if let Some(oldest) = lock.keys().next().copied() {
                    lock.remove(&oldest);
                }
            }
            let entry = lock.entry(key).or_default();
            if let Some(cb) = client_boot_build {
                entry.client_boot_build = Some(cb);
            }
            entry.server_boot_build = Some(server_boot_build);
            // Recompute build_mismatch cleanly when a fresh client build arrives so upgraded reconnects clear prior flags.
            entry.build_mismatch = if build_mismatch {
                Some(true)
            } else if let Some(cb) = client_boot_build.or(entry.client_boot_build) {
                Some(cb < server_boot_build)
            } else {
                None
            };
        }
    }

    /// Clears stale per-user gateway & handshake telemetry upon fresh MachoNet authentication (`BF-1344-03`).
    pub fn reset_user_telemetry(&self, user_id: u64) {
        if user_id == 0 {
            return;
        }
        if let Ok(mut lock) = self.user_gateway_counts.write() {
            lock.remove(&user_id);
        }
        if let Ok(mut lock) = self.user_handshakes.write() {
            lock.remove(&user_id);
        }
    }

    /// Removes per-session telemetry maps on disconnect alongside `remove_wire_state` (`BF-1344-02`, `BF-1344-03`).
    pub fn remove_session_telemetry(&self, session_id: u64) {
        if let Ok(mut lock) = self.session_request_counts.write() {
            lock.remove(&session_id);
        }
        if let Ok(mut lock) = self.session_gateway_counts.write() {
            lock.remove(&session_id);
        }
        if let Ok(mut lock) = self.session_handshakes.write() {
            lock.remove(&session_id);
        }
    }

    /// Records a server-reached Quasar gRPC / public gateway request (`BF-1344-03`).
    pub fn record_gateway_request(&self, online_session_id: Option<u64>, user_id: Option<u64>) {
        if let Some(sid) = online_session_id.filter(|&id| id > 0) {
            Self::increment_bounded_counter(&self.session_gateway_counts, sid);
        }
        if let Some(uid) = user_id.filter(|&id| id > 0) {
            Self::increment_bounded_counter(&self.user_gateway_counts, uid);
        }
    }

    /// Rate-limits header-rejection `UNAUTHENTICATED` warnings per `(peer_ip, route)` on this
    /// server context (`<= 1` warning per 10s per key, bounded at `64` keys with a fallback cap)
    /// so repeated probe floods cannot evict entries from the 256-entry `global_log_ring()`.
    pub fn should_warn_quasar_unauth_header(
        &self,
        peer_ip: Option<IpAddr>,
        path: &str,
        now_unix: u64,
    ) -> bool {
        let route: &'static str = match path {
            "/eve_public.gateway.Requests/Send" => "/eve_public.gateway.Requests/Send",
            "/eve_public.gateway.Notices/Consume" => "/eve_public.gateway.Notices/Consume",
            "/eve_public.gateway.Events/Publish" => "/eve_public.gateway.Events/Publish",
            _ => "other",
        };
        let now = now_unix.max(1);
        let key = (peer_ip, route);
        if let Ok(mut map) = self.quasar_unauth_warn_times.write() {
            if let Some(&prev) = map.get(&key) {
                if now < prev.saturating_add(UNAUTH_WARN_RATE_LIMIT_SECS) {
                    return false;
                }
                map.insert(key, now);
                return true;
            }
            if map.len() >= MAX_UNAUTH_WARN_KEYS {
                map.retain(|_, &mut prev| now < prev.saturating_add(UNAUTH_WARN_RATE_LIMIT_SECS));
            }
            if map.len() < MAX_UNAUTH_WARN_KEYS {
                map.insert(key, now);
                return true;
            }
        }
        let prev = self.quasar_unauth_warn_fallback_unix.load(Ordering::Relaxed);
        (prev == 0 || now >= prev.saturating_add(UNAUTH_WARN_RATE_LIMIT_SECS))
            && self
                .quasar_unauth_warn_fallback_unix
                .compare_exchange(prev, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
    }

    /// Records client and server boot builds observed during MachoNet handshake (`BF-1344-03`).
    pub fn record_handshake_builds(
        &self,
        session_id: u64,
        user_id: Option<u64>,
        client_boot_build: Option<u32>,
        server_boot_build: u32,
        build_mismatch: bool,
    ) {
        let uid_opt = user_id.filter(|&id| id > 0);
        if session_id > 0 {
            if uid_opt.is_some() {
                Self::upsert_bounded_handshake(
                    &self.session_handshakes,
                    session_id,
                    client_boot_build,
                    server_boot_build,
                    build_mismatch,
                );
                if let Ok(mut preauth) = self.preauth_handshakes.write() {
                    preauth.remove(&session_id);
                }
            } else {
                Self::upsert_bounded_handshake(
                    &self.preauth_handshakes,
                    session_id,
                    client_boot_build,
                    server_boot_build,
                    build_mismatch,
                );
            }
        }
        if let Some(uid) = uid_opt {
            let resolved_client = client_boot_build.or_else(|| {
                if session_id > 0 {
                    self.session_handshakes
                        .read()
                        .ok()
                        .and_then(|lock| lock.get(&session_id).and_then(|h| h.client_boot_build))
                } else {
                    None
                }
            });
            Self::upsert_bounded_handshake(
                &self.user_handshakes,
                uid,
                resolved_client,
                server_boot_build,
                build_mismatch,
            );
        }
    }

    /// Returns cumulative server/gateway RPC counts and handshake builds for `(session_id, user_id)`.
    pub fn get_session_telemetry(
        &self,
        session_id: u64,
        user_id: Option<u64>,
    ) -> SessionRpcTelemetry {
        let server_request_count = if session_id > 0 {
            self.session_request_counts
                .read()
                .ok()
                .and_then(|lock| lock.get(&session_id).copied())
                .unwrap_or(0)
        } else {
            0
        };
        let gateway_request_count = if session_id > 0 {
            self.session_gateway_counts
                .read()
                .ok()
                .and_then(|lock| lock.get(&session_id).copied())
                .unwrap_or(0)
        } else {
            user_id
                .filter(|&uid| uid > 0)
                .and_then(|uid| {
                    self.user_gateway_counts
                        .read()
                        .ok()
                        .and_then(|lock| lock.get(&uid).copied())
                })
                .unwrap_or(0)
        };
        let handshake = if session_id > 0 {
            self.session_handshakes
                .read()
                .ok()
                .and_then(|lock| lock.get(&session_id).cloned())
                .or_else(|| {
                    self.preauth_handshakes
                        .read()
                        .ok()
                        .and_then(|lock| lock.get(&session_id).cloned())
                })
        } else {
            user_id.filter(|&uid| uid > 0).and_then(|uid| {
                self.user_handshakes
                    .read()
                    .ok()
                    .and_then(|lock| lock.get(&uid).cloned())
            })
        };

        SessionRpcTelemetry {
            server_request_count,
            gateway_request_count,
            handshake,
        }
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
