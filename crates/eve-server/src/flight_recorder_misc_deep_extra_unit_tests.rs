//! Deep branch-coverage and mutation-killing unit tests for `flight_recorder`, `log_ring_buffer`, `sso`, and `formulas`.

use crate::flight_recorder::FlightRecorder;
use crate::formulas::{
    abyssal_attribute_roll, apply_security_tag_repair, apply_standing_decrease,
    booster_side_effect_check, breacher_pod_tick_damage, command_burst_aggregate,
    compute_daily_research_points, compute_ecm_jam_chance,
    compute_effective_standing, compute_insurance_levels, compute_layer_ehp,
    compute_manufacturing_material_qty, compute_manufacturing_time_seconds,
    compute_poco_export_tax, compute_probe_scan_signal_pct,
    compute_ship_insurance_reference_value, compute_warp_duration_seconds,
    dogma_stacking_penalty, entropic_spool_multiplier, missile_damage_factor,
    nanite_repair_paste_needed, passive_capacitor_or_shield_recharge,
    peak_passive_recharge_per_sec, propulsion_boosted_velocity, rack_heat_step,
    required_standing_for_agent_level, skill_injector_sp_yield, skill_training_time_seconds,
    slot_heat_damage_chance, sp_for_skill_level, subwarp_distance_at_time,
    subwarp_velocity_at_time, target_lock_time_sec, turret_angular_velocity,
    turret_chance_to_hit, turret_chance_to_hit_with_res, turret_shot_damage,
    vorton_arc_targets_with_cap,
};
use crate::log_ring_buffer::{LogRingBuffer, LogRingLayer, global_log_ring};
use crate::router::ServerContext;
use crate::sso::{HttpResponse, handle_http_connection, handle_http_request};
use eve_destiny::{AU_METERS, DamageVector};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tracing_subscriber::layer::SubscriberExt;

#[test]
fn flight_recorder_and_log_ring_buffer_cover_all_redaction_truncation_and_layer_branches() {
    // 1. LogRingBuffer capacity floor, snapshot_since/recent & tracing Layer visitor branches
    let global = global_log_ring();
    global.record("INFO", "test::global", "global ring ping");
    assert!(!global.snapshot_recent(5).is_empty());

    let tiny_ring = Arc::new(LogRingBuffer::new(0)); // clamps capacity to 1
    let layer = LogRingLayer::new(Arc::clone(&tiny_ring));
    let subscriber = tracing_subscriber::registry().with(layer);
    let now_before_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    tracing::subscriber::with_default(subscriber, || {
        tracing::info!(
            target: "evomu::ring_test",
            extra_num = 42,
            extra_str = "spaced value",
            "primary string message"
        );
        // Debug message field branch (non-&str message via Debug formatting)
        tracing::warn!(target: "evomu::ring_test", message = 999, code = "E2");
    });
    // Capacity clamped to 1 -> only the second event remains
    let recent = tiny_ring.snapshot_recent(10);
    assert_eq!(recent.len(), 1);
    assert!(recent[0].contains("999"));
    assert!(recent[0].contains("code=\"E2\""));
    assert!(!tiny_ring.snapshot_since(now_before_ms, 5).is_empty());
    assert!(tiny_ring.snapshot_since(now_before_ms.saturating_add(60_000), 5).is_empty());

    // 2. FlightRecorder redaction rules, PyRep sanitization variants, truncation, and eviction
    let recorder = FlightRecorder::default();
    assert!(recorder.get_history(12345, 1_000_000, 10).is_empty());

    for (svc, meth) in [
        ("client_login", "DoCheck"),
        ("authentication", "Verify"),
        ("diag_session", "UserLoginStep"),
        ("diag_session", "CheckAuthState"),
        ("diag_session", "ChangePassword"),
        ("diag_session", "MintToken"),
        ("diag_session", "RotateSecretKey"),
    ] {
        recorder.record(100, svc, Some(55), meth, &[PyRep::String("hunter2".into())], &HashMap::new(), Duration::from_millis(2), "ok");
    }
    let red_hist = recorder.get_history(100, i64::MAX as u64, 20);
    assert_eq!(red_hist.len(), 7);
    assert!(red_hist.iter().all(|e| e.args_summary == "(***redacted credentials***)" && e.offset_ms < 0));

    // Exercise every PyRep variant in sanitize_pyrep + >4 args + >2 kwargs + >160 char truncation
    let mut big_kwargs = HashMap::new();
    big_kwargs.insert("k_very_long_key_name_one_padding_here_extra_padding".into(), PyRep::String("a".repeat(80)));
    big_kwargs.insert("k_very_long_key_name_two_padding_here_extra_padding".into(), PyRep::String("b".repeat(80)));
    big_kwargs.insert("k_very_long_key_name_three_padding_here_extra_pad".into(), PyRep::Integer(3));
    let diverse_args = vec![
        PyRep::None,
        PyRep::Bool(true),
        PyRep::Integer(-99),
        PyRep::Real(12.345),
        PyRep::String("0123456789abcdefghijklmnopqrstuvwxyz".into()),
        PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
        PyRep::Tuple(vec![PyRep::None]),
        PyRep::Dict(vec![(PyRep::String("a".into()), PyRep::Integer(1))]),
        PyRep::Object {
            type_name: "utillib.KeyVal".into(),
            args: Box::new(PyRep::None),
        },
        PyRep::Bytes(vec![1, 2, 3]),
    ];
    recorder.record(200, "charMgr", None, "GetPublicInfo", &diverse_args, &big_kwargs, Duration::from_micros(500), "ok");
    let h200 = recorder.get_history(200, 0, 5);
    assert_eq!(h200[0].args_summary.chars().count(), 160);
    assert!(h200[0].args_summary.ends_with("...)"));
    assert!(h200[0].offset_ms >= 0);

    // Individual PyRep variant summary formatting without truncation
    let single_obj = [
        PyRep::List(vec![PyRep::None]),
        PyRep::Tuple(vec![PyRep::None, PyRep::None]),
        PyRep::Dict(vec![]),
        PyRep::Object {
            type_name: "eve.Foo".into(),
            args: Box::new(PyRep::None),
        },
    ];
    recorder.record(201, "charMgr", None, "Inspect", &single_obj, &HashMap::new(), Duration::from_millis(1), "ok");
    let s201 = &recorder.get_history(201, 0, 1)[0].args_summary;
    assert_eq!(s201, "([1 items], (2 items), {0 entries}, <obj eve.Foo>)");

    // Per-session ring eviction (MAX_HISTORY_PER_SESSION = 30) & MAX_TRACKED_SESSIONS (512) eviction
    for i in 0..35 {
        recorder.record(300, "svc", None, &format!("Call{i}"), &[], &HashMap::new(), Duration::ZERO, "ok");
    }
    let h300 = recorder.get_history(300, 0, 50);
    assert_eq!(h300.len(), 30);
    assert_eq!(h300[0].method, "Call5");

    for sid in 10_000..10_520_u64 {
        recorder.record(sid, "svc", None, "Ping", &[], &HashMap::new(), Duration::ZERO, "ok");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sso_http_routes_tcp_framing_pkce_json_basic_auth_and_error_branches() {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));

    // 1. Well-known discovery, JWKS, and 404 routes + HttpResponse header CRLF sanitization
    let wk = handle_http_request(&ctx, "GET", "/.well-known/oauth-authorization-server", &HashMap::new(), b"");
    assert_eq!(wk.status_code, 200);
    let mut custom_hdrs = HashMap::new();
    custom_hdrs.insert("host".into(), "auth.evomu.local:26003".into());
    let oidc = handle_http_request(&ctx, "GET", "/.well-known/openid-configuration", &custom_hdrs, b"");
    assert_eq!(oidc.status_code, 200);
    assert!(String::from_utf8_lossy(&oidc.body).contains("http://auth.evomu.local:26003/v2/oauth/token"));
    let jwks = handle_http_request(&ctx, "GET", "/oauth/jwks", &HashMap::new(), b"");
    assert_eq!(jwks.status_code, 200);
    let not_found = handle_http_request(&ctx, "DELETE", "/v2/oauth/unknown", &HashMap::new(), b"");
    assert_eq!(not_found.status_code, 404);

    let mut dirty_resp = HttpResponse::html(200, "OK", "<b>ok</b>".into());
    dirty_resp.headers.push(("X-Test\r\nInjected".into(), "val\r\nEvil".into()));
    let wire = dirty_resp.to_wire_bytes();
    assert!(!String::from_utf8_lossy(&wire).contains("val\r\nEvil"));

    // 2. Authorize GET HTML consent form & all 400 validation branches
    let consent = handle_http_request(
        &ctx,
        "GET",
        "/v2/oauth/authorize?client_id=c%3C1%3E&redirect_uri=https%3A%2F%2Fapp.local%2Fcb&code_challenge=abc123&scope=s1&state=st%261",
        &HashMap::new(),
        b"",
    );
    assert_eq!(consent.status_code, 200);
    let html_str = String::from_utf8_lossy(&consent.body);
    assert!(html_str.contains("c&lt;1&gt;") && html_str.contains("st&amp;1"));

    for bad_query in [
        "response_type=token&client_id=c&redirect_uri=https://a/b&code_challenge=x",
        "response_type=code&redirect_uri=https://a/b&code_challenge=x",
        "response_type=code&client_id=c&code_challenge=x",
        "response_type=code&client_id=c&redirect_uri=ftp://bad/uri&code_challenge=x",
        "response_type=code&client_id=c&redirect_uri=https://a/b",
        "response_type=code&client_id=c&redirect_uri=https://a/b&code_challenge=x&code_challenge_method=plain",
    ] {
        let r = handle_http_request(&ctx, "POST", &format!("/v2/oauth/authorize?{bad_query}"), &HashMap::new(), b"");
        assert_eq!(r.status_code, 400);
    }

    // Missing username / missing password / invalid Basic auth credentials
    let no_user = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize?client_id=c&redirect_uri=https://a/b&code_challenge=x",
        &HashMap::new(),
        b"",
    );
    assert_eq!(no_user.status_code, 401);
    let no_pass = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize?client_id=c&redirect_uri=https://a/b&code_challenge=x&username=alice",
        &HashMap::new(),
        b"",
    );
    assert_eq!(no_pass.status_code, 401);

    let mut bad_basic = HashMap::new();
    bad_basic.insert("authorization".into(), "Basic bm9jb2xvbg==".into()); // "nocolon"
    let bad_cred = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize?client_id=c&redirect_uri=https://a/b&code_challenge=x",
        &bad_basic,
        b"",
    );
    assert_eq!(bad_cred.status_code, 401);

    // 3. Provision user with 0 characters -> 400; then add character, authorize with Basic Auth + `?` in redirect_uri
    let uid = ctx.db.provision_player_account("sso_deep_u", "sso_deep_p");
    let verifier = "deep_pkce_verifier_0123456789abcdef";
    let challenge = eve_db::accounts::base64url_encode_no_pad(&eve_db::accounts::sha256_bytes(verifier.as_bytes()));
    let json_auth_body = serde_json::json!({
        "client_id": "eve-deep-client",
        "redirect_uri": "http://localhost:9000/cb?existing=1",
        "code_challenge": challenge,
        "code_challenge_method": "S256",
        "scope": ["esi-skills.read_skills.v1"],
        "state": "my state+1",
        "username": "sso_deep_u",
        "password": "sso_deep_p",
        "character_id": 90_000_001
    })
    .to_string();
    let mut json_hdrs = HashMap::new();
    json_hdrs.insert("content-type".into(), "application/json".into());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.user_id = uid;
        }
    });
    let auth_rsp = handle_http_request(&ctx, "POST", "/v2/oauth/authorize", &json_hdrs, json_auth_body.as_bytes());
    assert_eq!(auth_rsp.status_code, 302);
    let auth_json: serde_json::Value = serde_json::from_slice(&auth_rsp.body).unwrap();
    let code = auth_json["code"].as_str().unwrap().to_string();
    assert!(auth_json["redirect_uri"].as_str().unwrap().contains("&code="));

    // Token endpoint error paths + success exchange + refresh + verify scope check + revoke
    for bad_tok in [
        "{}",
        r#"{"client_id":"eve-deep-client"}"#,
        r#"{"client_id":"eve-deep-client","grant_type":"authorization_code"}"#,
        r#"{"client_id":"eve-deep-client","grant_type":"authorization_code","code":"c1"}"#,
        r#"{"client_id":"eve-deep-client","grant_type":"refresh_token"}"#,
        r#"{"client_id":"eve-deep-client","grant_type":"client_credentials"}"#,
    ] {
        let r = handle_http_request(&ctx, "POST", "/v2/oauth/token", &json_hdrs, bad_tok.as_bytes());
        assert_eq!(r.status_code, 400);
    }

    let tok_req = serde_json::json!({
        "client_id": "eve-deep-client",
        "grant_type": "authorization_code",
        "code": code,
        "code_verifier": verifier
    })
    .to_string();
    let tok_rsp = handle_http_request(&ctx, "POST", "/v2/oauth/token", &json_hdrs, tok_req.as_bytes());
    assert_eq!(tok_rsp.status_code, 200);
    let tok_val: serde_json::Value = serde_json::from_slice(&tok_rsp.body).unwrap();
    let access_tok = tok_val["access_token"].as_str().unwrap();
    let refresh_tok = tok_val["refresh_token"].as_str().unwrap();

    // Verify with matching scope (200) vs missing scope (403 insufficient_scope) vs non-Bearer (401)
    let mut non_bearer = HashMap::new();
    non_bearer.insert("authorization".into(), "Basic abc".into());
    assert_eq!(handle_http_request(&ctx, "GET", "/v2/oauth/verify", &non_bearer, b"").status_code, 401);

    let mut bearer_hdr = HashMap::new();
    bearer_hdr.insert("authorization".into(), format!("bearer {access_tok}"));
    assert_eq!(handle_http_request(&ctx, "GET", "/v2/oauth/verify?scope=esi-skills.read_skills.v1", &bearer_hdr, b"").status_code, 200);
    assert_eq!(handle_http_request(&ctx, "GET", "/v2/oauth/verify?scope=esi-wallet.read_character_wallet.v1", &bearer_hdr, b"").status_code, 403);

    // Refresh token rotation & revoke missing/valid token
    let ref_req = format!("grant_type=refresh_token&client_id=eve-deep-client&refresh_token={refresh_tok}");
    let ref_rsp = handle_http_request(&ctx, "POST", "/v2/oauth/token", &HashMap::new(), ref_req.as_bytes());
    assert_eq!(ref_rsp.status_code, 200);
    let new_ref = serde_json::from_slice::<serde_json::Value>(&ref_rsp.body).unwrap()["refresh_token"].as_str().unwrap().to_string();

    assert_eq!(handle_http_request(&ctx, "POST", "/v2/oauth/revoke", &HashMap::new(), b"").status_code, 400);
    let rev_rsp = handle_http_request(&ctx, "POST", "/v2/oauth/revoke", &HashMap::new(), format!("token={new_ref}").as_bytes());
    assert_eq!(rev_rsp.status_code, 200);
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&rev_rsp.body).unwrap()["revoked"], true);
    assert_eq!(handle_http_request(&ctx, "POST", "/v2/oauth/token", &HashMap::new(), format!("grant_type=refresh_token&client_id=eve-deep-client&refresh_token={new_ref}").as_bytes()).status_code, 400);
    let mut bad_bearer = HashMap::new();
    bad_bearer.insert("authorization".into(), "Bearer invalid-token".into());
    assert_eq!(handle_http_request(&ctx, "GET", "/v2/oauth/verify", &bad_bearer, b"").status_code, 401);

    // 4. Async TCP handle_http_connection: clean EOF, 100-Continue + POST body, oversized Content-Length -> 413
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // Case A: Expect: 100-continue + JSON POST
    let ctx_a = Arc::clone(&ctx);
    let srv_a = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        handle_http_connection(stream, Arc::clone(&ctx_a)).await.unwrap();

        let (stream2, _) = listener.accept().await.unwrap();
        handle_http_connection(stream2, Arc::clone(&ctx_a)).await.unwrap();

        let (stream3, _) = listener.accept().await.unwrap();
        handle_http_connection(stream3, ctx_a).await.unwrap();
    });

    let mut cli_a = TcpStream::connect(addr).await.unwrap();
    let body_a = b"{}";
    let req_a = format!(
        "POST /v2/oauth/revoke HTTP/1.1\r\nHost: 127.0.0.1\r\nExpect: 100-continue\r\nContent-Length: {}\r\n\r\n{}",
        body_a.len(),
        "{}"
    );
    cli_a.write_all(req_a.as_bytes()).await.unwrap();
    let mut out_a = Vec::new();
    cli_a.read_to_end(&mut out_a).await.unwrap();
    let str_a = String::from_utf8_lossy(&out_a);
    assert!(str_a.contains("100 Continue") && str_a.contains("400 Bad Request"));

    // Case B: Immediate EOF
    let cli_b = TcpStream::connect(addr).await.unwrap();
    drop(cli_b);

    // Case C: Oversized Content-Length -> 413 Payload Too Large
    let mut cli_c = TcpStream::connect(addr).await.unwrap();
    cli_c
        .write_all(b"POST /v2/oauth/token HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 99999999\r\n\r\n")
        .await
        .unwrap();
    let mut out_c = Vec::new();
    cli_c.read_to_end(&mut out_c).await.unwrap();
    assert!(String::from_utf8_lossy(&out_c).contains("413 Payload Too Large"));

    srv_a.await.unwrap();
}

#[test]
fn formulas_all_remaining_functions_boundaries_and_arithmetic_mutants_killed() {
    let sde = SdeRegistry::with_canonical_seed();

    // Skills & Learning (#1-#8)
    assert_eq!(sp_for_skill_level(2.0, 0), 0.0);
    assert_eq!(sp_for_skill_level(2.0, 3), 16_000.0);
    assert!((skill_training_time_seconds(0.0, 250.0, 20.0, 10.0, 1.0) - 600.0).abs() < 1e-6);
    assert_eq!(skill_training_time_seconds(500.0, 250.0, 20.0, 10.0, 1.0), 0.0);
    assert_eq!(skill_injector_sp_yield(5_000_000.0), 400_000.0);
    assert_eq!(skill_injector_sp_yield(50_000_000.0), 300_000.0);

    // Standings & Security (#9-#17)
    assert!((apply_standing_decrease(0.0, 25.0) - (-2.5)).abs() < 1e-6);
    assert!((compute_effective_standing(-4.0, 0, 5) - (-1.2)).abs() < 1e-6);
    assert_eq!(required_standing_for_agent_level(1), -1.99);
    assert_eq!(required_standing_for_agent_level(2), 1.0);
    assert_eq!(required_standing_for_agent_level(3), 3.0);
    assert_eq!(required_standing_for_agent_level(4), 5.0);
    assert_eq!(required_standing_for_agent_level(5), 7.0);
    assert_eq!(apply_security_tag_repair(0.5, 3, 0.0), 0.5);

    // Kinematics & Short Warp (#18-#26)
    let v_t = subwarp_velocity_at_time(300.0, 2.0, 1_000_000.0, 2.0);
    assert!((v_t - 300.0 * (1.0 - (-1.0_f64).exp())).abs() < 1e-6);
    let x_t = subwarp_distance_at_time(300.0, 2.0, 1_000_000.0, 2.0);
    assert!((x_t - 300.0 * (2.0 - 2.0 * (1.0 - (-1.0_f64).exp()))).abs() < 1e-6);
    assert!(propulsion_boosted_velocity(200.0, 500.0, 1_500_000.0, 1_000_000.0, 500_000.0) > 200.0);
    assert!(compute_warp_duration_seconds(0.2 * AU_METERS, 3.0, 250.0) >= 1.0);

    // Dogma, Thermodynamics, Boosters, Command Bursts, Abyssal (#27-#42)
    assert!((dogma_stacking_penalty(0) - 1.0).abs() < 1e-6);
    assert!(dogma_stacking_penalty(1) < 1.0);
    assert!(passive_capacitor_or_shield_recharge(250.0, 1000.0, 10_000.0, 1_000.0) > 250.0);
    assert!((peak_passive_recharge_per_sec(1000.0, 10.0) - 250.0).abs() < 1e-6);
    let h = rack_heat_step(0.0, 5.0, 1.0, 1.0);
    assert!(h > 0.0);
    assert!(slot_heat_damage_chance(0.5, 0, 0.5) >= 0.0);
    assert_eq!(nanite_repair_paste_needed(100.0, 0.05), 5);
    let _ = booster_side_effect_check(0.5, 20.0, 5, 5, 0.01);
    let cb = command_burst_aggregate("max", &[10.0, 25.0, 15.0]);
    assert!((cb - 25.0).abs() < 1e-6);
    let ab = abyssal_attribute_roll(
        &HashMap::from([(9, 100.0)]),
        &HashMap::from([(9, [0.9, 1.2])]),
        &HashMap::from([(9, 1.0)]),
    );
    assert!((ab[&9] - 120.0).abs() < 1e-6);
    assert!((compute_layer_ehp(500.0, [0.25, 0.5, 0.5, 0.5], [1.0, 0.0, 0.0, 0.0]) - 2000.0).abs() < 1e-6);
    assert!((compute_layer_ehp(500.0, [0.25; 4], [0.0; 4]) - 500.0 / 1e-6).abs() < 1e-3);

    // Combat, Vorton, Entropic, ECM (#43-#65)
    assert!(target_lock_time_sec(500.0, 40.0) > 0.0);
    let omega = turret_angular_velocity(DVec3::ZERO, DVec3::ZERO, DVec3::new(10_000.0, 0.0, 0.0), DVec3::new(0.0, 200.0, 0.0));
    assert!((omega - 0.02).abs() < 1e-6);
    assert!(turret_chance_to_hit(0.0, 1.0, 40.0, 10_000.0, 15_000.0, 5_000.0) > 0.9);
    assert!(turret_chance_to_hit_with_res(0.0, 1.0, 40.0, 40.0, 10_000.0, 15_000.0, 5_000.0) > 0.9);
    let (shot_dmg, _wrecking) = turret_shot_damage(DamageVector { em: 100.0, ..Default::default() }, 0.8, 0.2);
    assert!(shot_dmg.em > 0.0);
    assert!((missile_damage_factor(100.0, 0.0, 50.0, 100.0, 0.5) - 1.0).abs() < 1e-6);
    assert!((breacher_pod_tick_damage(1000.0, 0.02, 10_000.0) - 200.0).abs() < 1e-6);
    let v_cap = vorton_arc_targets_with_cap(
        1,
        DVec3::ZERO,
        &[(2, DVec3::new(100.0, 0.0, 0.0)), (2, DVec3::new(200.0, 0.0, 0.0)), (3, DVec3::new(50.0, 0.0, 0.0))],
        1_000.0,
        1,
    );
    assert_eq!(v_cap, vec![1, 3]);
    assert!((entropic_spool_multiplier(3, 5, 0.1) - 1.3).abs() < 1e-6);
    assert_eq!(compute_ecm_jam_chance(5.0, 10.0, 25_000.0, 20_000.0, 0.0), 0.0);
    assert!((compute_ecm_jam_chance(5.0, 10.0, 15_000.0, 20_000.0, 0.0) - 0.5).abs() < 1e-6);

    // Insurance, Industry, PI, Research, Probes (#66-#119)
    let rifter_ref = compute_ship_insurance_reference_value(&sde, 587);
    let tiers = compute_insurance_levels(rifter_ref);
    assert_eq!(tiers.len(), 6);
    assert_eq!(tiers[5].0, "Platinum");
    assert!((tiers[5].2 - rifter_ref).abs() < 1.0);
    assert_eq!(compute_manufacturing_material_qty(7, 1, 10, 5.0, 2.0), 7);
    let mfg_sec = compute_manufacturing_time_seconds(2, 1000, 20, 5, 5, 10.0);
    assert_eq!(mfg_sec, (2.0 * 1000.0 * 0.80 * 0.80 * 0.85 * 0.90_f64).ceil() as u64);
    assert!((compute_poco_export_tax(1_000.0, 0.10, 5, 0.02) - 70.0).abs() < 1e-6);
    assert!((compute_daily_research_points(50.0, 20.0, 4, 5) - 50.0 * 1.2 * 81.0).abs() < 1e-6);
    assert_eq!(compute_probe_scan_signal_pct(40.0, 10.0, 1.0, 1.5), 0.0);
}
