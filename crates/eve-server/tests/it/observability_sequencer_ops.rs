//! Integration tests for GH #1077 §A:
//! - Per-session 500-event outbound ledger & Destiny stamp triples
//! - Bootstrap packet-perf annotations & >100 ms `[PacketPerf]` warning capture
//! - Handler latency histogram summary + JSON export
//! - Bug report bundle attachment (`outbound_ledger` + `destiny_stamps`)
//! - GM `/session ledger <char>` happy & error paths
//! - Loopback `--gm` ops endpoint + non-`--gm` / non-loopback rejection
//! - Outbound sequencer strict stamp monotonicity & equal-stamp coalescing

use crate::common::{
    borrow_wire_harness, gm_pilot_in_space, new_wire_harness, pin_clock, slash_cmd,
    teardown_guard, SessionBorrowWireTransport,
};
use eve_machonet::service_roles::ROLE_LOGIN;
use eve_proto::PyRep;
use eve_server::bug_reporting::BugIncidentBundle;
use eve_server::config::ServerConfig;
use eve_server::flight_recorder::{
    OutboundDisposition, MAX_OUTBOUND_LEDGER_PER_SESSION,
};
use eve_server::outbound_sequencer::{
    sequence_tick_with_event, sequence_with_events, DestinyBatch, DestinyDrain, OutFrame,
    RpcCompletion, SequencerLedgerEventKind, SessionWireState,
};
use eve_server::router::{ServerContext, RPC_RATE_BURST_TOKENS};
use eve_server::sso::{handle_http_request_with_peer, HttpResponse};
use eve_testkit::client_harness::ClientHarness;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

fn parse_response_json(resp: &HttpResponse) -> serde_json::Value {
    serde_json::from_slice(&resp.body).expect("response body should be valid JSON")
}

#[tokio::test]
async fn observability_login_undock_bootstrap_outbound_ledger_and_destiny_stamps() {
    let ctx = ServerContext::new();
    let mut pilot = gm_pilot_in_space(&ctx, 50_001, 90_107_701, 587, 30_000_142).await;
    let mut update_harness = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut pilot.sess,
        preserve_char_notifications: true,
    });
    update_harness
        .call_ok("beyonce", Some(pilot.beyonce), "UpdateStateRequest", &[], &HashMap::new())
        .await;

    let ledger = ctx
        .flight_recorder
        .get_outbound_ledger(50_001, MAX_OUTBOUND_LEDGER_PER_SESSION);
    assert!(
        !ledger.is_empty(),
        "outbound ledger must record frames during login/undock"
    );
    assert!(
        ledger.iter().any(|e| {
            e.disposition == OutboundDisposition::Sent
                && e.bootstrap
                && e.byte_size > 0
                && e.elapsed_since_request_micros.is_some()
                && e.reason.contains("Undock")
        }),
        "bootstrap undock annotation missing in outbound ledger: {ledger:?}"
    );
    assert!(
        ledger.iter().any(|e| {
            e.disposition == OutboundDisposition::Sent
                && e.packet_kind == "DoDestinyUpdate"
                && e.stamp.is_some()
        }),
        "DoDestinyUpdate outbound ledger entry missing"
    );

    let stamps = ctx.flight_recorder.get_destiny_stamps(50_001, 100);
    assert!(
        !stamps.is_empty(),
        "destiny stamp triples must be recorded on DoDestinyUpdate"
    );
    assert!(
        stamps
            .iter()
            .any(|s| s.session_id == 50_001 && s.action == "SetState" && s.entity_id == Some(pilot.ship_id)),
        "expected SetState triple for ship {}, got {stamps:?}",
        pilot.ship_id
    );

    let report = ctx.flight_recorder.latency_histogram_report();
    assert!(report.total_calls >= 5);
    assert!(report
        .methods
        .iter()
        .any(|m| m.service == "charUnboundMgr" && m.method == "SelectCharacterID"));
    assert!(report
        .methods
        .iter()
        .any(|m| m.service == "stationSvc" && m.method == "Undock"));
}

#[tokio::test]
async fn observability_bug_report_attaches_outbound_ledger_and_destiny_stamps() {
    let ctx = ServerContext::new();
    let mut pilot = gm_pilot_in_space(&ctx, 50_002, 90_107_702, 587, 30_000_142).await;
    let mut update_harness = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut pilot.sess,
        preserve_char_notifications: true,
    });
    update_harness
        .call_ok("beyonce", Some(pilot.beyonce), "UpdateStateRequest", &[], &HashMap::new())
        .await;

    let bundle = BugIncidentBundle::build_snapshot(
        &ctx,
        &pilot.sess,
        12345,
        "client:test",
        "Traceback (most recent call last): test",
    );
    assert!(
        !bundle.outbound_ledger.is_empty(),
        "bug bundle must include outbound_ledger"
    );
    assert!(
        !bundle.destiny_stamps.is_empty(),
        "bug bundle must include destiny_stamps"
    );

    let serialized = serde_json::to_string(&bundle).expect("serialize bundle");
    let roundtrip: BugIncidentBundle =
        serde_json::from_str(&serialized).expect("deserialize bundle");
    assert_eq!(roundtrip.outbound_ledger.len(), bundle.outbound_ledger.len());
    assert_eq!(roundtrip.destiny_stamps.len(), bundle.destiny_stamps.len());
}

#[tokio::test]
async fn observability_gm_slash_session_ledger_and_negative_paths() {
    let ctx = ServerContext::new();
    let mut pilot = gm_pilot_in_space(&ctx, 50_003, 90_107_703, 587, 30_000_142).await;

    // Happy path via `me` and character ID
    let by_me = slash_cmd(&ctx, &mut pilot.sess, "/session ledger me 10").await;
    assert_eq!(
        by_me.dict_get("characterID").and_then(PyRep::as_i64),
        Some(pilot.char_id)
    );
    assert_eq!(
        by_me.dict_get("sessionID").and_then(PyRep::as_i64),
        Some(50_003)
    );
    let entries = match by_me.dict_get("entries") {
        Some(PyRep::List(items)) => items,
        other => panic!("expected entries PyRep::List, got {other:?}"),
    };
    assert!(!entries.is_empty() && entries.len() <= 10);

    let by_id = slash_cmd(&ctx, &mut pilot.sess, &format!("/session ledger {}", pilot.char_id)).await;
    assert_eq!(
        by_id.dict_get("sessionID").and_then(PyRep::as_i64),
        Some(50_003)
    );

    // Negative path 1: non-GM caller must be rejected with InsufficientRole
    let mut non_gm_sess = pilot.sess.clone();
    non_gm_sess.role = ROLE_LOGIN;
    let mut non_gm_harness = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut non_gm_sess,
        preserve_char_notifications: true,
    });
    let err = non_gm_harness
        .call(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/session ledger me".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("non-GM must be rejected from /session ledger");
    assert_eq!(err.code, "InsufficientRole");

    // Negative path 2: invalid syntax / invalid limit
    let mut gm_harness = ClientHarness::new(SessionBorrowWireTransport {
        ctx: &ctx,
        session: &mut pilot.sess,
        preserve_char_notifications: true,
    });
    for bad_cmd in [
        "/session unknown",
        "/session status",
        "/session ledger",
        "/session ledger me 0",
        "/session ledger me notanumber",
    ] {
        let err = gm_harness
            .call(
                "slash",
                None,
                "SlashCmd",
                &[PyRep::String(bad_cmd.into())],
                &HashMap::new(),
            )
            .await
            .expect_err(&format!("{bad_cmd} must fail"));
        assert_eq!(err.code, "InvalidArguments", "unexpected code for {bad_cmd}");
    }

    // Negative path 3: unknown character
    let err_missing = gm_harness
        .call(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/session ledger 999888777".into())],
            &HashMap::new(),
        )
        .await
        .expect_err("missing char must fail");
    assert_eq!(err_missing.code, "CharacterNotFound");

    // Negative path 4: offline character
    let offline_cid = 90_107_799;
    ctx.db
        .provision_gm_account("offline_obs_user", "pw_obs", offline_cid, "Offline Obs");
    let err_offline = gm_harness
        .call(
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(format!("/session ledger {offline_cid}"))],
            &HashMap::new(),
        )
        .await
        .expect_err("offline char must fail");
    assert_eq!(err_offline.code, "SessionNotFound");
}

#[tokio::test]
async fn observability_ops_endpoint_gm_and_loopback_enforcement() {
    assert!(ServerConfig::parse_cli_gm_flag(["eve-server", "--gm"]));
    assert!(!ServerConfig::parse_cli_gm_flag(["eve-server"]));
    assert!(!ServerConfig::parse_cli_gm_flag(["eve-server", "--", "--gm"]));

    let ctx = ServerContext::new();
    let loopback_v4: IpAddr = "127.0.0.1".parse().unwrap();
    let loopback_v6: IpAddr = "::1".parse().unwrap();
    let remote_v4: IpAddr = "203.0.113.10".parse().unwrap();
    let headers = HashMap::new();

    // Negative path 1: non---gm mode must reject even from loopback
    assert!(!ctx.is_gm_ops_enabled());
    let resp_disabled =
        handle_http_request_with_peer(&ctx, "GET", "/ops", &headers, b"", Some(loopback_v4));
    assert_eq!(resp_disabled.status_code, 403);

    // Enable --gm mode
    ctx.set_gm_ops_enabled(true);

    // Negative path 2: non-loopback peer IP rejected
    let resp_remote =
        handle_http_request_with_peer(&ctx, "GET", "/ops", &headers, b"", Some(remote_v4));
    assert_eq!(resp_remote.status_code, 403);

    // Negative path 3: spoofed X-Forwarded-For / non-loopback Host rejected
    let mut xff_headers = HashMap::new();
    xff_headers.insert("x-forwarded-for".into(), "198.51.100.7".into());
    let resp_xff =
        handle_http_request_with_peer(&ctx, "GET", "/ops", &xff_headers, b"", Some(loopback_v4));
    assert_eq!(resp_xff.status_code, 403);

    let mut host_headers = HashMap::new();
    host_headers.insert("host".into(), "ops.example.com:26004".into());
    let resp_host =
        handle_http_request_with_peer(&ctx, "GET", "/ops", &host_headers, b"", Some(loopback_v4));
    assert_eq!(resp_host.status_code, 403);

    // Negative path 4: POST rejected with 405
    let resp_post =
        handle_http_request_with_peer(&ctx, "POST", "/ops", &headers, b"", Some(loopback_v4));
    assert_eq!(resp_post.status_code, 405);

    // Seed observability telemetry & TiDi state
    ctx.flight_recorder
        .record_rpc_latency("beyonce", "MachoBindObject", Duration::from_millis(4));
    ctx.flight_recorder.record_packet_perf_warning(
        77,
        "stationSvc",
        "Undock",
        128,
        Duration::from_millis(145),
        true,
    );
    let _jita = ctx.systems.get_or_spawn(30_000_142);
    {
        let mut cn = ctx.systems.cluster_nodes.write().unwrap();
        cn.manual_tidi_overrides.insert(30_000_142, 0.5);
    }

    // Happy paths: IPv4 and IPv6 loopback on /ops, /ops/status, /ops/metrics
    for (route, ip) in [
        ("/ops", loopback_v4),
        ("/ops/status", loopback_v6),
        ("/ops/metrics", loopback_v4),
    ] {
        let mut local_headers = HashMap::new();
        local_headers.insert("host".into(), "localhost:26004".into());
        let resp =
            handle_http_request_with_peer(&ctx, "GET", route, &local_headers, b"", Some(ip));
        assert_eq!(resp.status_code, 200, "route {route} failed");
        let body = parse_response_json(&resp);
        assert_eq!(body["gm_mode"], true);
        assert_eq!(body["living_world"]["status"], "not_implemented");
        assert_eq!(body["living_world"]["enabled"], false);
        assert!(body["tidi"]["manual_overrides"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["solar_system_id"] == 30_000_142 && v["factor"] == 0.5));
        assert!(body["scheduler"]["systems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["solar_system_id"] == 30_000_142 && s["sleep_reason"].is_string()));
        assert_eq!(body["latency_histograms"]["total_calls"], 1);
        assert_eq!(body["recent_packet_perf"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn observability_sequencer_monotonicity_and_coalesce_integration() {
    let ctx = ServerContext::new();
    let mut wire = SessionWireState::new(88_001, 901);
    wire.note_client_tick(199);
    let epoch = wire.enter_space(Some(30_000_142));

    let set_state = DestinyBatch::new(
        epoch,
        200,
        true,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(200),
            PyRep::Tuple(vec![
                PyRep::String("SetState".into()),
                PyRep::Tuple(vec![PyRep::keyval([("ego", PyRep::Integer(7001))])]),
            ]),
        ])]),
    );
    let (frame_opt, drop_opt) = sequence_tick_with_event(&mut wire, set_state);
    assert!(frame_opt.is_some());
    assert!(drop_opt.is_none());

    // Advance stamp to 205
    let batch_205 = DestinyBatch::new(
        epoch,
        205,
        false,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(205),
            PyRep::Tuple(vec![
                PyRep::String("GotoDirection".into()),
                PyRep::Tuple(vec![PyRep::Integer(7001)]),
            ]),
        ])]),
    );
    assert!(sequence_tick_with_event(&mut wire, batch_205).0.is_some());

    // Strict regression 204 < 205 must be dropped and attributed
    let regress_204 = DestinyBatch::new(
        epoch,
        204,
        false,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(204),
            PyRep::Tuple(vec![
                PyRep::String("Stop".into()),
                PyRep::Tuple(vec![PyRep::Integer(7001)]),
            ]),
        ])]),
    );
    let (f_drop, ev_drop) = sequence_tick_with_event(&mut wire, regress_204);
    assert!(f_drop.is_none());
    let ev_drop = ev_drop.expect("dropped event expected");
    assert_eq!(ev_drop.kind, SequencerLedgerEventKind::Dropped);
    ctx.flight_recorder.record_outbound(
        88_001,
        OutboundDisposition::Dropped,
        "DoDestinyUpdate",
        0,
        Some(ev_drop.stamp),
        &ev_drop.reason,
        None,
        false,
    );

    // Equal stamp 205 pending + direct updates must coalesce into one DoDestinyUpdate frame
    let p_205 = DestinyBatch::new(
        epoch,
        205,
        false,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(205),
            PyRep::Tuple(vec![
                PyRep::String("AddBalls2".into()),
                PyRep::Tuple(vec![PyRep::Integer(7002)]),
            ]),
        ])]),
    );
    let d_205 = DestinyBatch::new(
        epoch,
        205,
        false,
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(205),
            PyRep::Tuple(vec![
                PyRep::String("OnSpecialFX".into()),
                PyRep::Tuple(vec![PyRep::Integer(7001)]),
            ]),
        ])]),
    );
    let outcome = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 5,
            response_frame: PyRep::None,
            session_change_frame: None,
            pending_tick_batches: vec![p_205],
            destiny_drain: DestinyDrain::Updates(d_205),
            notifications: vec![],
            item_change_before_session_change: false,
        },
    );
    assert!(outcome
        .events
        .iter()
        .any(|e| e.kind == SequencerLedgerEventKind::Coalesced && e.stamp == 205));
    let destiny_frames: Vec<&OutFrame> = outcome
        .frames
        .iter()
        .filter(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }))
        .collect();
    assert_eq!(destiny_frames.len(), 1);
    if let OutFrame::DoDestinyUpdate { updates, stamp, .. } = destiny_frames[0] {
        ctx.flight_recorder.record_destiny_update_stamps(
            88_001,
            *stamp,
            updates,
            wire.last_client_tick,
        );
    }

    let stamps = ctx.flight_recorder.get_destiny_stamps(88_001, 20);
    assert_eq!(stamps.len(), 2);
    assert_eq!(stamps[0].action, "AddBalls2");
    assert_eq!(stamps[0].entity_id, Some(7002));
    assert_eq!(stamps[0].client_current_tick, Some(199));
    assert_eq!(stamps[1].action, "OnSpecialFX");
    assert_eq!(stamps[1].entity_id, Some(7001));
}

#[tokio::test]
async fn wire_login_undock_bootstrap_burst_passes_and_tight_loop_isolates_sessions() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();

    // Provision a bystander pilot on a separate session before running login/undock + tight-loop flood.
    let mut bystander = gm_pilot_in_space(&ctx, 50_902, 90_107_799, 587, 30_000_142).await;

    let mut h_attacker = new_wire_harness(&ctx, 50_901);
    h_attacker
        .call_ok(
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &kw,
        )
        .await;
    h_attacker
        .call_ok(
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
        )
        .await;

    // 120 login bootstrap RPCs pass cleanly without throttling.
    for i in 0..60 {
        let m = if i % 2 == 0 { "GetTime" } else { "GetGlobalConfig" };
        h_attacker.call_ok("machoNet", None, m, &[], &kw).await;
        h_attacker
            .call_ok(
                "config",
                None,
                "GetMultiOwnersEx",
                &[PyRep::List(vec![PyRep::Integer(90_000_001)])],
                &kw,
            )
            .await;
    }

    // Undock grants a fresh transition bootstrap allowance; another 120 space bootstrap calls pass.
    h_attacker.call_ok("stationSvc", None, "Undock", &[], &kw).await;
    for _ in 0..60 {
        h_attacker
            .call_ok("machoNet", None, "GetTime", &[], &kw)
            .await;
        h_attacker
            .call_ok("dogmaIM", None, "GetAllInfo", &[], &kw)
            .await;
    }

    // Tight loop of non-bootstrap RPCs past the per-session burst ceiling gets cleanly rejected
    // with `RateLimited` without touching/starving the concurrent bystander session.
    let loop_total = RPC_RATE_BURST_TOKENS as usize + 100;
    let mut allowed = 0usize;
    let mut rate_limited = 0usize;
    for _ in 0..loop_total {
        let ex = h_attacker
            .call_exchange("account", None, "GetCashBalance", &[], &kw)
            .await;
        match ex.result {
            Ok(_) => allowed += 1,
            Err(err) => {
                assert_eq!(err.code, "RateLimited", "unexpected wire error: {err:?}");
                rate_limited += 1;
            }
        }
    }
    assert!(allowed >= RPC_RATE_BURST_TOKENS as usize);
    assert!(
        rate_limited > 0,
        "expected tight loop on session A to be throttled cleanly"
    );

    // Bystander session continues serving wire RPCs normally without starvation.
    let mut h_bystander = borrow_wire_harness(&ctx, &mut bystander.sess);
    h_bystander
        .call_ok("account", None, "GetCashBalance", &[], &kw)
        .await;
}

#[tokio::test]
async fn wire_bounded_rpc_args_reject_oversized_strings_without_mutation() {
    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let _guard = teardown_guard(&ctx);
    let kw = HashMap::new();
    let mut pilot = gm_pilot_in_space(&ctx, 50_910, 90_107_810, 587, 30_000_142).await;
    let saved_before = ctx.db.with_snapshot(|snap| snap.saved_fittings.len());

    let mut h = borrow_wire_harness(&ctx, &mut pilot.sess);

    // Oversized UTF-8 payloads (both byte-bound and char-bound) fail with `InvalidArgument`
    // across persisted text services without writing snapshot records.
    let huge_label = "🚀".repeat(100); // 100 chars, 400 bytes (> 64 chars & > 256 bytes)
    let err_label = h
        .call_exchange(
            "charMgr",
            None,
            "CreateLabel",
            &[PyRep::String(huge_label)],
            &kw,
        )
        .await
        .result
        .unwrap_err();
    assert_eq!(err_label.code, "InvalidArgument");

    let huge_subject = "S".repeat(200); // > 128 chars
    let err_pet = h
        .call_exchange(
            "petitioner",
            None,
            "CreatePetition",
            &[
                PyRep::String(huge_subject),
                PyRep::String("Valid description".into()),
                PyRep::Integer(1),
            ],
            &kw,
        )
        .await
        .result
        .unwrap_err();
    assert_eq!(err_pet.code, "InvalidArgument");

    let huge_fit_name = "F".repeat(150); // > 100 chars
    let err_fit = h
        .call_exchange(
            "charFittingMgr",
            None,
            "SaveFitting",
            &[
                PyRep::String(huge_fit_name),
                PyRep::Integer(587),
                PyRep::String("Valid desc".into()),
            ],
            &kw,
        )
        .await
        .result
        .unwrap_err();
    assert_eq!(err_fit.code, "InvalidArgument");

    let huge_item_label = "L".repeat(600); // > 128 chars
    let err_item = h
        .call_exchange(
            "invbroker",
            None,
            "SetLabel",
            &[PyRep::Integer(pilot.ship_id), PyRep::String(huge_item_label)],
            &kw,
        )
        .await
        .result
        .unwrap_err();
    assert_eq!(err_item.code, "InvalidArgument");

    // Verify no contact label or saved fitting was mutated by the rejected calls.
    let labels = h.call_ok("charMgr", None, "GetLabels", &[], &kw).await;
    assert_eq!(labels, PyRep::Dict(vec![]));
    let saved_after = ctx.db.with_snapshot(|snap| snap.saved_fittings.len());
    assert_eq!(saved_after, saved_before);
}
