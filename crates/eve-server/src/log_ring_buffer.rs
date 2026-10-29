//! In-memory circular buffer for server log lines.
//!
//! Captures recent server logs to attach to incident reports (before and after).

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

const DEFAULT_RING_CAPACITY: usize = 500;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogRingEntry {
    pub timestamp_millis: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

pub struct LogRingBuffer {
    capacity: usize,
    entries: RwLock<VecDeque<LogRingEntry>>,
}

static GLOBAL_LOG_RING: OnceLock<Arc<LogRingBuffer>> = OnceLock::new();

pub fn global_log_ring() -> Arc<LogRingBuffer> {
    GLOBAL_LOG_RING
        .get_or_init(|| Arc::new(LogRingBuffer::new(DEFAULT_RING_CAPACITY)))
        .clone()
}

impl LogRingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: RwLock::new(VecDeque::with_capacity(capacity)),
        }
    }

    /// Records a log event into the buffer.
    pub fn record(
        &self,
        level: impl Into<String>,
        target: impl Into<String>,
        message: impl Into<String>,
    ) {
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let entry = LogRingEntry {
            timestamp_millis: now_millis,
            level: level.into(),
            target: target.into(),
            message: message.into(),
        };

        if let Ok(mut lock) = self.entries.write() {
            if lock.len() >= self.capacity {
                lock.pop_front();
            }
            lock.push_back(entry);
        }
    }

    /// Returns the last `limit` log lines formatted as readable strings.
    pub fn snapshot_recent(&self, limit: usize) -> Vec<String> {
        let lock = match self.entries.read() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };

        let count = lock.len().min(limit);
        lock.iter()
            .rev()
            .take(count)
            .rev()
            .map(|e| format_log_line(e))
            .collect()
    }

    /// Returns log entries recorded since `since_millis`.
    pub fn snapshot_since(&self, since_millis: u64, limit: usize) -> Vec<String> {
        let lock = match self.entries.read() {
            Ok(l) => l,
            Err(_) => return Vec::new(),
        };

        lock.iter()
            .filter(|e| e.timestamp_millis >= since_millis)
            .take(limit)
            .map(|e| format_log_line(e))
            .collect()
    }
}

fn format_log_line(e: &LogRingEntry) -> String {
    let secs = e.timestamp_millis / 1000;
    let millis = e.timestamp_millis % 1000;
    let (y, m, d, hh, mm, ss) = crate::esi::sample_routes::civil_from_unix(secs);
    format!(
        "{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}.{millis:03}Z {:<5} [{}] {}",
        e.level, e.target, e.message
    )
}

/// A `tracing_subscriber::Layer` that writes formatted log events into `LogRingBuffer`.
pub struct LogRingLayer {
    buffer: Arc<LogRingBuffer>,
}

impl LogRingLayer {
    pub fn new(buffer: Arc<LogRingBuffer>) -> Self {
        Self { buffer }
    }
}

impl<S> tracing_subscriber::Layer<S> for LogRingLayer
where
    S: tracing::Subscriber,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let meta = event.metadata();
        let level = meta.level().to_string();
        let target = meta.target().to_string();

        struct MsgVisitor(String);
        impl tracing::field::Visit for MsgVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                } else {
                    if !self.0.is_empty() {
                        self.0.push(' ');
                    }
                    self.0.push_str(&format!("{}={:?}", field.name(), value));
                }
            }
            fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
                if field.name() == "message" {
                    self.0 = value.to_string();
                } else {
                    if !self.0.is_empty() {
                        self.0.push(' ');
                    }
                    self.0.push_str(&format!("{}=\"{}\"", field.name(), value));
                }
            }
        }

        let mut visitor = MsgVisitor(String::new());
        event.record(&mut visitor);
        self.buffer.record(level, target, visitor.0);
    }
}
