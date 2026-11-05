//! Additional unit and mutation-killing tests for `flight_recorder`.

#![forbid(unsafe_code)]

use super::*;
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

#[test]
fn summarize_args_exact_boundaries_for_args_kwargs_and_160_char_limit() {
    let empty_kw = HashMap::new();

    // 0 args, 0 kwargs -> "()"
    assert_eq!(summarize_args("svc", "op", &[], &empty_kw), "()");

    // Exactly 4 args must NOT emit "... (+0 args)" (kills `> 4` -> `>= 4` mutant).
    let four_args = vec![
        PyRep::Integer(10),
        PyRep::Integer(20),
        PyRep::Integer(30),
        PyRep::Integer(40),
    ];
    assert_eq!(
        summarize_args("svc", "op", &four_args, &empty_kw),
        "(10, 20, 30, 40)"
    );

    // 5 args emits "... (+1 args)".
    let mut five_args = four_args.clone();
    five_args.push(PyRep::Integer(50));
    assert_eq!(
        summarize_args("svc", "op", &five_args, &empty_kw),
        "(10, 20, 30, 40, ... (+1 args))"
    );

    // Exactly 2 kwargs must NOT emit "... (+0 kwargs)" (kills `> 2` -> `>= 2` mutant).
    let mut two_kw = HashMap::new();
    two_kw.insert("a".to_string(), PyRep::Integer(1));
    two_kw.insert("b".to_string(), PyRep::Integer(2));
    let s_two_kw = summarize_args("svc", "op", &[], &two_kw);
    assert!(!s_two_kw.contains("kwargs"));
    assert!(s_two_kw.contains("a=1") && s_two_kw.contains("b=2"));

    // 4 kwargs emits "... (+2 kwargs)".
    let mut four_kw = two_kw.clone();
    four_kw.insert("c".to_string(), PyRep::Integer(3));
    four_kw.insert("d".to_string(), PyRep::Integer(4));
    let s_four_kw = summarize_args("svc", "op", &[], &four_kw);
    assert!(s_four_kw.ends_with(", ... (+2 kwargs))"));

    // Non-redacted near-miss service names must NOT be redacted.
    assert_eq!(
        summarize_args("client_login_ext", "op", &[PyRep::Integer(1)], &empty_kw),
        "(1)"
    );
    assert_eq!(
        summarize_args("authentication_ext", "op", &[PyRep::Integer(1)], &empty_kw),
        "(1)"
    );

    // Case-insensitive method redaction checks.
    for kw_part in ["LOGIN", "AuTh", "PaSsWoRd", "ToKeN", "SeCrEt"] {
        let m = format!("do_{kw_part}_check");
        assert_eq!(
            summarize_args("svc", &m, &[PyRep::String("pw".into())], &empty_kw),
            "(***redacted credentials***)"
        );
    }

    // Exact 160-character boundary (kills `> MAX_ARGS_REPR_LEN` -> `>= MAX_ARGS_REPR_LEN`).
    // Suppose 4 args + 1 kwarg:
    // 4 args of `"x".repeat(32)` -> each is 34 chars -> 4 * 34 + 3 * 2 (", ") = 142 chars inside parens.
    // With outer parens that is 144 chars.
    // If we add 1 kwarg `, k="yyyyyyyyyyyy"` -> `, ` (2) + `k=` (2) + `"yyyyyyyyyyyy"` (12 chars) = 16 chars -> 144 + 16 = 160 chars!
    let args_4x32 = vec![PyRep::String("x".repeat(32)); 4];
    let mut kw_exact = HashMap::new();
    kw_exact.insert("k".to_string(), PyRep::String("y".repeat(10)));
    let s_160 = summarize_args("svc", "op", &args_4x32, &kw_exact);
    assert_eq!(s_160.chars().count(), MAX_ARGS_REPR_LEN);
    assert!(
        !s_160.ends_with("...)"),
        "exact 160-char summary must not be truncated: {s_160}"
    );

    // 161-character summary is truncated to 156 chars + "...)" == 160 chars.
    let mut kw_161 = HashMap::new();
    kw_161.insert("k".to_string(), PyRep::String("y".repeat(11)));
    let s_161 = summarize_args("svc", "op", &args_4x32, &kw_161);
    assert_eq!(s_161.chars().count(), MAX_ARGS_REPR_LEN);
    assert!(s_161.ends_with("...)"));
    let expected_prefix: String = format!(
        "({}, k=\"{}\")",
        vec![format!("\"{}\"", "x".repeat(32)); 4].join(", "),
        "y".repeat(11)
    )
    .chars()
    .take(156)
    .collect();
    assert_eq!(s_161, format!("{expected_prefix}...)"));
}

#[test]
fn flight_recorder_session_and_entry_exact_boundaries_and_poisoned_lock() {
    let recorder = Arc::new(FlightRecorder::new());
    let kw = HashMap::new();

    // Recording 29 entries (< 30) and then the 30th entry (== 30) must keep entry 0.
    for i in 0..MAX_HISTORY_PER_SESSION {
        recorder.record(
            77,
            "dogmaIM",
            Some(900 + i as u64),
            "GetAllInfo",
            &[PyRep::Integer(i as i64)],
            &kw,
            Duration::from_micros(100 + i as u64),
            "OK",
        );
    }
    let hist_30 = recorder.get_history(77, 0, 50);
    assert_eq!(hist_30.len(), MAX_HISTORY_PER_SESSION);
    assert_eq!(hist_30[0].service, "dogmaIM");
    assert_eq!(hist_30[0].method, "GetAllInfo");
    assert_eq!(hist_30[0].args_summary, "(0)");
    assert_eq!(hist_30[29].args_summary, "(29)");

    // Exact offset_ms == 0 when ref_timestamp_millis equals entry timestamp_millis.
    let exact_ref = hist_30[29].timestamp_millis;
    let hist_zero_off = recorder.get_history(77, exact_ref, 1);
    assert_eq!(hist_zero_off.len(), 1);
    assert_eq!(hist_zero_off[0].offset_ms, 0);

    // Unknown session when other sessions exist returns empty Vec.
    assert!(recorder.get_history(999_999, 0, 10).is_empty());

    // Serde round-trip including default offset_ms.
    let raw_json = r#"{"timestamp_millis":1234,"service":"s","method":"m","bound_id":null,"args_summary":"()","duration_micros":5,"status":"OK"}"#;
    let parsed: FlightRecorderEntry = serde_json::from_str(raw_json).expect("deserialize entry");
    assert_eq!(parsed.offset_ms, 0);
    assert_eq!(parsed.timestamp_millis, 1234);

    // Poisoned RwLock returns empty Vec from get_history and does not panic on record.
    let rec_clone = Arc::clone(&recorder);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = rec_clone.sessions.write().unwrap();
        panic!("poison flight recorder rwlock");
    }));
    assert!(recorder.get_history(77, 0, 10).is_empty());
    recorder.record(77, "svc", None, "op", &[], &kw, Duration::from_micros(1), "OK");
}
