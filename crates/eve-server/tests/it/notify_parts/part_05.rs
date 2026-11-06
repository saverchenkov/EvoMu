#![forbid(unsafe_code)]
#[allow(unused_imports)]
use super::*;

mod notify_live_rpc_closure {
    #[allow(unused_imports)]
    use super::*;

// Live client-RPC notification closure tests (`notify:On*`).
//
// Verifies server-to-client V24 notifications emitted by real domain RPCs
// (`raffleProxy.CreateRaffle`, `raffleProxy.BuyRandomTickets`).


use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const PILOT_C: i64 = 90_000_003;
const JITA_STATION: i64 = 60_003_760;

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton,
        version: 1,
        ..Default::default()
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_in_station(
    ctx: &ServerContext,
    conn_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(conn_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(JITA_STATION);
            ch.balance = IskCents::from_isk(100_000_000);
        }
    });
    sess.station_id = Some(JITA_STATION);
    let _ = drain_all(ctx, &mut sess, char_id);
    sess
}

fn drain_all(ctx: &ServerContext, sess: &mut SessionState, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    let mut out = sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(char_id) {
        out.push((n.event, n.args));
    }
    out
}

fn find_notif<'a>(notifs: &'a [(String, Vec<PyRep>)], name: &str) -> &'a [PyRep] {
    notifs
        .iter()
        .find(|(e, _)| e == name)
        .map(|(_, args)| args.as_slice())
        .unwrap_or_else(|| {
            panic!(
                "Expected notification {name}, got {:?}",
                notifs.iter().map(|(e, _)| e).collect::<Vec<_>>()
            )
        })
}

// parity: notify:OnRaffleCreatedServer, notify:OnRaffleUpdatedServer, notify:OnRaffleFinishedServer
#[tokio::test]
async fn test_notify_live_hypernet_raffle_notifications() {
    eve_testkit::attest(&[
        "notify:OnRaffleCreatedServer",
        "notify:OnRaffleUpdatedServer",
        "notify:OnRaffleFinishedServer",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let prize_id = 640_000_110_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "test_c".to_string(),
            eve_db::AccountRecord {
                user_id: 3,
                username: "test_c".to_string(),
                password_token: eve_db::WorldRepository::hash_password_token("test_c", "test_c-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        if let Some(mut char_c) = snap.characters.get(&PILOT_A).cloned() {
            char_c.character_id = PILOT_C;
            char_c.user_id = 3;
            char_c.name = "Test C".to_string();
            snap.characters.insert(PILOT_C, char_c);
        }
    });
    let mut sess_a = login_in_station(&ctx, 9_501, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_in_station(&ctx, 9_502, "other", "other-secret", PILOT_B).await;
    let mut sess_c = login_in_station(&ctx, 9_503, "test_c", "test_c-secret", PILOT_C).await;
    rpc(&ctx, &mut sess_c, "raffleProxy", "SubscribeToRaffle", &[]).await.expect("sub");

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(prize_id, make_item(prize_id, 587, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 1, true));
        snap.items.insert(prize_id + 1, make_item(prize_id + 1, 52_568, PILOT_A, JITA_STATION, InventoryFlag::Hangar, 10, false));
    });
    let _ = drain_all(&ctx, &mut sess_a, PILOT_A);

    let raffle_rep = rpc(
        &ctx,
        &mut sess_a,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(prize_id),
            PyRep::Integer(8),
            PyRep::Integer(1_000_000),
        ],
    )
    .await
    .expect("CreateRaffle");
    let raffle_id = raffle_rep.dict_get("raffle_id").and_then(|v| v.as_i64()).expect("raffle_id");
    let create_notifs = drain_all(&ctx, &mut sess_a, PILOT_A);
    let created_args = find_notif(&create_notifs, "OnRaffleCreatedServer");
    assert_eq!(created_args[0], PyRep::Integer(raffle_id));
    assert_eq!(created_args[1].dict_get("raffle_id").and_then(|v| v.as_i64()), Some(raffle_id));

    let create_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
    let created_args_c = find_notif(&create_notifs_c, "OnRaffleCreatedServer");
    assert_eq!(created_args_c[0], PyRep::Integer(raffle_id));

    rpc(&ctx, &mut sess_b, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_id), PyRep::Integer(8)])
        .await
        .expect("BuyRandomTickets");
    let buy_notifs_b = drain_all(&ctx, &mut sess_b, PILOT_B);
    let updated_args = find_notif(&buy_notifs_b, "OnRaffleUpdatedServer");
    assert_eq!(updated_args[0], PyRep::Integer(raffle_id));
    let finished_args = find_notif(&buy_notifs_b, "OnRaffleFinishedServer");
    assert_eq!(finished_args[0], PyRep::Integer(raffle_id));
    let win_ticket = finished_args[1].as_i64().expect("winning ticket index");
    assert!((0..8).contains(&win_ticket));

    let buy_notifs_c = drain_all(&ctx, &mut sess_c, PILOT_C);
    let updated_args_c = find_notif(&buy_notifs_c, "OnRaffleUpdatedServer");
    assert_eq!(updated_args_c[0], PyRep::Integer(raffle_id));
    let finished_args_c = find_notif(&buy_notifs_c, "OnRaffleFinishedServer");
    assert_eq!(finished_args_c[0], PyRep::Integer(raffle_id));
    assert_eq!(finished_args_c[1], PyRep::Integer(win_ticket));
}

}

// Folded submodule: notify_p7_sp1_sp2_closure
#[allow(unused_imports, dead_code)]
mod notify_p7_sp1_sp2_closure {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for `track/p7-sp1-sp2` notification parity closure.
// Exercises real server-side RPCs and verifies emitted V24 MachoNet notifications.


use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;

use crate::common;

const P1: i64 = 90_000_001;
const JITA_SYSTEM: u32 = 30000142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("open sde"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(sde, db);
    ctx.set_clock_paused(true);
    ctx
}

async fn pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let args = [PyRep::Integer(char_id)];
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &args, &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(char_id);
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    let all = ctx.drain_notifications_for_char(char_id);
    let mut matched = Vec::new();
    let mut keep = Vec::new();
    for n in all {
        if n.event == event {
            matched.push(n.args);
        } else {
            keep.push(n);
        }
    }
    if !keep.is_empty() {
        if let Ok(mut guard) = ctx.pending_notifications.write() {
            guard.entry(char_id).or_default().extend(keep);
        }
    }
    matched
}

#[tokio::test]
async fn test_notify_operations_and_progression_transitions() {
    // parity: notify:OnOperationDeactivated
    let ctx = new_ctx();
    let mut s = pilot(&ctx, 2, "demo", "demo-secret", P1).await;
    s.solar_system_id = Some(JITA_SYSTEM);
    let kw = HashMap::new();
    let mut op_ids: Vec<i64> = eve_server::services::operations_manager::get_operation_defs(&ctx.sde)
        .op_tasks
        .keys()
        .copied()
        .collect();
    op_ids.sort_unstable();
    let (op1, op2) = (op_ids[0], op_ids[1]);

    ctx.dispatch_rpc(
        &mut s,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op1), PyRep::Integer(i64::from(JITA_SYSTEM))],
        &kw,
        None,
    )
    .await
    .expect("activate op 1");
    assert!(
        events(&ctx, P1, "OnOperationDeactivated").is_empty(),
        "First operation activation must not emit OnOperationDeactivated"
    );

    ctx.dispatch_rpc(
        &mut s,
        "operationsManager",
        None,
        "activate_operation_in_solar_system",
        &[PyRep::Integer(op2), PyRep::Integer(i64::from(JITA_SYSTEM))],
        &kw,
        None,
    )
    .await
    .expect("activate op 2");
    let deact_ev = events(&ctx, P1, "OnOperationDeactivated");
    assert_eq!(deact_ev.len(), 1);
    assert!(deact_ev[0].is_empty());
}

}

// Folded submodule: notify_real_emitters_batch8_part5
#[allow(unused_imports, dead_code)]
mod notify_real_emitters_batch8_part5 {
    #[allow(unused_imports)]
    use super::*;

// Real server-side RPC tests for Notify/RPC Batch 8 (Part 5: Session, Localization & LoadService RPCs).
//
// Covers all 8 Group 5 Session / Localization / LoadService parity rows:
// - `rpc:userSvc.ProcessEVESessionCreated`
// - `rpc:sessionMgr.EndAllGameSessions`
// - `rpc:sessionMgr.RemoveSessionsFromServer`
// - `rpc:localizationServer.ReloadFSDPickle`
// - `rpc:localizationServer.UpdateLocalizationQAWrap`
// - `rpc:loadService.GetTotalStats`
// - `rpc:loadService.StartLoad`
// - `rpc:loadService.StopLoad`


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
// parity: rpc:sessionMgr.RemoveSessionsFromServer
#[tokio::test]
async fn notify_batch8_session_localization_and_load_rpcs() {
    eve_testkit::attest(&[
        "rpc:userSvc.ProcessEVESessionCreated",
        "rpc:sessionMgr.EndAllGameSessions",
        "rpc:sessionMgr.RemoveSessionsFromServer",
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
    assert_eq!(active_after_end_all, vec![9_501]);

    // 3. localizationServer.ReloadFSDPickle & UpdateLocalizationQAWrap
    let reload_err = rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "ReloadFSDPickle",
        &[PyRep::String("en-us.pickle".into())],
    )
    .await
    .unwrap_err();
    assert_eq!(reload_err.code, "UnimplementedRPC");

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

    // GM caller succeeds with boolean toggle (DERIVED QA-wrap); non-bool overload fails closed with UnimplementedRPC
    let qawrap_rep = rpc(
        &ctx,
        &mut sess_a,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect("UpdateLocalizationQAWrap");
    assert_eq!(qawrap_rep, PyRep::Bool(true));
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

}

// Folded submodule: parity_notifies_part1
