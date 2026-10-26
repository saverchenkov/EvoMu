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
// parity: rpc:expertSystemMgr.GetMyExpertSystems
// parity: rpc:expertSystemMgr.ConsumeExpertSystem
// parity: rpc:expertSystemMgr.RemoveMyExpertSystem
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
    let initial_es = ctx
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
    assert_eq!(initial_es, PyRep::Dict(vec![]));

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
    assert_eq!(consume_rep, PyRep::None);

    let active_es = ctx
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
        .expect("GetMyExpertSystems active");
    let PyRep::Dict(ref es_dict) = active_es else {
        panic!("Expected Dict from GetMyExpertSystems");
    };
    assert_eq!(es_dict.len(), 1);
    assert_eq!(es_dict[0].0, PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64));
    let es_kv = &es_dict[0].1;
    assert_eq!(es_kv.dict_get("typeID"), Some(&PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)));
    assert!(es_kv.dict_get("expiresAt").is_some());

    let remove_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "expertSystemMgr",
            None,
            "RemoveMyExpertSystem",
            &[PyRep::Integer(EXPERT_SYSTEM_TYPE_ID as i64)],
            &kw,
            None,
        )
        .await
        .expect("RemoveMyExpertSystem");
    assert_eq!(remove_rep, PyRep::None);

    let after_removal = ctx
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
        .expect("GetMyExpertSystems after removal");
    assert_eq!(after_removal, PyRep::Dict(vec![]));
}

#[tokio::test]
// parity: rpc:node_graph.process_client_blackboard_request
// parity: rpc:node_graph.process_client_graph_message
async fn test_node_graph_blackboard_and_qa_lifecycle() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut sess = setup_pilot(&ctx, 9_302, "demo", "demo-secret", PILOT1).await;

    // 1. node_graph.process_client_blackboard_request set and get
    let bb_set = ctx
        .dispatch_rpc(
            &mut sess,
            "node_graph",
            None,
            "process_client_blackboard_request",
            &[
                PyRep::String("set".into()),
                PyRep::String("npe.step".into()),
                PyRep::String("undock_complete".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("process_client_blackboard_request set");
    assert_eq!(bb_set, PyRep::String("undock_complete".into()));

    let bb_get = ctx
        .dispatch_rpc(
            &mut sess,
            "node_graph",
            None,
            "process_client_blackboard_request",
            &[
                PyRep::String("get".into()),
                PyRep::String("npe.step".into()),
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect("process_client_blackboard_request get");
    assert_eq!(bb_get, PyRep::String("undock_complete".into()));

    // 2. node_graph.process_client_graph_message
    let gm_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "node_graph",
            None,
            "process_client_graph_message",
            &[
                PyRep::Integer(473),
                PyRep::String("ObjectiveCompleted".into()),
                PyRep::None,
            ],
            &kw,
            None,
        )
        .await
        .expect("process_client_graph_message");
    assert_eq!(gm_rep, PyRep::None);

    let qa_methods = [
        ("qa_start_node", vec![PyRep::Integer(473), PyRep::Integer(10)]),
        ("qa_start_node_graph", vec![PyRep::Integer(473)]),
        ("qa_stop_active_nodes", vec![PyRep::Integer(473)]),
        ("qa_stop_node", vec![PyRep::Integer(473), PyRep::Integer(10)]),
        ("qa_stop_node_graph", vec![PyRep::Integer(473)]),
        ("qa_subscribe", vec![PyRep::Integer(473)]),
        ("qa_unsubscribe", vec![PyRep::Integer(473)]),
    ];

    // 3. Unprivileged client without GM/QA role gets InsufficientRole on qa_*
    let denied = ctx
        .dispatch_rpc(&mut sess, "node_graph", None, "qa_start_node", &[PyRep::Integer(473)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(denied.code, ERR_INSUFFICIENT_ROLE);

    // 4. Authorized GM pilot returns UnimplementedRPC on unimplemented QA methods
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    for (meth, args) in qa_methods {
        let err = ctx
            .dispatch_rpc(&mut sess, "node_graph", None, meth, &args, &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, ERR_UNIMPLEMENTED);
    }
}
