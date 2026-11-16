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
    let pos_offsets = recorder.get_history(1, first_ts.saturating_sub(120), MAX_HISTORY_PER_SESSION);
    assert_eq!(pos_offsets[0].offset_ms, 120);
    let neg_offsets = recorder.get_history(1, first_ts + 500, MAX_HISTORY_PER_SESSION);
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

#[test]
fn outbound_ledger_evicts_oldest_at_500_cap() {
    let fr = FlightRecorder::new();
    let total = MAX_OUTBOUND_LEDGER_PER_SESSION + 10;
    for i in 0..total {
        let disp = match i % 3 {
            0 => OutboundDisposition::Sent,
            1 => OutboundDisposition::Dropped,
            _ => OutboundDisposition::Coalesced,
        };
        fr.record_outbound(
            42,
            disp,
            "DoDestinyUpdate",
            100 + i,
            Some(i as u32),
            &format!("reason_{i}"),
            Some((i as u64) * 5),
            i < 5,
        );
    }

    let entries = fr.get_outbound_ledger(42, 1000);
    assert_eq!(entries.len(), MAX_OUTBOUND_LEDGER_PER_SESSION);
    assert_eq!(entries.first().unwrap().stamp, Some(10));
    assert_eq!(entries.first().unwrap().reason, "reason_10");
    assert_eq!(entries.last().unwrap().stamp, Some((total - 1) as u32));
    assert_eq!(entries.last().unwrap().byte_size, 100 + total - 1);
    assert_eq!(fr.get_outbound_ledger(42, 0).len(), 0);
    assert_eq!(fr.get_outbound_ledger(999, 50).len(), 0);
}

#[test]
fn destiny_stamp_triples_extract_and_evict_at_500_cap() {
    let updates = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::Integer(100),
            PyRep::Tuple(vec![
                PyRep::String("SetState".into()),
                PyRep::Tuple(vec![PyRep::keyval([("ego", PyRep::Integer(900_001))])]),
            ]),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(101),
            PyRep::Tuple(vec![
                PyRep::String("GotoDirection".into()),
                PyRep::Tuple(vec![
                    PyRep::Integer(900_001),
                    PyRep::Real(1.0),
                    PyRep::Real(0.0),
                    PyRep::Real(0.0),
                ]),
            ]),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(102),
            PyRep::Tuple(vec![
                PyRep::String("RemoveBall".into()),
                PyRep::Tuple(vec![PyRep::Integer(900_002)]),
            ]),
        ]),
    ]);

    let triples = extract_destiny_stamp_triples(99, &updates);
    assert_eq!(triples.len(), 3);
    assert_eq!(triples[0], (100, "SetState".into(), Some(900_001)));
    assert_eq!(triples[1], (101, "GotoDirection".into(), Some(900_001)));
    assert_eq!(triples[2], (102, "RemoveBall".into(), Some(900_002)));

    let fr = FlightRecorder::new();
    for i in 0..515 {
        let single = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(i),
            PyRep::Tuple(vec![
                PyRep::String("Stop".into()),
                PyRep::Tuple(vec![PyRep::Integer(1000 + i)]),
            ]),
        ])]);
        fr.record_destiny_update_stamps(7, i as u32, &single, Some((i as u64) + 10));
    }

    let recorded = fr.get_destiny_stamps(7, 1000);
    assert_eq!(recorded.len(), MAX_DESTINY_STAMPS_PER_SESSION);
    assert_eq!(recorded.first().unwrap().stamp, 15);
    assert_eq!(recorded.first().unwrap().entity_id, Some(1015));
    assert_eq!(recorded.first().unwrap().client_current_tick, Some(25));
    assert_eq!(recorded.last().unwrap().stamp, 514);
}

#[test]
fn latency_histogram_computes_p99_and_exports_json() {
    let fr = FlightRecorder::new();
    for us in 1..=100 {
        fr.record_rpc_latency("beyonce", "MachoBindObject", Duration::from_micros(us * 100));
    }
    fr.record_rpc_latency("charUnboundMgr", "SelectCharacterID", Duration::from_millis(25));

    let report = fr.latency_histogram_report();
    assert_eq!(report.total_calls, 101);
    assert_eq!(report.methods.len(), 2);
    let select_char = report
        .methods
        .iter()
        .find(|m| m.service == "charUnboundMgr" && m.method == "SelectCharacterID")
        .unwrap();
    assert_eq!(select_char.count, 1);
    assert_eq!(select_char.p99_micros, 25_000);

    let bind = report
        .methods
        .iter()
        .find(|m| m.service == "beyonce" && m.method == "MachoBindObject")
        .unwrap();
    assert_eq!(bind.count, 100);
    assert_eq!(bind.min_micros, 100);
    assert_eq!(bind.p50_micros, 5_000);
    assert_eq!(bind.p95_micros, 9_500);
    assert_eq!(bind.p99_micros, 9_900);
    assert_eq!(bind.max_micros, 10_000);

    let out_path = std::env::temp_dir().join(format!(
        "evomu_fr_hist_test_{}.json",
        std::process::id()
    ));
    fr.export_latency_histogram_json(&out_path)
        .expect("export latency histogram json");
    let raw = std::fs::read_to_string(&out_path).expect("read exported json");
    let parsed: LatencyHistogramReport =
        serde_json::from_str(&raw).expect("parse exported json");
    assert_eq!(parsed.total_calls, 101);
    assert_eq!(parsed.methods.len(), 2);
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn packet_perf_warnings_bounded_at_200() {
    let fr = FlightRecorder::new();
    for i in 0..205 {
        fr.record_packet_perf_warning(
            11,
            "stationSvc",
            "Undock",
            64 + i,
            Duration::from_millis(120 + i as u64),
            true,
        );
    }
    let warnings = fr.recent_packet_perf_warnings(500);
    assert_eq!(warnings.len(), 200);
    assert_eq!(warnings.first().unwrap().arg_size, 69);
    assert_eq!(warnings.last().unwrap().arg_size, 268);
    assert!(warnings.last().unwrap().bootstrap);
}

#[test]
fn sequencer_counters_violations_and_prevented_jolts_are_tracked_and_bounded() {
    let fr = FlightRecorder::new();
    fr.record_dropped_dependent_notification(77, "OnSlimItemChange");
    fr.record_dropped_dependent_notification(77, "OnSlimItemChange");
    fr.record_dropped_dependent_notification(77, "OnSpecialFX");

    for i in 0..205 {
        fr.record_invariant_violation(
            77,
            1,
            100 + i as u32,
            "remove-after-addballs",
            &format!("detail_{i}"),
        );
        fr.record_prevented_jolt(77, 1, 90 + i as u32, 100 + i as u32, "stamp_regression");
    }
    fr.record_invariant_violation(88, 2, 10, "group-stamp-matches-update-stamp", "mismatch");

    let c77 = fr.session_sequencer_counters(77);
    assert_eq!(c77.dropped_dependent_notifications, 3);
    assert_eq!(c77.dropped_dependent_by_name.get("OnSlimItemChange"), Some(&2));
    assert_eq!(c77.dropped_dependent_by_name.get("OnSpecialFX"), Some(&1));
    assert_eq!(c77.invariant_violations, 205);
    assert_eq!(c77.invariant_violations_by_name.get("remove-after-addballs"), Some(&205));
    assert_eq!(c77.prevented_jolts, 205);

    let v77 = fr.get_session_invariant_violations(77, 500);
    assert_eq!(v77.len(), MAX_INVARIANT_VIOLATIONS_PER_SESSION);
    assert_eq!(v77.first().unwrap().stamp, 105);
    assert_eq!(v77.last().unwrap().stamp, 304);

    let j77 = fr.get_session_prevented_jolts(77, 500);
    assert_eq!(j77.len(), MAX_PREVENTED_JOLTS_PER_SESSION);
    assert_eq!(j77.first().unwrap().batch_stamp, 95);
    assert_eq!(j77.last().unwrap().batch_stamp, 294);

    let summary = fr.sequencer_observability_summary(10);
    assert_eq!(summary.dropped_dependent_notifications_total, 3);
    assert_eq!(summary.invariant_violations_total, 206);
    assert_eq!(summary.prevented_jolts_total, 205);
    assert_eq!(summary.sessions_with_counters, 2);
    assert_eq!(summary.recent_invariant_violations.len(), 10);
    assert_eq!(summary.recent_prevented_jolts.len(), 10);
}
