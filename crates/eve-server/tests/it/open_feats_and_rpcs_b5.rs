//! Integration tests for open RPC and UI feature rows (Batch 5).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_db::service_roles::{ROLE_ADMIN, ROLE_CONTENT, ROLE_GMH};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYSTEM: u32 = 30_000_142;

fn new_ctx() -> ServerContext {
    let sde = std::sync::Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = std::sync::Arc::new(eve_db::WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64, role: u64) -> SessionState {
    let mut sess = SessionState::new(char_id as u64);
    let (username, password) = if char_id == PILOT_2 {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    common::login_pilot(ctx, &mut sess, username, password).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess.role |= role;
    sess
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v),
        _ => None,
    }
}

fn kv_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v),
        PyRep::Object { args, .. } => kv_get(args, key),
        _ => dict_get(rep, key),
    }
}

#[tokio::test]
async fn test_load_service_start_stop_and_stats() {
    // parity: rpc:loadService.GetTotalStats
    let ctx = new_ctx();
    let mut player_sess = setup_pilot(&ctx, PILOT_1, 0).await;
    let err = rpc(&ctx, &mut player_sess, "loadService", "StartLoad", &[])
        .await
        .expect_err("non-GM should be rejected");
    assert_eq!(err.code, "UserError");

    let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_ADMIN).await;
    let res = rpc(
        &ctx,
        &mut gm_sess,
        "loadService",
        "StartLoad",
        &[PyRep::String("stress_profile_v1".into())],
    )
    .await
    .expect("StartLoad");
    assert_eq!(res, PyRep::None);

    let stats_active = rpc(&ctx, &mut gm_sess, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats");
    assert_eq!(dict_get(&stats_active, "active"), Some(&PyRep::Bool(true)));
    assert_eq!(
        dict_get(&stats_active, "runs_started"),
        Some(&PyRep::Integer(1))
    );

    let stop_res = rpc(&ctx, &mut gm_sess, "loadService", "StopLoad", &[])
        .await
        .expect("StopLoad");
    assert_eq!(stop_res, PyRep::None);

    let stats_stopped = rpc(&ctx, &mut gm_sess, "loadService", "GetTotalStats", &[])
        .await
        .expect("GetTotalStats after stop");
    assert_eq!(
        dict_get(&stats_stopped, "active"),
        Some(&PyRep::Bool(false))
    );
    assert_eq!(
        dict_get(&stats_stopped, "total_requests"),
        Some(&PyRep::Integer(2))
    );
}

#[tokio::test]
async fn test_session_mgr_remove_and_end_all_sessions() {
    let ctx = new_ctx();
    let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_ADMIN).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("active_sessions".into(), "[101, 102, 103]".into());
    });

    let rem_res = rpc(
        &ctx,
        &mut gm_sess,
        "sessionMgr",
        "RemoveSessionsFromServer",
        &[PyRep::None, PyRep::List(vec![PyRep::Integer(102)])],
    )
    .await
    .expect("RemoveSessionsFromServer");
    assert_eq!(rem_res, PyRep::None);

    let remaining = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("active_sessions")
            .cloned()
            .unwrap_or_default()
    });
    assert_eq!(remaining, "[101,103]");

    let end_res = rpc(&ctx, &mut gm_sess, "sessionMgr", "EndAllGameSessions", &[])
        .await
        .expect("EndAllGameSessions");
    assert_eq!(end_res, PyRep::None);

    let after_end = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("active_sessions")
            .cloned()
            .unwrap_or_default()
    });
    assert_eq!(after_end, "[]");
}

#[tokio::test]
async fn test_raffle_qa_seed_db2_callproc_and_debug_exec() {
    // parity: rpc:raffleMgr.QA_SeedRaffles
    let ctx = new_ctx();
    let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_ADMIN | ROLE_GMH).await;

    let seeded = rpc(
        &ctx,
        &mut gm_sess,
        "raffleMgr",
        "QA_SeedRaffles",
        &[PyRep::Integer(2)],
    )
    .await
    .expect("QA_SeedRaffles");
    match seeded {
        PyRep::List(items) => assert_eq!(items.len(), 2),
        other => panic!("expected list of 2 seeded raffles, got {other:?}"),
    }

    let polaris = rpc(
        &ctx,
        &mut gm_sess,
        "DB2",
        "CallProc",
        &[PyRep::String("zcluster.Nodes_PolarisID".into())],
    )
    .await
    .expect("DB2.CallProc");
    assert_eq!(polaris, PyRep::Integer(1));

    let exec_err = rpc(
        &ctx,
        &mut gm_sess,
        "debug",
        "Exec",
        &[PyRep::String("status_check".into())],
    )
    .await
    .expect_err("debug.Exec fail-closed");
    assert_eq!(exec_err.code, "UnimplementedRPC");
}

#[tokio::test]
async fn test_localization_server_and_user_svc_session_created() {
    let ctx = new_ctx();
    let mut gm_sess = setup_pilot(&ctx, PILOT_1, ROLE_GMH | ROLE_CONTENT).await;

    let toggle_err = rpc(
        &ctx,
        &mut gm_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Bool(true)],
    )
    .await
    .expect_err("boolean toggle must fail");
    assert_eq!(toggle_err.code, "UnimplementedRPC");

    let wrap_err = rpc(
        &ctx,
        &mut gm_sess,
        "localizationServer",
        "UpdateLocalizationQAWrap",
        &[PyRep::Dict(vec![(
            PyRep::Integer(777_001),
            PyRep::String("QA Localized Label".into()),
        )])],
    )
    .await
    .expect_err("UpdateLocalizationQAWrap fail-closed");
    assert_eq!(wrap_err.code, "UnimplementedRPC");

    let reload_res = rpc(
        &ctx,
        &mut gm_sess,
        "localizationServer",
        "ReloadFSDPickle",
        &[PyRep::String("res:/localization/eve_en.pickle".into())],
    )
    .await
    .expect("ReloadFSDPickle");
    assert_eq!(reload_res, PyRep::Bool(true));

    let sess_created = rpc(
        &ctx,
        &mut gm_sess,
        "userSvc",
        "ProcessEVESessionCreated",
        &[PyRep::Integer(PILOT_1)],
    )
    .await
    .expect("ProcessEVESessionCreated");
    assert_eq!(sess_created, PyRep::None);
}

#[tokio::test]
async fn test_ui_neocom_badges_map_stats_and_agency() {
    let ctx = new_ctx();
    let mut sess = setup_pilot(&ctx, PILOT_1, 0).await;
    sess.solar_system_id = Some(JITA_SYSTEM);

    let set_pop = rpc(
        &ctx,
        &mut sess,
        "activityMgr",
        "SetActivitiesPopState",
        &[PyRep::Dict(vec![(
            PyRep::String("daily".into()),
            PyRep::Bool(true),
        )])],
    )
    .await
    .expect("SetActivitiesPopState");
    assert_eq!(set_pop, PyRep::Bool(true));

    let pop_state = rpc(&ctx, &mut sess, "activityMgr", "GetActivitiesPopState", &[])
        .await
        .expect("GetActivitiesPopState");
    assert_eq!(dict_get(&pop_state, "daily"), Some(&PyRep::Bool(true)));

    let badges = rpc(
        &ctx,
        &mut sess,
        "activityMgr",
        "GetActivityBadgeStatuses",
        &[],
    )
    .await
    .expect("GetActivityBadgeStatuses");
    assert!(matches!(badges, PyRep::PySet(_)));

    let history = rpc(
        &ctx,
        &mut sess,
        "map",
        "GetHistory",
        &[PyRep::Integer(0), PyRep::Integer(24)],
    )
    .await
    .expect("map.GetHistory");
    assert!(matches!(history, PyRep::ObjectEx { .. }));

    let visits = rpc(&ctx, &mut sess, "map", "GetSolarSystemVisits", &[])
        .await
        .expect("map.GetSolarSystemVisits");
    assert!(matches!(visits, PyRep::List(_)));

    let agency_data = rpc(
        &ctx,
        &mut sess,
        "custom_agency_provider",
        "get_content_data",
        &[],
    )
    .await
    .expect("custom_agency_provider.get_content_data");
    match &agency_data {
        PyRep::List(items) => {
            if let Some(first) = items.first() {
                assert_eq!(
                    kv_get(first, "solarSystemID"),
                    Some(&PyRep::Integer(JITA_SYSTEM as i64))
                );
                assert!(kv_get(first, "contentType").is_some());
                assert!(kv_get(first, "instanceID").is_some());
            }
        }
        other => panic!("expected PyRep::List from get_content_data, got {other:?}"),
    }
}
