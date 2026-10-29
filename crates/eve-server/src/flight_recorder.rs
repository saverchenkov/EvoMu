//! In-memory rolling ring buffer of recent MachoNet RPC calls per session ("Flight Recorder").
//!
//! Provides the diagnostic causality chain attached to client exception alerts.

#![forbid(unsafe_code)]

use eve_proto::PyRep;
use std::collections::{HashMap, VecDeque};
use std::sync::RwLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_HISTORY_PER_SESSION: usize = 30;
const MAX_TRACKED_SESSIONS: usize = 512;
const MAX_ARGS_REPR_LEN: usize = 160;

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

pub struct FlightRecorder {
    sessions: RwLock<HashMap<u64, VecDeque<FlightRecorderEntry>>>,
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
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

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
