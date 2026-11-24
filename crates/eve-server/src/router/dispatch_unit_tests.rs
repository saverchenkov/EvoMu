//! Unit and mutation-killing tests for `router::dispatch`
//! (WAL error hooks, PyRep summary formatting, session revocation, feature flags,
//! GM/QA role gates, `timeControl`, `MachoResolveObject`, `MachoBindObject`, and mining site seeding).

use super::*;
use crate::router::ServerContext;
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use std::collections::HashMap;
use std::sync::Arc;

#[tokio::test]
async fn format_pyrep_summary_variants() {
    assert_eq!(format_pyrep_summary(&PyRep::None), "None");
    assert_eq!(format_pyrep_summary(&PyRep::Bool(true)), "Bool(true)");
    assert_eq!(format_pyrep_summary(&PyRep::Integer(42)), "Int(42)");
    assert_eq!(format_pyrep_summary(&PyRep::Real(3.14159)), "Real(3.14)");
    assert_eq!(
        format_pyrep_summary(&PyRep::String("hello".into())),
        "Str[5]"
    );
    assert_eq!(
        format_pyrep_summary(&PyRep::List(vec![PyRep::None, PyRep::None])),
        "List[2]"
    );
    assert_eq!(
        format_pyrep_summary(&PyRep::Tuple(vec![PyRep::Integer(1)])),
        "Tuple[1]"
    );
    assert_eq!(format_pyrep_summary(&PyRep::Dict(Vec::new())), "Dict[0]");
    assert_eq!(
        format_pyrep_summary(&PyRep::Object {
            type_name: "util.KeyVal".into(),
            args: Box::new(PyRep::None),
        }),
        "Obj(util.KeyVal)"
    );
    assert_eq!(format_pyrep_summary(&PyRep::Token("tok".into())), "PyRep");
}

#[tokio::test]
async fn dispatch_rpc_feature_flags_role_gates_and_time_control() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    {
        let mut flags = ctx.feature_flags.write().unwrap();
        flags.enable_living_world = false;
        flags.enable_quasar_gateway = false;
    }

    let mut sess = SessionState::new(94_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    // 1. FeatureDisabled gates for taleMgr and publicGatewaySvc
    let tale_err = ctx
        .dispatch_rpc(&mut sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(tale_err.code, "FeatureDisabled");

    let q_err = ctx
        .dispatch_rpc(&mut sess, "publicGatewaySvc", None, "SendRequest", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(q_err.code, "FeatureDisabled");

    {
        let mut flags = ctx.feature_flags.write().unwrap();
        flags.enable_living_world = true;
        flags.enable_quasar_gateway = true;
    }

    // 2. GM & QA role gates
    sess.role = 0;
    let gm_err = ctx
        .dispatch_rpc(&mut sess, "slash", None, "SlashCmd", &[PyRep::String("/healself".into())], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(gm_err.code, "InsufficientRole");

    // `publicQaToolsServer.CanGiveItemForMultifit` is a capability query called by V24 Multifit
    // for all authenticated player sessions (`role = 0` -> `false`, GM/QA -> `true`), while
    // `MoveMeTo` and `SlashCmd` remain gated behind `InsufficientRole`.
    let can_give_player = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await
        .expect("CanGiveItemForMultifit for role=0 must succeed with false");
    assert_eq!(can_give_player, PyRep::Bool(false));

    let move_err = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "MoveMeTo", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(move_err.code, "InsufficientRole");

    let qa_slash_err = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "SlashCmd", &[PyRep::String("/heal".into())], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(qa_slash_err.code, "InsufficientRole");

    // Verify authenticated session without character selection fails cleanly with CharacterNotSelected
    let mut no_char_sess = SessionState::new(94_002);
    no_char_sess.user_id = Some(1);
    no_char_sess.role = 0;
    let no_char_err = ctx
        .dispatch_rpc(&mut no_char_sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(no_char_err.code, "CharacterNotSelected");

    let tc_err = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(5)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(tc_err.code, "InsufficientRole");

    // Grant GM role and test CanGiveItemForMultifit, AdvanceClock (negative vs valid), and ClearSessionTimer
    sess.role = service_roles::ROLE_GMH;
    let can_give_gm = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await
        .expect("CanGiveItemForMultifit for ROLE_GMH");
    assert_eq!(can_give_gm, PyRep::Bool(true));

    sess.role = service_roles::ROLE_QA;
    let can_give_qa = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await
        .expect("CanGiveItemForMultifit for ROLE_QA");
    assert_eq!(can_give_qa, PyRep::Bool(true));

    sess.role = service_roles::ROLE_GMH;
    let neg_err = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(-5)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(neg_err.code, "InvalidArgument");

    let before_unix = ctx.now_unix();
    let adv_ok = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(15)], &kw, None)
        .await
        .expect("AdvanceClock");
    assert_eq!(adv_ok, PyRep::Bool(true));
    assert!(ctx.now_unix() >= before_unix + 15);

    let clr_ok = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "ClearSessionTimer", &[], &kw, None)
        .await
        .expect("ClearSessionTimer");
    assert_eq!(clr_ok, PyRep::Bool(true));
}

#[tokio::test]
async fn macho_resolve_bind_object_and_session_revocation() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut sess = SessionState::new(94_010);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    let kw = HashMap::new();

    // 1. MachoResolveObject
    let node_rep = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .expect("MachoResolveObject");
    assert_eq!(node_rep, PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));

    // 2. MachoBindObject with integer, string, tuple, and piggybacked sub-call
    let bind_int = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject int");
    assert!(matches!(bind_int, PyRep::Tuple(_)));

    let bind_str_err = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::String("solarsystem".into()), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bind_str_err.code, "BindAuthorizationDenied");

    let bind_agent = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(3_008_416), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject valid SDE agent");
    assert!(matches!(bind_agent, PyRep::Tuple(_)));

    let bind_tuple_piggy = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetSkills".into()),
                    PyRep::Tuple(Vec::new()),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("MachoBindObject with piggybacked GetSkills");
    if let PyRep::Tuple(parts) = bind_tuple_piggy {
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[1], PyRep::Dict(_)));
    } else {
        panic!("expected 2-tuple");
    }

    // Invalid MachoBindObject argument
    let bind_err = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoBindObject", &[PyRep::None], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(bind_err.code, "InvalidArgument");

    // 3. Session displacement when another session holds the online lock ("MultiLoginBlocked")
    ctx.db
        .acquire_online_lock(1, 99_999, Some(90_000_001), true, ctx.now_unix())
        .expect("other session lock");
    let displaced = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(displaced.code, "MultiLoginBlocked");

    // 4. Account ban / session revocation clears session state
    ctx.db.with_mut_snapshot(|snap| {
        for acc in snap.accounts.values_mut().filter(|a| a.user_id == 1) {
            acc.banned_until_unix = Some(u64::MAX);
        }
    });
    let banned = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(banned.code, "UserBanned");
    assert_eq!(sess.user_id, None);
    assert_eq!(sess.character_id, None);
}

#[tokio::test]
async fn seed_system_mining_sites_and_sov_prospecting_respawn() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let sys_id = 30_000_142;

    seed_system_mining_sites_from_sde(&ctx, sys_id).await;
    // Second call hits the already_seeded fast path
    seed_system_mining_sites_from_sde(&ctx, sys_id).await;
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;

    let has_anoms_or_sigs = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.contains_key(&sys_id) || snap.cosmic_signatures.contains_key(&sys_id)
    });
    assert!(has_anoms_or_sigs);
}

#[tokio::test]
async fn dispatch_rpc_downtime_exemptions_public_gateway_and_role_gate_matrix() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    // 1. publicGatewaySvc.SendRequest errors: require_character, UnknownQuasarDomain, UnimplementedRPC
    let mut no_char_sess = SessionState::new(94_100);
    no_char_sess.user_id = Some(1);
    let err_no_char = ctx
        .dispatch_rpc(
            &mut no_char_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::String("eve_public.character".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_no_char.code, "CharacterNotSelected");

    let mut sess = SessionState::new(94_101);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.station_id = Some(60_003_760);
    sess.solar_system_id = Some(30_000_142);
    sess.corporation_id = Some(98_000_001);

    let err_unknown_dom = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::String("nonexistent.quasar.domain".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_unknown_dom.code, "UnknownQuasarDomain");

    if let Some(known_domain) = ctx.quasar_domains.iter().next().cloned() {
        let err_unimpl_quasar = ctx
            .dispatch_rpc(
                &mut sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &[
                    PyRep::String(known_domain),
                    PyRep::String("CustomMsgRequest".into()),
                ],
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err_unimpl_quasar.code, "UnimplementedRPC");
    }

    // 2. Role gate matrix driven by RpcCallerClass on compiled RpcRegistry entries;
    //    unregistered names fall through to UnimplementedRPC for logged-in players.
    sess.role = service_roles::ROLE_PLAYER;
    for (svc, method) in [
        ("keeper", "GetLevelEditor"),
        ("dungeon", "DEGetDungeons"),
        ("sessionMgr", "EndAllGameSessions"),
        ("contractProxy", "GM_ExpireContract"),
        ("timeControl", "AdvanceClock"),
        ("raffleMgr", "QA_SeedRaffles"),
        ("userSvc", "CreateRedeemTokenQA"),
        ("serviceGatewayMgr", "qa_enable_hijack_mode"),
        ("warRegistry", "GMJoinDefender"),
        ("charUnboundMgr", "GetQAStarterSystemIDs"),
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "InsufficientRole", "expected InsufficientRole on {svc}.{method}");
    }
    for (svc, method) in [
        ("taleMgr", "MaterializeSystemLOD2"), // rpc-lint: allow unregistered dispatch test fixture
        ("devToolsProvider", "AnyMethod"), // rpc-lint: allow unregistered dispatch test fixture
        ("sessionMgr", "AnyMethod"), // rpc-lint: allow unregistered dispatch test fixture
        ("character", "GM_ForceRename"), // rpc-lint: allow unregistered dispatch test fixture
        ("character", "QA_Reset"), // rpc-lint: allow unregistered dispatch test fixture
        ("character", "qa_seed"), // rpc-lint: allow unregistered dispatch test fixture
        ("character", "RunDiagnosticsQA"), // rpc-lint: allow unregistered dispatch test fixture
        ("character", "QA_Probe"), // rpc-lint: allow unregistered dispatch test fixture
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "UnimplementedRPC", "expected UnimplementedRPC on unregistered {svc}.{method}");
    }

    // `slash.SlashCmd("/")` is allowed past role gate even for ROLE_PLAYER
    if let Err(err) = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String(" / ".into())],
            &kw,
            None,
        )
        .await
    {
        assert_ne!(err.code, "InsufficientRole");
    }

    // `ROLE_QA` satisfies `dispatch`'s `slash_subrole` and `RpcCallerClass::Qa` gates
    // while still failing GM-only (`Gm`) and `SeniorGm` entries.
    sess.role = service_roles::ROLE_QA;
    if let Err(err) = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/help".into())],
            &kw,
            None,
        )
        .await
    {
        assert!(
            !err.message.contains("V24.01 GM/Admin service role bitmask"),
            "ROLE_QA must pass central dispatch.rs slash gate, got: {}",
            err.message
        );
    }
    let qa_hijack = ctx
        .dispatch_rpc(
            &mut sess,
            "serviceGatewayMgr",
            None,
            "qa_is_hijack_mode_enabled",
            &[],
            &kw,
            None,
        )
        .await
        .expect("ROLE_QA must pass Qa gate on serviceGatewayMgr.qa_is_hijack_mode_enabled");
    assert_eq!(qa_hijack, PyRep::Bool(false));
    let qa_unimpl = ctx
        .dispatch_rpc(&mut sess, "character", None, "QA_Probe", &[], &kw, None) // rpc-lint: allow unregistered dispatch test fixture
        .await
        .unwrap_err();
    assert_eq!(qa_unimpl.code, "UnimplementedRPC");

    // 3. Control-char sanitization in caller_trace + UnimplementedRPC
    let unimpl_err = ctx
        .dispatch_rpc(
            &mut sess,
            "nonexistentSvc", // rpc-lint: allow test fixture
            None,
            "NoSuchMethod",
            &[],
            &kw,
            Some("caller\nwith\rcontrol\tchars"),
        )
        .await
        .unwrap_err();
    assert_eq!(unimpl_err.code, "UnimplementedRPC");

    // 4. ClusterShuttingDown gate rejects non-GM gameplay RPCs while sparing exempt auth/petitioner & GM
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    let exempt_ping = ctx
        .dispatch_rpc(&mut sess, "authentication", None, "Ping", &[], &kw, None)
        .await;
    if let Err(err) = exempt_ping {
        assert_ne!(err.code, "ClusterShuttingDown");
    }
    let exempt_pet = ctx
        .dispatch_rpc(&mut sess, "petitioner", None, "MayPetition", &[], &kw, None)
        .await;
    if let Err(err) = exempt_pet {
        assert_ne!(err.code, "ClusterShuttingDown");
    }

    let mut gm_sess = sess.clone();
    gm_sess.role = service_roles::ROLE_GMH;
    let gm_node = ctx
        .dispatch_rpc(&mut gm_sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .expect("GM bypasses downtime gate");
    assert!(matches!(gm_node, PyRep::Integer(_)));

    let shut_err = ctx
        .dispatch_rpc(&mut sess, "invbroker", None, "MachoResolveObject", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(shut_err.code, "ClusterShuttingDown");
    assert_eq!(sess.user_id, None);
    assert_eq!(sess.character_id, None);
    assert_eq!(sess.corporation_id, None);
    assert_eq!(sess.alliance_id, None);
    assert_eq!(sess.solar_system_id, None);
}

#[tokio::test]
async fn dispatch_rpc_bind_edges_wal_rollback_and_sov_respawn_triggered() {
    use crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS;
    use eve_db::{
        CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
        UpwellStructureRecord,
    };

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    // 1. populationCap.MachoBindObject needs auth only (no character_id); invbroker needs character_id
    let mut auth_only = SessionState::new(94_120);
    auth_only.user_id = Some(1);
    let err_inv = ctx
        .dispatch_rpc(
            &mut auth_only,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_inv.code, "CharacterNotSelected");

    let err_pop_denied = ctx
        .dispatch_rpc(
            &mut auth_only,
            "populationCap",
            None,
            "MachoBindObject",
            &[PyRep::Integer(99_999_999), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_pop_denied.code, "BindAuthorizationDenied");

    let pop_ok = ctx
        .dispatch_rpc(
            &mut auth_only,
            "populationCap",
            None,
            "MachoBindObject",
            &[PyRep::Integer(90_000_001), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("populationCap binds with user_id only for owned character");
    assert!(matches!(pop_ok, PyRep::Tuple(_)));

    // 2. MachoBindObject tuple string key, empty tuple key, non-string piggyback method, and beyonce space warp-clear
    let mut sess = SessionState::new(94_121);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        let active_ship_id = ch.active_ship_id;
        if let Some(ship_item) = snap.items.get_mut(&active_ship_id) {
            ship_item.location_id = 30_000_142;
        }
        active_ship_id
    });
    let sys_handle = ctx.systems.get_or_spawn(30_000_142);
    sys_handle.insert_active_warp_for_test(ship_id, 60_003_760);

    let err_empty_tuple = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::String(String::new())]), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(err_empty_tuple.code, "InvalidArgument");

    let tuple_str_err = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::String("system".into())]), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(tuple_str_err.code, "BindAuthorizationDenied");

    let token_bind_err = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Token("agent_key".into()), PyRep::None],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(token_bind_err.code, "BindAuthorizationDenied");

    let agent_tuple_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[PyRep::Tuple(vec![PyRep::Integer(3_008_416)]), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("valid SDE agent tuple binds");
    assert!(matches!(agent_tuple_ok, PyRep::Tuple(_)));

    let bad_piggy = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(3_008_416),
                PyRep::Tuple(vec![PyRep::Integer(42)]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(bad_piggy.code, "InvalidArgument");

    let beyonce_ok = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30_000_142), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("beyonce bind in space");
    assert!(matches!(beyonce_ok, PyRep::Tuple(_)));
    assert!(!sys_handle.is_active_warp(ship_id));

    // 3. SQLite commit failure inside real `dispatch_rpc` rolls back the snapshot cleanly via CoW tx engine.
    let wal_db = std::env::temp_dir().join(format!(
        "evomu_dispatch_wal_rollback_{}_{:?}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let wal_db_wal = std::path::PathBuf::from(format!("{}-wal", wal_db.display()));
    let wal_db_shm = std::path::PathBuf::from(format!("{}-shm", wal_db.display()));
    let _ = std::fs::remove_file(&wal_db);
    let _ = std::fs::remove_file(&wal_db_wal);
    let _ = std::fs::remove_file(&wal_db_shm);
    let wal_repo = eve_db::WorldRepository::open_sqlite(&wal_db).expect("sqlite repo");
    let wal_ctx = ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), Arc::new(wal_repo));
    let mut wal_ch = ctx.db.get_character(90_000_001).unwrap();
    let wal_uid = wal_ch.user_id;
    let wal_corp_id = 98_000_001_i64;
    wal_ch.corporation_id = wal_corp_id;
    let mut wal_corp = ctx
        .db
        .with_snapshot(|snap| snap.corporations.get(&wal_corp_id).cloned())
        .unwrap_or_else(|| {
            let mut c = eve_db::CorporationRecord::default();
            c.corporation_id = wal_corp_id;
            c.name = "WAL Rollback Corp".into();
            c.ticker = "WALR".into();
            c
        });
    wal_corp.ceo_id = 90_000_001;
    wal_corp.members.insert(90_000_001);
    let wal_acct = ctx.db.with_snapshot(|snap| {
        snap.accounts
            .iter()
            .find(|(_, a)| a.user_id == wal_uid)
            .map(|(k, a)| (k.clone(), a.clone()))
    });
    wal_ctx.db.with_mut_snapshot(|snap| {
        if let Some((uname, acct)) = wal_acct {
            snap.accounts.insert(uname, acct);
        }
        snap.characters.insert(90_000_001, wal_ch);
        snap.corporations.insert(wal_corp_id, wal_corp);
    });
    wal_ctx
        .db
        .acquire_online_lock(wal_uid, 94_199, Some(90_000_001), true, wal_ctx.now_unix())
        .expect("online lock for wal_uid");
    let mut wal_sess = SessionState::new(94_199);
    wal_sess.user_id = Some(wal_uid);
    wal_sess.character_id = Some(90_000_001);
    wal_sess.corporation_id = Some(wal_corp_id);
    wal_sess.solar_system_id = Some(30_000_142);
    wal_ctx.notify(
        crate::router::NotificationTarget::Character(90_000_001),
        "PreExisting",
        "charid",
        vec![],
    );
    assert_eq!(wal_ctx.pending_notifications.read().unwrap()[&90_000_001].len(), 1);
    wal_ctx
        .scanned_signatures
        .write()
        .unwrap()
        .insert((90_000_001, 30_000_142, 777));
    wal_ctx
        .db
        .set_fault_plan_for_test(eve_db::TxFaultPlan::FailAtCommit);
    let wal_err = wal_ctx
        .dispatch_rpc(
            &mut wal_sess,
            "corpRegistry",
            None,
            "SetCorpWelcomeMail",
            &[PyRep::String("hello wal".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        wal_err.code.as_str(),
        "PersistenceFailure",
        "unexpected error code on failed SQLite dispatch: {:?}",
        wal_err
    );
    assert!(!wal_ctx
        .db
        .with_snapshot(|s| s.corp_governance.contains_key(&wal_corp_id)));
    assert_eq!(wal_ctx.pending_notifications.read().unwrap()[&90_000_001].len(), 1);
    let _ = wal_ctx.scanned_signatures.write().unwrap().clear();
    drop(wal_ctx);
    let _ = std::fs::remove_file(&wal_db);
    let _ = std::fs::remove_file(&wal_db_wal);
    let _ = std::fs::remove_file(&wal_db_shm);

    // 4. `tick_sov_prospecting_deposit_respawn` clears & respawns an elapsed SovOre anomaly when upgrade installed
    let null_sys = 30_000_144_u32;
    let dep = &SOV_PROSPECTING_DEPOSITS[0];
    let now = ctx.now_unix();
    let hub_id = 1_055_099_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32_458,
                owner_corp_id: 98_000_001,
                solar_system_id: null_sys,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                ..Default::default()
            },
        );
        snap.items.insert(
            1_055_099_002,
            InventoryItemRecord {
                item_id: 1_055_099_002,
                type_id: dep.upgrade_type_id,
                owner_id: 98_000_001,
                location_id: hub_id,
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.cosmic_anomalies.entry(null_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "DNG-OLD-SOV".into(),
            solar_system_id: null_sys,
            name: "Depleted Sov Deposit".into(),
            anomaly_type: "SovOre".into(),
            position: [0.0, 0.0, 0.0],
            signal_strength: 1.0,
            ore_types: vec![1230],
            remaining_volume: 100.0,
            created_unix: now.saturating_sub(dep.respawn_seconds + 100),
            lifetime_seconds: 0,
            cleared: false,
        });
        snap.cosmic_signatures.entry(null_sys).or_default().push(CosmicSignatureRecord {
            signature_id: "DNG-OLD-SOV".into(),
            solar_system_id: null_sys,
            sig_group: "SovOre".into(),
            type_name: "Depleted Sov Deposit".into(),
            signal_strength: 1.0,
            position: [0.0, 0.0, 0.0],
            wormhole_type_id: None,
            destination_system_id: None,
            created_unix: now.saturating_sub(dep.respawn_seconds + 100),
            lifetime_seconds: 0,
            max_mass_kg: 0.0,
            remaining_mass_kg: 0.0,
            max_jump_mass_kg: 0.0,
            collapsed: false,
        });
    });

    // First tick marks the depleted `cleared == false` anomaly cleared & collapses its signature
    tick_sov_prospecting_deposit_respawn(&ctx, null_sys).await;
    ctx.db.with_mut_snapshot(|snap| {
        let old_anom = snap
            .cosmic_anomalies
            .get_mut(&null_sys)
            .unwrap()
            .iter_mut()
            .find(|a| a.anomaly_id == "DNG-OLD-SOV")
            .unwrap();
        assert!(old_anom.cleared);
        assert_eq!(old_anom.lifetime_seconds, dep.respawn_seconds);
        let old_sig = snap.cosmic_signatures[&null_sys]
            .iter()
            .find(|s| s.signature_id == "DNG-OLD-SOV")
            .unwrap();
        assert!(old_sig.collapsed);
        assert_eq!(old_sig.signal_strength, 0.0);
        // Advance past respawn timer so second tick replaces it and spawns asteroids
        old_anom.created_unix = now.saturating_sub(dep.respawn_seconds + 100);
    });

    tick_sov_prospecting_deposit_respawn(&ctx, null_sys).await;
    ctx.db.with_snapshot(|snap| {
        let sov_anoms: Vec<_> = snap.cosmic_anomalies[&null_sys]
            .iter()
            .filter(|a| a.anomaly_type == "SovOre")
            .collect();
        assert_eq!(sov_anoms.len(), 1);
        assert_ne!(sov_anoms[0].anomaly_id, "DNG-OLD-SOV");
        assert!(!sov_anoms[0].cleared);
        assert!(snap.asteroids.values().any(|a| a.solar_system_id == null_sys && a.belt_id == 0));
    });
}

#[tokio::test]
async fn market_get_orders_cached_result_and_station_dock_undock_excludes_self() {
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let st_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut peer = snap.characters[&char_a].clone();
        peer.character_id = char_b;
        peer.user_id = 2;
        peer.station_id = Some(st_id);
        snap.characters.insert(char_b, peer);
    });
    ctx.db
        .acquire_online_lock(1, 95_001, Some(char_a), true, ctx.now_unix())
        .unwrap();
    ctx.db
        .acquire_online_lock(2, 95_002, Some(char_b), true, ctx.now_unix())
        .unwrap();

    let mut sess_a = SessionState::new(95_001);
    sess_a.user_id = Some(1);
    sess_a.character_id = Some(char_a);
    sess_a.station_id = Some(st_id);
    sess_a.solar_system_id = Some(30_000_142);
    sess_a.region_id = Some(10_000_002);
    sess_a.live_notify_listener = true;
    let kw = HashMap::new();

    // 1. marketProxy.GetOrders & GetPlexOrders over machoNet:26000 return V24 CachedMethodCallResult (#954, #789)
    for (method, call_args) in [
        ("GetOrders", vec![PyRep::Integer(34)]),
        ("GetPlexOrders", vec![]),
    ] {
        let rep = ctx
            .dispatch_rpc(
                &mut sess_a,
                "marketProxy",
                None,
                method,
                &call_args,
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .expect(method);
        match rep {
            PyRep::Object { type_name, args } => {
                assert_eq!(type_name, eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS);
                if let PyRep::Tuple(items) = *args {
                    assert_eq!(items.len(), 3);
                    if let PyRep::Dict(entries) = &items[0] {
                        let vc = entries
                            .iter()
                            .find(|(k, _)| k.as_str() == Some("versionCheck"))
                            .map(|(_, v)| v);
                        assert_eq!(
                            vc,
                            Some(&PyRep::Tuple(vec![
                                PyRep::String("always".into()),
                                PyRep::None,
                                PyRep::None,
                            ]))
                        );
                    } else {
                        panic!("expected versionCheck dict in CachedMethodCallResult");
                    }
                    assert!(matches!(items[1], PyRep::SubStream(_)));
                } else {
                    panic!("expected 3-tuple args in CachedMethodCallResult");
                }
            }
            other => panic!("expected V24 CachedMethodCallResult, got {other:?}"),
        }
    }

    // 2. Undock and Dock emit OnCharNoLongerInStation / OnCharNowInStation to station peers but NOT to self (#790)
    let _ = ctx.drain_notifications_for_char(char_a);
    let _ = ctx.drain_notifications_for_char(char_b);
    let _ = ctx
        .dispatch_rpc(&mut sess_a, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");
    let notifs_a_undock = ctx.drain_notifications_for_char(char_a);
    let notifs_b_undock = ctx.drain_notifications_for_char(char_b);
    assert!(
        !notifs_a_undock.iter().any(|n| n.event == "OnCharNoLongerInStation"),
        "Undocking character must not receive self OnCharNoLongerInStation"
    );
    assert!(
        notifs_b_undock.iter().any(|n| n.event == "OnCharNoLongerInStation"),
        "Station peer must receive OnCharNoLongerInStation"
    );

    let _ = ctx.drain_notifications_for_char(char_a);
    let _ = ctx.drain_notifications_for_char(char_b);
    let _ = ctx
        .dispatch_rpc(
            &mut sess_a,
            "stationSvc",
            None,
            "Dock",
            &[PyRep::Integer(st_id)],
            &kw,
            None,
        )
        .await
        .expect("Dock");
    let notifs_a_dock = ctx.drain_notifications_for_char(char_a);
    let notifs_b_dock = ctx.drain_notifications_for_char(char_b);
    assert!(
        !notifs_a_dock.iter().any(|n| n.event == "OnCharNowInStation"),
        "Docking character must not receive self OnCharNowInStation"
    );
    assert!(
        notifs_b_dock.iter().any(|n| n.event == "OnCharNowInStation"),
        "Station peer must receive OnCharNowInStation"
    );
}

#[tokio::test]
async fn maybe_emit_login_sim_clock_rebase_stays_client_anchored_across_advancetime() {
    let base = 1_750_000_000_u64;
    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.set_clock(Arc::new(eve_db::VirtualServerClock::with_fixed_base(base)));
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
    let user_id = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(user_id, 9_101, Some(char_id), true, base)
        .unwrap();
    let handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Un-dilated ballpark caught up to real client wallclock must NOT emit a bogus
    //    DoSimClockRebase when `/advancetime` shifts server virtual time +12.95 days ahead.
    handle.ballpark.write().unwrap().sim_unix_secs = base;
    ctx.advance_secs_without_sim_clock_rebase(1_119_223);
    let _ = ctx.drain_notifications_for_char(char_id);
    maybe_emit_login_sim_clock_rebase(&ctx, char_id, sys_id);
    assert!(
        !ctx.drain_notifications_for_char(char_id)
            .iter()
            .any(|n| n.event == "DoSimClockRebase"),
        "/advancetime alone must not trigger negative DoSimClockRebase on login"
    );

    // 2. Dilated/anchored system after `/advancetime` emits DoSimClockRebase anchored to client
    //    wallclock (within seconds of now_machonet_wallclock_filetime_ticks), never +12.95 days.
    crate::services::serverops_cluster::set_system_tidi_factor(&ctx, sys_id, 0.50);
    let _ = ctx.drain_notifications_for_char(char_id);
    ctx.advance_secs(10); // 10s real/rebased wall advance => sim clock ~5s behind wallclock
    let _ = ctx.drain_notifications_for_char(char_id);
    maybe_emit_login_sim_clock_rebase(&ctx, char_id, sys_id);
    let rebases: Vec<_> = ctx
        .drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == "DoSimClockRebase")
        .collect();
    assert_eq!(rebases.len(), 1, "expected single login DoSimClockRebase for dilated system");
    let PyRep::Tuple(times) = &rebases[0].args[0] else {
        panic!("expected 2-tuple (oldSimTime, newSimTime)");
    };
    let old_sim = times[0].as_i64().unwrap();
    let new_sim = times[1].as_i64().unwrap();
    let client_ft = ctx.now_machonet_wallclock_filetime_ticks();
    let day_ticks: i64 = 86_400 * 10_000_000;
    assert!(old_sim > new_sim);
    assert!(
        old_sim - client_ft < day_ticks && (old_sim - client_ft).abs() < 5 * 10_000_000,
        "login DoSimClockRebase oldSimTime drifted from client clock"
    );
    assert!(
        new_sim - client_ft < day_ticks && (client_ft - new_sim).abs() < 15 * 10_000_000,
        "login DoSimClockRebase newSimTime drifted from client clock"
    );
}

#[tokio::test]
async fn ship_macho_bind_dirt_timestamp_grace_survives_undock_and_slash_moveme_while_blocking_mutations()
{
    use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
    use std::sync::OnceLock;

    static SHARED_DISPATCH_SDE: OnceLock<Arc<eve_sde::SdeRegistry>> = OnceLock::new();
    fn test_ctx_with_sde() -> ServerContext {
        if let Some(sde) = SHARED_DISPATCH_SDE.get() {
            ServerContext::with_sde_and_repository(
                Arc::clone(sde),
                Arc::new(WorldRepository::with_demo_seed()),
            )
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
        } else {
            let ctx = ServerContext::new();
            let _ = SHARED_DISPATCH_SDE.set(Arc::clone(&ctx.sde));
            ctx
        }
    }

    fn unwrap_bound_id(rep: &PyRep) -> u64 {
        match rep {
            PyRep::SubStruct(inner) | PyRep::SubStream(inner) => unwrap_bound_id(inner),
            PyRep::Tuple(items) => items[0]
                .as_str()
                .and_then(|s| s.rsplit_once(':'))
                .and_then(|(_, right)| right.parse::<u64>().ok())
                .expect("numeric bound id in oid_rep"),
            other => panic!("expected SubStruct/SubStream oid_rep, got {other:?}"),
        }
    }

    let ctx = test_ctx_with_sde();
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_800_000_000)));

    let char_id = 90_000_001_i64;
    let other_char = 90_000_002_i64;
    let user_id = 1_u64;
    let sid = 96_150_u64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let other_st = ctx
        .sde
        .stations
        .values()
        .find(|s| s.station_id != st_id && s.solar_system_id != 30_000_144)
        .expect("alternate station");
    let ship_1 = 1_897_301_923_i64;
    let ship_2 = 1_897_147_421_i64;
    let foreign_ship = 1_897_999_999_i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = Some(st_id);
            ch.structure_id = None;
            ch.active_ship_id = ship_1;
            ch.home_station_id = st_id;
        }
        for (item_id, owner_id) in [(ship_1, char_id), (ship_2, char_id), (foreign_ship, other_char)]
        {
            snap.items.insert(
                item_id,
                InventoryItemRecord {
                    item_id,
                    type_id: 587,
                    owner_id,
                    location_id: st_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(user_id, sid, Some(char_id), false, ctx.now_unix())
        .expect("acquire online lock");

    let mut sess = SessionState::new(sid);
    sess.user_id = Some(user_id);
    sess.role = service_roles::ROLE_GMH;
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.station_id = Some(st_id);
    sess.ship_id = Some(ship_1);
    let kw = HashMap::new();

    // Failed piggybacked call during docked MachoBindObject must not leak bound_objects.
    let pre_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetDirtTimestamp".into()),
                    PyRep::Tuple(vec![PyRep::Integer(999_999_999)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(pre_err.code, "ItemNotFound");
    assert!(
        ctx.bound_objects.read().unwrap().is_empty(),
        "failed docked MachoBindObject must not leak bound object"
    );

    // Pre-bound station ship handle (`pre_ship_bid`) plus immediate post-Undock MachoBindObject (#1150).
    let pre_ship_bid = ctx.bind_object(&sess, "ship", st_id).expect("pre-bind docked ship");
    ctx.dispatch_rpc(
        &mut sess,
        "ship",
        None,
        "Undock",
        &[],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("ship.Undock");
    assert!(sess.station_id.is_none());
    assert_eq!(
        ctx.verify_bound_object(&sess, pre_ship_bid).unwrap_err().code,
        "InvalidBoundObject"
    );

    // Pre-bound disconnected ship handle can still read GetDirtTimestamp within 30s grace.
    let pre_dirt = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            Some(pre_ship_bid),
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_1)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("pre-bound GetDirtTimestamp within 30s grace");
    assert!(pre_dirt.as_i64().is_some());
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "ship",
            Some(pre_ship_bid),
            "Board",
            &[PyRep::Integer(ship_2)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBoundObject"
    );

    // Fresh ship.MachoBindObject((60003760, 15), ('GetDirtTimestamp', (ship,), {})) after Undock succeeds.
    for target_ship in [ship_1, ship_2] {
        let rep = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "MachoBindObject",
                &[
                    PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(15)]),
                    PyRep::Tuple(vec![
                        PyRep::String("GetDirtTimestamp".into()),
                        PyRep::Tuple(vec![PyRep::Integer(target_ship)]),
                        PyRep::Dict(Vec::new()),
                    ]),
                ],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .expect("MachoBindObject(60003760) + GetDirtTimestamp immediately after Undock");
        let PyRep::Tuple(items) = rep else {
            panic!("expected (oid_rep, dirt_ts) tuple");
        };
        assert_eq!(items.len(), 2);
        assert!(items[1].as_i64().is_some());
        let grace_bid = unwrap_bound_id(&items[0]);
        assert!(
            !ctx.bound_objects.read().unwrap().contains_key(&grace_bid),
            "grace MachoBindObject must never insert into active bound_objects"
        );
        // Subsequent ObjectCall retry on grace_bid succeeds for GetDirtTimestamp only.
        assert!(
            ctx.dispatch_rpc(
                &mut sess,
                "",
                Some(grace_bid),
                "GetDirtTimestamp",
                &[PyRep::Integer(target_ship)],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .is_ok()
        );
        assert_eq!(
            ctx.dispatch_rpc(
                &mut sess,
                "ship",
                Some(grace_bid),
                "ResetDirtTimestamp",
                &[PyRep::Integer(target_ship)],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .unwrap_err()
            .code,
            "InvalidBoundObject"
        );
    }

    // Security negatives right after Undock: bare bind, mutations, foreign ship, unrelated station.
    let no_call_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(15)]),
                PyRep::None,
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(no_call_err.code, "BindAuthorizationDenied");

    for (blocked_method, blocked_args) in [
        ("Board", vec![PyRep::Integer(ship_2)]),
        ("ResetDirtTimestamp", vec![PyRep::Integer(ship_1)]),
        ("TrashItems", vec![PyRep::List(vec![PyRep::Integer(ship_2)])]),
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "MachoBindObject",
                &[
                    PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(15)]),
                    PyRep::Tuple(vec![
                        PyRep::String(blocked_method.into()),
                        PyRep::Tuple(blocked_args),
                        PyRep::Dict(Vec::new()),
                    ]),
                ],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.code, "BindAuthorizationDenied",
            "{blocked_method} must be rejected post-Undock"
        );
    }

    let foreign_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetDirtTimestamp".into()),
                    PyRep::Tuple(vec![PyRep::Integer(foreign_ship)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(foreign_err.code, "BindAuthorizationDenied");

    let unrelated_st_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(other_st.station_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetDirtTimestamp".into()),
                    PyRep::Tuple(vec![PyRep::Integer(ship_1)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(unrelated_st_err.code, "BindAuthorizationDenied");

    // Grace expires after >30s.
    ctx.advance_secs(31);
    let expired_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(st_id), PyRep::Integer(15)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetDirtTimestamp".into()),
                    PyRep::Tuple(vec![PyRep::Integer(ship_1)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(expired_err.code, "BindAuthorizationDenied");
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "ship",
            Some(pre_ship_bid),
            "GetDirtTimestamp",
            &[PyRep::Integer(ship_1)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBoundObject"
    );

    // Part B (#1152 / #1153): `/moveme 30000144` from both home station and non-home station
    // authorizes stale GetDirtTimestamp via transition grace while still denying Board.
    for (dock_st, dock_sys) in [(st_id, sys_id), (other_st.station_id, other_st.solar_system_id)] {
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = dock_sys;
                ch.station_id = Some(dock_st);
                ch.structure_id = None;
            }
            if let Some(ship) = snap.items.get_mut(&ship_1) {
                ship.location_id = dock_st;
            }
        });
        sess.solar_system_id = Some(dock_sys);
        sess.station_id = Some(dock_st);
        sess.structure_id = None;
        ctx.invalidate_bound_objects_for_session_transition(
            char_id,
            dock_sys,
            Some(dock_st),
            "dock",
        );

        ctx.dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/moveme 30000144".into())],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("/moveme 30000144");
        assert_eq!(sess.solar_system_id, Some(30_000_144));
        assert!(sess.station_id.is_none());

        let moveme_dirt = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "MachoBindObject",
                &[
                    PyRep::Tuple(vec![PyRep::Integer(dock_st), PyRep::Integer(15)]),
                    PyRep::Tuple(vec![
                        PyRep::String("GetDirtTimestamp".into()),
                        PyRep::Tuple(vec![PyRep::Integer(ship_1)]),
                        PyRep::Dict(Vec::new()),
                    ]),
                ],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .expect("MachoBindObject(departed_dock) + GetDirtTimestamp immediately after /moveme");
        let PyRep::Tuple(items) = moveme_dirt else {
            panic!("expected (oid_rep, dirt_ts) tuple");
        };
        assert!(items[1].as_i64().is_some());

        let moveme_board_err = ctx
            .dispatch_rpc(
                &mut sess,
                "ship",
                None,
                "MachoBindObject",
                &[
                    PyRep::Tuple(vec![PyRep::Integer(dock_st), PyRep::Integer(15)]),
                    PyRep::Tuple(vec![
                        PyRep::String("Board".into()),
                        PyRep::Tuple(vec![PyRep::Integer(ship_2)]),
                        PyRep::Dict(Vec::new()),
                    ]),
                ],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .unwrap_err();
        assert_eq!(moveme_board_err.code, "BindAuthorizationDenied");
    }

    // Part C (#1163): `/moveme 30000142` while docked in an Upwell structure (`station_id = None`,
    // `structure_id = Some(struct_id)`) records departed-dock grace for stale `GetDirtTimestamp`
    // and pre-bound `invbroker.GetSelfInvItem`, clears `structure_id` + ship `flag`, and blocks `Board`/`List`.
    let struct_id = 1_001_896_944_936_i64;
    let struct_sys = 30_000_144_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: struct_sys,
                name: "1DQ1-A Keepstar".into(),
                position: [12_000_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1.0,
                armor_hp: 1.0,
                hull_hp: 1.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = struct_sys;
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.active_ship_id = ship_1;
        }
        for id in [ship_1, ship_2] {
            if let Some(ship) = snap.items.get_mut(&id) {
                ship.location_id = struct_id;
                ship.flag = InventoryFlag::Hangar;
            }
        }
    });
    sess.solar_system_id = Some(struct_sys);
    sess.station_id = None;
    sess.structure_id = Some(struct_id);
    sess.ship_id = Some(ship_1);
    ctx.invalidate_bound_objects_for_session_transition(
        char_id,
        struct_sys,
        Some(struct_id),
        "dock",
    );

    let pre_struct_inv_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(struct_id), PyRep::None],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("bind structure invbroker while docked");
    let PyRep::Tuple(inv_bind_items) = pre_struct_inv_rep else {
        panic!("expected (oid_rep, res) tuple from invbroker.MachoBindObject");
    };
    let pre_struct_inv_bid = unwrap_bound_id(&inv_bind_items[0]);

    ctx.dispatch_rpc(
        &mut sess,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/moveme 30000142".into())],
        &kw,
        Some("machoNet:26000"),
    )
    .await
    .expect("/moveme 30000142 from docked Upwell structure");
    assert_eq!(sess.solar_system_id, Some(30_000_142));
    assert!(sess.station_id.is_none());
    assert!(sess.structure_id.is_none());
    assert_eq!(
        ctx.verify_bound_object(&sess, pre_struct_inv_bid)
            .unwrap_err()
            .code,
        "InvalidBoundObject"
    );

    assert!(
        ctx.dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(pre_struct_inv_bid),
            "GetSelfInvItem",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .is_ok(),
        "pre-bound structure GetSelfInvItem() must succeed within 30s grace"
    );
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "machoNet",
            Some(pre_struct_inv_bid),
            "List",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err()
        .code,
        "InvalidBoundObject"
    );

    let struct_moveme_dirt = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(struct_id), PyRep::Integer(65)]),
                PyRep::Tuple(vec![
                    PyRep::String("GetDirtTimestamp".into()),
                    PyRep::Tuple(vec![PyRep::Integer(ship_1)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("MachoBindObject(departed_structure) + GetDirtTimestamp immediately after /moveme");
    let PyRep::Tuple(items) = struct_moveme_dirt else {
        panic!("expected (oid_rep, dirt_ts) tuple");
    };
    assert!(items[1].as_i64().is_some());

    let struct_moveme_board_err = ctx
        .dispatch_rpc(
            &mut sess,
            "ship",
            None,
            "MachoBindObject",
            &[
                PyRep::Tuple(vec![PyRep::Integer(struct_id), PyRep::Integer(65)]),
                PyRep::Tuple(vec![
                    PyRep::String("Board".into()),
                    PyRep::Tuple(vec![PyRep::Integer(ship_2)]),
                    PyRep::Dict(Vec::new()),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap_err();
    assert_eq!(struct_moveme_board_err.code, "BindAuthorizationDenied");

    assert!(
        !ctx.bound_objects
            .read()
            .unwrap()
            .values()
            .any(|e| {
                e.service == "ship"
                    && (e.object_id == st_id
                        || e.object_id == other_st.station_id
                        || e.object_id == struct_id)
            }),
        "station-scoped or structure-scoped ship bindings must never linger in active bound_objects after transitions"
    );
}

#[tokio::test]
async fn dispatch_rpc_p0_1350_preauth_gm_role_and_moniker_gates() {
    use eve_db::{CorporationRecord, InventoryFlag, InventoryItemRecord, TourneyMatch, TourneyPilot, WarRecord};

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let kw = HashMap::new();

    // 1. Central pre-auth gate (`session.user_id.is_none()`): reject gameplay/service RPCs
    //    without mutating state while permitting genuine pre-auth endpoints.
    let before_preauth = ctx.db.snapshot_tables();
    let mut unauth_sess = SessionState::new(94_500);
    for (svc, method, args) in [
        ("charUnboundMgr", "GetNumCharacters", vec![]),
        ("config", "GetMap", vec![PyRep::Integer(30_000_142)]),
        ("stationSvc", "GetStation", vec![PyRep::Integer(60_003_760)]),
        ("wormholeMgr", "GetWormholePolarization", vec![PyRep::Integer(30_000_142)]),
        ("timeControl", "AdvanceClock", vec![PyRep::Integer(1)]),
        ("publicQaToolsServer", "CanGiveItemForMultifit", vec![]),
        ("publicQaToolsServer", "MoveMeTo", vec![]),
        ("slash", "SlashCmd", vec![PyRep::String("/moveme".into())]),
        ("invbroker", "MachoResolveObject", vec![]),
        ("keeper", "MachoBindObject", vec![PyRep::Integer(30_000_142), PyRep::None]),
        ("warRegistry", "GetWars", vec![]),
        ("machoNet", "GetConnectedClusterMode", vec![]),
        ("machoNet", "GetConnectedClusterName", vec![]),
        ("machoNet", "GetConnectionProperties", vec![]),
        ("petitioner", "MayPetition", vec![]),
        ("pingService", "Ping", vec![]),
        ("ship", "ClientHasReleasedTheseObjects", vec![]),
    ] {
        let err = ctx
            .dispatch_rpc(&mut unauth_sess, svc, None, method, &args, &kw, None)
            .await
            .expect_err(&format!("{svc}.{method} must require authentication"));
        assert_eq!(err.code, "Unauthorized", "{svc}.{method}");
    }
    assert_eq!(ctx.db.snapshot_tables(), before_preauth);
    assert!(ctx.bound_objects.read().unwrap().is_empty());

    for (svc, method, args) in [
        ("authentication", "Ping", vec![]),
        ("machoNet", "GetTime", vec![]),
        ("machoNet", "GetServiceInfo", vec![]),
        ("machoNet", "NoBlock", vec![]),
        ("machoNet", "GetGlobalConfig", vec![]),
        ("machoNet", "ClientHasReleasedTheseObjects", vec![]),
        ("cache", "GetServerVersionAndBuild", vec![]),
    ] {
        let res = ctx
            .dispatch_rpc(&mut unauth_sess, svc, None, method, &args, &kw, None)
            .await;
        if let Err(err) = res {
            assert_ne!(
                err.code, "Unauthorized",
                "genuine pre-auth endpoint {svc}.{method} must not fail Unauthorized"
            );
        }
    }

    // 2. GM / QA camelCase role gates reject player roles and ROLE_QA without state mutation,
    //    allowing ROLE_GMH, ROLE_GML, and ROLE_ADMIN.
    eve_testkit::legal_fixture::seed_legal_corp_member(
        &ctx.db,
        90_000_001,
        98_000_001,
        "Dispatch Test Corp",
        "DTCP",
        true,
        u64::MAX,
        None,
    );
    let mut sess = SessionState::new(94_501);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    ctx.sync_session(&mut sess);

    let before_gm = ctx.db.snapshot_tables();
    for bad_role in [0u64, service_roles::ROLE_PLAYER_DEFAULT, service_roles::ROLE_QA] {
        sess.role = bad_role;
        for (svc, method) in [
            ("warRegistry", "GMActivateDefender"),
            ("warRegistry", "GMDeactivateDefender"),
            ("warRegistry", "GMExtendAllyContract"),
            ("warRegistry", "GMJoinDefender"),
            ("warRegistry", "GMSetDeclareTime"),
            ("warRegistry", "GMSetWarFinishTime"),
            ("charUnboundMgr", "ToggleValidation"),
            ("charUnboundMgr", "GetQAStarterSystemIDs"),
        ] {
            let err = ctx
                .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
                .await
                .expect_err(&format!("{svc}.{method} must deny role {bad_role:#x}"));
            assert_eq!(err.code, "InsufficientRole", "{svc}.{method} role={bad_role:#x}");
        }
    }
    assert_eq!(ctx.db.snapshot_tables(), before_gm);

    let gm_war_id = 700_910_099_i64;
    let gm_ally_corp = 98_999_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            gm_ally_corp,
            {
                let mut c = CorporationRecord::default();
                c.corporation_id = gm_ally_corp;
                c.name = "GM Test Ally".into();
                c.ticker = "GMA".into();
                c.ceo_id = 90_999_001;
                c
            },
        );
        snap.wars.insert(
            gm_war_id,
            WarRecord {
                war_id: gm_war_id,
                aggressor_corp_id: 98_000_001,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: 100,
                active: true,
            },
        );
    });
    for ok_role in [
        service_roles::ROLE_GMH,
        service_roles::ROLE_GML,
        service_roles::ROLE_GMS,
        service_roles::ROLE_ADMIN,
    ] {
        sess.role = ok_role;
        let now_u = ctx.now_unix();
        for (method, gm_args) in [
            ("GMJoinDefender", vec![PyRep::Integer(gm_war_id), PyRep::Integer(gm_ally_corp)]),
            (
                "GMExtendAllyContract",
                vec![
                    PyRep::Integer(gm_war_id),
                    PyRep::Integer(gm_ally_corp),
                    PyRep::Integer(86_400),
                ],
            ),
            ("GMActivateDefender", vec![PyRep::Integer(gm_war_id), PyRep::Integer(gm_ally_corp)]),
            ("GMDeactivateDefender", vec![PyRep::Integer(gm_war_id), PyRep::Integer(gm_ally_corp)]),
            (
                "GMSetDeclareTime",
                vec![
                    PyRep::Integer(gm_war_id),
                    PyRep::Integer(now_u.saturating_sub(1_000) as i64),
                ],
            ),
            (
                "GMSetWarFinishTime",
                vec![
                    PyRep::Integer(gm_war_id),
                    PyRep::Integer((now_u + 14_400) as i64),
                ],
            ),
        ] {
            assert_eq!(
                ctx.dispatch_rpc(&mut sess, "warRegistry", None, method, &gm_args, &kw, None)
                    .await
                    .expect("GM warRegistry call"),
                PyRep::Bool(true)
            );
        }
        assert!(matches!(
            ctx.dispatch_rpc(
                &mut sess,
                "charUnboundMgr",
                None,
                "GetQAStarterSystemIDs",
                &[],
                &kw,
                None
            )
            .await
            .expect("GM GetQAStarterSystemIDs"),
            PyRep::List(_)
        ));
        assert!(matches!(
            ctx.dispatch_rpc(
                &mut sess,
                "charUnboundMgr",
                None,
                "ToggleValidation",
                &[],
                &kw,
                None
            )
            .await
            .expect("GM ToggleValidation"),
            PyRep::Bool(_)
        ));
    }

    // 3. MachoResolveObject & MachoBindObject scoping on keeper, tourneyMgr, planetMgr,
    //    agentMgr, officeManager, warStatisticMgr, and invbroker.
    let valid_dungeon_id = ctx
        .sde
        .dungeons
        .keys()
        .next()
        .copied()
        .expect("SDE dungeons seeded");
    let jita_planet = ctx
        .sde
        .solar_systems
        .get(&30_000_142)
        .and_then(|s| s.planet_ids.first().copied())
        .expect("Jita planet");
    let foreign_planet = ctx
        .sde
        .solar_systems
        .get(&30_000_144)
        .and_then(|s| s.planet_ids.first().copied())
        .unwrap_or(40_999_999);
    let war_own_id = 700_910_001_i64;
    let war_foreign_id = 700_910_002_i64;
    let tourney_match_id = 800_910_001_i64;
    let other_char_item_id = 880_910_001_i64;
    let rank_char_id = 90_000_777_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut rank_ch = snap.characters[&90_000_001].clone();
        rank_ch.character_id = rank_char_id;
        rank_ch.user_id = 777;
        rank_ch.corporation_id = 98_000_001;
        rank_ch.station_id = Some(60_003_760);
        rank_ch.structure_id = None;
        rank_ch.solar_system_id = 30_000_142;
        snap.characters.insert(rank_char_id, rank_ch);
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.members.insert(rank_char_id);
            corp.member_roles.insert(rank_char_id, 0);
        }
        snap.corporations.insert(
            98_888_111,
            {
                let mut c = CorporationRecord::default();
                c.corporation_id = 98_888_111;
                c.name = "Other Corp A".into();
                c.ticker = "OCA".into();
                c.ceo_id = 90_999_111;
                c
            },
        );
        snap.corporations.insert(
            98_888_222,
            {
                let mut c = CorporationRecord::default();
                c.corporation_id = 98_888_222;
                c.name = "Other Corp B".into();
                c.ticker = "OCB".into();
                c.ceo_id = 90_999_222;
                c
            },
        );
        snap.wars.insert(
            war_own_id,
            WarRecord {
                war_id: war_own_id,
                aggressor_corp_id: 98_000_001,
                defender_corp_id: 98_888_111,
                mutual: false,
                started_unix: 10,
                active: true,
            },
        );
        snap.wars.insert(
            war_foreign_id,
            WarRecord {
                war_id: war_foreign_id,
                aggressor_corp_id: 98_888_111,
                defender_corp_id: 98_888_222,
                mutual: false,
                started_unix: 10,
                active: true,
            },
        );
        let mut m = TourneyMatch::new(tourney_match_id, 77, 30_000_144, 100, 600, 120, 1);
        m.red_players.push(TourneyPilot {
            character_id: 90_000_001,
            ship_type_id: 587,
            ship_id: 0,
            points: 5,
        });
        snap.tourney_state.matches.insert(tourney_match_id, m);
        snap.items.insert(
            other_char_item_id,
            InventoryItemRecord {
                item_id: other_char_item_id,
                type_id: 587,
                owner_id: rank_char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    ctx.bound_objects.write().unwrap().clear();

    // keeper & planetMgr & tourneyMgr MachoResolveObject checks
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None
        )
        .await
        .is_ok());
    for bad_keeper in [1i64, 30_000_144, i64::from(valid_dungeon_id)] {
        assert_eq!(
            ctx.dispatch_rpc(
                &mut sess,
                "keeper",
                None,
                "MachoResolveObject",
                &[PyRep::Integer(bad_keeper)],
                &kw,
                None
            )
            .await
            .unwrap_err()
            .code,
            "BindAuthorizationDenied"
        );
    }
    sess.role = service_roles::ROLE_GMH;
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(i64::from(valid_dungeon_id))],
            &kw,
            None
        )
        .await
        .is_ok());
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;

    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(jita_planet)],
            &kw,
            None
        )
        .await
        .is_ok());
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "planetMgr",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(foreign_planet)],
            &kw,
            None
        )
        .await
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );

    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "tourneyMgr",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(tourney_match_id)],
            &kw,
            None
        )
        .await
        .is_ok());
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "tourneyMgr",
            None,
            "MachoResolveObject",
            &[PyRep::Integer(1)],
            &kw,
            None
        )
        .await
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );

    // Non-participant/cross-owner MachoBindObject rejections must leave zero new bindings
    let mut rank_sess = SessionState::new(94_502);
    rank_sess.user_id = Some(777);
    rank_sess.character_id = Some(rank_char_id);
    rank_sess.corporation_id = Some(98_000_001);
    rank_sess.station_id = Some(60_003_760);
    rank_sess.solar_system_id = Some(30_000_142);

    let denied_binds: Vec<(&str, i64)> = vec![
        ("keeper", 1),
        ("agentMgr", 999_999_999),
        ("tourneyMgr", tourney_match_id), // rank_char_id is not participant
        ("warStatisticMgr", war_foreign_id),
        ("officeManager", 98_888_111), // cross-corp officeManager bind denied
    ];
    for (svc, oid) in denied_binds {
        assert_eq!(
            ctx.dispatch_rpc(
                &mut rank_sess,
                svc,
                None,
                "MachoBindObject",
                &[PyRep::Integer(oid), PyRep::None],
                &kw,
                None
            )
            .await
            .unwrap_err()
            .code,
            "BindAuthorizationDenied",
            "{svc}({oid}) must be denied"
        );
    }
    assert_eq!(
        ctx.dispatch_rpc(
            &mut sess,
            "invbroker",
            None,
            "MachoBindObject",
            &[PyRep::Integer(other_char_item_id), PyRep::None],
            &kw,
            None
        )
        .await
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );
    assert!(ctx.bound_objects.read().unwrap().is_empty());

    // Positive authorized binds
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "warStatisticMgr",
            None,
            "MachoBindObject",
            &[PyRep::Integer(war_own_id), PyRep::None],
            &kw,
            None
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut sess,
            "officeManager",
            None,
            "MachoBindObject",
            &[PyRep::Integer(98_000_001), PyRep::None],
            &kw,
            None
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut rank_sess,
            "officeManager",
            None,
            "MachoBindObject",
            &[PyRep::Integer(98_000_001), PyRep::None],
            &kw,
            None
        )
        .await
        .is_ok());
    assert!(ctx
        .dispatch_rpc(
            &mut rank_sess,
            "officeManager",
            None,
            "MachoBindObject",
            &[PyRep::Integer(60_003_760), PyRep::None],
            &kw,
            None
        )
        .await
        .is_ok());
    let mut undocked_rank_sess = rank_sess.clone();
    undocked_rank_sess.station_id = None;
    undocked_rank_sess.structure_id = None;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&rank_char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    assert_eq!(
        ctx.dispatch_rpc(
            &mut undocked_rank_sess,
            "officeManager",
            None,
            "MachoBindObject",
            &[PyRep::Integer(98_000_001), PyRep::None],
            &kw,
            None
        )
        .await
        .unwrap_err()
        .code,
        "BindAuthorizationDenied"
    );
}

#[tokio::test]
async fn dispatch_rpc_p0_1393_preauth_quasar_framed_and_feature_flags_gates() {
    use crate::quasar_gateway::STATUS_UNAUTHORIZED;
    use eve_proto::quasar::{
        EvePublicRequest, GoogleAny, GoogleEmpty, GoogleTimestamp, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };

    let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut unauth_sess = SessionState::new(94_991);
    let kw = HashMap::new();

    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: ctx.now_unix() as i64,
            nanos: 0,
        }),
        correlation_uuid: vec![0x42; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"),
            value: GoogleEmpty.encode_proto(),
        }),
        external_origin: "client".into(),
        ..Default::default()
    };
    let req_bytes = req.encode_proto();

    // 1. Feature flags must gate before require_auth even for unauthenticated sessions and framed Quasar payloads
    {
        let mut flags = ctx.feature_flags.write().unwrap();
        flags.enable_living_world = false;
        flags.enable_quasar_gateway = false;
    }
    let tale_err = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "taleMgr",
            None,
            "GetGlobalWorldEventTales", // rpc-lint: allow verified taleMgr method
            &[],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(tale_err.code, "FeatureDisabled"); // rpc-lint: allow error code assertion

    for args in [Vec::new(), vec![PyRep::Bytes(req_bytes.clone())]] {
        let q_err = ctx
            .dispatch_rpc(
                &mut unauth_sess,
                "publicGatewaySvc",
                None,
                "SendRequest",
                &args,
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(q_err.code, "FeatureDisabled"); // rpc-lint: allow error code assertion
    }

    {
        let mut flags = ctx.feature_flags.write().unwrap();
        flags.enable_living_world = true;
        flags.enable_quasar_gateway = true;
    }

    // 2. Framed publicGatewaySvc request payloads bypass MachoNet require_auth and return
    //    structured STATUS_UNAUTHORIZED (401) without mutating DB or bound-object state
    let before_chars = ctx.db.with_snapshot(|s| s.characters.len());
    let mut kw_req = HashMap::new();
    kw_req.insert("request".into(), PyRep::Bytes(req_bytes.clone()));
    for (method, args, call_kw) in [
        ("SendRequest", vec![PyRep::Bytes(req_bytes.clone())], &kw),
        ("SendRequest", vec![PyRep::Buffer(req_bytes.clone())], &kw),
        ("Send", Vec::new(), &kw_req),
        ("Send", vec![PyRep::Buffer(req_bytes.clone())], &kw),
    ] {
        let rep = ctx
            .dispatch_rpc(
                &mut unauth_sess,
                "publicGatewaySvc",
                None,
                method,
                &args,
                call_kw,
                None,
            )
            .await
            .expect("framed unauth Quasar call returns KeyVal response");
        assert_eq!(
            rep.dict_get("status_code"),
            Some(&PyRep::Integer(i64::from(STATUS_UNAUTHORIZED))),
            "framed unauth {method} must return STATUS_UNAUTHORIZED"
        );
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.characters.len()), before_chars);
    assert!(ctx.bound_objects.read().unwrap().is_empty());

    // 3. Unframed publicGatewaySvc calls and all notice/event RPCs stay hard-gated by Unauthorized
    for (method, args) in [
        ("SendRequest", Vec::new()),
        ("SendRequest", vec![PyRep::Integer(90_000_001)]),
        ("Send", Vec::new()),
        ("Send", vec![PyRep::Integer(90_000_001)]),
        ("PollNotices", Vec::new()),
        ("Subscribe", Vec::new()),
        ("Subscribe", vec![PyRep::Bytes(req_bytes.clone())]),
        ("SubscribeToNotice", Vec::new()),
        ("ConsumeNotices", Vec::new()),
        ("PublishEvent", Vec::new()),
        ("PublishEvent", vec![PyRep::Bytes(req_bytes.clone())]),
        ("PublishEventPayload", Vec::new()),
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut unauth_sess,
                "publicGatewaySvc",
                None,
                method,
                &args,
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.code,
            "Unauthorized", // rpc-lint: allow error code assertion
            "unframed/notice unauth {method} must return Unauthorized"
        );
    }

    // 4. Direct helper defense-in-depth: PollNotices must reject unauthenticated sessions directly
    let helper_err = ctx
        .quasar_gateway
        .dispatch_public_gateway_rpc(&unauth_sess, "PollNotices", &[], &kw, ctx.now_unix())
        .expect("PollNotices returns Some")
        .unwrap_err();
    assert_eq!(helper_err.code, "Unauthorized"); // rpc-lint: allow error code assertion
}

#[tokio::test]
async fn dispatch_caller_class_character_account_and_gm_gates_run_before_handler() {
    use crate::router::rpc_registry::{
        rpc_entry_with_caller, RpcCallerClass, RpcDispatchHandler, RpcEntry, RpcRegistry,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CHAR_CALLS: AtomicUsize = AtomicUsize::new(0);
    static ACC_CALLS: AtomicUsize = AtomicUsize::new(0);
    static GM_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn count_char(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _service: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        CHAR_CALLS.fetch_add(1, Ordering::SeqCst);
        Some(Ok(PyRep::Bool(true)))
    }

    fn count_acc(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _service: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        ACC_CALLS.fetch_add(1, Ordering::SeqCst);
        Some(Ok(PyRep::Bool(true)))
    }

    fn count_gm(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _service: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        GM_CALLS.fetch_add(1, Ordering::SeqCst);
        Some(Ok(PyRep::Bool(true)))
    }

    static SPY_SLICE: &[RpcEntry] = &[
        rpc_entry_with_caller(
            "skillMgr2",
            "GetSkills",
            RpcCallerClass::Character,
            RpcDispatchHandler::Sync(count_char),
        ),
        rpc_entry_with_caller(
            "userSvc",
            "GetUserName",
            RpcCallerClass::Account,
            RpcDispatchHandler::Sync(count_acc),
        ),
        rpc_entry_with_caller(
            "machoNet",
            "GetMetrics",
            RpcCallerClass::Gm,
            RpcDispatchHandler::Sync(count_gm),
        ),
    ];

    CHAR_CALLS.store(0, Ordering::SeqCst);
    ACC_CALLS.store(0, Ordering::SeqCst);
    GM_CALLS.store(0, Ordering::SeqCst);

    let mut spy_ctx =
        ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    spy_ctx.rpc_registry = Arc::new(RpcRegistry::from_slices(&[SPY_SLICE]));
    let kw = HashMap::new();

    // Character rejects character_id=None with CharacterNotSelected BEFORE entering handler.
    let mut sess = SessionState::new(94_901);
    sess.user_id = Some(1);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    let err_char = spy_ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_char.code, "CharacterNotSelected");
    assert_eq!(CHAR_CALLS.load(Ordering::SeqCst), 0);

    // Account rejects user_id=None before entering handler, and allows user_id=Some(_), character_id=None.
    let mut unauth = SessionState::new(94_902);
    let err_unauth = spy_ctx
        .dispatch_rpc(&mut unauth, "userSvc", None, "GetUserName", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_unauth.code, "Unauthorized");
    assert_eq!(ACC_CALLS.load(Ordering::SeqCst), 0);

    let ok_acc = spy_ctx
        .dispatch_rpc(&mut sess, "userSvc", None, "GetUserName", &[], &kw, None)
        .await
        .expect("Account allows authenticated session without character");
    assert_eq!(ok_acc, PyRep::Bool(true));
    assert_eq!(ACC_CALLS.load(Ordering::SeqCst), 1);

    // Gm rejects non-GM before entering handler and allows GM without character.
    let err_gm = spy_ctx
        .dispatch_rpc(&mut sess, "machoNet", None, "GetMetrics", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(err_gm.code, "InsufficientRole");
    assert_eq!(GM_CALLS.load(Ordering::SeqCst), 0);

    let mut gm_sess = SessionState::new(94_903);
    gm_sess.user_id = Some(1);
    gm_sess.role = service_roles::ROLE_GMH;
    let ok_gm = spy_ctx
        .dispatch_rpc(&mut gm_sess, "machoNet", None, "GetMetrics", &[], &kw, None)
        .await
        .expect("Gm allows GM session without character");
    assert_eq!(ok_gm, PyRep::Bool(true));
    assert_eq!(GM_CALLS.load(Ordering::SeqCst), 1);

    // Character succeeds once character_id is selected.
    sess.character_id = Some(90_000_001);
    let ok_char = spy_ctx
        .dispatch_rpc(&mut sess, "skillMgr2", None, "GetSkills", &[], &kw, None)
        .await
        .expect("Character allows session with character selected");
    assert_eq!(ok_char, PyRep::Bool(true));
    assert_eq!(CHAR_CALLS.load(Ordering::SeqCst), 1);

    // Verify the production compiled registry enforces the same contract end-to-end.
    let prod_ctx =
        ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    let mut prod_no_char = SessionState::new(94_904);
    prod_no_char.user_id = Some(1);
    prod_no_char.role = service_roles::ROLE_PLAYER_DEFAULT;
    for (svc, method) in [
        ("skillMgr2", "GetSkills"),
        ("lookupSvc", "LookupCharacters"),
        ("userSvc", "CastUpvote"),
        ("publicGatewaySvc", "PublishEvent"),
    ] {
        let err = prod_ctx
            .dispatch_rpc(&mut prod_no_char, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "CharacterNotSelected", "{svc}.{method}");
    }

    let mut prod_unauth = SessionState::new(94_905);
    for (svc, method, args) in [
        ("charUnboundMgr", "GetCharactersToSelect", vec![]),
        ("userSvc", "GetUserName", vec![]),
        ("config", "GetMap", vec![PyRep::Integer(30_000_142)]),
        ("petitioner", "MayPetition", vec![]),
    ] {
        let err = prod_ctx
            .dispatch_rpc(&mut prod_unauth, svc, None, method, &args, &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "Unauthorized", "{svc}.{method}");
        assert!(
            prod_ctx
                .dispatch_rpc(&mut prod_no_char, svc, None, method, &args, &kw, None)
                .await
                .is_ok(),
            "Account RPC {svc}.{method} must allow authenticated session without character"
        );
    }

    for (svc, method) in [
        ("machoNet", "GetMetrics"),
        ("objectCaching", "InvalidateCachedObjects"),
    ] {
        let err = prod_ctx
            .dispatch_rpc(&mut prod_no_char, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "InsufficientRole", "{svc}.{method}");
        let mut prod_gm = SessionState::new(94_906);
        prod_gm.user_id = Some(1);
        prod_gm.role = service_roles::ROLE_GMH;
        assert!(
            prod_ctx
                .dispatch_rpc(&mut prod_gm, svc, None, method, &[], &kw, None)
                .await
                .is_ok(),
            "Gm RPC {svc}.{method} must allow GM session without character"
        );
    }
}

#[tokio::test]
async fn dispatch_scene_class_enforces_in_space_and_docked_before_handler() {
    use crate::router::rpc_registry::{
        rpc_entry, rpc_entry_with_caller, RpcCallerClass, RpcDispatchHandler, RpcEntry,
        RpcRegistry, RpcSceneClass,
    };
    use eve_machonet::service_roles;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SPACE_CALLS: AtomicUsize = AtomicUsize::new(0);
    static DOCKED_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn hit_space(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _svc: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        SPACE_CALLS.fetch_add(1, Ordering::SeqCst);
        Some(Ok(PyRep::Bool(true)))
    }

    fn hit_docked(
        _ctx: &ServerContext,
        _session: &mut SessionState,
        _svc: &str,
        _method: &str,
        _args: &[PyRep],
        _kwargs: &HashMap<String, PyRep>,
    ) -> Option<Result<PyRep, BridgeRpcError>> {
        DOCKED_CALLS.fetch_add(1, Ordering::SeqCst);
        Some(Ok(PyRep::Bool(true)))
    }

    static SPY_SCENE_SLICE: &[RpcEntry] = &[
        rpc_entry("beyonce", "CmdStop", RpcDispatchHandler::Sync(hit_space))
            .with_scene(RpcSceneClass::InSpace),
        rpc_entry_with_caller(
            "keeper",
            "PlayDungeon",
            RpcCallerClass::Gm,
            RpcDispatchHandler::Sync(hit_space),
        )
        .with_scene(RpcSceneClass::InSpace),
        rpc_entry(
            "corpRegistry",
            "ResignFromCorp",
            RpcDispatchHandler::Sync(hit_docked),
        )
        .with_scene(RpcSceneClass::Docked),
    ];

    SPACE_CALLS.store(0, Ordering::SeqCst);
    DOCKED_CALLS.store(0, Ordering::SeqCst);

    let mut spy_ctx =
        ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    spy_ctx.rpc_registry = Arc::new(RpcRegistry::from_slices(&[SPY_SCENE_SLICE]));
    let kw = HashMap::new();

    // NoChar GM session cannot bypass RpcSceneClass::InSpace even when station/system fields are None.
    let mut gm_no_char = SessionState::new(94_909);
    gm_no_char.user_id = Some(1);
    gm_no_char.role = service_roles::ROLE_GMH;
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut gm_no_char, "keeper", None, "PlayDungeon", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "CharacterNotSelected"
    );
    assert_eq!(SPACE_CALLS.load(Ordering::SeqCst), 0);

    let mut sess = SessionState::new(94_910);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);

    // Docked caller blocked on InSpace before handler runs; Docked succeeds.
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CmdStop", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(SPACE_CALLS.load(Ordering::SeqCst), 0);
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "corpRegistry", None, "ResignFromCorp", &[], &kw, None)
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(DOCKED_CALLS.load(Ordering::SeqCst), 1);

    // Undocked caller blocked on Docked before handler runs; InSpace succeeds.
    spy_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = None;
        }
    });
    sess.station_id = None;
    sess.ship_id = Some(100_000_001);
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "corpRegistry", None, "ResignFromCorp", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    assert_eq!(DOCKED_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CmdStop", &[], &kw, None)
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(SPACE_CALLS.load(Ordering::SeqCst), 1);

    // Structure-docked (DB ch.structure_id = Some(st_id), ch.active_ship_id = 100_000_001): InSpace rejects, Docked succeeds.
    let st_id = 1_000_000_009_001_i64;
    spy_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.structure_id = Some(st_id);
            ch.active_ship_id = 100_000_001;
        }
    });
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CmdStop", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "NotInSpace"
    );
    assert_eq!(sess.structure_id, Some(st_id));
    assert_eq!(SPACE_CALLS.load(Ordering::SeqCst), 1);
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "corpRegistry", None, "ResignFromCorp", &[], &kw, None)
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(DOCKED_CALLS.load(Ordering::SeqCst), 2);

    // Active Upwell structure control (DB ch.structure_id == Some(st_id) & ch.active_ship_id == st_id):
    // InSpace passes so structure-control guards run, while Docked rejects with MustBeDocked.
    spy_ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.structure_id = Some(st_id);
            ch.active_ship_id = st_id;
        }
    });
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "beyonce", None, "CmdStop", &[], &kw, None)
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(sess.structure_id, Some(st_id));
    assert_eq!(sess.ship_id, Some(st_id));
    assert_eq!(SPACE_CALLS.load(Ordering::SeqCst), 2);
    assert_eq!(
        spy_ctx
            .dispatch_rpc(&mut sess, "corpRegistry", None, "ResignFromCorp", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "MustBeDocked"
    );
    assert_eq!(DOCKED_CALLS.load(Ordering::SeqCst), 2);
}
