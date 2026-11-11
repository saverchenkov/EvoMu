//! Deep branch-coverage and mutation-killing unit tests for `router::dispatch`:
//! session revocation & `MultiLoginBlocked` exemptions, `enable_living_world` / `enable_quasar_gateway`
//! feature flag gates, GM/QA role bitmask enforcement across `slash` / `devToolsProvider` /
//! `sessionMgr` / `dungeon` / `keeper` / `publicQaToolsServer`, `MachoBindObject` argument shapes &
//! piggybacked sub-calls, WAL error rollback, and Sovereignty Hub prospecting deposit respawn ticks.

#![forbid(unsafe_code)]

use super::dispatch::{
    current_dispatch_wal_error, record_dispatch_wal_error, take_dispatch_wal_error,
    tick_sov_prospecting_deposit_respawn,
};
use super::ServerContext;
use crate::services::mining_sites::{seed_system_mining_sites_from_sde, SOV_PROSPECTING_DEPOSITS};
use eve_db::{
    AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord, InventoryFlag, InventoryItemRecord,
    UpwellStructureRecord, VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::service_roles::{
    ROLE_GML, ROLE_PLAYER, ROLE_QA, ROLE_ROLEADMIN, ROLE_SPAWN,
};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_authed_session(session_id: u64, user_id: u64, char_id: i64, role: u64) -> SessionState {
    let mut s = SessionState::new(session_id);
    s.user_id = Some(user_id);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(30_000_142);
    s.station_id = Some(60_003_760);
    s.ship_id = Some(100_000_001);
    s.role = role;
    s
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_revocation_multilogin_blocked_exemptions_and_kick_ban_state_wipe() {
    let ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    let kw = HashMap::new();

    // Ensure user 1 is a normal player (not ROLE_ADMIN) so kick/ban operations apply
    let user_id = 1u64;
    let char_id = 90_000_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        for acc in snap.accounts.values_mut() {
            if acc.user_id == user_id {
                acc.role = ROLE_PLAYER;
            }
        }
    });

    // Session 10 acquires the online lock for user 1
    ctx.db
        .acquire_online_lock_with_computer_hash(
            user_id,
            10,
            Some(char_id),
            Some("hw-hash-1".into()),
            true,
            now,
        )
        .expect("session 10 acquires lock");

    // Session 20 is a second concurrent session for the same user_id
    let mut sess20 = make_authed_session(20, user_id, char_id, ROLE_PLAYER);
    let b20 = ctx
        .bind_object(&sess20, "station", 60_003_760)
        .expect("bind station on sess20");

    // 1) Exempt calls (`authentication.Ping` and `petitioner.MayPetition`) bypass revocation check
    let ping_res = ctx
        .dispatch_rpc(&mut sess20, "authentication", None, "Ping", &[], &kw, None)
        .await;
    assert_ne!(ping_res.as_ref().err().map(|e| e.code.as_str()), Some("MultiLoginBlocked"));
    assert!(ctx.verify_bound_object(&sess20, b20).is_ok());
    let pet_res = ctx
        .dispatch_rpc(&mut sess20, "petitioner", None, "MayPetition", &[], &kw, None)
        .await;
    assert_ne!(pet_res.as_ref().err().map(|e| e.code.as_str()), Some("MultiLoginBlocked"));

    // 2) `multiLoginBlocker.Login` bypasses MultiLoginBlocked in dispatch_rpc_inner
    let mlb_res = ctx
        .dispatch_rpc(&mut sess20, "multiLoginBlocker", None, "Login", &[PyRep::String("hw-hash-2".into())], &kw, None)
        .await;
    assert_ne!(mlb_res.as_ref().err().map(|e| e.code.as_str()), Some("MultiLoginBlocked"));

    // Re-lock to session 10 and verify a normal RPC on session 20 returns MultiLoginBlocked,
    // releases session 20's bound objects, but preserves session 20's user_id!
    ctx.db
        .acquire_online_lock_with_computer_hash(user_id, 10, Some(char_id), Some("hw-hash-1".into()), true, now)
        .unwrap();
    ctx.db.clear_revocation_on_fresh_login(user_id, 20);
    let b20_again = ctx.bind_object(&sess20, "station", 60_003_760).unwrap();

    let blocked_err = ctx
        .dispatch_rpc(&mut sess20, "charMgr", None, "GetPublicInfo3", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(blocked_err.code, "MultiLoginBlocked");
    assert_eq!(sess20.user_id, Some(user_id), "MultiLoginBlocked must not wipe user_id");
    assert_eq!(ctx.verify_bound_object(&sess20, b20_again).unwrap_err().code, "InvalidBoundObject");

    // 3) Kicking user 1 revokes session 10 with SessionKicked and wipes all session fields
    let mut sess10 = make_authed_session(10, user_id, char_id, ROLE_GML);
    let b10 = ctx.bind_object(&sess10, "station", 60_003_760).unwrap();
    ctx.db.kick_user(user_id, "GM kick test", now);

    let kick_err = ctx
        .dispatch_rpc(&mut sess10, "charMgr", None, "GetPublicInfo3", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(kick_err.code, "SessionKicked");
    assert_eq!(
        (sess10.user_id, sess10.role, sess10.character_id, sess10.corporation_id, sess10.station_id, sess10.solar_system_id, sess10.ship_id),
        (None, 0, None, None, None, None, None)
    );
    assert_eq!(ctx.verify_bound_object(&sess10, b10).unwrap_err().code, "InvalidBoundObject");

    // 4) Banning user 1 revokes with UserBanned and wipes session state
    ctx.db.clear_revocation_on_fresh_login(user_id, 30);
    ctx.db
        .ban_account_by_id_or_username(&user_id.to_string(), now + 3_600, "Bottling")
        .expect("ban user 1");
    let mut sess30 = make_authed_session(30, user_id, char_id, ROLE_PLAYER);
    let ban_err = ctx
        .dispatch_rpc(&mut sess30, "charMgr", None, "GetPublicInfo3", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(ban_err.code, "UserBanned");
    assert_eq!((sess30.user_id, sess30.character_id), (None, None));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn feature_flags_time_control_and_gm_qa_role_bitmask_enforcement_matrix() {
    let mut ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    let char_id = 90_000_001i64;
    let kw = HashMap::new();

    // 1) Feature flag gates: taleMgr (enable_living_world) and publicGatewaySvc (enable_quasar_gateway)
    ctx.feature_flags.enable_living_world = false;
    ctx.feature_flags.enable_quasar_gateway = false;
    let mut gm_sess = make_authed_session(101, 1, char_id, ROLE_GML);

    let tale_err = ctx
        .dispatch_rpc(&mut gm_sess, "taleMgr", None, "GetGlobalWorldEventTales", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(tale_err.code, "FeatureDisabled"); // rpc-lint: allow error code assertion

    let q_err = ctx
        .dispatch_rpc(&mut gm_sess, "publicGatewaySvc", None, "SendRequest", &[], &kw, None)
        .await
        .unwrap_err();
    assert_eq!(q_err.code, "FeatureDisabled"); // rpc-lint: allow error code assertion

    ctx.feature_flags.enable_living_world = true;
    ctx.feature_flags.enable_quasar_gateway = true;

    // 2) timeControl.AdvanceClock: non-GM rejected, negative secs rejected, default +12s & explicit +5s + warp tick
    let mut player_sess = make_authed_session(102, 1, char_id, ROLE_PLAYER);
    assert_eq!(
        ctx.dispatch_rpc(&mut player_sess, "timeControl", None, "AdvanceClock", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut gm_sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(-1)], &kw, None)
            .await
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // Spawn active ship in Warp mode so AdvanceClock exercises the warp-tick loop
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&char_id].active_ship_id);
    let sys_id = ctx.db.with_snapshot(|s| s.characters[&char_id].solar_system_id);
    ctx.systems.get_or_spawn(sys_id).try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id: 606,
            position: DVec3::ZERO,
            velocity: DVec3::new(1_000.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(100_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 100_000_000.0,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 1.0,
            },
        },
        CombatState::default(),
    );

    let before = ctx.now_unix();
    let adv_default = ctx
        .dispatch_rpc(&mut gm_sess, "timeControl", None, "AdvanceClock", &[], &kw, None)
        .await
        .expect("AdvanceClock default 12s");
    assert_eq!((adv_default, ctx.now_unix()), (PyRep::Bool(true), before + 12));

    let adv_explicit = ctx
        .dispatch_rpc(&mut gm_sess, "timeControl", None, "AdvanceClock", &[PyRep::Integer(5)], &kw, None)
        .await
        .expect("AdvanceClock explicit 5s");
    assert_eq!((adv_explicit, ctx.now_unix()), (PyRep::Bool(true), before + 17));

    // 3) timeControl.FastForwardSessionTimer / harness.ClearSessionTimer
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix(), sys_id, 1, "jump");
    assert!(crate::services::space_destiny::check_session_change_timer_active(&ctx, char_id, ctx.now_unix(), None).is_some());
    assert_eq!(
        ctx.dispatch_rpc(&mut player_sess, "timeControl", None, "FastForwardSessionTimer", &[], &kw, None)
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    let ff_ok = ctx
        .dispatch_rpc(&mut gm_sess, "harness", None, "ClearSessionTimer", &[], &kw, None)
        .await
        .expect("ClearSessionTimer");
    assert_eq!(ff_ok, PyRep::Bool(true));
    assert!(crate::services::space_destiny::check_session_change_timer_active(&ctx, char_id, ctx.now_unix(), None).is_none());

    // 4) GM vs QA role gates across slash, devToolsProvider, sessionMgr, keeper, dungeon, publicQaToolsServer
    for (svc, method) in [
        ("slash", "SlashCmd"),
        ("devToolsProvider", "GetDevTools"), // rpc-lint: allow pre-existing GM gate for a server-internal name; refusing it is the point
        ("sessionMgr", "PerformSessionChange"),
        ("keeper", "GetLevelEditor"),
        ("dungeon", "DEGetDungeons"),
        ("dungeon", "DEGetRooms"),
        ("dungeon", "DEGetArchetypes"),
        ("dungeon", "DEGetFactions"),
        ("dungeon", "DEGetRoomObjectPaletteData"),
        ("publicQaToolsServer", "CanGiveItemForMultifit"),
    ] {
        let err = ctx
            .dispatch_rpc(&mut player_sess, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "InsufficientRole", "player must be denied {svc}.{method}");
    }

    // ROLE_QA is allowed on `slash` (via slash_subrole) and `publicQaToolsServer`,
    // but DENIED on `devToolsProvider`, `sessionMgr`, `keeper.GetLevelEditor`, and `dungeon.DEGet*`!
    let mut qa_sess = make_authed_session(103, 1, char_id, ROLE_QA);
    let slash_qa = ctx
        .dispatch_rpc(&mut qa_sess, "slash", None, "SlashCmd", &[PyRep::String("/whoami".into())], &kw, None)
        .await;
    assert!(slash_qa.is_ok(), "ROLE_QA must pass slash_subrole check for /whoami");
    for (subrole, cmd) in [(ROLE_SPAWN, "/spawn 606"), (ROLE_ROLEADMIN, "/role 90000001 0")] {
        let mut sub_sess = make_authed_session(104, 1, char_id, subrole);
        let res = ctx
            .dispatch_rpc(&mut sub_sess, "slash", None, "SlashCmd", &[PyRep::String(cmd.into())], &kw, None)
            .await;
        assert_ne!(
            res.as_ref().err().map(|e| e.message.starts_with("UserError: GM_InsufficientRole")),
            Some(true),
            "slash_subrole must bypass central dispatch GM gate"
        );
    }
    let qa_tools_res = ctx
        .dispatch_rpc(&mut qa_sess, "publicQaToolsServer", None, "CanGiveItemForMultifit", &[], &kw, None)
        .await;
    assert_ne!(qa_tools_res.as_ref().err().map(|e| e.code.as_str()), Some("InsufficientRole"));
    for (svc, method) in [
        ("devToolsProvider", "GetDevTools"), // rpc-lint: allow pre-existing GM gate for a server-internal name; refusing it is the point
        ("sessionMgr", "PerformSessionChange"),
        ("keeper", "GetLevelEditor"),
        ("dungeon", "DEGetDungeons"),
    ] {
        let err = ctx
            .dispatch_rpc(&mut qa_sess, svc, None, method, &[], &kw, None)
            .await
            .unwrap_err();
        assert_eq!(err.code, "InsufficientRole", "ROLE_QA without GM role must be rejected on {svc}.{method}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn macho_resolve_and_bind_object_argument_shapes_beyonce_and_piggybacked_subcalls() {
    let ctx = test_ctx();
    let char_id = 90_000_001i64;
    let kw = HashMap::new();

    // 1) MachoResolveObject requires auth and returns MACHO_PROXY_NODE_ID
    let mut unauth = SessionState::new(301);
    assert!(ctx
        .dispatch_rpc(&mut unauth, "charMgr", None, "MachoResolveObject", &[], &kw, None)
        .await
        .is_err());

    let mut sess = make_authed_session(302, 1, char_id, ROLE_PLAYER);
    let node_rep = ctx
        .dispatch_rpc(&mut sess, "charMgr", None, "MachoResolveObject", &[], &kw, None)
        .await
        .expect("MachoResolveObject");
    assert_eq!(node_rep, PyRep::Integer(eve_machonet::MACHO_PROXY_NODE_ID as i64));

    // 2) MachoBindObject argument shapes: String, Token, Tuple(Int), Tuple(Str), empty Tuple, None
    for bind_arg in [
        PyRep::String("moniker_alpha".into()),
        PyRep::Token("moniker_token".into()),
        PyRep::Tuple(vec![PyRep::Integer(3_018_922)]),
        PyRep::Tuple(vec![PyRep::String("moniker_tuple_str".into())]),
    ] {
        let rep = ctx
            .dispatch_rpc(
                &mut sess,
                "agentMgr",
                None,
                "MachoBindObject",
                &[bind_arg],
                &kw,
                None,
            )
            .await
            .expect("MachoBindObject valid arg shape");
        let PyRep::Tuple(items) = rep else {
            panic!("Expected (oid, call_result) tuple");
        };
        assert_eq!(items.len(), 2);
        assert!(items[0].as_i64().unwrap_or(0) > 0);
        assert_eq!(items[1], PyRep::None);
    }

    for invalid_arg in [
        vec![],
        vec![PyRep::None],
        vec![PyRep::String(String::new())],
        vec![PyRep::Tuple(vec![])],
        vec![PyRep::Tuple(vec![PyRep::None])],
    ] {
        let err = ctx
            .dispatch_rpc(
                &mut sess,
                "agentMgr",
                None,
                "MachoBindObject",
                &invalid_arg,
                &kw,
                None,
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, "InvalidArgument");
    }

    // 3) Piggybacked sub-call on charMgr with args & kwargs + machoNet:26000 wire format
    let piggy_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "charMgr",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(char_id),
                PyRep::Tuple(vec![
                    PyRep::String("GetPublicInfo3".into()),
                    PyRep::Tuple(vec![PyRep::Integer(char_id)]),
                    PyRep::Dict(vec![(PyRep::String("flag".into()), PyRep::Integer(1))]),
                ]),
            ],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("MachoBindObject with piggybacked GetPublicInfo3");
    let PyRep::Tuple(pair) = piggy_rep else {
        panic!("Expected 2-tuple from MachoBindObject");
    };
    assert!(matches!(pair[0], PyRep::SubStruct(_)));
    assert_ne!(pair[1], PyRep::None);

    // 4) skillMgr2.GetSkills over machoNet:26000 converts Rowset into Dict of KeyVals with skillRank
    let skills_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "skillMgr2",
            None,
            "GetSkills",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("skillMgr2.GetSkills machoNet:26000");
    let PyRep::Dict(entries) = skills_rep else {
        panic!("Expected Dict from skillMgr2.GetSkills for machoNet:26000");
    };
    assert!(!entries.is_empty());
    let (_, first_kv) = &entries[0];
    assert!(first_kv.dict_get("skillRank").is_some());
    assert!(first_kv.dict_get("effectiveSkillLevel").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wal_error_rollback_and_sov_prospecting_deposit_respawn_lifecycle() {
    let ctx = test_ctx();
    let now = 1_800_000_000u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));

    // 1) Task-local WAL error recording outside scope returns false / None
    assert!(!record_dispatch_wal_error("outside_scope"));
    assert_eq!(current_dispatch_wal_error(), None);
    assert_eq!(take_dispatch_wal_error(), None);

    // 2) Sovereignty Hub prospecting deposit depletion -> cleared timer -> respawn tick
    let sys_id = 30_004_759u32;
    let hub_id = 1_000_000_888_001i64;
    let upgrade_def = &SOV_PROSPECTING_DEPOSITS[0];
    let upgrade_tid = upgrade_def.upgrade_type_id;
    let respawn_secs = upgrade_def.respawn_seconds;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            hub_id,
            UpwellStructureRecord {
                structure_id: hub_id,
                type_id: 32_458,
                owner_corp_id: 98_000_001,
                solar_system_id: sys_id,
                name: "Sov Hub".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 10_000.0,
                armor_hp: 10_000.0,
                hull_hp: 10_000.0,
                ..Default::default()
            },
        );
        snap.items.insert(
            888_101,
            InventoryItemRecord {
                item_id: 888_101,
                type_id: upgrade_tid,
                owner_id: 98_000_001,
                location_id: hub_id,
                flag: InventoryFlag::SubSystemSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: "SOV-ORE-1".into(),
                solar_system_id: sys_id,
                name: "Depleted Sov Deposit".into(),
                anomaly_type: "SovOre".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                ore_types: vec![],
                remaining_volume: 100.0,
                created_unix: now - 100,
                lifetime_seconds: 0,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(CosmicSignatureRecord {
                signature_id: "SOV-ORE-1".into(),
                solar_system_id: sys_id,
                sig_group: "SovOre".into(),
                type_name: "Depleted Sov Deposit".into(),
                position: [1_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now - 100,
                lifetime_seconds: 0,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
        // Depleted asteroid (quantity == 0)
        snap.asteroids.insert(
            888_201,
            AsteroidRecord {
                asteroid_id: 888_201,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 0,
                initial_quantity: 1_000,
                position: (1_000.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });

    // Tick 1 at `now`: marks anomaly cleared, stamps respawn_secs, collapses signature
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;
    ctx.db.with_snapshot(|snap| {
        let anom = &snap.cosmic_anomalies[&sys_id][0];
        assert!(anom.cleared);
        assert_eq!(anom.lifetime_seconds, respawn_secs);
        assert_eq!(anom.created_unix, now);
        let sig = &snap.cosmic_signatures[&sys_id][0];
        assert!(sig.collapsed);
        assert_eq!(sig.signal_strength, 0.0);
    });

    // Tick 2 at `now + respawn_secs`: respawns fresh SovOre anomaly, signature, and asteroid rocks
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
        now + respawn_secs + 1,
    )));
    tick_sov_prospecting_deposit_respawn(&ctx, sys_id).await;
    ctx.db.with_snapshot(|snap| {
        let anoms = &snap.cosmic_anomalies[&sys_id];
        assert_eq!(anoms.len(), 1);
        assert!(!anoms[0].cleared);
        assert!(anoms[0].remaining_volume > 0.0);
        let has_fresh_asteroids = snap
            .asteroids
            .values()
            .any(|a| a.solar_system_id == sys_id && a.quantity > 0);
        assert!(has_fresh_asteroids, "respawned SovOre site must seed fresh asteroids");
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_switch_char_dock_undock_charges_and_gas_site_seeding() {
    let ctx = test_ctx();
    let now = 1_800_000_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    let kw = HashMap::new();
    let mut sess = SessionState::new(991);
    sess.user_id = Some(1);
    sess.role = eve_machonet::service_roles::ROLE_PLAYER;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c2) = snap.characters.get_mut(&90_000_002) {
            c2.user_id = 1;
        }
    });

    // 1. Select character A then switch to character B (triggers pause_milestone_playtime for old_cid)
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .unwrap();
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // Select back 90_000_001, Undock -> OnCharNoLongerInStation, then advance clock & Dock -> OnCharNowInStation
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let st_id = sess.station_id.unwrap_or(60_003_760);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap();
    ctx.advance_virtual_clock(15);
    ctx.dispatch_rpc(
        &mut sess,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(st_id)],
        &kw,
        None,
    )
    .await
    .unwrap();

    // 2. scanMgr.GetSystemScanMgr over machoNet:26000 rewrites boundID into bound_object_rep
    ctx.advance_virtual_clock(15);
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .unwrap();
    let scan_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemScanMgr",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .unwrap();
    assert!(matches!(scan_rep, PyRep::SubStruct(_)));

    // 3. WAL failure during dispatch_rpc overrides Ok result with DatabasePersistenceError
    let wal_err = ctx
        .dispatch_rpc(
            &mut sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/".into())],
            &kw,
            None,
        )
        .await;
    assert!(wal_err.is_err());

    // 4. seed_system_mining_sites_from_sde in a system whose region has GAS_SITES (e.g. Delve)
    let gas_sys_id = crate::services::mining_sites::GAS_SITES
        .first()
        .and_then(|def| {
            ctx.sde.solar_systems.values().find(|sys| {
                def.band.matches_system(sys)
                    && ctx
                        .sde
                        .regions
                        .get(&sys.region_id)
                        .is_some_and(|r| def.region_names.contains(&r.name.as_str()))
            })
        })
        .map(|s| s.solar_system_id);
    if let Some(gsid) = gas_sys_id {
        seed_system_mining_sites_from_sde(&ctx, gsid).await;
        ctx.db.with_snapshot(|snap| {
            assert!(
                snap.cosmic_signatures
                    .get(&gsid)
                    .is_some_and(|l| l.iter().any(|s| s.sig_group == "Gas"))
            );
        });
    }

    // 5. context_charges: swap ammo when no matching cargo stack exists (moves old charge to Cargo),
    //    plus UnknownWeaponType / UnknownChargeType error paths.
    let char_id = 90_000_001_i64;
    let ship_id = 100_000_001_i64;
    let weapon_id = 966_001_i64;
    let old_charge_id = 966_002_i64;
    let new_charge_id = 966_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = Some(60_003_760);
            c.structure_id = None;
            c.active_ship_id = ship_id;
        }
        if let Some(s) = snap.items.get_mut(&ship_id) {
            s.owner_id = char_id;
            s.location_id = 60_003_760;
        }
        snap.items.insert(
            weapon_id,
            InventoryItemRecord {
                item_id: weapon_id,
                type_id: 2881, // 150mm Light AutoCannon I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            old_charge_id,
            InventoryItemRecord {
                item_id: old_charge_id,
                type_id: 178, // EMP S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            new_charge_id,
            InventoryItemRecord {
                item_id: new_charge_id,
                type_id: 180, // Phased Plasma S
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });
    let res = ctx.load_charge_into_weapon_slot(
        char_id,
        ship_id,
        InventoryFlag::HiSlot0,
        new_charge_id,
        Some(60_003_760),
    );
    assert!(res.is_ok(), "swap ammo to unstacked cargo should succeed: {res:?}");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&old_charge_id].flag, InventoryFlag::Cargo);
    });
    assert_eq!(
        ctx.load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot1,
            new_charge_id,
            Some(60_003_760),
        )
        .unwrap_err()
        .code,
        "NoWeaponFitted"
    );
    assert_eq!(
        ctx.load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            999_999_777,
            Some(60_003_760),
        )
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&old_charge_id) {
            c.type_id = 999_999_888;
        }
    });
    assert_eq!(
        ctx.load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            old_charge_id,
            Some(60_003_760),
        )
        .unwrap_err()
        .code,
        "UnknownChargeType"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.items.get_mut(&old_charge_id) {
            c.type_id = 212; // Scourge Light Missile incompatible with 150mm AutoCannon
        }
    });
    assert_eq!(
        ctx.load_charge_into_weapon_slot(
            char_id,
            ship_id,
            InventoryFlag::HiSlot0,
            old_charge_id,
            Some(60_003_760),
        )
        .unwrap_err()
        .code,
        "IncompatibleAmmo"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dispatch_box_fut_station_peer_notify_seq_filter_market_cache_and_wal_pre_seq() {
    let ctx = test_ctx();
    let now = 1_800_100_000_u64;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now)));
    let kw = HashMap::new();

    // 1. box_fut helper preserves inner Future output
    assert_eq!(super::dispatch::box_fut(|| async { 4242_i64 }).await, 4242);

    // 2. Undock/Dock notify ONLY OTHER peers at station (peer_cid != cid, L258 & L274),
    //    NOT the acting character itself, and dispatch_rpc delivers only seq_id > pre_seq when !live_notify_listener (L313, L317).
    let char_a = 90_000_001_i64;
    let char_b = 90_000_002_i64;
    let station_id = 60_003_760_i64;
    ctx.db.provision_gm_account("peer_b_acct", "pw", char_b, "PeerB");
    let uid_b = ctx.db.with_snapshot(|s| s.characters[&char_b].user_id);
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [char_a, char_b] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.station_id = Some(station_id);
                ch.structure_id = None;
                ch.solar_system_id = 30_000_142;
            }
        }
    });
    ctx.db
        .acquire_online_lock(1, 51_001, Some(char_a), true, now)
        .expect("online lock char_a");
    ctx.db
        .acquire_online_lock(uid_b, 51_002, Some(char_b), true, now)
        .expect("online lock char_b");

    let mut sess_a = make_authed_session(51_001, 1, char_a, ROLE_GML | ROLE_PLAYER);
    sess_a.station_id = Some(station_id);
    ctx.pending_notifications.write().unwrap().clear();

    // Undock char_a -> char_b gets OnCharNoLongerInStation; char_a does NOT
    ctx.dispatch_rpc(&mut sess_a, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock char_a");
    {
        let pending = ctx.pending_notifications.read().unwrap();
        let b_events: Vec<&str> = pending
            .get(&char_b)
            .map(|v| v.iter().map(|n| n.event.as_str()).collect())
            .unwrap_or_default();
        let a_events: Vec<&str> = pending
            .get(&char_a)
            .map(|v| v.iter().map(|n| n.event.as_str()).collect())
            .unwrap_or_default();
        assert!(b_events.contains(&"OnCharNoLongerInStation"));
        assert!(!a_events.contains(&"OnCharNoLongerInStation"));
    }

    // Dock char_a -> char_b gets OnCharNowInStation; char_a does NOT
    ctx.pending_notifications.write().unwrap().clear();
    ctx.advance_virtual_clock(15);
    ctx.dispatch_rpc(
        &mut sess_a,
        "stationSvc",
        None,
        "Dock",
        &[PyRep::Integer(station_id)],
        &kw,
        None,
    )
    .await
    .expect("Dock char_a");
    {
        let pending = ctx.pending_notifications.read().unwrap();
        let b_events: Vec<&str> = pending
            .get(&char_b)
            .map(|v| v.iter().map(|n| n.event.as_str()).collect())
            .unwrap_or_default();
        let a_events: Vec<&str> = pending
            .get(&char_a)
            .map(|v| v.iter().map(|n| n.event.as_str()).collect())
            .unwrap_or_default();
        assert!(b_events.contains(&"OnCharNowInStation"));
        assert!(!a_events.contains(&"OnCharNowInStation"));
    }

    // Pre-existing notifications (seq <= pre_seq) must NOT be forwarded when !live_notify_listener;
    // ONLY new seq > pre_seq from `/giveisk 500` is forwarded. With live_notify_listener=true, nothing is pushed.
    ctx.notify(
        crate::router::NotificationTarget::Character(char_a),
        "OldPreSeq1",
        "charid",
        vec![],
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_a),
        "OldPreSeq2",
        "charid",
        vec![],
    );
    let _ = sess_a.drain_notifications();
    sess_a.live_notify_listener = false;
    ctx.dispatch_rpc(
        &mut sess_a,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveisk 500".into())],
        &kw,
        None,
    )
    .await
    .expect("/giveisk 500");
    let pushed = sess_a.drain_notifications();
    assert_eq!(pushed.len(), 1, "expected only 1 post-pre_seq notification, got {pushed:?}");
    assert_eq!(pushed[0].0, "OnAccountChange");

    sess_a.live_notify_listener = true;
    ctx.dispatch_rpc(
        &mut sess_a,
        "slash",
        None,
        "SlashCmd",
        &[PyRep::String("/giveisk 250".into())],
        &kw,
        None,
    )
    .await
    .expect("/giveisk 250 with live_notify_listener");
    assert!(sess_a.drain_notifications().is_empty());

    // 3. machoNet:26000 marketProxy caching gate (L357-358):
    //    `marketProxy.GetOrders` / `GetPlexOrders` wrapped in V24 CachedMethodCallResult,
    //    while `marketProxy.StartupCheck` and `charMgr.GetPublicInfo` are NOT wrapped.
    let orders_rep = ctx
        .dispatch_rpc(
            &mut sess_a,
            "marketProxy",
            None,
            "GetOrders",
            &[PyRep::Integer(34)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("marketProxy.GetOrders");
    assert!(matches!(
        &orders_rep,
        PyRep::Object { type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
    ));

    let plex_rep = ctx
        .dispatch_rpc(
            &mut sess_a,
            "marketProxy",
            None,
            "GetPlexOrders",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("marketProxy.GetPlexOrders");
    assert!(matches!(
        &plex_rep,
        PyRep::Object { type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
    ));

    let startup_rep = ctx
        .dispatch_rpc(
            &mut sess_a,
            "marketProxy",
            None,
            "StartupCheck",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("marketProxy.StartupCheck");
    assert!(!matches!(
        &startup_rep,
        PyRep::Object { type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
                || type_name == "objectCaching.CachedMethodCallResult"
    ));

    let pub_rep = ctx
        .dispatch_rpc(
            &mut sess_a,
            "charMgr",
            None,
            "GetPublicInfo",
            &[PyRep::Integer(char_a)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("charMgr.GetPublicInfo");
    assert!(!matches!(
        &pub_rep,
        PyRep::Object { type_name, .. }
            if type_name == eve_proto::marshal::V24_CACHED_METHOD_CALL_RESULT_CLASS
                || type_name == "objectCaching.CachedMethodCallResult"
    ));

    // 4. WAL failure drops post-pre_seq notifications and retains <= pre_seq notifications (L213)
    super::dispatch::ensure_wal_error_hook_installed();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let wal_tmp = std::env::temp_dir().join(format!(
        "evomu_dispatch_wal_deep_{}_{}.json",
        std::process::id(),
        nanos
    ));
    let wal_dir = wal_tmp.with_extension("json.wal");
    let _ = std::fs::remove_file(&wal_tmp);
    let _ = std::fs::remove_dir_all(&wal_dir);
    let wal_repo = WorldRepository::new_empty_with_snapshot_path(Some(wal_tmp.clone()))
        .expect("wal repo");
    std::fs::write(&wal_tmp, b"{}").expect("seed wal json");
    let wal_ctx = ServerContext::with_sde_and_repository(Arc::clone(&ctx.sde), Arc::new(wal_repo));
    let ch_seed = ctx.db.get_character(char_a).unwrap();
    let wal_uid = ch_seed.user_id;
    let wal_acct = ctx.db.with_snapshot(|s| s.accounts.values().find(|a| a.user_id == wal_uid).cloned());
    wal_ctx.db.with_mut_snapshot(|snap| {
        if let Some(a) = wal_acct {
            snap.accounts.insert(a.username.clone(), a);
        }
        snap.characters.insert(char_a, ch_seed);
    });
    wal_ctx
        .db
        .acquire_online_lock(wal_uid, 51_901, Some(char_a), true, wal_ctx.now_unix())
        .expect("wal online lock");
    let mut wal_sess = make_authed_session(51_901, wal_uid, char_a, ROLE_GML | ROLE_PLAYER);
    wal_ctx.notify(
        crate::router::NotificationTarget::Character(char_a),
        "PreWalRetained",
        "charid",
        vec![],
    );
    let _ = std::fs::remove_file(&wal_dir);
    std::fs::create_dir_all(&wal_dir).expect("block wal path");
    let wal_err = wal_ctx
        .dispatch_rpc(
            &mut wal_sess,
            "slash",
            None,
            "SlashCmd",
            &[PyRep::String("/giveisk 999".into())],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    let _ = std::fs::remove_dir_all(&wal_dir);
    let _ = std::fs::remove_file(&wal_tmp);
    assert_eq!(wal_err.code, "PersistenceFailure");
    let retained = wal_ctx.pending_notifications.read().unwrap();
    let evs: Vec<&str> = retained
        .get(&char_a)
        .map(|v| v.iter().map(|n| n.event.as_str()).collect())
        .unwrap_or_default();
    assert_eq!(evs, vec!["PreWalRetained"]);
}
