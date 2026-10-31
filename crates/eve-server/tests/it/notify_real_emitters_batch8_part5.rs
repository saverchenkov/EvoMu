//! Real server-side RPC tests for Notify/RPC Batch 8 (Part 5: Session, Localization & LoadService RPCs).
//!
//! Covers all 8 Group 5 Session / Localization / LoadService parity rows:
//! - `rpc:userSvc.ProcessEVESessionCreated`
//! - `rpc:sessionMgr.EndAllGameSessions`
//! - `rpc:sessionMgr.RemoveSessionsFromServer`
//! - `rpc:localizationServer.ReloadFSDPickle`
//! - `rpc:localizationServer.UpdateLocalizationQAWrap`
//! - `rpc:loadService.GetTotalStats`
//! - `rpc:loadService.StartLoad`
//! - `rpc:loadService.StopLoad`

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::{service_roles::ROLE_GM_DEFAULT, SessionState};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

// parity: rpc:userSvc.ProcessEVESessionCreated, rpc:sessionMgr.EndAllGameSessions
// parity: rpc:sessionMgr.RemoveSessionsFromServer, rpc:localizationServer.ReloadFSDPickle
// parity: rpc:loadService.GetTotalStats, rpc:loadService.StartLoad, rpc:loadService.StopLoad
#[tokio::test]
async fn notify_batch8_session_localization_and_load_rpcs() {
    eve_testkit::attest(&[
        "rpc:userSvc.ProcessEVESessionCreated",
        "rpc:sessionMgr.EndAllGameSessions",
        "rpc:sessionMgr.RemoveSessionsFromServer",
        "rpc:localizationServer.ReloadFSDPickle",
        "rpc:loadService.GetTotalStats",
        "rpc:loadService.StartLoad",
        "rpc:loadService.StopLoad",
    ]);

    let ctx = new_ctx();
    let mut unauth = SessionState::new(9_500);

    // Negative: unauthenticated rejections across all 8 RPCs
    for (svc, method, args) in [
        ("userSvc", "ProcessEVESessionCreated", vec![]),
        ("sessionMgr", "EndAllGameSessions", vec![]),
        ("sessionMgr", "RemoveSessionsFromServer", vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])]),
        ("localizationServer", "ReloadFSDPickle", vec![]),
        ("localizationServer", "UpdateLocalizationQAWrap", vec![PyRep::Dict(Vec::new())]),
        ("loadService", "GetTotalStats", vec![]),
        ("loadService", "StartLoad", vec![]),
        ("loadService", "StopLoad", vec![]),
    ] {
        assert_eq!(
            rpc(&ctx, &mut unauth, svc, method, &args).await.unwrap_err().code,
            "Unauthorized",
            "{svc}.{method} must require authentication"
        );
    }

    let mut sess_a = SessionState::new(9_501);
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess_a, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_A)])
        .await
        .expect("SelectCharacterID A");

    let mut sess_b = SessionState::new(9_502);
    common::login_pilot(&ctx, &mut sess_b, "other", "other-secret").await;
    rpc(&ctx, &mut sess_b, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)])
        .await
        .expect("SelectCharacterID B");

    // 1. userSvc.ProcessEVESessionCreated: succeeds for owner, rejects mismatched character ID
    let created_rep = rpc(
        &ctx,
        &mut sess_a,
        "userSvc",
        "ProcessEVESessionCreated",
        &[PyRep::Integer(PILOT_A)],
    )
    .await
    .expect("ProcessEVESessionCreated");
    assert_eq!(created_rep, PyRep::None);
    let recorded_sessions = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs.get("user_sessions").cloned().unwrap_or_default()
    });
    assert!(recorded_sessions.contains("\"session_id\":9501"));

    let mut sess_a_user = SessionState::new(9_503);
    common::login_pilot(&ctx, &mut sess_a_user, "demo", "demo-secret").await;
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a_user,
            "userSvc",
            "ProcessEVESessionCreated",
            &[PyRep::Integer(PILOT_B)],
        )
        .await
        .unwrap_err()
        .code,
        "UserError"
    );
    // Re-bind sess_a after sess_a_user displaced it
    common::login_pilot(&ctx, &mut sess_a, "demo", "demo-secret").await;
    sess_a.character_id = Some(PILOT_A);
    sess_a.role |= ROLE_GM_DEFAULT;

    // Negative: non-GM player rejected from sessionMgr, localizationServer, and loadService admin RPCs
    for (svc, method, args, expected_code) in [
        ("sessionMgr", "EndAllGameSessions", vec![], "InsufficientRole"),
        ("sessionMgr", "RemoveSessionsFromServer", vec![PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])], "InsufficientRole"),
        ("localizationServer", "ReloadFSDPickle", vec![], "RoleRequired"),
        ("localizationServer", "UpdateLocalizationQAWrap", vec![PyRep::Dict(Vec::new())], "RoleRequired"),
        ("loadService", "GetTotalStats", vec![], "UserError"),
        ("loadService", "StartLoad", vec![], "UserError"),
        ("loadService", "StopLoad", vec![], "UserError"),
    ] {
        assert_eq!(
            rpc(&ctx, &mut sess_b, svc, method, &args).await.unwrap_err().code,
            expected_code,
            "{svc}.{method} must reject non-GM caller"
        );
    }

    // Grant GM role to sess_a
    sess_a.role |= ROLE_GM_DEFAULT;

    // 2. sessionMgr.RemoveSessionsFromServer & sessionMgr.EndAllGameSessions
    let (tx_a, mut rx_a) = tokio::sync::mpsc::unbounded_channel();
    let (tx_b, mut rx_b) = tokio::sync::mpsc::unbounded_channel();
    let (tx_c, mut rx_c) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(9_501, tx_a);
    ctx.register_session_closer(9_502, tx_b);
    ctx.register_session_closer(9_503, tx_c);
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "active_sessions".into(),
            serde_json::to_string(&vec![9_501_i64, 9_502_i64, 9_503_i64]).unwrap(),
        );
    });
    let rem_rep = rpc(
        &ctx,
        &mut sess_a,
        "sessionMgr",
        "RemoveSessionsFromServer",
        &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(9_502)])],
    )
    .await
    .expect("RemoveSessionsFromServer");
    assert_eq!(rem_rep, PyRep::None);
    assert_eq!(
        rx_b.try_recv().expect("sess_b disconnect notification").0,
        "RemoveSession"
    );
    assert!(!ctx.session_closers.read().unwrap().contains_key(&9_502));
    assert!(ctx.session_closers.read().unwrap().contains_key(&9_501));
    assert!(ctx.session_closers.read().unwrap().contains_key(&9_503));
    let active_after_rem: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("active_sessions")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });
    assert_eq!(active_after_rem, vec![9_501, 9_503]);

    let end_all_rep = rpc(&ctx, &mut sess_a, "sessionMgr", "EndAllGameSessions", &[])
        .await
        .expect("EndAllGameSessions");
    assert_eq!(end_all_rep, PyRep::None);
    assert_eq!(
        rx_c.try_recv().expect("sess_c disconnect notification").0,
        "EndAllGameSessions"
    );
    assert!(!ctx.session_closers.read().unwrap().contains_key(&9_503));
    assert!(rx_a.try_recv().is_err(), "caller session must not be disconnected");
    assert!(
        ctx.session_closers.read().unwrap().contains_key(&9_501),
        "caller session must survive EndAllGameSessions"
    );
    let active_after_end_all: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("active_sessions")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    });
    assert!(active_after_end_all.is_empty());

    // 3. localizationServer.ReloadFSDPickle & UpdateLocalizationQAWrap
    let reload_rep = rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "ReloadFSDPickle",
        &[PyRep::String("en-us.pickle".into())],
    )
    .await
    .expect("ReloadFSDPickle");
    assert_eq!(reload_rep, PyRep::Bool(true));
    let reload_blob = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("localization:fsd_pickle_reloads")
            .cloned()
            .unwrap_or_default()
    });
    assert!(reload_blob.contains("en-us.pickle"));
    let reload_json: serde_json::Value = serde_json::from_str(&reload_blob).expect("reload_blob json");
    assert!(
        reload_json["localized_entries_count"].as_u64().unwrap_or(0) > 1_000,
        "ReloadFSDPickle must scan real SDE localized entries"
    );

    // Non-GM caller rejected from UpdateLocalizationQAWrap(bool) with RoleRequired
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_b,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
        )
        .await
        .unwrap_err()
        .code,
        "RoleRequired"
    );

    // GM caller fails closed with UnimplementedRPC until QA-wrap state exists
    assert_eq!(
        rpc(
            &ctx,
            &mut sess_a,
            "localizationServer",
            "UpdateLocalizationQAWrap",
            &[PyRep::Dict(vec![(
                PyRep::Integer(42_001),
                PyRep::String("[QA] Warp Drive Active".into()),
            )])],
        )
        .await
        .unwrap_err()
        .code,
        "UnimplementedRPC"
    );

    // 4. loadService.StartLoad, GetTotalStats, StopLoad
    let stats_0 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats initial");
    assert_eq!(stats_0.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(stats_0.dict_get("runs_started").and_then(|v| v.as_i64()), Some(0));
    assert!(stats_0.dict_get("sde_types_count").and_then(|v| v.as_i64()).unwrap_or(0) > 1_000);

    let start_rep = rpc(
        &ctx,
        &mut sess_a,
        "loadService",
        "StartLoad",
        &[PyRep::String("stress_profile_v24".into())],
    )
    .await
    .expect("StartLoad");
    assert_eq!(start_rep, PyRep::None);

    let stats_1 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats active");
    assert_eq!(stats_1.dict_get("active").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(stats_1.dict_get("runs_started").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(stats_1.dict_get("total_requests").and_then(|v| v.as_i64()), Some(1));
    assert!(stats_1.dict_get("systems_stepped").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    assert!(stats_1.dict_get("active_systems_count").and_then(|v| v.as_i64()).unwrap_or(0) >= 1);
    assert!(stats_1.dict_get("world_items_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(stats_1.dict_get("world_characters_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let stop_rep = rpc(&ctx, &mut sess_a, "loadService", "StopLoad", &[])
        .await
        .expect("StopLoad");
    assert_eq!(stop_rep, PyRep::None);

    let stats_2 = rpc(&ctx, &mut sess_a, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats stopped");
    assert_eq!(stats_2.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(stats_2.dict_get("total_requests").and_then(|v| v.as_i64()), Some(2));
}
