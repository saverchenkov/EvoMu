//! Behavioral edge-case tests for `macho_cluster_mgmt.rs`.

#![forbid(unsafe_code)]

use super::{handle, reset_cluster_state};
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

fn gm_sess(sid: u64, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_125);
    s.solar_system_id = Some(30_000_142);
    s.role = eve_db::service_roles::ROLE_GMH;
    s
}

fn player_sess(sid: u64, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.solar_system_id = Some(30_000_142);
    s.role = eve_db::service_roles::ROLE_PLAYER;
    s
}

#[test]
fn session_attribute_update_validation_station_structure_exclusivity_and_remote_sync() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut gm = gm_sess(77_100, 90_000_001);
    let mut player = player_sess(77_101, 90_000_002);

    // 1. Non-GM player is rejected with InsufficientRole
    let err = handle(
        &ctx,
        &mut player,
        "sessionMgr",
        "UpdateSessionAttributes",
        &[PyRep::String("languageID".into()), PyRep::String("DE".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InsufficientRole");

    // 2. Invalid target sessionID (0 or negative positional and kwarg) -> InvalidSessionId
    for bad_sid in [0_i64, -5_i64] {
        let e = handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "UpdateSessionAttributes",
            &[
                PyRep::Integer(bad_sid),
                PyRep::String("languageID".into()),
                PyRep::String("EN".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(e.code, "InvalidSessionId");
    }
    let mut kw_bad_sid = HashMap::new();
    kw_bad_sid.insert("sessionID".into(), PyRep::Integer(-1));
    kw_bad_sid.insert("languageID".into(), PyRep::String("EN".into()));
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "UpdateSessionAttributes",
            &[],
            &kw_bad_sid,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidSessionId"
    );

    // 3. InvalidArguments: non-string dict key, invalid UTF-8 bytes key, wrong arity
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "UpdateSessionAttributes",
            &[PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(2))])],
            &kw,
        )
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
            "UpdateSessionAttributes",
            &[PyRep::Bytes(vec![0xFF, 0xFE]), PyRep::Integer(1)],
            &kw,
        )
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
            "UpdateSessionAttributes",
            &[PyRep::String("onlyKey".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArguments"
    );

    // 4. InvalidSessionAttribute across all typed parsers
    let bad_attrs: Vec<(&str, PyRep)> = vec![
        ("unknownAttr", PyRep::Integer(1)),
        ("charid", PyRep::Integer(-1)),
        ("userid", PyRep::String("abc".into())),
        ("solarsystemid", PyRep::Integer(i64::from(u32::MAX) + 1)),
        ("corprole", PyRep::Integer(-2)),
        ("corpAccountKey", PyRep::Integer(i64::from(i32::MAX) + 1)),
        ("userType", PyRep::String("not-int".into())),
        ("languageID", PyRep::Integer(42)),
        ("address", PyRep::Bytes(vec![0xFF, 0x80])),
        ("maxSessionTime", PyRep::Integer(-10)),
    ];
    for (k, v) in bad_attrs {
        let e = handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "PerformHorridSessionAttributeUpdate",
            &[PyRep::String(k.into()), v],
            &kw,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(e.code, "InvalidSessionAttribute", "attr={k}");
    }

    // 5. Updating unknown remote session -> SessionNotFound
    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "sessionMgr",
            "UpdateSessionAttributes",
            &[
                PyRep::Integer(999_888_777),
                PyRep::String("languageID".into()),
                PyRep::String("FR".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SessionNotFound"
    );

    // 6. stationid vs structureid exclusivity and simultaneous explicit retention
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "UpdateSessionAttributes",
        &[PyRep::Dict(vec![
            (PyRep::String("stationid".into()), PyRep::Integer(60_003_760)),
            (PyRep::String("structureid".into()), PyRep::Integer(1_000_000_123)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm.station_id, Some(60_003_760));
    assert_eq!(gm.structure_id, Some(1_000_000_123));

    // Setting stationid alone clears structureid
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "UpdateSessionAttributes",
        &[PyRep::String("stationid".into()), PyRep::Integer(60_003_761)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm.station_id, Some(60_003_761));
    assert_eq!(gm.structure_id, None);

    // Setting structureid alone clears stationid
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "UpdateSessionAttributes",
        &[PyRep::String("structureid".into()), PyRep::Integer(1_000_000_456)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm.station_id, None);
    assert_eq!(gm.structure_id, Some(1_000_000_456));

    // 7. Extra registry-backed attributes (hqID, rolesAtAll, fleetrole) emit OnSessionChanged extra_diffs
    let before_notifs = gm.notifications.len();
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "UpdateSessionAttributes",
        &[PyRep::Dict(vec![
            (PyRep::String("hqID".into()), PyRep::Integer(60_003_760)),
            (PyRep::String("rolesAtAll".into()), PyRep::Integer(128)),
            (PyRep::String("fleetrole".into()), PyRep::Integer(1)),
        ])],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(gm.notifications.len() > before_notifs);
    let last_notif = gm.notifications.last().unwrap();
    assert_eq!(last_notif.0, "OnSessionChanged");
    let PyRep::Dict(diff) = &last_notif.1[2] else {
        panic!("expected diff dict in OnSessionChanged");
    };
    assert!(diff.iter().any(|(k, _)| k.as_str() == Some("hqID")));
    assert!(diff.iter().any(|(k, _)| k.as_str() == Some("rolesAtAll")));
    assert!(diff.iter().any(|(k, _)| k.as_str() == Some("fleetrole")));

    // 8. Remote session update syncs character, account role, and forced_session_corp_sync
    let remote_sid = 88_222_u64;
    let remote_uid = 88_222_u64;
    let remote_cid = 90_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = remote_cid;
        ch.user_id = remote_uid;
        ch.corporation_id = 1_000_125;
        snap.characters.insert(remote_cid, ch);
        snap.accounts.insert(
            "remote_acc".into(),
            AccountRecord {
                user_id: remote_uid,
                username: "remote_acc".into(),
                password_token: "h".into(),
                role: eve_db::service_roles::ROLE_PLAYER,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let reg = serde_json::json!({
            "88222": {
                "user_id": remote_uid,
                "character_id": remote_cid,
                "corporation_id": 1_000_125,
            }
        });
        snap.cluster_and_runtime.session_registry = serde_json::from_value(reg).unwrap();
    });
    handle(
        &ctx,
        &mut gm,
        "sessionMgr",
        "UpdateSessionAttributes",
        &[
            PyRep::Integer(remote_sid as i64),
            PyRep::Dict(vec![
                (PyRep::String("corpid".into()), PyRep::Integer(98_555_001)),
                (PyRep::String("allianceid".into()), PyRep::Integer(99_555_001)),
                (PyRep::String("corprole".into()), PyRep::Integer(256)),
                (
                    PyRep::String("role".into()),
                    PyRep::Integer(eve_db::service_roles::ROLE_GMH as i64),
                ),
            ]),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&remote_cid).unwrap();
        assert_eq!(ch.corporation_id, 98_555_001);
        assert_eq!(ch.alliance_id, Some(99_555_001));
        assert_eq!(
            snap.accounts.get("remote_acc").unwrap().role,
            eve_db::service_roles::ROLE_GMH
        );
    });
    let synced = crate::services::fleet_social::fleet_social_persistence::with_social_state(
        &ctx,
        |s| s.forced_session_corp_sync.get(&remote_cid).copied(),
    );
    assert_eq!(synced, Some((98_555_001, Some(99_555_001), 256)));
}

#[test]
fn load_service_macho_net_and_debug_exec_edge_cases() {
    let ctx = test_ctx();
    let kw = HashMap::new();
    let mut gm = gm_sess(77_200, 90_000_010);
    let mut player = player_sess(77_201, 90_000_011);

    // 1. loadService authorization and invalid specs
    for meth in ["StartLoad", "StopLoad", "SetConfig"] {
        let err = handle(&ctx, &mut player, "loadService", meth, &[], &kw)
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "UserError");
        assert!(err.message.contains("Authorized"));
    }
    for bad_real in [0.0_f64, -2.5_f64, f64::NAN] {
        assert_eq!(
            handle(
                &ctx,
                &mut gm,
                "loadService",
                "StartLoad",
                &[PyRep::Real(bad_real)],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .message,
            "InvalidLoadSpec"
        );
    }

    let mut no_sys_gm = gm.clone();
    no_sys_gm.solar_system_id = None;
    assert_eq!(
        handle(
            &ctx,
            &mut no_sys_gm,
            "loadService",
            "StartLoad",
            &[PyRep::Integer(2)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "MissingSolarSystemID"
    );

    let mut bad_sys_gm = gm.clone();
    bad_sys_gm.solar_system_id = Some(99_999_999);
    assert_eq!(
        handle(
            &ctx,
            &mut bad_sys_gm,
            "loadService",
            "StartLoad",
            &[PyRep::Integer(2)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "InvalidSolarSystemID"
    );

    assert_eq!(
        handle(
            &ctx,
            &mut gm,
            "loadService",
            "SetConfig",
            &[PyRep::Dict(vec![])],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .message,
        "InvalidConfig"
    );

    // 2. machoNet.GetMetrics non-GM -> InsufficientRole; GM -> structured metrics
    assert_eq!(
        handle(&ctx, &mut player, "machoNet", "GetMetrics", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    let metrics = handle(&ctx, &mut gm, "machoNet", "GetMetrics", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::List(nodes) = metrics else { panic!("expected list") };
    assert_eq!(nodes.len(), 1);
    let PyRep::Tuple(parts) = &nodes[0] else { panic!("expected tuple") };
    assert_eq!(parts[0].as_i64(), Some(1));
    assert_eq!(parts[1].as_str(), Some("machoNet"));
    assert!(parts[2].dict_get("NotificationsSent").is_some());

    // 3. SetConnectionProperty syncs clusterName, clusterMode, internaladdress to GlobalConfig & accessors
    handle(
        &ctx,
        &mut gm,
        "machoNet",
        "SetConnectionProperty",
        &[PyRep::String("clusterName".into()), PyRep::String("SINGULARITY".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut gm,
        "machoNet",
        "SetConnectionProperty",
        &[PyRep::String("clusterMode".into()), PyRep::String("TEST".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    handle(
        &ctx,
        &mut gm,
        "machoNet",
        "SetConnectionProperty",
        &[PyRep::String("internaladdress".into()), PyRep::String("10.10.0.5".into())],
        &kw,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        handle(&ctx, &mut gm, "machoNet", "GetConnectedClusterName", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("SINGULARITY".into())
    );
    assert_eq!(
        handle(&ctx, &mut gm, "machoNet", "GetConnectedClusterMode", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("TEST".into())
    );
    assert_eq!(
        handle(&ctx, &mut gm, "machoNet", "GetPolarisExternalTunnelingAddress", &[], &kw)
            .unwrap()
            .unwrap(),
        PyRep::String("10.10.0.5".into())
    );
    let gcfg = handle(&ctx, &mut gm, "machoNet", "GetGlobalConfig", &[], &kw)
        .unwrap()
        .unwrap();
    assert_eq!(gcfg.dict_get("clusterName").and_then(|v| v.as_str()), Some("SINGULARITY"));
    assert_eq!(gcfg.dict_get("clusterMode").and_then(|v| v.as_str()), Some("TEST"));

    // 4. debug.Exec authorization, UnimplementedRPC for status/self.stats(), and OK response otherwise
    assert_eq!(
        handle(&ctx, &mut player, "debug", "Exec", &[PyRep::String("1+1".into())], &kw)
            .unwrap()
            .unwrap_err()
            .message,
        "NotAuthorized"
    );
    for blocked_cmd in ["status", "self.stats()"] {
        assert_eq!(
            handle(
                &ctx,
                &mut gm,
                "debug",
                "Exec",
                &[PyRep::String(blocked_cmd.into())],
                &kw,
            )
            .unwrap()
            .unwrap_err()
            .code,
            "UnimplementedRPC"
        );
    }
    let ok_exec = handle(
        &ctx,
        &mut gm,
        "debug",
        "Exec",
        &[PyRep::String("print('hi')".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(rows) = ok_exec else { panic!("expected list") };
    assert_eq!(rows.len(), 1);
}
