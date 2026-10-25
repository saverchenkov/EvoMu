//! Integration tests for the 13 `machoNet` cluster management and operational RPCs.

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::macho_cluster_mgmt::{get_cluster_state, reset_cluster_state};
use std::collections::HashMap;

fn get_dict_entry<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match dict {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| {
            if k.as_str() == Some(key) {
                Some(v)
            } else {
                None
            }
        }),
        _ => None,
    }
}

#[tokio::test]
async fn test_macho_net_cluster_mgmt_rpcs() {
    // parity: rpc:machoNet.GetConnectedClusterMode, rpc:machoNet.GetConnectedClusterName, rpc:machoNet.GetConnectionProperties, rpc:machoNet.GetGlobalConfig, rpc:machoNet.GetMetrics, rpc:machoNet.GetNodeFromAddress, rpc:machoNet.GetPolarisExternalTunnelingAddress, rpc:machoNet.CheckACL, rpc:machoNet.SetConnectionProperty, rpc:machoNet.GracefulShutdown, rpc:machoNet.GracefulShutdownStarted, rpc:machoNet.GracefulShutdownComplete, rpc:machoNet.TerminateAllClients
    eve_testkit::attest(&[
        "rpc:machoNet.GetConnectedClusterMode",
        "rpc:machoNet.GetConnectedClusterName",
        "rpc:machoNet.GetConnectionProperties",
        "rpc:machoNet.GetGlobalConfig",
        "rpc:machoNet.GetMetrics",
        "rpc:machoNet.GetNodeFromAddress",
        "rpc:machoNet.GetPolarisExternalTunnelingAddress",
        "rpc:machoNet.CheckACL",
        "rpc:machoNet.SetConnectionProperty",
        "rpc:machoNet.GracefulShutdown",
        "rpc:machoNet.GracefulShutdownStarted",
        "rpc:machoNet.GracefulShutdownComplete",
        "rpc:machoNet.TerminateAllClients",
    ]);

    reset_cluster_state();
    let ctx = ServerContext::new();

    let mut regular_sess = SessionState::new(9001);
    regular_sess.user_id = Some(9001);
    regular_sess.role = 0;

    let mut gm_sess = SessionState::new(9002);
    gm_sess.user_id = Some(9002);
    gm_sess.role = eve_machonet::service_roles::ROLE_GMS;

    let mut unauth_sess = SessionState::new(9003);
    unauth_sess.user_id = None;
    unauth_sess.role = 0;

    let kw = HashMap::new();

    // 1. machoNet.GetConnectedClusterMode
    let res = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetConnectedClusterMode", &[], &kw, None)
        .await
        .expect("GetConnectedClusterMode should succeed");
    assert_eq!(res, PyRep::String("LIVE".into()));

    // 2. machoNet.GetConnectedClusterName
    let res = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetConnectedClusterName", &[], &kw, None)
        .await
        .expect("GetConnectedClusterName should succeed");
    assert_eq!(res, PyRep::String("TRANQUILITY".into()));

    // 3. machoNet.GetConnectionProperties
    let res = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetConnectionProperties", &[], &kw, None)
        .await
        .expect("GetConnectionProperties should succeed");
    assert_eq!(
        get_dict_entry(&res, "internaladdress"),
        Some(&PyRep::String("127.0.0.1:26000".into()))
    );
    assert_eq!(get_dict_entry(&res, "vipMode"), Some(&PyRep::Bool(false)));

    // 4. machoNet.GetGlobalConfig
    let res = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetGlobalConfig", &[], &kw, None)
        .await
        .expect("GetGlobalConfig should succeed");
    assert_eq!(get_dict_entry(&res, "machoVersion"), Some(&PyRep::Integer(496)));
    assert_eq!(get_dict_entry(&res, "build"), Some(&PyRep::Integer(3_624_425)));
    assert_eq!(
        get_dict_entry(&res, "clusterName"),
        Some(&PyRep::String("TRANQUILITY".into()))
    );

    // 5. machoNet.GetMetrics
    let res = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetMetrics", &[], &kw, None)
        .await
        .expect("GetMetrics should succeed");
    match &res {
        PyRep::List(items) => {
            assert_eq!(items.len(), 1);
            match &items[0] {
                PyRep::Tuple(tuple_items) => {
                    assert_eq!(tuple_items.len(), 3);
                    assert_eq!(tuple_items[0], PyRep::Integer(1));
                    assert_eq!(tuple_items[1], PyRep::String("machoNet".into()));
                    let metrics_dict = &tuple_items[2];
                    assert!(get_dict_entry(metrics_dict, "CallsReceived").is_some());
                    assert!(get_dict_entry(metrics_dict, "CallsSent").is_some());
                    assert!(get_dict_entry(metrics_dict, "NotificationsReceived").is_some());
                    assert!(get_dict_entry(metrics_dict, "NotificationsSent").is_some());
                }
                other => panic!("expected 3-tuple, got {other:?}"),
            }
        }
        other => panic!("expected List of tuples, got {other:?}"),
    }

    // 6. machoNet.GetNodeFromAddress
    let res = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::String("127.0.0.1:26000".into())],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress should succeed");
    assert_eq!(res, PyRep::Integer(1));

    let res_2arg = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::String("127.0.0.1:26000".into()), PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress with 2 args should succeed");
    assert_eq!(res_2arg, PyRep::Integer(42));

    // 7. machoNet.GetPolarisExternalTunnelingAddress
    let res = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetPolarisExternalTunnelingAddress",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetPolarisExternalTunnelingAddress should succeed");
    assert_eq!(res, PyRep::String("127.0.0.1".into()));

    // 8. machoNet.CheckACL
    // Unauthenticated session is rejected
    let unauth_err = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("proxy".into())],
            &kw,
            None,
        )
        .await
        .expect_err("unauthenticated CheckACL must fail");
    assert_eq!(unauth_err.code, "InsufficientRole");

    // Regular authenticated session on standard service succeeds
    let reg_acl = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("proxy".into())],
            &kw,
            None,
        )
        .await
        .expect("regular CheckACL on standard service should succeed");
    assert_eq!(reg_acl, PyRep::Bool(true));

    // Non-GM session with espCheck=true fails
    let mut acl_kw = HashMap::new();
    acl_kw.insert("espCheck".to_string(), PyRep::Bool(true));
    let non_gm_acl_err = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("proxy".into())],
            &acl_kw,
            None,
        )
        .await
        .expect_err("non-GM CheckACL with espCheck must fail");
    assert_eq!(non_gm_acl_err.code, "InsufficientRole");

    // GM session with espCheck=true succeeds
    let gm_acl = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("proxy".into())],
            &acl_kw,
            None,
        )
        .await
        .expect("GM CheckACL with espCheck should succeed");
    assert_eq!(gm_acl, PyRep::Bool(true));

    // 9. machoNet.SetConnectionProperty: GM authorization check and key allowlist
    let non_gm_err = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "SetConnectionProperty",
            &[PyRep::String("vipMode".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM SetConnectionProperty must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let invalid_key_err = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "SetConnectionProperty",
            &[PyRep::String("disallowedKey".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("SetConnectionProperty with unlisted key must fail");
    assert_eq!(invalid_key_err.code, "InvalidArgument");

    let gm_res = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "SetConnectionProperty",
            &[PyRep::String("vipMode".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("GM SetConnectionProperty should succeed");
    assert_eq!(gm_res, PyRep::None);

    let props_after = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetConnectionProperties", &[], &kw, None)
        .await
        .expect("GetConnectionProperties after update");
    assert_eq!(get_dict_entry(&props_after, "vipMode"), Some(&PyRep::Bool(true)));

    // 10. machoNet.GracefulShutdown: GM authorization check
    let non_gm_err = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GracefulShutdown",
            &[
                PyRep::Integer(1),
                PyRep::String("Scheduled update".into()),
                PyRep::Integer(300),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM GracefulShutdown must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let gm_res = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "GracefulShutdown",
            &[
                PyRep::Integer(10),
                PyRep::String("Scheduled update".into()),
                PyRep::Integer(300),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("GM GracefulShutdown should succeed");
    assert_eq!(gm_res, PyRep::None);

    let state = get_cluster_state();
    assert!(state.shutdown_in_progress);
    assert_eq!(state.origin_node_id, Some(10));
    assert_eq!(state.shutdown_reason.as_deref(), Some("Scheduled update"));
    assert_eq!(state.shutdown_timeout_seconds, Some(300));
    assert_eq!(state.shutdown_graceful, Some(true));

    // 11. machoNet.GracefulShutdownStarted: GM authorization check
    let non_gm_err = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GracefulShutdownStarted", &[], &kw, None)
        .await
        .expect_err("non-GM GracefulShutdownStarted must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let gm_res = ctx
        .dispatch_rpc(&mut gm_sess, "machoNet", None, "GracefulShutdownStarted", &[], &kw, None)
        .await
        .expect("GM GracefulShutdownStarted should succeed");
    assert_eq!(gm_res, PyRep::None);
    assert!(get_cluster_state().shutdown_in_progress);

    // 12. machoNet.GracefulShutdownComplete: GM authorization check and deduplication
    let non_gm_err = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GracefulShutdownComplete",
            &[PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM GracefulShutdownComplete must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let gm_res = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "GracefulShutdownComplete",
            &[PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect("GM GracefulShutdownComplete should succeed");
    assert_eq!(gm_res, PyRep::None);
    assert!(get_cluster_state().shutdown_complete_nodes.contains(&42));

    // Deduplication check: repeated node ID is not added again
    let gm_res2 = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "GracefulShutdownComplete",
            &[PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect("GM GracefulShutdownComplete duplicate should succeed");
    assert_eq!(gm_res2, PyRep::None);
    assert_eq!(
        get_cluster_state().shutdown_complete_nodes.iter().filter(|&&id| id == 42).count(),
        1
    );

    // 13. machoNet.TerminateAllClients: GM authorization check and active session termination
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let victim_sess_id = 7777;
    ctx.register_session_closer(victim_sess_id, tx);
    assert!(ctx.session_closers.read().unwrap().contains_key(&victim_sess_id));

    let non_gm_err = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "TerminateAllClients",
            &[PyRep::String("Maintenance reboot".into())],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM TerminateAllClients must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let gm_res = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "TerminateAllClients",
            &[PyRep::String("Maintenance reboot".into())],
            &kw,
            None,
        )
        .await
        .expect("GM TerminateAllClients should succeed");
    assert_eq!(gm_res, PyRep::None);

    let final_state = get_cluster_state();
    assert!(final_state.clients_terminated);
    assert_eq!(final_state.termination_reason.as_deref(), Some("Maintenance reboot"));

    // Verify victim session received termination signal and was removed from active closers
    let term_msg = rx.try_recv().expect("victim closer must receive termination signal");
    assert_eq!(term_msg.0, "ClusterShutdown");
    assert_eq!(term_msg.1, "Maintenance reboot");
    assert!(!ctx.session_closers.read().unwrap().contains_key(&victim_sess_id));
}
