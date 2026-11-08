//! Deep branch and mutation-killing unit tests for `services::macho_cluster_mgmt`.

#![forbid(unsafe_code)]

use super::macho_cluster_mgmt::*;
use crate::router::ServerContext;
use eve_db::{AccountRecord, VirtualServerClock, WorldRepository};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx() -> ServerContext {
    let ctx = ServerContext::with_sde_and_repository(
        shared_sde(),
        Arc::new(WorldRepository::with_demo_seed()),
    );
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_850_000_000)));
    reset_cluster_state(&ctx);
    ctx
}

fn gm_sess(char_id: i64, station_id: Option<i64>, sys_id: Option<u32>) -> SessionState {
    let mut s = SessionState::new(77_001);
    s.user_id = Some(1);
    s.character_id = Some(char_id);
    s.station_id = station_id;
    s.solar_system_id = sys_id;
    s.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;
    s
}

fn player_sess(uid: u64, char_id: i64) -> SessionState {
    let mut s = SessionState::new(uid);
    s.user_id = Some(uid);
    s.character_id = Some(char_id);
    s.solar_system_id = Some(30_000_142);
    s.role = 0;
    s
}

#[test]
fn load_service_set_config_start_stop_and_total_stats_branches() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut gm = gm_sess(90_000_001, Some(60_003_760), Some(30_000_142));
    let mut player = player_sess(2, 90_000_001);

    // 1. Non-GM rejected on SetConfig, StopLoad, GetTotalStats
    assert_eq!(handle(&ctx, &mut player, "loadService", "SetConfig", &[], &kw).unwrap().unwrap_err().message, "NotAuthorized");
    assert_eq!(handle(&ctx, &mut player, "loadService", "StopLoad", &[], &kw).unwrap().unwrap_err().message, "NotAuthorized");
    assert_eq!(handle(&ctx, &mut player, "loadService", "GetTotalStats", &[], &kw).unwrap().unwrap_err().message, "NotAuthorized");

    // 2. SetConfig via Dict, KeyVal, String+arg, kwargs, and error cases
    assert_eq!(
        handle(&ctx, &mut gm, "loadService", "SetConfig", &[], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "InvalidConfig"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "loadService",
            "SetConfig",
            &[PyRep::Dict(vec![(PyRep::String("".into()), PyRep::Integer(5))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "InvalidConfig"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "loadService",
            "SetConfig",
            &[PyRep::keyval([("steps", PyRep::Integer(0))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "InvalidConfig"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "loadService",
            "SetConfig",
            &[PyRep::String("solar_system_id".into()), PyRep::Integer(-5)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "InvalidConfig"
    );

    handle(
        &ctx,
        &mut gm,
        "loadService",
        "SetConfig",
        &[PyRep::keyval([
            ("iterations", PyRep::Real(3.2)),
            ("solarSystemID", PyRep::Integer(30_000_142)),
        ])],
        &kw,
    )
    .unwrap()
    .expect("SetConfig KeyVal");

    handle(
        &ctx,
        &mut gm,
        "loadService",
        "SetConfig",
        &[PyRep::String("duration".into()), PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .expect("SetConfig String");

    let mut cfg_kw = HashMap::new();
    cfg_kw.insert("ticks".into(), PyRep::Integer(4));
    handle(&ctx, &mut gm, "loadService", "SetConfig", &[PyRep::None], &cfg_kw)
        .unwrap()
        .expect("SetConfig kwargs");

    // 3. StartLoad error branches: invalid Real, MissingSolarSystemID, InvalidSolarSystemID
    assert_eq!(
        handle(&ctx, &mut gm, "loadService", "StartLoad", &[PyRep::Real(0.0)], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "InvalidLoadSpec"
    );

    let mut gm_no_sys = gm_sess(90_000_001, None, None);
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.remove("load_service_state");
    });
    assert_eq!(
        handle(&ctx, &mut gm_no_sys, "loadService", "StartLoad", &[PyRep::Integer(2)], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "MissingSolarSystemID"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm_no_sys,
            "loadService",
            "StartLoad",
            &[PyRep::keyval([("solarSystemID", PyRep::Integer(99_999_999))])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "InvalidSolarSystemID"
    );

    // 4. StartLoad with Real > 0, then StopLoad, then GetTotalStats
    handle(&ctx, &mut gm, "loadService", "StartLoad", &[PyRep::Real(2.5)], &kw)
        .unwrap()
        .expect("StartLoad Real");
    handle(&ctx, &mut gm, "loadService", "StopLoad", &[], &kw)
        .unwrap()
        .expect("StopLoad");

    let stats = handle(&ctx, &mut gm, "loadService", "GetTotalStats", &[], &kw)
        .unwrap()
        .expect("GetTotalStats");
    assert_eq!(stats.dict_get("active").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(stats.dict_get("runs_started").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(stats.dict_get("total_requests").and_then(|v| v.as_i64()), Some(2));
    assert_eq!(stats.dict_get("last_stopped_unix").and_then(|v| v.as_i64()), Some(1_850_000_000));
}

#[test]
fn session_mgr_raffle_seeding_db2_callproc_and_debug_exec() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut gm = gm_sess(90_000_001, Some(60_003_760), Some(30_000_142));
    let mut gm_undocked = gm_sess(90_000_001, None, Some(30_000_142));
    let mut player = player_sess(2, 90_000_001);

    // 1. sessionMgr.EndAllGameSessions & RemoveSessionsFromServer & PerformHorridSessionAttributeUpdate
    ctx.db.with_mut_snapshot(|s| {
        s.service_state_blobs.insert(
            "active_sessions".into(),
            serde_json::to_string(&vec![77_001_i64, 88_001, 88_002]).unwrap(),
        );
        s.service_state_blobs.insert(
            "session_registry".into(),
            serde_json::to_string(&HashMap::from([
                ("77001".to_string(), serde_json::json!({"ok": true})),
                ("88001".to_string(), serde_json::json!({"ok": true})),
                ("88002".to_string(), serde_json::json!({"ok": true})),
            ]))
            .unwrap(),
        );
    });
    // RemoveSessionsFromServer for foreign node_id (e.g. 99) is a no-op
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "RemoveSessionsFromServer",
        &[PyRep::Integer(99), PyRep::Integer(88_001)],
        &kw,
    )
    .unwrap()
    .expect("foreign node no-op");
    // RemoveSessionsFromServer for node 1 with single Integer removes 88_001
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "RemoveSessionsFromServer",
        &[PyRep::Integer(1), PyRep::Integer(88_001)],
        &kw,
    )
    .unwrap()
    .expect("remove 88001");
    // EndAllGameSessions retains only gm.session_id (77_001)
    handle(&ctx, &mut gm, "sessionMgr", "EndAllGameSessions", &[], &kw)
        .unwrap()
        .expect("EndAllGameSessions");
    ctx.db.with_snapshot(|s| {
        let active: Vec<i64> =
            serde_json::from_str(s.service_state_blobs.get("active_sessions").unwrap()).unwrap();
        assert_eq!(active, vec![77_001]);
    });
    assert_eq!(
        handle(&ctx, &mut gm, "sessionMgr", "PerformHorridSessionAttributeUpdate", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArguments"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "PerformHorridSessionAttributeUpdate",
            &[
                PyRep::Integer(999_999),
                PyRep::Dict(vec![(PyRep::String("corprole".into()), PyRep::Integer(1))]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SessionNotFound"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "PerformHorridSessionAttributeUpdate",
            &[
                PyRep::Integer(77_001),
                PyRep::Dict(vec![(PyRep::String("bogusAttr".into()), PyRep::Integer(1))]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSessionAttribute"
    );
    let gm_sid = gm.session_id as i64;
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "PerformHorridSessionAttributeUpdate",
            &[
                PyRep::Integer(gm_sid),
                PyRep::Dict(vec![
                    (PyRep::String("corpid".into()), PyRep::Integer(98_000_777)),
                    (PyRep::String("corprole".into()), PyRep::Integer(1)),
                ]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(gm.corporation_id, Some(98_000_777));
    assert_eq!(gm.corp_role, 1);
    assert!(
        gm.drain_notifications()
            .into_iter()
            .any(|(ev, _)| ev == "OnSessionChanged")
    );
    ctx.db.with_snapshot(|s| {
        let reg: HashMap<String, serde_json::Value> =
            serde_json::from_str(s.service_state_blobs.get("session_registry").unwrap()).unwrap();
        let entry = reg.get(&gm.session_id.to_string()).unwrap();
        assert_eq!(entry.get("corporation_id").and_then(|v| v.as_i64()), Some(98_000_777));
        assert_eq!(entry.get("corp_role").and_then(|v| v.as_u64()), Some(1));
    });

    // 2. raffleMgr.QA_SeedRaffles & character_Seeding error and success branches
    assert_eq!(
        handle(&ctx, &mut player, "raffleMgr", "QA_SeedRaffles", &[], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "NotAuthorized"
    );
    assert_eq!(
        handle(&ctx, &mut gm_undocked, "raffleMgr", "QA_SeedRaffles", &[], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "CharacterNotDocked"
    );
    let seeded_qa = handle(
        &ctx,
        &mut gm,
        "raffleMgr",
        "QA_SeedRaffles",
        &[PyRep::Integer(2), PyRep::Integer(587)],
        &kw,
    )
    .unwrap()
    .expect("QA_SeedRaffles");
    assert!(matches!(seeded_qa, PyRep::List(ref l) if l.len() == 2));

    let raffle_svc = ["raffle", "Mgr"].concat();
    let seed_char = |s: &mut SessionState, a: &[PyRep]| {
        handle(&ctx, s, &raffle_svc, "character_Seeding", a, &kw).unwrap()
    };
    assert_eq!(seed_char(&mut player, &[]).unwrap_err().message, "NotAuthorized");
    assert_eq!(
        seed_char(&mut gm, &[PyRep::Integer(90_000_001), PyRep::Integer(0)]).unwrap_err().message,
        "InvalidArguments"
    );
    assert_eq!(
        seed_char(&mut gm, &[PyRep::Integer(90_000_001), PyRep::Integer(1), PyRep::Integer(999_999_999)])
            .unwrap_err()
            .message,
        "InvalidTypeID"
    );
    assert_eq!(
        seed_char(&mut gm, &[PyRep::Integer(90_000_001), PyRep::Integer(1), PyRep::Integer(587), PyRep::Integer(7)])
            .unwrap_err()
            .message,
        "InvalidTicketCount"
    );
    assert_eq!(
        seed_char(
            &mut gm,
            &[PyRep::Integer(90_000_001), PyRep::Integer(1), PyRep::Integer(587), PyRep::Integer(16), PyRep::Real(-10.0)],
        )
        .unwrap_err()
        .message,
        "InvalidTicketPrice"
    );
    assert_eq!(
        seed_char(&mut gm, &[PyRep::Integer(999_888_777), PyRep::Integer(1)]).unwrap_err().message,
        "CharacterNotFound"
    );
    let seeded_char = seed_char(
        &mut gm,
        &[PyRep::Integer(90_000_001), PyRep::Integer(2), PyRep::Integer(587), PyRep::Integer(16), PyRep::Real(5_000.0)],
    )
    .expect("character_Seeding");
    assert!(matches!(seeded_char, PyRep::List(ref l) if l.len() == 2));

    // 3. DB2.CallProc & debug.Exec
    ctx.db.with_mut_snapshot(|s| {
        s.accounts.insert(
            "admin_acc".into(),
            AccountRecord {
                user_id: 500,
                username: "admin_acc".into(),
                password_token: String::new(),
                role: eve_db::service_roles::ROLE_ADMIN,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
    });
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "DB2",
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "NotAuthorized"
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "DB2",
            "CallProc",
            &[PyRep::String("zcluster.Nodes_PolarisID".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );
    let users_rs = handle(
        &ctx,
        &mut gm,
        "DB2",
        "CallProc",
        &[
            PyRep::String("zuser.Users_SelectByRole".into()),
            PyRep::Integer(eve_db::service_roles::ROLE_ADMIN as i64),
        ],
        &kw,
    )
    .unwrap()
    .expect("Users_SelectByRole");
    assert!(matches!(users_rs, PyRep::Rowset { ref rows, .. } if !rows.is_empty()));

    // zcluster.Attacks_Insert: >8 args rejected, 16 calls succeed for non-GM, 17th rate-limited
    let nine_args: Vec<PyRep> = (0..9)
        .map(|i| {
            if i == 0 {
                PyRep::String("zcluster.Attacks_Insert".into())
            } else {
                PyRep::Integer(i)
            }
        })
        .collect();
    assert_eq!(
        handle(&ctx, &mut player, "DB2", "CallProc", &nine_args, &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "InvalidArguments"
    );
    let long_str = "x".repeat(300);
    for _ in 0..16 {
        handle(
            &ctx,
            &mut player,
            "DB2",
            "CallProc",
            &[
                PyRep::String("zcluster.Attacks_Insert".into()),
                PyRep::String(long_str.clone()),
            ],
            &kw,
        )
        .unwrap()
        .expect("Attacks_Insert within rate limit");
    }
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "DB2",
            "CallProc",
            &[PyRep::String("zcluster.Attacks_Insert".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "RateLimited"
    );
    assert_eq!(
        handle(&ctx, &mut gm, "DB2", "CallProc", &[PyRep::String("unknown.Proc".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "UnimplementedRPC"
    );

    assert_eq!(
        handle(&ctx, &mut player, "debug", "Exec", &[PyRep::String("1+1".into())], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "NotAuthorized"
    );
    assert_eq!(
        handle(&ctx, &mut gm, "debug", "Exec", &[PyRep::String("status".into())], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "UnimplementedRPC"
    );
    let exec_ok = handle(&ctx, &mut gm, "debug", "Exec", &[PyRep::String("print(1)".into())], &kw)
        .unwrap()
        .expect("debug.Exec");
    assert!(matches!(exec_ok, PyRep::List(ref l) if l.len() == 1));
}

#[test]
fn macho_net_node_lookup_acl_connection_properties_and_shutdown_cancel() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut gm = gm_sess(90_000_001, Some(60_003_760), Some(30_000_142));
    let mut player = player_sess(2, 90_000_001);

    // 1. GetNodeFromAddress with Tuple (host, port), Tuple (host,), fallback arg/kwarg
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "machoNet",
            "GetNodeFromAddress",
            &[PyRep::Tuple(vec![
                PyRep::String("127.0.0.1".into()),
                PyRep::Integer(26001),
            ])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(2)
    );
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "machoNet",
            "GetNodeFromAddress",
            &[PyRep::Tuple(vec![PyRep::String("localhost".into())])],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(1)
    );
    let mut fb_kw = HashMap::new();
    fb_kw.insert("default_node_id".into(), PyRep::Integer(42));
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "machoNet",
            "GetNodeFromAddress",
            &[PyRep::String("10.99.99.99:26000".into())],
            &fb_kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Integer(42)
    );

    // 2. CheckACL: invalid IP, espCheck non-GM vs GM, valid IP
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "machoNet",
            "CheckACL",
            &[PyRep::String("not-an-ip".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String("Invalid IP address format".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &mut player,
            "machoNet",
            "CheckACL",
            &[PyRep::String("127.0.0.1".into()), PyRep::Bool(true)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::String("GM role required for ESP access".into())
    );
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "machoNet",
            "CheckACL",
            &[PyRep::String("127.0.0.1".into()), PyRep::Bool(true)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // 3. SetConnectionProperty: non-GM, missing key, disallowed key, allowed keys
    assert_eq!(
        handle(&ctx, &mut player, "machoNet", "SetConnectionProperty", &[], &kw).unwrap().unwrap_err().code,
        "InsufficientRole"
    );
    assert_eq!(
        handle(&ctx, &mut gm, "machoNet", "SetConnectionProperty", &[], &kw).unwrap().unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        handle(&ctx, &mut gm, "machoNet", "SetConnectionProperty", &[PyRep::String("forbiddenKey".into()), PyRep::Integer(1)], &kw).unwrap().unwrap_err().code,
        "InvalidArgument"
    );

    for (k, v) in [("clusterName", "SINGULARITY"), ("clusterMode", "TEST"), ("internaladdress", "10.0.0.5:26000")] {
        handle(&ctx, &mut gm, "machoNet", "SetConnectionProperty", &[PyRep::String(k.into()), PyRep::String(v.into())], &kw).unwrap().expect("SetConnectionProperty");
    }
    let st = get_cluster_state(&ctx);
    assert_eq!(st.cluster_name, "SINGULARITY");
    assert_eq!(st.cluster_mode, "TEST");
    assert_eq!(st.polaris_address, "10.0.0.5:26000");

    // 4. GracefulShutdown with timeout <= 0 cancels shutdown
    handle(&ctx, &mut gm, "machoNet", "GracefulShutdown", &[PyRep::Integer(1), PyRep::String("Aborted".into()), PyRep::Integer(0), PyRep::Bool(true)], &kw).unwrap().expect("cancel shutdown");
    assert!(!get_cluster_state(&ctx).shutdown_in_progress);
}
