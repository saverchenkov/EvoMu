//! Unit tests for `macho_cluster_mgmt` (`machoNet` cluster state, shutdown, code reload, `sessionMgr`, `loadService`).

#![forbid(unsafe_code)]

use super::*;
use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn gm_session(char_id: i64) -> SessionState {
    let mut s = SessionState::default();
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.solar_system_id = Some(30000142);
    s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    s
}

#[test]
fn cluster_queries_metrics_node_lookup_and_load_settings() {
    let mut ticks = 5_u64;
    let mut target_sys = None;
    apply_load_setting(
        "ticks",
        &PyRep::Integer(250),
        Some(&mut ticks),
        Some(&mut target_sys),
        true,
    )
    .unwrap();
    assert_eq!(ticks, 100);
    apply_load_setting(
        "solarSystemID",
        &PyRep::Integer(30000142),
        Some(&mut ticks),
        Some(&mut target_sys),
        true,
    )
    .unwrap();
    assert_eq!(target_sys, Some(30000142));
    assert_eq!(
        apply_load_setting(
            "ticks",
            &PyRep::Integer(-1),
            Some(&mut ticks),
            Some(&mut target_sys),
            true,
        )
        .unwrap_err()
        .code,
        "UserError"
    );

    let ctx = test_ctx();
    reset_cluster_state(&ctx);
    let kw = HashMap::new();
    let player_sess = SessionState::default();

    assert_eq!(
        handle(&ctx, &player_sess, "machoNet", "GetConnectedClusterMode", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("LIVE".into())
    );
    assert_eq!(
        handle(&ctx, &player_sess, "machoNet", "GetConnectedClusterName", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("TRANQUILITY".into())
    );
    assert_eq!(
        handle(&ctx, &player_sess, "machoNet", "GetEjabberdAddress", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("127.0.0.1".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &player_sess,
            "machoNet",
            "GetPolarisExternalTunnelingAddress",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String("127.0.0.1".into())
    );

    let props = handle(&ctx, &player_sess, "machoNet", "GetConnectionProperties", &[], &kw)
        .unwrap()
        .unwrap();
    assert!(props.dict_get("vipMode").is_some());

    let global_cfg = handle(&ctx, &player_sess, "machoNet", "GetGlobalConfig", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(
        global_cfg.dict_get("clusterName").and_then(|v| v.as_str()),
        Some("TRANQUILITY")
    );

    let node_id = handle(
        &ctx,
        &player_sess,
        "machoNet",
        "GetNodeFromAddress",
        &[PyRep::String("127.0.0.1:26001".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(node_id, PyRep::Integer(2));

    let unauth_metrics = handle(&ctx, &player_sess, "machoNet", "GetMetrics", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert!(unauth_metrics.code.starts_with("Unauth"));

    let admin = gm_session(940_001);
    let metrics = handle(&ctx, &admin, "machoNet", "GetMetrics", &[], &kw)
        .unwrap()
        .expect("GM GetMetrics");
    assert!(matches!(metrics, PyRep::List(ref items) if items.len() == 1));
}

#[test]
fn cluster_shutdown_code_reload_and_auxiliary_services() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut non_gm = SessionState::default();
    non_gm.user_id = Some(2);
    non_gm.character_id = Some(940_002);

    // Non-GM rejected on admin endpoints
    assert_eq!(
        handle(&ctx, &non_gm, "machoNet", "GracefulShutdown", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(&ctx, &non_gm, "machoNet", "ReloadClientCodeHash", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "UserError"
    );

    let gm = gm_session(940_001);

    // GracefulShutdown + GracefulShutdownStarted + GracefulShutdownComplete
    handle(
        &ctx,
        &gm,
        "machoNet",
        "GracefulShutdown",
        &[
            PyRep::Integer(1),
            PyRep::String("Daily downtime".into()),
            PyRep::Integer(120),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .unwrap()
    .expect("GracefulShutdown");
    handle(&ctx, &gm, "machoNet", "GracefulShutdownStarted", &[], &kw)
        .unwrap()
        .expect("GracefulShutdownStarted");
    handle(
        &ctx,
        &gm,
        "machoNet",
        "GracefulShutdownComplete",
        &[PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .expect("GracefulShutdownComplete");

    let st = get_cluster_state(&ctx);
    assert!(st.shutdown_in_progress);
    assert_eq!(st.shutdown_reason.as_deref(), Some("Daily downtime"));
    assert!(st.shutdown_complete_nodes.contains(&2));

    // TerminateAllClients sets clients_terminated
    handle(
        &ctx,
        &gm,
        "machoNet",
        "TerminateAllClients",
        &[PyRep::String("Restarting".into())],
        &kw,
    )
    .unwrap()
    .expect("TerminateAllClients");
    assert!(get_cluster_state(&ctx).clients_terminated);

    // ReloadClientCodeHash updates service_state_blobs
    let hash_rep = handle(
        &ctx,
        &gm,
        "machoNet",
        "ReloadClientCodeHash",
        &[PyRep::String("deadbeef01020304".into())],
        &kw,
    )
    .unwrap()
    .expect("ReloadClientCodeHash");
    assert_eq!(hash_rep, PyRep::String("deadbeef01020304".into()));

    // externalQueueMgr.PublishProxyEvent & tcpRawProxyService.GetESPTunnelingAddressByNodeID
    handle(
        &ctx,
        &non_gm,
        "externalQueueMgr",
        "PublishProxyEvent",
        &[PyRep::String("player.login".into()), PyRep::Integer(940_002)],
        &kw,
    )
    .unwrap()
    .expect("PublishProxyEvent");
    let tunnel = handle(
        &ctx,
        &non_gm,
        "tcpRawProxyService",
        "GetESPTunnelingAddressByNodeID",
        &[],
        &kw,
    )
    .unwrap()
    .expect("GetESPTunnelingAddressByNodeID");
    assert!(matches!(tunnel, PyRep::Tuple(ref parts) if parts.len() == 2));

    // sessionMgr methods
    assert_eq!(
        handle(
            &ctx,
            &non_gm,
            "sessionMgr",
            "ConnectToClientService",
            &[PyRep::String("chat".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(
            &ctx,
            &gm,
            "sessionMgr",
            "ConnectToClientService",
            &[PyRep::String("chat".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );
    assert_eq!(
        handle(
            &ctx,
            &gm,
            "sessionMgr",
            "UpdateSessionAttributes",
            &[],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    handle(
        &ctx,
        &gm,
        "sessionMgr",
        "RemoveSessionsFromServer",
        &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(999)])],
        &kw,
    )
    .unwrap()
    .expect("RemoveSessionsFromServer");

    // loadService.StartLoad + unknown service/method
    handle(
        &ctx,
        &gm,
        "loadService",
        "StartLoad",
        &[PyRep::Dict(vec![(PyRep::String("ticks".into()), PyRep::Integer(1))])],
        &kw,
    )
    .unwrap()
    .expect("StartLoad");

    assert!(handle(&ctx, &gm, "machoNet", "", &[], &kw).is_none());
    assert!(handle(&ctx, &gm, "unhandledService", "", &[], &kw).is_none());
}

#[test]
fn non_gm_permission_checks_and_cluster_property_defaults() {
    let ctx = test_ctx();
    let cid = 900_055_i64;
    let mut non_gm = SessionState::default();
    non_gm.user_id = Some(55);
    non_gm.character_id = Some(cid);
    non_gm.solar_system_id = Some(30000142);
    let kw = HashMap::new();

    // Non-GM callers cannot invoke privileged shutdown/reload/load operations
    let shutdown_err = handle(&ctx, &non_gm, "machoNet", "GracefulShutdown", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(shutdown_err.code, "InsufficientRole");

    let reload_err = handle(&ctx, &non_gm, "machoNet", "ReloadClientCodeHash", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(reload_err.code, "UserError");

    let load_err = handle(&ctx, &non_gm, "loadService", "StartLoad", &[], &kw)
        .unwrap()
        .unwrap_err();
    assert_eq!(load_err.code, "UserError");

    // Cluster queries succeed for normal authenticated sessions
    let mode = handle(&ctx, &non_gm, "machoNet", "GetConnectedClusterMode", &[], &kw)
        .unwrap()
        .expect("GetConnectedClusterMode");
    assert!(mode.as_str().is_some());

    let name = handle(&ctx, &non_gm, "machoNet", "GetConnectedClusterName", &[], &kw)
        .unwrap()
        .expect("GetConnectedClusterName");
    assert!(name.as_str().is_some());

    let tunnel = handle(
        &ctx,
        &non_gm,
        "machoNet",
        "GetPolarisExternalTunnelingAddress",
        &[],
        &kw,
    )
    .unwrap()
    .expect("GetPolarisExternalTunnelingAddress");
    assert!(matches!(tunnel, PyRep::Tuple(_) | PyRep::None | PyRep::String(_)));
}
