//! Integration test suite for final 20 missing RPC rows.
//!
//! Validates real V24 argument layouts, persistent state mutations,
//! and live-server return shapes for all 20 endpoints.

#![forbid(unsafe_code)]

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;

#[tokio::test]
async fn test_rpc_missing_batch3_part1() {
    // parity: rpc:alert.LogModeChanged, rpc:alert.SendProxyStackTraceAlert
    // DATA GAP: Triaged NA in docs/parity/triage.json (dev/QA/internal RPCs): rpc:localizationServer.UpdateLocalizationQAWrap, rpc:localizationServer.ReloadFSDPickle
    // parity: rpc:clientStatLogger.LogString, rpc:externalQueueMgr.PublishProxyEvent
    // parity: rpc:machoNet.ReloadClientCodeHash, rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID
    // DATA GAP: Triaged NA in docs/parity/triage.json (cluster load test RPCs): rpc:loadService.StartLoad, rpc:loadService.StopLoad
    eve_testkit::attest(&[
        "rpc:alert.LogModeChanged",
        "rpc:alert.SendProxyStackTraceAlert",
        "rpc:clientStatLogger.LogString",
        "rpc:externalQueueMgr.PublishProxyEvent",
        "rpc:machoNet.ReloadClientCodeHash",
        "rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    sess1.role |= eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    // 1. rpc:alert.LogModeChanged
    let log_mode_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "alert",
            None,
            "LogModeChanged",
            &[PyRep::Integer(201), PyRep::String("I".into())],
            &kw,
            None,
        )
        .await
        .expect("alert.LogModeChanged");
    assert_eq!(log_mode_res, PyRep::None);

    let get_mode_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "alert",
            None,
            "GetLogModeForError",
            &[PyRep::Integer(201)],
            &kw,
            None,
        )
        .await
        .expect("alert.GetLogModeForError");
    match get_mode_res {
        PyRep::Tuple(items) => {
            assert_eq!(items[0], PyRep::Integer(201));
            assert_eq!(items[1], PyRep::ascii_bytes("I"));
        }
        other => panic!("Unexpected GetLogModeForError shape: {other:?}"),
    }
    let persisted_modes = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("alert_log_modes").cloned()
    });
    assert!(persisted_modes.is_some(), "alert_log_modes must be persisted");
    assert!(persisted_modes.unwrap().contains("201"));

    // 2. rpc:alert.SendProxyStackTraceAlert
    let proxy_alert_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "alert",
            None,
            "SendProxyStackTraceAlert",
            &[
                PyRep::Tuple(vec![PyRep::Integer(777_888), PyRep::String("proxy_key".into())]),
                PyRep::String("Traceback:\n  File foo.py, in bar\nError".into()),
                PyRep::String("B".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("alert.SendProxyStackTraceAlert");
    match proxy_alert_res {
        PyRep::Tuple(items) => {
            assert!(items[0].as_i64().unwrap_or(0) > 0);
            assert_eq!(items[1], PyRep::ascii_bytes("B"));
        }
        other => panic!("Unexpected SendProxyStackTraceAlert shape: {other:?}"),
    }
    let proxy_traces = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("alert_proxy_traces").cloned()
    });
    assert!(proxy_traces.is_some(), "alert_proxy_traces must be persisted");
    assert!(proxy_traces.unwrap().contains("777888"));

    // 3. rpc:localizationServer.UpdateLocalizationQAWrap (fail-closed UnimplementedRPC)
    let mut overrides = Vec::new();
    overrides.push((PyRep::Integer(99991), PyRep::String("Overridden QA String".into())));
    let qa_wrap_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "localizationServer",
            None,
            "UpdateLocalizationQAWrap",
            &[PyRep::Dict(overrides)],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(qa_wrap_err.code, "UnimplementedRPC");

    // 4. rpc:localizationServer.ReloadFSDPickle
    let reload_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "localizationServer",
            None,
            "ReloadFSDPickle",
            &[PyRep::String("fsd_english.pickle".into())],
            &kw,
            None,
        )
        .await
        .expect("localizationServer.ReloadFSDPickle");
    assert_eq!(reload_res, PyRep::Bool(true));
    let fsd_reload_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("localization:fsd_pickle_reloads").cloned()
    });
    assert!(fsd_reload_blob.is_some());
    assert!(fsd_reload_blob.unwrap().contains("fsd_english.pickle"));

    // 5. rpc:clientStatLogger.LogString
    let log_str_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "clientStatLogger",
            None,
            "LogString",
            &[PyRep::String("jump_monitor:jump_done:30000142".into())],
            &kw,
            None,
        )
        .await
        .expect("clientStatLogger.LogString");
    assert_eq!(log_str_res, PyRep::None);
    let logged_strings = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("client_stat_logger:strings").cloned()
    });
    assert!(logged_strings.is_some());
    assert!(logged_strings.unwrap().contains("jump_monitor:jump_done:30000142"));

    // 6. rpc:externalQueueMgr.PublishProxyEvent (7 args)
    let queue_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "externalQueueMgr",
            None,
            "PublishProxyEvent",
            &[
                PyRep::String("proxy_session_init".into()),
                PyRep::String("session_token_123".into()),
                PyRep::Integer(1),
                PyRep::Integer(2),
                PyRep::None,
                PyRep::None,
                PyRep::Dict(vec![]),
            ],
            &kw,
            None,
        )
        .await
        .expect("externalQueueMgr.PublishProxyEvent");
    assert_eq!(queue_res, PyRep::None);
    let queue_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("external_queue_events").cloned()
    });
    assert!(queue_blob.is_some());
    assert!(queue_blob.unwrap().contains("proxy_session_init"));

    // 7. rpc:machoNet.ReloadClientCodeHash (0 args)
    let hash_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "machoNet",
            None,
            "ReloadClientCodeHash",
            &[],
            &kw,
            None,
        )
        .await
        .expect("machoNet.ReloadClientCodeHash");
    match hash_res {
        PyRep::String(s) => assert!(!s.is_empty()),
        other => panic!("Unexpected ReloadClientCodeHash shape: {other:?}"),
    }
    let hash_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("macho_client_code_hash").cloned()
    });
    assert!(hash_blob.is_some());

    // 8. rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID (0 args -> (default_addr, node_map))
    ctx.db.with_mut_snapshot(|s| {
        let mut table = HashMap::new();
        table.insert("42".to_string(), "10.0.0.42:28042".to_string());
        s.service_state_blobs.insert("cluster_node_table".into(), serde_json::to_string(&table).unwrap());
    });
    let tunnel_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "tcpRawProxyService",
            None,
            "GetESPTunnelingAddressByNodeID",
            &[],
            &kw,
            None,
        )
        .await
        .expect("tcpRawProxyService.GetESPTunnelingAddressByNodeID");
    match tunnel_res {
        PyRep::Tuple(items) => {
            assert_eq!(items.len(), 2);
            assert!(items[0].as_str().is_some());
            match &items[1] {
                PyRep::Dict(map) => {
                    let entry = map.iter().find(|(k, _)| k.as_i64() == Some(42));
                    assert_eq!(entry.map(|(_, v)| v), Some(&PyRep::String("10.0.0.42:28042".into())));
                }
                other => panic!("Unexpected node map shape: {other:?}"),
            }
        }
        other => panic!("Unexpected GetESPTunnelingAddressByNodeID shape: {other:?}"),
    }

    // 9. rpc:loadService.StartLoad (1 arg: loadSpec)
    let start_load_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "loadService",
            None,
            "StartLoad",
            &[PyRep::String("load_profile_std".into())],
            &kw,
            None,
        )
        .await
        .expect("loadService.StartLoad");
    assert_eq!(start_load_res, PyRep::None);
    let load_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("load_service_state").cloned()
    });
    assert!(load_blob.is_some());
    assert!(load_blob.unwrap().contains("\"active\":true"));

    // 10. rpc:loadService.StopLoad
    let stop_load_res = ctx
        .dispatch_rpc(&mut sess1, "loadService", None, "StopLoad", &[], &kw, None)
        .await
        .expect("loadService.StopLoad");
    assert_eq!(stop_load_res, PyRep::None);
    let load_blob2 = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("load_service_state").cloned()
    });
    assert!(load_blob2.unwrap().contains("\"active\":false"));
}

#[tokio::test]
async fn test_rpc_missing_batch3_part2() {
    // parity: rpc:loadService.SetConfig, rpc:cache.Rowset, rpc:cache.Setting
    // DATA GAP: Triaged NA in docs/parity/triage.json (dev/QA/cluster RPCs): rpc:loadService.GetTotalStats, rpc:sessionMgr.EndAllGameSessions, rpc:sessionMgr.RemoveSessionsFromServer, rpc:userSvc.ProcessEVESessionCreated, rpc:raffleMgr.QA_SeedRaffles, rpc:DB2.CallProc, rpc:debug.Exec
    eve_testkit::attest(&[
        "rpc:loadService.SetConfig",
        "rpc:cache.Rowset",
        "rpc:cache.Setting",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(1001);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    sess1.role |= eve_db::service_roles::ROLE_ADMIN;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT1)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    let mut sess2 = SessionState::new(1002);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(PILOT2)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    // 11. rpc:loadService.SetConfig
    let set_config_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "loadService",
            None,
            "SetConfig",
            &[PyRep::String("concurrency".into()), PyRep::Integer(16)],
            &kw,
            None,
        )
        .await
        .expect("loadService.SetConfig");
    assert_eq!(set_config_res, PyRep::None);

    // 12. rpc:loadService.GetTotalStats
    let stats_res = ctx
        .dispatch_rpc(&mut sess1, "loadService", None, "GetTotalStats", &[], &kw, None)
        .await
        .expect("loadService.GetTotalStats");
    match stats_res {
        PyRep::Dict(entries) => {
            let active = entries.iter().find(|(k, _)| k.as_str() == Some("active"));
            assert!(active.is_some());
            let config = entries.iter().find(|(k, _)| k.as_str() == Some("config"));
            assert!(config.is_some());
        }
        other => panic!("Unexpected GetTotalStats shape: {other:?}"),
    }

    // 13. rpc:sessionMgr.EndAllGameSessions
    let end_sessions_res = ctx
        .dispatch_rpc(&mut sess1, "sessionMgr", None, "EndAllGameSessions", &[], &kw, None)
        .await
        .expect("sessionMgr.EndAllGameSessions");
    assert_eq!(end_sessions_res, PyRep::None);
    let active_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("active_sessions").cloned()
    });
    assert_eq!(active_blob.as_deref(), Some("[]"));

    // 14. rpc:sessionMgr.RemoveSessionsFromServer
    // Verify unprivileged rejection
    let unpriv_rm_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "sessionMgr",
            None,
            "RemoveSessionsFromServer",
            &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1001)])],
            &kw,
            None,
        )
        .await;
    assert!(unpriv_rm_err.is_err(), "Unprivileged session must be rejected for RemoveSessionsFromServer");

    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert("active_sessions".into(), "[1001, 1002, 1003]".into());
    });
    let rm_sessions_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "sessionMgr",
            None,
            "RemoveSessionsFromServer",
            &[
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Integer(1002), PyRep::Integer(1003)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("sessionMgr.RemoveSessionsFromServer");
    assert_eq!(rm_sessions_res, PyRep::None);
    let remaining_sessions = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("active_sessions").cloned().unwrap()
    });
    assert!(remaining_sessions.contains("1001"));
    assert!(!remaining_sessions.contains("1002"));

    // 15. rpc:userSvc.ProcessEVESessionCreated
    let process_sess_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "userSvc",
            None,
            "ProcessEVESessionCreated",
            &[PyRep::Integer(PILOT1), PyRep::Integer(1001)],
            &kw,
            None,
        )
        .await
        .expect("userSvc.ProcessEVESessionCreated");
    assert_eq!(process_sess_res, PyRep::None);
    let user_sess_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("user_sessions").cloned().unwrap()
    });
    assert!(user_sess_blob.contains(&PILOT1.to_string()));

    // 16. rpc:cache.Rowset
    let cache_rowset_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "cache",
            None,
            "Rowset",
            &[PyRep::String("roles".into())],
            &kw,
            None,
        )
        .await
        .expect("cache.Rowset");
    match cache_rowset_res {
        PyRep::Rowset { columns, rows } => {
            assert!(columns.contains(&"roleID".to_string()));
            assert!(columns.contains(&"roleName".to_string()));
            assert!(!rows.is_empty());
        }
        other => panic!("Unexpected cache.Rowset shape: {other:?}"),
    }

    // 17. rpc:cache.Setting
    let cache_setting_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "cache",
            None,
            "Setting",
            &[PyRep::String("zsystem".into()), PyRep::String("SP-Allowed-Extensions".into())],
            &kw,
            None,
        )
        .await
        .expect("cache.Setting");
    match cache_setting_res {
        PyRep::String(exts) => {
            assert!(exts.contains("txt"));
            assert!(exts.contains("csv"));
        }
        other => panic!("Unexpected cache.Setting shape: {other:?}"),
    }
    let custom_set_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "cache",
            None,
            "Setting",
            &[
                PyRep::String("zsystem".into()),
                PyRep::String("CustomTestKey".into()),
                PyRep::String("CustomTestVal".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("cache.Setting update");
    assert_eq!(custom_set_res, PyRep::String("CustomTestVal".into()));

    // 18. rpc:raffleMgr.QA_SeedRaffles
    // Verify unprivileged rejection
    let unpriv_seed_err = ctx
        .dispatch_rpc(&mut sess2, "raffleMgr", None, "QA_SeedRaffles", &[PyRep::Integer(1)], &kw, None)
        .await;
    assert!(unpriv_seed_err.is_err(), "Unprivileged session must be rejected for QA_SeedRaffles");

    let raffle_seed_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "raffleMgr",
            None,
            "QA_SeedRaffles",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("raffleMgr.QA_SeedRaffles");
    match raffle_seed_res {
        PyRep::List(items) => {
            assert_eq!(items.len(), 2);
        }
        other => panic!("Unexpected QA_SeedRaffles shape: {other:?}"),
    }
    let (seeded_notes, item_is_neg) = ctx.db.with_snapshot(|s| {
        let count = s.owner_notes.values().filter(|n| n.label.starts_with("raffle:")).count();
        let neg = s.items.keys().any(|&k| k < 0);
        (count, neg)
    });
    assert!(seeded_notes >= 2);
    assert!(item_is_neg, "Seeded raffle item must use negative item ID in snap.items");

    // 19. rpc:DB2.CallProc (2 args: proc_name, role_mask)
    let mut unpriv = SessionState::new(9999);
    unpriv.user_id = Some(8888);
    unpriv.role = eve_db::service_roles::ROLE_PLAYER_DEFAULT;
    let auth_err = ctx
        .dispatch_rpc(&mut unpriv, "DB2", None, "CallProc", &[PyRep::String("zcluster.Nodes_PolarisID".into())], &kw, None)
        .await;
    assert!(auth_err.is_err(), "Non-admin must be rejected for DB2.CallProc");

    let polaris_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "DB2",
            None,
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
            &kw,
            None,
        )
        .await
        .expect("DB2.CallProc polaris");
    assert_eq!(polaris_res, PyRep::Integer(1));

    let users_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "DB2",
            None,
            "CallProc",
            &[
                PyRep::String("zuser.Users_SelectByRole".into()),
                PyRep::Integer(eve_db::service_roles::ROLE_PLAYER_DEFAULT as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("DB2.CallProc select users");
    match users_res {
        PyRep::Rowset { columns, rows } => {
            assert!(columns.contains(&"userID".to_string()));
            assert!(columns.contains(&"userName".to_string()));
            assert!(!rows.is_empty());
        }
        other => panic!("Unexpected Users_SelectByRole shape: {other:?}"),
    }

    // 20. rpc:debug.Exec (5 args + kwargs)
    let debug_auth_err = ctx
        .dispatch_rpc(&mut unpriv, "debug", None, "Exec", &[PyRep::String("print('hello')".into())], &kw, None)
        .await;
    assert!(debug_auth_err.is_err(), "Non-admin/programmer must be rejected for debug.Exec");

    let debug_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "debug",
            None,
            "Exec",
            &[
                PyRep::String("self.stats()".into()),
                PyRep::Integer(1),
                PyRep::None,
                PyRep::String("extra".into()),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("debug.Exec");
    match debug_res {
        PyRep::List(items) => {
            assert!(!items.is_empty());
            match &items[0] {
                PyRep::Tuple(parts) => {
                    assert_eq!(parts[1], PyRep::String("ok".into()));
                    assert!(parts[2].as_str().unwrap().contains("self.stats()"));
                }
                other => panic!("Unexpected debug.Exec item shape: {other:?}"),
            }
        }
        other => panic!("Unexpected debug.Exec shape: {other:?}"),
    }
    let debug_log = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("debug_exec_log").cloned().unwrap()
    });
    assert!(debug_log.contains("self.stats()"));
}
