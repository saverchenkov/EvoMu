//! Unit tests for `flight_recorder` ring buffer, argument redaction, and PyRep sanitization.

#![forbid(unsafe_code)]

use super::*;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::time::Duration;

#[test]
fn flight_recorder_records_entries_and_enforces_per_session_capacity() {
    let recorder = FlightRecorder::default();
    assert!(recorder.get_history(1, 0, 10).is_empty());

    let kwargs = HashMap::new();
    for i in 0..35u64 {
        recorder.record(
            1,
            "svc",
            if i % 2 == 0 { Some(i + 100) } else { None },
            "op",
            &[PyRep::Integer(i as i64)],
            &kwargs,
            Duration::from_micros(250 + i),
            if i == 34 { "ERR" } else { "OK" },
        );
    }

    // MAX_HISTORY_PER_SESSION is 30: entries 0..5 were evicted, leaving 5..35.
    let all = recorder.get_history(1, 0, 100);
    assert_eq!(all.len(), MAX_HISTORY_PER_SESSION);
    assert_eq!(all[0].args_summary, "(5)");
    assert_eq!(all[0].bound_id, None);
    assert_eq!(all[0].duration_micros, 255);
    assert_eq!(all[0].status, "OK");
    assert_eq!(all[29].args_summary, "(34)");
    assert_eq!(all[29].bound_id, Some(134));
    assert_eq!(all[29].duration_micros, 284);
    assert_eq!(all[29].status, "ERR");

    // Limit caps returned tail entries and limit=0 returns empty.
    let tail = recorder.get_history(1, 0, 3);
    assert_eq!(tail.len(), 3);
    assert_eq!(tail[0].args_summary, "(32)");
    assert_eq!(tail[2].args_summary, "(34)");
    assert!(recorder.get_history(1, 0, 0).is_empty());

    // Relative offset_ms can be positive or negative depending on ref_timestamp_millis.
    let first_ts = all[0].timestamp_millis;
    let pos_offsets = recorder.get_history(1, first_ts.saturating_sub(120), 2);
    assert_eq!(pos_offsets[0].offset_ms, 120);
    let neg_offsets = recorder.get_history(1, first_ts + 500, 2);
    assert_eq!(neg_offsets[0].offset_ms, -500);
}

#[test]
fn flight_recorder_evicts_oldest_session_only_when_at_capacity_for_new_session() {
    let recorder = FlightRecorder::new();
    let kwargs = HashMap::new();

    for sid in 1..=(MAX_TRACKED_SESSIONS as u64) {
        recorder.record(
            sid,
            "stationSvc",
            None,
            "GetStation",
            &[PyRep::Integer(sid as i64)],
            &kwargs,
            Duration::from_micros(10),
            "OK",
        );
    }

    // Recording another event on an already-tracked session must not evict any session.
    recorder.record(
        1,
        "stationSvc",
        None,
        "GetStation",
        &[PyRep::Integer(999)],
        &kwargs,
        Duration::from_micros(20),
        "OK",
    );
    let present_before: usize = (1..=(MAX_TRACKED_SESSIONS as u64))
        .filter(|&sid| !recorder.get_history(sid, 0, 10).is_empty())
        .count();
    assert_eq!(present_before, MAX_TRACKED_SESSIONS);
    assert_eq!(recorder.get_history(1, 0, 10).len(), 2);

    // Adding a brand-new session beyond MAX_TRACKED_SESSIONS evicts one existing session.
    let overflow_sid = (MAX_TRACKED_SESSIONS as u64) + 1;
    recorder.record(
        overflow_sid,
        "stationSvc",
        None,
        "GetStation",
        &[],
        &kwargs,
        Duration::from_micros(30),
        "OK",
    );
    assert_eq!(recorder.get_history(overflow_sid, 0, 10).len(), 1);
    let present_after: usize = (1..=overflow_sid)
        .filter(|&sid| !recorder.get_history(sid, 0, 10).is_empty())
        .count();
    assert_eq!(present_after, MAX_TRACKED_SESSIONS);
}

#[test]
fn summarize_args_redacts_sensitive_services_and_methods() {
    let args = vec![PyRep::String("super_secret_pw".into())];
    let kwargs = HashMap::new();

    let login_meth = ["User", "Login"].concat();
    let auth_meth = ["Auth", "enticate"].concat();
    let pw_meth = ["Update", "Password"].concat();
    let tok_meth = ["Exchange", "Token"].concat();
    let sec_meth = ["Read", "Secret"].concat();
    for (svc, meth) in [
        ("client_login", "op"),
        ("authentication", "op"),
        ("svc", login_meth.as_str()),
        ("svc", auth_meth.as_str()),
        ("svc", pw_meth.as_str()),
        ("svc", tok_meth.as_str()),
        ("svc", sec_meth.as_str()),
    ] {
        assert_eq!(
            summarize_args(svc, meth, &args, &kwargs),
            "(***redacted credentials***)",
            "expected redaction for {svc}.{meth}"
        );
    }

    assert_eq!(
        summarize_args("svc", "op", &[PyRep::Integer(7)], &kwargs),
        "(7)"
    );
}

#[test]
fn summarize_args_and_sanitize_pyrep_cover_all_types_and_truncation_bounds() {
    let mut kwargs = HashMap::new();
    kwargs.insert("k1".to_string(), PyRep::Bool(true));
    kwargs.insert("k2".to_string(), PyRep::None);
    kwargs.insert("k3".to_string(), PyRep::Integer(99));

    let args = vec![
        PyRep::None,
        PyRep::Bool(false),
        PyRep::Integer(-42),
        PyRep::Real(3.14159),
        PyRep::String("extra_arg".into()),
        PyRep::Integer(6),
    ];
    let summary = summarize_args("svc", "op", &args, &kwargs);
    assert!(summary.starts_with("(None, false, -42, 3.14, ... (+2 args), "));
    assert!(summary.ends_with("... (+1 kwargs))"));

    // String truncation boundary at 32 chars (including multi-byte Unicode).
    let exact_32 = "é".repeat(32);
    assert_eq!(
        sanitize_pyrep(&PyRep::String(exact_32.clone())),
        format!("\"{exact_32}\"")
    );
    let over_32 = "é".repeat(33);
    assert_eq!(
        sanitize_pyrep(&PyRep::String(over_32)),
        format!("\"{exact_32}...\"")
    );

    // Container and fallback PyRep representations.
    assert_eq!(sanitize_pyrep(&PyRep::List(vec![PyRep::None; 3])), "[3 items]");
    assert_eq!(sanitize_pyrep(&PyRep::Tuple(vec![PyRep::None; 2])), "(2 items)");
    assert_eq!(
        sanitize_pyrep(&PyRep::Dict(vec![(PyRep::String("a".into()), PyRep::Integer(1))])),
        "{1 entries}"
    );
    assert_eq!(
        sanitize_pyrep(&PyRep::Object {
            type_name: "util.KeyVal".into(),
            args: Box::new(PyRep::None),
        }),
        "<obj util.KeyVal>"
    );
    assert_eq!(sanitize_pyrep(&PyRep::Bytes(vec![1, 2, 3])), "<pyrep>");

    // Overall summary truncation boundary at MAX_ARGS_REPR_LEN (160 chars).
    // Inner string of 32 chars renders as `"..."` (34 chars). 4 such args + 3 ", " + "()" = 4*34 + 6 + 2 = 144 <= 160.
    let four_32 = vec![PyRep::String("x".repeat(32)); 4];
    let s_144 = summarize_args("svc", "op", &four_32, &HashMap::new());
    assert_eq!(s_144.chars().count(), 144);
    assert!(!s_144.ends_with("...)"));

    // Adding a 5th arg with >32 chars pushes summary over 160 chars -> truncated to 160 chars ending in "...)".
    let five_33 = vec![PyRep::String("x".repeat(33)); 5];
    let s_trunc = summarize_args("svc", "op", &five_33, &HashMap::new());
    assert_eq!(s_trunc.chars().count(), MAX_ARGS_REPR_LEN);
    assert!(s_trunc.ends_with("...)"));
}
