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
    // parity: rpc:localizationServer.UpdateLocalizationQAWrap, rpc:localizationServer.ReloadFSDPickle
    // parity: rpc:clientStatLogger.LogString, rpc:externalQueueMgr.PublishProxyEvent
    // parity: rpc:machoNet.ReloadClientCodeHash, rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID
    // parity: rpc:loadService.StartLoad, rpc:loadService.StopLoad
    eve_testkit::attest(&[
        "rpc:alert.LogModeChanged",
        "rpc:alert.SendProxyStackTraceAlert",
        "rpc:localizationServer.UpdateLocalizationQAWrap",
        "rpc:localizationServer.ReloadFSDPickle",
        "rpc:clientStatLogger.LogString",
        "rpc:externalQueueMgr.PublishProxyEvent",
        "rpc:machoNet.ReloadClientCodeHash",
        "rpc:tcpRawProxyService.GetESPTunnelingAddressByNodeID",
        "rpc:loadService.StartLoad",
        "rpc:loadService.StopLoad",
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

    // 3. rpc:localizationServer.UpdateLocalizationQAWrap (role-gated boolean toggle + GetAllTextChanges wrapping)
    let mut unpriv_loc_sess = SessionState::new(902_099);
    common::login_pilot(&ctx, &mut unpriv_loc_sess, "other", "other-secret").await;
    unpriv_loc_sess.role = 0;
    let unpriv_qa_err = ctx
        .dispatch_rpc(
            &mut unpriv_loc_sess,
            "localizationServer",
            None,
            "UpdateLocalizationQAWrap",
            &[PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("unprivileged UpdateLocalizationQAWrap must fail with RoleRequired");
    assert_eq!(unpriv_qa_err.code, "RoleRequired");

    ctx.db.with_mut_snapshot(|snap| {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(99991_i64, "Overridden QA String".to_string());
        snap.owner_notes.insert(
            -8_960_000_000_000,
            eve_db::OwnerNoteRecord {
                note_id: -8_960_000_000_000,
                character_id: 0,
                item_id: 0,
                label: "localization_overrides".into(),
                note: serde_json::to_string(&overrides).unwrap(),
                timestamp_unix: 0,
            },
        );
    });

    // Enable QA wrap for sess1 and verify GetAllTextChanges wraps localized strings
    assert_eq!(
        ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Bool(true)], &kw, None).await.expect("UpdateLocalizationQAWrap(true)"),
        PyRep::Bool(true)
    );
    let wrapped_changes = ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "GetAllTextChanges", &[], &kw, None).await.expect("GetAllTextChanges wrapped");
    assert_eq!(
        match &wrapped_changes {
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_i64() == Some(99991)).map(|(_, v)| v),
            _ => None,
        },
        Some(&PyRep::String("[QA:99991] Overridden QA String".into()))
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "UpdateLocalizationQAWrap", &[PyRep::Bool(false)], &kw, None).await.expect("UpdateLocalizationQAWrap(false)"),
        PyRep::Bool(false)
    );
    let unwrapped_changes = ctx.dispatch_rpc(&mut sess1, "localizationServer", None, "GetAllTextChanges", &[], &kw, None).await.expect("GetAllTextChanges unwrapped");
    assert_eq!(
        match &unwrapped_changes {
            PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_i64() == Some(99991)).map(|(_, v)| v),
            _ => None,
        },
        Some(&PyRep::String("Overridden QA String".into()))
    );

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
    let target_sys = sess1.solar_system_id.unwrap_or(30_000_142);
    let elapsed_before = ctx.systems.get_system_sim_elapsed(target_sys);
    let start_load_res = ctx
        .dispatch_rpc(
            &mut sess1,
            "loadService",
            None,
            "StartLoad",
            &[PyRep::Dict(vec![
                (PyRep::String("profile".into()), PyRep::String("load_profile_std".into())),
                (PyRep::String("ticks".into()), PyRep::Integer(4)),
            ])],
            &kw,
            None,
        )
        .await
        .expect("loadService.StartLoad");
    assert_eq!(start_load_res, PyRep::None);
    let elapsed_after = ctx.systems.get_system_sim_elapsed(target_sys);
    assert!(
        elapsed_after >= elapsed_before + 4.0,
        "StartLoad must step target system (before={elapsed_before}, after={elapsed_after})"
    );
    let load_blob = ctx.db.with_snapshot(|s| {
        s.service_state_blobs.get("load_service_state").cloned()
    });
    assert!(load_blob.is_some());
    let load_blob_str = load_blob.unwrap();
    assert!(load_blob_str.contains("\"active\":true"));
    assert!(load_blob_str.contains("\"ticks_executed\":4"));

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
    // parity: rpc:loadService.GetTotalStats, rpc:sessionMgr.EndAllGameSessions, rpc:sessionMgr.RemoveSessionsFromServer
    // parity: rpc:userSvc.ProcessEVESessionCreated, rpc:raffleMgr.QA_SeedRaffles, rpc:DB2.CallProc
    // DATA GAP: Triaged NA in docs/parity/triage.json (dev/QA/cluster RPCs): rpc:debug.Exec
    eve_testkit::attest(&[
        "rpc:loadService.SetConfig",
        "rpc:cache.Rowset",
        "rpc:cache.Setting",
        "rpc:loadService.GetTotalStats",
        "rpc:sessionMgr.EndAllGameSessions",
        "rpc:sessionMgr.RemoveSessionsFromServer",
        "rpc:userSvc.ProcessEVESessionCreated",
        "rpc:raffleMgr.QA_SeedRaffles",
        "rpc:DB2.CallProc",
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
    assert_eq!(stats_res.dict_get("active"), Some(&PyRep::Bool(false)));
    assert!(stats_res.dict_get("sde_types_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(stats_res.dict_get("sde_solar_systems_count").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let cfg = stats_res.dict_get("config").expect("config dict");
    assert_eq!(cfg.dict_get("concurrency"), Some(&PyRep::Integer(16)));

    // 13. rpc:sessionMgr.EndAllGameSessions
    let (tx_victim, mut rx_victim) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(2002, tx_victim);
    assert!(ctx.session_closers.read().unwrap().contains_key(&2002));
    let end_sessions_res = ctx
        .dispatch_rpc(&mut sess1, "sessionMgr", None, "EndAllGameSessions", &[], &kw, None)
        .await
        .expect("sessionMgr.EndAllGameSessions");
    assert_eq!(end_sessions_res, PyRep::None);
    assert_eq!(
        rx_victim.try_recv().expect("victim session must receive EndAllGameSessions"),
        ("EndAllGameSessions".into(), "All game sessions ended by administrator".into())
    );
    assert!(!ctx.session_closers.read().unwrap().contains_key(&2002));
    let active_blob = ctx.db.with_snapshot(|s| s.service_state_blobs.get("active_sessions").cloned());
    assert_eq!(active_blob.as_deref(), Some("[]"));

    // 14. rpc:sessionMgr.RemoveSessionsFromServer
    let unpriv_rm_err = ctx
        .dispatch_rpc(&mut sess2, "sessionMgr", None, "RemoveSessionsFromServer", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1001)])], &kw, None)
        .await;
    assert!(unpriv_rm_err.is_err(), "Unprivileged session must be rejected for RemoveSessionsFromServer");

    let (tx_1002, mut rx_1002) = tokio::sync::mpsc::unbounded_channel();
    let (tx_1003, mut rx_1003) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(1002, tx_1002);
    ctx.register_session_closer(1003, tx_1003);
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert("active_sessions".into(), "[1001, 1002, 1003]".into());
    });
    let rm_sessions_res = ctx
        .dispatch_rpc(&mut sess1, "sessionMgr", None, "RemoveSessionsFromServer", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1002), PyRep::Integer(1003)])], &kw, None)
        .await
        .expect("sessionMgr.RemoveSessionsFromServer");
    assert_eq!(rm_sessions_res, PyRep::None);
    assert_eq!(rx_1002.try_recv().expect("1002 closed").0, "RemoveSession");
    assert_eq!(rx_1003.try_recv().expect("1003 closed").0, "RemoveSession");
    assert!(!ctx.session_closers.read().unwrap().contains_key(&1002));
    assert!(!ctx.session_closers.read().unwrap().contains_key(&1003));
    let remaining_sessions = ctx.db.with_snapshot(|s| s.service_state_blobs.get("active_sessions").cloned().unwrap());
    assert!(remaining_sessions.contains("1001"));
    assert!(!remaining_sessions.contains("1002"));

    // 15. rpc:userSvc.ProcessEVESessionCreated
    let saved_cid = sess2.character_id.take();
    let wrong_char_err = ctx
        .dispatch_rpc(&mut sess2, "userSvc", None, "ProcessEVESessionCreated", &[PyRep::Integer(PILOT1)], &kw, None)
        .await
        .expect_err("foreign character_id must be rejected");
    assert_eq!(wrong_char_err.message, "CharacterDoesNotBelongToUser");
    sess2.character_id = saved_cid;

    let process_sess_res = ctx
        .dispatch_rpc(&mut sess1, "userSvc", None, "ProcessEVESessionCreated", &[PyRep::Integer(PILOT1), PyRep::Integer(1001)], &kw, None)
        .await
        .expect("userSvc.ProcessEVESessionCreated");
    assert_eq!(process_sess_res, PyRep::None);
    let user_sess_blob = ctx.db.with_snapshot(|s| s.service_state_blobs.get("user_sessions").cloned().unwrap());
    let entries: Vec<serde_json::Value> = serde_json::from_str(&user_sess_blob).expect("valid user_sessions json");
    assert!(entries.iter().any(|e| {
        e["user_id"].as_u64() == sess1.user_id
            && e["session_id"].as_u64() == Some(1001)
            && e["character_id"].as_i64() == Some(PILOT1)
    }));

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
    let (seeded_notes, item_is_escrowed) = ctx.db.with_snapshot(|s| {
        let count = s.owner_notes.values().filter(|n| n.label.starts_with("raffle:")).count();
        let escrowed = s.items.values().any(|it| {
            it.owner_id == -52_568 && it.location_id == -52_568 && it.flag == eve_db::InventoryFlag::Locked
        });
        (count, escrowed)
    });
    assert!(seeded_notes >= 2);
    assert!(item_is_escrowed, "Seeded raffle item must be escrowed at -52_568 with Locked flag");

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

    let debug_err = ctx
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
        .expect_err("debug.Exec must fail closed with UnimplementedRPC");
    assert_eq!(debug_err.code, "UnimplementedRPC");
}
