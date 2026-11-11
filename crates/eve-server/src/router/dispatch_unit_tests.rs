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
async fn wal_error_hooks_and_format_pyrep_summary_variants() {
    ensure_wal_error_hook_installed();
    assert!(!record_dispatch_wal_error("outside_scope"));
    assert!(current_dispatch_wal_error().is_none());
    assert!(take_dispatch_wal_error().is_none());

    DISPATCH_WAL_ERROR
        .scope(std::cell::RefCell::new(None), async {
            assert!(record_dispatch_wal_error("first_err"));
            assert!(record_dispatch_wal_error("second_err_ignored"));
            assert_eq!(current_dispatch_wal_error().as_deref(), Some("first_err"));
            assert_eq!(take_dispatch_wal_error().as_deref(), Some("first_err"));
            assert!(current_dispatch_wal_error().is_none());
        })
        .await;

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
    let mut ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    ctx.feature_flags.enable_living_world = false;
    ctx.feature_flags.enable_quasar_gateway = false;

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

    ctx.feature_flags.enable_living_world = true;
    ctx.feature_flags.enable_quasar_gateway = true;

    // 2. GM & QA role gates
    sess.role = 0;
    let gm_err = ctx
        .dispatch_rpc(&mut sess, "slash", None, "SlashCmd", &[PyRep::String("/healself".into())], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(gm_err.code, "InsufficientRole");

    let qa_err = ctx
        .dispatch_rpc(&mut sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(qa_err.code, "InsufficientRole");

    let tc_err = ctx
        .dispatch_rpc(&mut sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(5)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(tc_err.code, "InsufficientRole");

    // Grant GM role and test AdvanceClock (negative vs valid) and ClearSessionTimer
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

    let bind_str = ctx
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
        .expect("MachoBindObject str");
    assert!(matches!(bind_str, PyRep::Tuple(_)));

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

    // 1. publicGatewaySvc.SendRequest/CallQuasar errors: require_character, UnknownQuasarDomain, UnimplementedRPC
    let mut no_char_sess = SessionState::new(94_100);
    no_char_sess.user_id = Some(1);
    let err_no_char = ctx
        .dispatch_rpc(
            &mut no_char_sess,
            "publicGatewaySvc",
            None,
            "CallQuasar", // rpc-lint: allow test fixture
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
                "CallQuasar", // rpc-lint: allow test fixture
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

    // 2. Role gate matrix: slash "/", QA subrole, keeper/dungeon/taleMgr/GM_/QA_/qa_/QA suffix
    sess.role = service_roles::ROLE_PLAYER;
    for (svc, method) in [
        ("keeper", "GetLevelEditor"),
        ("dungeon", "DEGetDungeons"),
        ("taleMgr", "MaterializeSystemLOD2"), // rpc-lint: allow test fixture
        ("devToolsProvider", "AnyMethod"), // rpc-lint: allow test fixture
        ("sessionMgr", "AnyMethod"), // rpc-lint: allow test fixture
        ("character", "GM_ForceRename"),
        ("character", "QA_Reset"),
        ("character", "qa_seed"),
        ("character", "RunDiagnosticsQA"),
    ] {
        let err = ctx
            .dispatch_rpc(&mut sess, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "InsufficientRole", "expected InsufficientRole on {svc}.{method}");
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

    // `ROLE_QA` satisfies `dispatch`'s `slash_subrole` and `is_qa_surface` gates
    // (reaches handler-level role mask check rather than `dispatch`'s central V24.01 GM gate).
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
    let qa_ok_or_unimpl = ctx
        .dispatch_rpc(&mut sess, "character", None, "QA_Probe", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(qa_ok_or_unimpl.code, "UnimplementedRPC");

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
        ch.active_ship_id
    });
    ctx.active_warps.lock().unwrap().insert(ship_id, 60_003_760);

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

    let tuple_str_ok = ctx
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
        .expect("Tuple(String) binds");
    assert!(matches!(tuple_str_ok, PyRep::Tuple(_)));

    let token_bind_ok = ctx
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
        .expect("Token binds on agentMgr");
    assert!(matches!(token_bind_ok, PyRep::Tuple(_)));

    let bad_piggy = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(30_000_142),
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
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // 3. WAL error hook inside real `dispatch_rpc` triggers `DatabasePersistenceError`,
    // rehydration rollback of dirty runtime state, and `pending_notifications` truncation.
    ensure_wal_error_hook_installed();
    let wal_tmp = std::env::temp_dir().join(format!(
        "evomu_dispatch_wal_rollback_{}_{:?}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let wal_dir = wal_tmp.with_extension("json.wal");
    let _ = std::fs::remove_file(&wal_tmp);
    let _ = std::fs::remove_dir_all(&wal_dir);
    let wal_repo = eve_db::WorldRepository::new_empty_with_snapshot_path(Some(wal_tmp.clone()))
        .expect("wal repo");
    std::fs::write(&wal_tmp, b"{}").expect("seed wal json");
    let wal_ctx = ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), Arc::new(wal_repo));
    let mut wal_ch = ctx.db.get_character(90_000_001).unwrap();
    let wal_uid = wal_ch.user_id;
    let wal_corp_id = if wal_ch.corporation_id > 0 {
        wal_ch.corporation_id
    } else {
        98_000_001
    };
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
    // Initial snapshot seeding above created a regular `.wal` file; remove it before blocking with a dir.
    let _ = std::fs::remove_file(&wal_dir);
    std::fs::create_dir_all(&wal_dir).expect("block wal dir");
    // NOTE: `dispatch.rs:217` calls `self.rehydrate_runtime_from_db()` after exiting its own
    // `DISPATCH_WAL_ERROR.scope`, which in turn calls `prune_offline_fleets_and_channels`
    // (`ctx.db.with_mut_snapshot`) outside the inner scope. While production files are reserved
    // here, keep an outer `DISPATCH_WAL_ERROR` scope around the test call so only that secondary
    // post-scope prune failure is absorbed after the inner `dispatch_rpc` WAL error is captured.
    let wal_err = DISPATCH_WAL_ERROR
        .scope(std::cell::RefCell::new(None), async {
            wal_ctx
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
        })
        .await
        .unwrap_err();
    let _ = std::fs::remove_dir_all(&wal_dir);
    let _ = std::fs::remove_file(&wal_tmp);
    assert_eq!(wal_err.code, "DatabasePersistenceError");
    assert!(!wal_ctx
        .db
        .with_snapshot(|s| s.corp_governance.contains_key(&wal_corp_id)));
    assert_eq!(wal_ctx.pending_notifications.read().unwrap()[&90_000_001].len(), 1);
    assert!(wal_ctx.scanned_signatures.read().unwrap().is_empty());

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
