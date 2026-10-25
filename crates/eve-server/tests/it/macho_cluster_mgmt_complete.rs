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

    let ctx = ServerContext::new();
    reset_cluster_state(&ctx);

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

    // 5. machoNet.GetMetrics: auth & GM role checks, 3-tuple shape, KeyVal metrics object
    let unauth_err = ctx
        .dispatch_rpc(&mut unauth_sess, "machoNet", None, "GetMetrics", &[], &kw, None)
        .await
        .expect_err("unauthenticated GetMetrics must fail");
    assert_eq!(unauth_err.code, "Unauthorized");

    let non_gm_err = ctx
        .dispatch_rpc(&mut regular_sess, "machoNet", None, "GetMetrics", &[], &kw, None)
        .await
        .expect_err("non-GM GetMetrics must fail");
    assert_eq!(non_gm_err.code, "InsufficientRole");

    let res = ctx
        .dispatch_rpc(&mut gm_sess, "machoNet", None, "GetMetrics", &[], &kw, None)
        .await
        .expect("GM GetMetrics should succeed");
    match &res {
        PyRep::List(items) => {
            assert_eq!(items.len(), 1);
            match &items[0] {
                PyRep::Tuple(tuple_items) => {
                    assert_eq!(tuple_items.len(), 3);
                    assert_eq!(tuple_items[0], PyRep::Integer(1));
                    assert_eq!(tuple_items[1], PyRep::String("machoNet".into()));
                    match &tuple_items[2] {
                        PyRep::KeyVal(metrics_kv) => {
                            assert!(metrics_kv.contains_key("CallsReceived"));
                            assert!(metrics_kv.contains_key("CallsSent"));
                            assert!(metrics_kv.contains_key("NotificationsReceived"));
                            assert!(metrics_kv.contains_key("NotificationsSent"));
                            assert!(matches!(metrics_kv.get("CallsReceived"), Some(PyRep::Dict(_))));
                            assert!(matches!(metrics_kv.get("CallsSent"), Some(PyRep::Dict(_))));
                            assert!(matches!(metrics_kv.get("NotificationsReceived"), Some(PyRep::Dict(_))));
                            assert!(matches!(metrics_kv.get("NotificationsSent"), Some(PyRep::Dict(_))));
                        }
                        other => panic!("expected KeyVal for [][2], got {other:?}"),
                    }
                }
                other => panic!("expected 3-tuple, got {other:?}"),
            }
        }
        other => panic!("expected List of tuples, got {other:?}"),
    }

    // 6. machoNet.GetNodeFromAddress: address resolution and positive node ID guarantee (> 0)
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

    // V24 callers pass 0 as fallback; must resolve address to sol node ID (> 0), never echoing 0
    let res_v24 = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::String("127.0.0.1:26000".into()), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress with 0 fallback should succeed");
    assert_eq!(res_v24, PyRep::Integer(1));

    // Secondary node address resolves to sol node ID 2
    let res_node2 = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::String("127.0.0.1:26001".into()), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress node 2 should succeed");
    assert_eq!(res_node2, PyRep::Integer(2));

    // Unknown address with fallback 0 defaults to positive node ID 1 (> 0)
    let res_unknown = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::String("unknown:9999".into()), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress unknown with 0 should succeed");
    assert_eq!(res_unknown, PyRep::Integer(1));

    // Unknown address with valid positive fallback returns the positive fallback
    let res_fallback = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::String("unknown:9999".into()), PyRep::Integer(42)],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress with positive fallback should succeed");
    assert_eq!(res_fallback, PyRep::Integer(42));

    // Address provided as tuple (host, port)
    let res_tuple = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "GetNodeFromAddress",
            &[PyRep::Tuple(vec![PyRep::String("127.0.0.1".into()), PyRep::Integer(26000)])],
            &kw,
            None,
        )
        .await
        .expect("GetNodeFromAddress tuple should succeed");
    assert_eq!(res_tuple, PyRep::Integer(1));

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

    // 8. machoNet.CheckACL: IP address check, truthy rejection string on denial, None on allowed
    let unauth_err = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("127.0.0.1".into())],
            &kw,
            None,
        )
        .await
        .expect_err("unauthenticated CheckACL must fail");
    assert_eq!(unauth_err.code, "Unauthorized");

    // Regular authenticated session on valid IP succeeds with None (falsy -> allowed)
    let reg_acl = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("127.0.0.1".into())],
            &kw,
            None,
        )
        .await
        .expect("regular CheckACL on allowed IP should succeed");
    assert_eq!(reg_acl, PyRep::None);

    // Non-GM session with espCheck=true returns truthy rejection reason string
    let mut acl_kw = HashMap::new();
    acl_kw.insert("espCheck".to_string(), PyRep::Bool(true));
    let non_gm_acl = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("127.0.0.1".into())],
            &acl_kw,
            None,
        )
        .await
        .expect("non-GM CheckACL with espCheck returns truthy denial string");
    assert_eq!(non_gm_acl, PyRep::String("GM role required for ESP access".into()));

    // Invalid IP address returns truthy rejection reason string
    let invalid_ip_acl = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("not-an-ip".into())],
            &kw,
            None,
        )
        .await
        .expect("invalid IP CheckACL returns truthy denial string");
    assert_eq!(invalid_ip_acl, PyRep::String("Invalid IP address format".into()));

    // GM session with espCheck=true succeeds with None (allowed)
    let gm_acl = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "machoNet",
            None,
            "CheckACL",
            &[PyRep::String("127.0.0.1".into())],
            &acl_kw,
            None,
        )
        .await
        .expect("GM CheckACL with espCheck should succeed");
    assert_eq!(gm_acl, PyRep::None);

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

    // 10. machoNet.GracefulShutdown: GM authorization check and lifecycle wiring
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

    assert!(ctx.is_shutting_down());
    let state = get_cluster_state(&ctx);
    assert!(state.shutdown_in_progress);
    assert_eq!(state.origin_node_id, Some(10));
    assert_eq!(state.shutdown_reason.as_deref(), Some("Scheduled update"));
    assert_eq!(state.shutdown_timeout_seconds, Some(300));
    assert_eq!(state.shutdown_graceful, Some(true));

    // Verify non-GM login is rejected when shutdown is in progress
    let login_err = ctx
        .dispatch_rpc(
            &mut regular_sess,
            "authentication",
            None,
            "Login",
            &[PyRep::String("testuser".into()), PyRep::String("testpass".into())],
            &kw,
            None,
        )
        .await
        .expect_err("non-GM login during shutdown must fail");
    assert!(login_err.code == "ClusterShuttingDown" || login_err.code == "AuthenticationFailed");

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
    assert!(ctx.is_shutting_down());
    assert!(get_cluster_state(&ctx).shutdown_in_progress);

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
    assert!(get_cluster_state(&ctx).shutdown_complete_nodes.contains(&42));

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
        get_cluster_state(&ctx).shutdown_complete_nodes.iter().filter(|&&id| id == 42).count(),
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

    let final_state = get_cluster_state(&ctx);
    assert!(final_state.clients_terminated);
    assert_eq!(final_state.termination_reason.as_deref(), Some("Maintenance reboot"));

    // Verify victim session received termination signal and was removed from active closers
    let term_msg = rx.try_recv().expect("victim closer must receive termination signal");
    assert_eq!(term_msg.0, "ClusterShutdown");
    assert_eq!(term_msg.1, "Maintenance reboot");
    assert!(!ctx.session_closers.read().unwrap().contains_key(&victim_sess_id));
}
