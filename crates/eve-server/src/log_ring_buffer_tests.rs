//! Unit tests for `log_ring_buffer` circular buffer, timestamp filtering, and `tracing` layer.

#![forbid(unsafe_code)]

use super::*;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn log_ring_buffer_capacity_zero_and_rollover_behave_deterministically() {
    let min_buf = LogRingBuffer::new(0);
    min_buf.record("INFO", "boot", "first");
    min_buf.record("WARN", "boot", "second");
    let recent = min_buf.snapshot_recent(10);
    assert_eq!(recent.len(), 1);
    assert!(recent[0].ends_with("WARN  [boot] second"));

    let buf = LogRingBuffer::new(3);
    for i in 1..=5 {
        buf.record("INFO", "net", format!("msg-{i}"));
    }
    assert!(buf.snapshot_recent(0).is_empty());
    let last_two = buf.snapshot_recent(2);
    assert_eq!(last_two.len(), 2);
    assert!(last_two[0].ends_with("INFO  [net] msg-4"));
    assert!(last_two[1].ends_with("INFO  [net] msg-5"));

    let all_three = buf.snapshot_recent(10);
    assert_eq!(all_three.len(), 3);
    assert!(all_three[0].ends_with("INFO  [net] msg-3"));
}

#[test]
fn log_ring_buffer_formats_utc_and_filters_snapshot_since_inclusively() {
    let buf = LogRingBuffer::new(10);
    {
        let mut guard = buf.entries.write().unwrap();
        // 1_700_000_000_123 ms == 2023-11-14T22:13:20.123Z
        guard.push_back(LogRingEntry {
            timestamp_millis: 1_700_000_000_123,
            level: "INFO".to_string(),
            target: "evomu::rpc".to_string(),
            message: "alpha".to_string(),
        });
        guard.push_back(LogRingEntry {
            timestamp_millis: 1_700_000_001_005,
            level: "ERROR".to_string(),
            target: "evomu::db".to_string(),
            message: "beta".to_string(),
        });
        guard.push_back(LogRingEntry {
            timestamp_millis: 1_700_000_002_000,
            level: "DEBUG".to_string(),
            target: "evomu::sim".to_string(),
            message: "gamma".to_string(),
        });
    }

    let exact_since = buf.snapshot_since(1_700_000_000_123, 10);
    assert_eq!(exact_since.len(), 3);
    assert_eq!(
        exact_since[0],
        "2023-11-14T22:13:20.123Z INFO  [evomu::rpc] alpha"
    );
    assert_eq!(
        exact_since[1],
        "2023-11-14T22:13:21.005Z ERROR [evomu::db] beta"
    );

    // One millisecond later excludes the first entry; limit caps the remainder.
    let after_first = buf.snapshot_since(1_700_000_000_124, 1);
    assert_eq!(after_first.len(), 1);
    assert_eq!(
        after_first[0],
        "2023-11-14T22:13:21.005Z ERROR [evomu::db] beta"
    );

    assert!(buf.snapshot_since(1_700_000_002_001, 10).is_empty());
}

#[test]
fn log_ring_layer_captures_tracing_messages_and_structured_fields() {
    let g1 = global_log_ring();
    let g2 = global_log_ring();
    assert!(Arc::ptr_eq(&g1, &g2));

    let buf = Arc::new(LogRingBuffer::new(16));
    let subscriber = tracing_subscriber::registry().with(LogRingLayer::new(Arc::clone(&buf)));

    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(target: "quasar_gw", session_id = 42u64, region = "The Forge", "session ready");
        tracing::warn!(target: "quasar_gw", code = 503u32, detail = "overloaded");
    });

    let lines = buf.snapshot_recent(10);
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("INFO  [quasar_gw]"));
    assert!(lines[0].contains("session ready"));
    assert!(lines[0].contains("session_id=42"));
    assert!(lines[0].contains("region=\"The Forge\""));
    assert!(lines[1].contains("WARN  [quasar_gw]"));
    assert!(lines[1].contains("code=503 detail=\"overloaded\""));
}
