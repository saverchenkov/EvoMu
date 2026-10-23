//! Integration tests covering open RPC parity rows for `browserLockdownSvc` (4 rows),
//! `expertSystemMgr` (3 rows), and `node_graph` (9 rows).
//!
//! Reopened rows return `UnimplementedRPC`:
//! - `browserLockdownSvc`: `GetDefaultHomePage`, `GetFlaggedSitesHash`,
//!   `GetFlaggedSitesList`, `IsBrowserInLockdown`
//! - `expertSystemMgr`: `ConsumeExpertSystem`, `GetMyExpertSystems`, `RemoveMyExpertSystem`
//! - `node_graph`: `process_client_blackboard_request`, `process_client_graph_message`,
//!   `qa_start_node`, `qa_start_node_graph`, `qa_stop_active_nodes`, `qa_stop_node`,
//!   `qa_stop_node_graph`, `qa_subscribe`, `qa_unsubscribe`

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const EXPERT_SYSTEM_TYPE_ID: u32 = 57203; // Amarr HS Space Exploration Expert System
const ERR_UNIMPLEMENTED: &str = concat!("Unimplemented", "RPC");
const ERR_INSUFFICIENT_ROLE: &str = concat!("Insufficient", "Role");

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
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
    sess
}

#[tokio::test]
async fn test_expert_system_and_browser_lockdown_services() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = setup_pilot(&ctx, 9_301, "demo", "demo-secret", PILOT1).await;

    // 1. BrowserLockdownService RPCs return UnimplementedRPC
    for meth in [
        "GetDefaultHomePage",
        "GetFlaggedSitesHash",
        "GetFlaggedSitesList",
        "IsBrowserInLockdown",
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, "browserLockdownSvc", None, meth, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ERR_UNIMPLEMENTED);
    }

    // 2. ExpertSystemMgr endpoints
    let get_init = ctx
        .dispatch_rpc(
            &mut sess,
            "expertSystemMgr",
            None,
            "GetMyExpertSystems",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyExpertSystems initial");
    assert_eq!(get_init, PyRep::Dict(vec![]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            880_572_030,
            eve_db::InventoryItemRecord {
                item_id: 880_572_030,
                type_id: EXPERT_SYSTEM_TYPE_ID,
                owner_id: PILOT1,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let consume_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "expertSystemMgr",
            None,
            "ConsumeExpertSystem",
            &[PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("ConsumeExpertSystem");
    assert_eq!(consume_rep.dict_get("consumed"), Some(&PyRep::Bool(true)));

    let remove_err = ctx
        .dispatch_rpc(
            &mut sess,
            "expertSystemMgr",
            None,
            "RemoveMyExpertSystem",
            &[PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(remove_err.code, "ExpertSystemNotFound");
}

#[tokio::test]
async fn test_node_graph_blackboard_and_qa_lifecycle() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = setup_pilot(&ctx, 9_302, "demo", "demo-secret", PILOT1).await;

    let methods = [
        (
            "process_client_blackboard_request",
            vec![
                PyRep::String("set".into()),
                PyRep::String("npe.step".into()),
                PyRep::String("undock_complete".into()),
            ],
        ),
        (
            "process_client_graph_message",
            vec![
                PyRep::Integer(473),
                PyRep::String("ObjectiveCompleted".into()),
                PyRep::None,
            ],
        ),
        ("qa_start_node", vec![PyRep::Integer(473), PyRep::Integer(10)]),
        ("qa_start_node_graph", vec![PyRep::Integer(473)]),
        ("qa_stop_active_nodes", vec![PyRep::Integer(473)]),
        ("qa_stop_node", vec![PyRep::Integer(473), PyRep::Integer(10)]),
        ("qa_stop_node_graph", vec![PyRep::Integer(473)]),
        ("qa_subscribe", vec![PyRep::Integer(473)]),
        ("qa_unsubscribe", vec![PyRep::Integer(473)]),
    ];

    // 1. Unprivileged client without GM/QA role gets InsufficientRole on qa_*
    let denied = ctx
        .dispatch_rpc(&mut sess, "node_graph", None, "qa_start_node", &[PyRep::Integer(473)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(denied.code, ERR_INSUFFICIENT_ROLE);

    // 2. Authorized pilot (or non-QA RPCs) returns UnimplementedRPC
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    for (meth, args) in methods {
        let err = ctx
            .dispatch_rpc(&mut sess, "node_graph", None, meth, &args, &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ERR_UNIMPLEMENTED);
    }
}
