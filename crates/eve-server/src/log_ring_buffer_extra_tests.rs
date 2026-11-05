//! Additional unit and mutation-killing tests for `log_ring_buffer`.

#![forbid(unsafe_code)]

use super::*;
use std::sync::Arc;
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn log_ring_buffer_exact_capacity_boundaries_and_format_line_epochs() {
    assert_eq!(global_log_ring().capacity, 500);
    assert_eq!(LogRingBuffer::new(0).capacity, 1);
    assert_eq!(LogRingBuffer::new(1).capacity, 1);
    assert_eq!(LogRingBuffer::new(4).capacity, 4);

    let buf = LogRingBuffer::new(2);
    buf.record("INFO", "t1", "m1");
    assert_eq!(buf.entries.read().unwrap().len(), 1);
    assert!(buf.entries.read().unwrap()[0].timestamp_millis > 1_700_000_000_000);

    // At len == 1 (< capacity 2), second record does NOT evict m1.
    buf.record("WARN", "t2", "m2");
    let snap2 = buf.snapshot_recent(2);
    assert_eq!(snap2.len(), 2);
    assert!(snap2[0].ends_with("INFO  [t1] m1"));
    assert!(snap2[1].ends_with("WARN  [t2] m2"));

    // At len == 2 (== capacity 2), third record evicts m1 and keeps m2, m3.
    buf.record("ERROR", "t3", "m3");
    let snap3 = buf.snapshot_recent(5);
    assert_eq!(snap3.len(), 2);
    assert!(snap3[0].ends_with("WARN  [t2] m2"));
    assert!(snap3[1].ends_with("ERROR [t3] m3"));

    // Direct format_log_line checks for epoch 0, sub-second millis, and 1-second rollover.
    let e0 = LogRingEntry {
        timestamp_millis: 0,
        level: "TRACE".into(),
        target: "sys".into(),
        message: "zero".into(),
    };
    assert_eq!(
        format_log_line(&e0),
        "1970-01-01T00:00:00.000Z TRACE [sys] zero"
    );

    let e999 = LogRingEntry {
        timestamp_millis: 999,
        level: "INFO".into(),
        target: "sys".into(),
        message: "subsec".into(),
    };
    assert_eq!(
        format_log_line(&e999),
        "1970-01-01T00:00:00.999Z INFO  [sys] subsec"
    );

    let e1000 = LogRingEntry {
        timestamp_millis: 1_000,
        level: "WARN".into(),
        target: "sys".into(),
        message: "onesec".into(),
    };
    assert_eq!(
        format_log_line(&e1000),
        "1970-01-01T00:00:01.000Z WARN  [sys] onesec"
    );

    // Serde round-trip on LogRingEntry
    let json = serde_json::to_string(&e999).expect("serialize LogRingEntry");
    let decoded: LogRingEntry = serde_json::from_str(&json).expect("deserialize LogRingEntry");
    assert_eq!(decoded.timestamp_millis, 999);
    assert_eq!(decoded.level, "INFO");
    assert_eq!(decoded.target, "sys");
    assert_eq!(decoded.message, "subsec");
}

#[test]
fn log_ring_layer_visitor_handles_str_and_debug_message_and_field_ordering() {
    let buf = Arc::new(LogRingBuffer::new(16));
    let subscriber = tracing_subscriber::registry().with(LogRingLayer::new(Arc::clone(&buf)));

    tracing::subscriber::with_default(subscriber, || {
        // 1) String field first (when self.0 is empty), followed by second string field.
        tracing::info!(target: "vis", first = "alpha", second = "beta");
        // 2) Explicit string `message` field + structured field (`record_str` on "message").
        tracing::error!(target: "vis", message = "explicit_str_message", tag = " gamma ");
        // 3) Debug formatting on `message` (`format_args!`) + debug field.
        tracing::debug!(target: "vis", count = 7usize, "fmt_msg_{}", 99);
    });

    let lines = buf.snapshot_recent(10);
    assert_eq!(lines.len(), 3);
    assert!(
        lines[0].ends_with("INFO  [vis] first=\"alpha\" second=\"beta\""),
        "got: {}",
        lines[0]
    );
    assert!(
        lines[1].contains("ERROR [vis]") && lines[1].contains("explicit_str_message"),
        "got: {}",
        lines[1]
    );
    assert!(
        lines[1].contains("tag=\" gamma \""),
        "got: {}",
        lines[1]
    );
    assert!(
        lines[2].contains("DEBUG [vis]")
            && lines[2].contains("fmt_msg_99")
            && lines[2].contains("count=7"),
        "got: {}",
        lines[2]
    );
}

#[test]
fn log_ring_buffer_poisoned_lock_returns_empty_snapshots() {
    let buf = Arc::new(LogRingBuffer::new(4));
    buf.record("INFO", "ok", "before_poison");
    assert_eq!(buf.snapshot_recent(4).len(), 1);
    assert_eq!(buf.snapshot_since(0, 0).len(), 0);
    assert_eq!(buf.snapshot_since(0, 4).len(), 1);

    let b_clone = Arc::clone(&buf);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = b_clone.entries.write().unwrap();
        panic!("poison log ring rwlock");
    }));

    assert!(buf.snapshot_recent(10).is_empty());
    assert!(buf.snapshot_since(0, 10).is_empty());
    // Recording into a poisoned buffer safely no-ops without panicking.
    buf.record("ERROR", "poison", "ignored");
}
