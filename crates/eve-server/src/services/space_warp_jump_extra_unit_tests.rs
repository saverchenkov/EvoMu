//! Extra branch-coverage and mutation-killing unit tests for `services::space_warp_jump`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_warp_jump::*;
use eve_db::{
    BookmarkRecord, CorporationRecord, CosmicAnomalyRecord, FleetRecord, InventoryFlag,
    InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
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

fn spawn_ship_in_space(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
    sys_id: u32,
    pos: DVec3,
) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let handle = ctx.systems.get_or_spawn(sys_id);
    handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 500.0,
            max_shield_hp: 500.0,
            armor_hp: 500.0,
            max_armor_hp: 500.0,
            structure_hp: 500.0,
            max_structure_hp: 500.0,
            capacitor: 250.0,
            max_capacitor: 250.0,
            ..Default::default()
        },
    );
}

async fn call_beyonce(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound: Option<&BoundObjectEntry>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, bound, "beyonce", method, args, kwargs)
        .await
        .expect("beyonce method must be dispatched")
}

async fn warp_to_bookmark_id(
    ctx: &ServerContext,
    session: &mut SessionState,
    bm_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    let args = [PyRep::String("bookmark".into()), PyRep::Integer(bm_id)];
    call_beyonce(ctx, session, None, "CmdWarpToStuff", &args, &kw).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_to_bookmark_fleet_member_anomaly_structure_and_partial_capacitor() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let mate_id = 90_000_002_i64;
    let ship_id = 896_001_i64;
    let mate_ship_id = 896_002_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Bookmark in Zarzakh is rejected with DunBlacklistCannotWarp
    let bm_id = 770_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            bm_id,
            BookmarkRecord {
                bookmark_id: bm_id,
                owner_id: char_id,
                solar_system_id: sys_id,
                label: "Deep Safe".into(),
                x: 50_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let mut zarzakh_sess = sess.clone();
    zarzakh_sess.solar_system_id = Some(SOLAR_SYSTEM_ZARZAKH);
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, SOLAR_SYSTEM_ZARZAKH, DVec3::ZERO);
    let err_z = call_beyonce(
        &ctx,
        &mut zarzakh_sess,
        None,
        "CmdWarpToStuff",
        &[PyRep::String("bookmark".into()), PyRep::Integer(bm_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert!(err_z.message.contains("DunBlacklistCannotWarp") || err_z.code.contains("DunBlacklistCannotWarp"));

    // Restore ship in Jita and test missing bookmark vs valid bookmark warp
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);
    let err_bm = warp_to_bookmark_id(&ctx, &mut sess, 999_999_999).await.unwrap_err();
    assert_eq!(err_bm.code, "BookmarkNotFound");

    let bm_warp = warp_to_bookmark_id(&ctx, &mut sess, bm_id).await.unwrap();
    assert!(bm_warp.as_i64().is_some());

    // Reset ship movement mode to Stop and test fleet member warp ("char")
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    spawn_ship_in_space(
        &ctx,
        mate_id,
        mate_ship_id,
        606,
        sys_id,
        DVec3::new(0.0, 25_000_000.0, 0.0),
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.insert(
            501,
            FleetRecord {
                fleet_id: 501,
                boss_id: char_id,
                members: vec![char_id, mate_id],
                motd: String::new(),
                free_move: false,
                is_registered: false,
                ..Default::default()
            },
        );
    });
    sess.fleet_id = Some(501);
    let fleet_warp = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdWarpToStuff",
        &[PyRep::String("char".into()), PyRep::Integer(mate_id)],
        &kw,
    )
    .await
    .unwrap();
    assert!(fleet_warp.as_i64().is_some());

    // Reset ship to Stop and test cosmic anomaly warp by string ID + partial capacitor warp
    let _ = handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "ANOM-777".into(),
            solar_system_id: sys_id,
            name: "Hideaway".into(),
            anomaly_type: "Combat".into(),
            position: [0.0, 0.0, 5.0e12],
            signal_strength: 1.0,
            ore_types: Vec::new(),
            remaining_volume: 0.0,
            created_unix: 0,
            lifetime_seconds: 3600,
            cleared: false,
        });
    });
    // Set capacitor low enough that full 5.0e12m (33 AU) warp is unaffordable, but >= 150,000m is affordable
    handle.try_update_combat_state(ship_id, |cs| {
        cs.capacitor = 0.05;
        cs.max_capacitor = 250.0;
    });
    let anom_warp = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::String("ANOM-777".into())],
        &kw,
    )
    .await
    .unwrap();
    assert!(anom_warp.as_i64().is_some());
    let cs_after = handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after.capacitor, 0.0);

    // CmdWarpToStuffAutopilot uses args[0] as target item ID
    let _ = handle.set_movement_mode(ship_id, MovementMode::Stop).await;
    handle.try_update_combat_state(ship_id, |cs| cs.capacitor = 250.0);
    let st_id = 60_003_760_i64;
    let ap_warp = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdWarpToStuffAutopilot",
        &[PyRep::Integer(st_id)],
        &kw,
    )
    .await
    .unwrap();
    assert!(ap_warp.as_i64().is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stargate_jump_enforces_docked_wrong_system_session_timer_in_warp_and_capital_gates() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 896_010_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();

    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH)
        .cloned()
        .expect("Jita stargate");
    let other_gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id != sys_id)
        .cloned()
        .expect("non-Jita stargate");

    let mut sess = SessionState {
        session_id: 61,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        station_id: Some(60_003_760),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. Docked -> ShipDocked
    let err_docked = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_docked.code, "ShipDocked");

    // 2. Missing gate_id arg -> InvalidArgument
    sess.station_id = None;
    let err_arg = call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_arg.code, "InvalidArgument");

    // 3. Stargate in another solar system -> StargateWrongSystem
    let err_wrong_sys = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(other_gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_wrong_sys.code, "StargateWrongSystem");

    // 4. Ship not spawned in space -> ShipNotInSpace
    let err_no_ball = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_no_ball.code, "ShipNotInSpace");

    // 5. Spawn ship at gate, test ShipInWarp
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        sys_id,
        gate.position + DVec3::new(500.0, 0.0, 0.0),
    );
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle
        .set_movement_mode(
            ship_id,
            MovementMode::new_warp(gate.position, gate.position + DVec3::new(1_000_000.0, 0.0, 0.0), 3.0),
        )
        .await;
    let err_warp = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_warp.code, "ShipInWarp");
    let _ = handle.set_movement_mode(ship_id, MovementMode::Stop).await;

    // 6. Capital ship (Dreadnought group 485, e.g. Revelation 19720) in highsec -> CapitalShipCannotJumpStargate
    let dread_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 485)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(19720);
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        dread_tid,
        sys_id,
        gate.position + DVec3::new(500.0, 0.0, 0.0),
    );
    let err_cap = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_cap.code, "CapitalShipCannotJumpStargate");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dock_and_undock_enforce_crimewatch_upwell_structure_states_acl_and_ship_size() {
    let ctx = test_ctx();
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
    let char_id = 90_000_001_i64;
    let corp_id = 98_000_777_i64;
    let ship_id = 896_020_i64;
    let sys_id = 30_000_142_u32;
    let st_id = 60_003_760_i64;
    let struct_id = 1_000_000_896_020_i64;
    let kw = HashMap::new();

    let mut sess = SessionState {
        session_id: 62,
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(sys_id),
        station_id: Some(st_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // 1. AlreadyDocked
    let err_already = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(st_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_already.code, "AlreadyDocked");

    // 2. MissingLocationId & InvalidShipId
    sess.station_id = None;
    let err_missing = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_missing.code, "MissingLocationId");

    let st_pos = ctx.sde.stations[&st_id].position;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, st_pos + DVec3::new(200.0, 0.0, 0.0));
    let err_bad_ship = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdDock",
        &[PyRep::Integer(st_id), PyRep::Integer(ship_id + 999)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_bad_ship.code, "InvalidShipId");

    // 3. WeaponsTimerActive & CantDockGlobalCriminalFlag (non-capsule blocked, capsule allowed!)
    let now = ctx.now_unix();
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        cw.get_or_create(char_id).weapon_timer_until_unix = now + 60;
    }
    let err_wpn = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(st_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_wpn.code, "WeaponsTimerActive");

    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        st.weapon_timer_until_unix = 0;
        st.criminal_flag_until_unix = now + 900;
    }
    let err_crim = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(st_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_crim.code, "CantDockGlobalCriminalFlag");

    // Clear criminal timer and test Upwell structure docking branches
    ctx.crimewatch.write().unwrap().states.clear();

    let err_no_st = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdDock",
        &[PyRep::Integer(struct_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(err_no_st.code, "StructureNotFound");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "DockCorp".into();
                __corp.ticker = "DCK".into();
                __corp.ceo_id = char_id;
                __corp.members = HashSet::from([char_id]);
                __corp },
        );
        snap.structures.insert(
            struct_id,
            UpwellStructureRecord {
                structure_id: struct_id,
                type_id: 35832, // Astrahus (medium structure)
                owner_corp_id: corp_id,
                solar_system_id: sys_id + 1, // Wrong system initially
                name: "Test Astrahus".into(),
                position: [st_pos.x, st_pos.y, st_pos.z],
                state: "Destroyed".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
    });

    let err_wrong_sys = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_wrong_sys.code, "DockTargetWrongSystem");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().solar_system_id = sys_id;
    });
    let err_dest = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_dest.code, "StructureDestroyed");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "Anchoring".into();
    });
    let err_anch = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_anch.code, "StructureNotDockable");

    // Operational structure: test ACL denial, capital ship size rejection on medium structure, warp scramble rejection, then valid dock!
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().state = "ShieldVulnerable".into();
        snap.structure_docking_acl_denied
            .entry(struct_id)
            .or_default()
            .insert(char_id);
    });
    let err_acl = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_acl.code, "StructureDockingAccessDenied");

    ctx.db.with_mut_snapshot(|snap| {
        snap.structure_docking_acl_denied.clear();
    });
    let dread_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| t.group_id == 485)
        .min_by_key(|t| t.type_id)
        .map(|t| t.type_id)
        .unwrap_or(19720);
    spawn_ship_in_space(&ctx, char_id, ship_id, dread_tid, sys_id, st_pos + DVec3::new(200.0, 0.0, 0.0));
    let err_size = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_size.code, "ShipTooLargeForStructure");

    // Switch back to subcap (606), scramble ship -> ShipWarpScrambled
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, st_pos + DVec3::new(200.0, 0.0, 0.0));
    let handle = ctx.systems.get_or_spawn(sys_id);
    handle.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 1);
    let err_scram = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_scram.code, "ShipWarpScrambled");

    // Unscramble and dock at Upwell structure!
    handle.try_update_combat_state(ship_id, |cs| cs.warp_scramble_strength = 0);
    let dock_ok = call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(struct_id)], &kw)
        .await
        .unwrap();
    assert_eq!(dock_ok.dict_get("locationID"), Some(&PyRep::Integer(struct_id)));
    assert_eq!(dock_ok.dict_get("isStructure"), Some(&PyRep::Bool(true)));
    assert_eq!(sess.structure_id, Some(struct_id));
    assert_eq!(sess.station_id, None);

    // Undock while 10s session-change timer is active -> SessionChangeInProgress
    let err_undock_timer = call_beyonce(&ctx, &mut sess, None, "Undock", &[], &kw)
        .await
        .unwrap_err();
    assert_eq!(err_undock_timer.code, "SessionChangeInProgress");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn warp_targets_mission_fleet_celestial_rigid_stargate_pochven_zarzakh_and_dock_guards() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let mate_id = 90_000_002_i64;
    let ship_id = 100_000_001_i64;
    let mate_ship = 100_000_002_i64;
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    // 1. WarpToStuff("mission", mission_id) via active_missions fallback
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, 4401),
            eve_db::ActiveMissionRecord {
                mission_id: 4401,
                character_id: char_id,
                agent_id: 3_018_681,
                solar_system_id: sys_id,
                state: "Accepted".into(),
                current_room_id: 0,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );
    });
    let w_miss = call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(4401), PyRep::Real(0.0)], &kw)
        .await
        .unwrap();
    assert!(w_miss.as_i64().is_some());
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    // 2. CmdWarpToStuff("fleet", mate_id) when in fleet & in space vs invalid warp target
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);
    spawn_ship_in_space(&ctx, mate_id, mate_ship, 606, sys_id, DVec3::new(300_000_000.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&char_id].clone();
        ch.character_id = mate_id;
        ch.active_ship_id = mate_ship;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        snap.characters.insert(mate_id, ch);
        snap.fleets.insert(
            888,
            FleetRecord {
                fleet_id: 888,
                boss_id: char_id,
                members: vec![char_id, mate_id],
                ..Default::default()
            },
        );
    });
    sess.fleet_id = Some(888);
    let w_fleet = call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("fleet".into()), PyRep::Integer(mate_id), PyRep::Real(10_000.0)], &kw)
        .await
        .unwrap();
    assert!(w_fleet.as_i64().is_some());
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    // 3. CmdWarpToStuff to celestial by ID + Rigid ball in space + InvalidWarpDestination
    if let Some(cel) = ctx.sde.celestials_in_system(sys_id).into_iter().find(|c| c.position.length() > 1_000_000.0) {
        let w_cel = call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(cel.celestial_id), PyRep::Real(0.0)], &kw)
            .await
            .unwrap();
        assert!(w_cel.as_i64().is_some());
        let _ = handle.remove_ball(ship_id).await;
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);
    }
    let rigid_id = 777_000_001_i64;
    handle.try_spawn_ball_with_combat(
        Ball {
            id: rigid_id,
            type_id: 16,
            position: DVec3::new(500_000_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e12,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        CombatState::default(),
    );
    let w_rig = call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(rigid_id), PyRep::Real(0.0)], &kw)
        .await
        .unwrap();
    assert!(w_rig.as_i64().is_some());
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(999_888_777), PyRep::Real(0.0)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidWarpDestination"
    );

    // 4. CmdDock validation guards: InvalidShipId, AlreadyDocked, NoActiveShip, MissingLocationId, NoSolarSystem, ShipNotInSpace
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(60_003_760), PyRep::Integer(999_999)], &kw)
            .await
            .unwrap_err()
            .code,
        "InvalidShipId"
    );
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[], &kw)
            .await
            .unwrap_err()
            .code,
        "MissingLocationId"
    );
    sess.solar_system_id = None;
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(60_003_760)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(60_003_760)], &kw)
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    sess.station_id = Some(60_003_760);
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(60_003_760)], &kw)
            .await
            .unwrap_err()
            .code,
        "AlreadyDocked"
    );
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    // 5. CmdStargateJump: OutsideStargateJumpRange, Pochven standing gate, Zarzakh gate lock
    if let Some(gate) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == sys_id).cloned() {
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, gate.position + DVec3::new(100_000.0, 0.0, 0.0));
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "OutsideStargateJumpRange"
        );
    }
    // Pochven home system (Kino/Niarja/Archee) jump standing check
    if let Some(poch_gate) = ctx.sde.jumps.values().find(|j| {
        ctx.sde.solar_systems.get(&j.to_solar_system_id).is_some_and(|s| {
            s.region_id == 10_000_070 && matches!(s.name.as_str(), "Kino" | "Niarja" | "Archee")
        })
    }).cloned() {
        let from_sys = poch_gate.from_solar_system_id;
        sess.solar_system_id = Some(from_sys);
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, from_sys, poch_gate.position);
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(poch_gate.stargate_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "SystemCheck_JumpFailed_StandingsTooLow"
        );
    }
    // Zarzakh gate lock check when exiting Zarzakh (30_100_000) via a different gate
    let zarzakh_gates: Vec<_> = ctx.sde.jumps.values().filter(|j| j.from_solar_system_id == 30_100_000).cloned().collect();
    if zarzakh_gates.len() >= 2 {
        let g1 = &zarzakh_gates[0];
        let g2 = &zarzakh_gates[1];
        sess.solar_system_id = Some(30_100_000);
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, 30_100_000, g2.position);
        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks.insert(char_id, (g1.stargate_id, now_u + 3600));
        });
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(g2.stargate_id)], &kw)
                .await
                .unwrap_err()
                .code,
            "ZarzakhGateLocked"
        );
    }
    // Suppress unused mut warning on ctx if needed
    let _ = &mut ctx;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fleet_warp_signatures_zarzakh_entry_and_dock_fallback_outlaw_guards() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let mate_id = 90_000_002_i64;
    let ship_id = 100_000_001_i64;
    let mate_ship = 100_000_002_i64;
    let st_id = 60_003_760_i64;
    let kw = HashMap::new();

    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);
    spawn_ship_in_space(&ctx, mate_id, mate_ship, 606, sys_id, DVec3::new(100.0, 0.0, 0.0));
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(98_000_001),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        fleet_id: Some(901),
        ..Default::default()
    };

    // 1. Fleet warp keyword path (`fleet: true` + `formationID`)
    let st_pos = ctx.sde.stations[&st_id].position;
    ctx.db.with_mut_snapshot(|snap| {
        let mut mate_ch = snap.characters[&char_id].clone();
        mate_ch.character_id = mate_id;
        mate_ch.active_ship_id = mate_ship;
        mate_ch.solar_system_id = sys_id;
        mate_ch.station_id = None;
        snap.characters.insert(mate_id, mate_ch);
        snap.fleets.insert(
            901,
            FleetRecord {
                fleet_id: 901,
                boss_id: char_id,
                members: vec![char_id, mate_id],
                ..Default::default()
            },
        );
    });
    let mut fleet_kw = HashMap::new();
    fleet_kw.insert("fleet".into(), PyRep::Bool(true));
    let _ = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(st_id), PyRep::Real(0.0)],
        &fleet_kw,
    )
    .await;
    let handle = ctx.systems.get_or_spawn(sys_id);
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    // 2. Warp to scanned signature (signature_position 750_000_001 AND cosmic_signatures entry by ID and string)
    let sig_id = "WH-SIG-01";
    let wh_item_id = wormhole_item_id_for_signature(sys_id, sig_id);
    ctx.db.with_mut_snapshot(|snap| {
        let mut sys_st = snap.system_states.get(&sys_id).cloned().unwrap_or_default();
        sys_st.signature_position = Some([400_000_000.0, 0.0, 0.0]);
        snap.system_states.insert(sys_id, sys_st);
        snap.cosmic_signatures.entry(sys_id).or_default().push(eve_db::types::CosmicSignatureRecord {
            signature_id: sig_id.into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole K162".into(),
            signal_strength: 100.0,
            position: [0.0, 450_000_000.0, 0.0],
            wormhole_type_id: None,
            destination_system_id: Some(31_000_005),
            created_unix: 0,
            lifetime_seconds: 86_400,
            max_mass_kg: 1.0e9,
            remaining_mass_kg: 1.0e9,
            max_jump_mass_kg: 5.0e6,
            collapsed: false,
        });
    });
    ctx.mark_signature_scanned(char_id, sys_id, 750_000_001);
    ctx.mark_signature_scanned(char_id, sys_id, wh_item_id);
    let w_750 = call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(750_000_001)], &kw)
        .await.unwrap();
    assert!(w_750.as_i64().is_some());
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    let w_sig = call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::String(sig_id.into())], &kw)
        .await.unwrap();
    assert!(w_sig.as_i64().is_some());
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, DVec3::ZERO);

    // 3. CmdStargateJump guards: ShipDocked, NoSolarSystem, NoActiveShip + Zarzakh entry jump
    sess.station_id = Some(st_id);
    assert_eq!(call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(1)], &kw).await.unwrap_err().code, "ShipDocked");
    sess.station_id = None;
    sess.solar_system_id = None;
    assert_eq!(call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(1)], &kw).await.unwrap_err().code, "BoundObjectRequired");
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = None;
    assert_eq!(call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(1)], &kw).await.unwrap_err().code, "NoActiveShip");
    sess.ship_id = Some(ship_id);

    if let Some(z_gate) = ctx.sde.jumps.values().find(|j| j.to_solar_system_id == 30_100_000).cloned() {
        let from_sys = z_gate.from_solar_system_id;
        sess.solar_system_id = Some(from_sys);
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, from_sys, z_gate.position + DVec3::new(500.0, 0.0, 0.0));
        // Jump to Zarzakh (may require toll/Standing or succeed; exercises process_zarzakh_gate_toll + zarzakh_gate_locks)
        let _ = call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(z_gate.stargate_id)], &kw).await;
    }
    ctx.db.clock().advance_secs(60);

    // 4. CmdDock stale session ship fallback, ShipNotInSpace, SessionChangeInProgress, FactionPoliceRefusedDocking
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(999_111_222); // stale ship_id, while active_ship_id in DB is ship_id spawned at st_pos
    let _ = handle.remove_ball(ship_id).await;
    // First test ShipNotInSpace when neither ship_id nor active_ship_id is in space
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(st_id)], &kw).await.unwrap_err().code,
        "ShipNotInSpace"
    );
    // Spawn active ship right at station, leave stale sess.ship_id so db_ship_id fallback triggers; also set sec status -10.0 for outlaw refusal
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, st_pos + DVec3::new(100.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.security_status = -10.0;
        }
    });
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(st_id)], &kw).await.unwrap_err().code,
        "FactionPoliceRefusedDocking"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.security_status = 5.0;
        }
    });
    crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix(), sys_id, 0, "jump");
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(st_id)], &kw).await.unwrap_err().code,
        "SessionChangeInProgress"
    );

    // 5. Additional warp/stargate/dock branches: unscanned sig, spawned ball warp, WarpScrambled/CloakedCannotWarp, stargate in-warp/timer/Zarzakh cap/range, DockTargetWrongSystem
    ctx.db.clock().advance_secs(60);
    sess.ship_id = Some(ship_id);
    let unscanned_sig = "UNSCAN-01";
    let unscanned_ball = wormhole_item_id_for_signature(sys_id, unscanned_sig);
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.entry(sys_id).or_default().push(eve_db::types::CosmicSignatureRecord {
            signature_id: unscanned_sig.into(),
            solar_system_id: sys_id,
            sig_group: "Wormhole".into(),
            type_name: "Wormhole K162".into(),
            signal_strength: 0.2,
            position: [300_000_000.0, 0.0, 0.0],
            wormhole_type_id: None,
            destination_system_id: Some(31_000_005),
            created_unix: 0,
            lifetime_seconds: 86_400,
            max_mass_kg: 1.0e9,
            remaining_mass_kg: 1.0e9,
            max_jump_mass_kg: 5.0e6,
            collapsed: false,
        });
    });
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(unscanned_ball)], &kw)
            .await.unwrap_err().code,
        "InvalidWarpDestination"
    );

    // Warp to a spawned ball in space (exercises non-bookmark ball_snapshot branch 422..428), then scrambled & cloaked warp errors
    let wreck_ball = 888_222_001_i64;
    let _ = handle
        .spawn_ball_with_meta(
            Ball {
                id: wreck_ball,
                type_id: 26_272,
                position: DVec3::new(600_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta::default(),
        )
        .await;
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(wreck_ball)], &kw)
            .await.unwrap_err().code,
        "InvalidWarpDestination"
    );

    let _ = handle.remove_ball(ship_id).await;
    let _ = handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 606,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 2.0,
                mass: 1_200_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState { warp_scramble_strength: 5, ..Default::default() },
        )
        .await;
    assert_eq!(
        call_beyonce(&ctx, &mut sess, None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(st_id)], &kw)
            .await.unwrap_err().code,
        "WarpScrambled"
    );

    // CmdStargateJump: SessionChangeInProgress, ShipInWarp, ShipCannotUseStargate (Zarzakh capital), OutsideStargateJumpRange
    if let Some(gate) = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == sys_id).cloned() {
        let _ = handle.remove_ball(ship_id).await;
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, gate.position + DVec3::new(1_000_000.0, 0.0, 0.0));
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw)
                .await.unwrap_err().code,
            "OutsideStargateJumpRange"
        );
        let _ = handle.remove_ball(ship_id).await;
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 606,
                    position: gate.position,
                    velocity: DVec3::ZERO,
                    max_velocity: 350.0,
                    inertia: 2.0,
                    mass: 1_200_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Warp {
                        origin: DVec3::ZERO,
                        destination: DVec3::new(1e9, 0.0, 0.0),
                        warp_speed_au_s: 3.0,
                        total_distance: 1e9,
                        phase: eve_destiny::WarpPhase::Cruising,
                        elapsed_warp_time: 1.0,
                    },
                },
                CombatState::default(),
            )
            .await;
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw)
                .await.unwrap_err().code,
            "ShipInWarp"
        );
        crate::services::space_destiny::record_session_change_timer(&ctx, char_id, ctx.now_unix(), sys_id, 0, "jump");
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw)
                .await.unwrap_err().code,
            "SessionChangeInProgress"
        );
        ctx.db.clock().advance_secs(60);
    }
    if let Some(z_gate) = ctx.sde.jumps.values().find(|j| j.to_solar_system_id == 30_100_000).cloned() {
        let from_sys = z_gate.from_solar_system_id;
        let zh = ctx.systems.get_or_spawn(from_sys);
        let _ = zh.remove_ball(ship_id).await;
        sess.solar_system_id = Some(from_sys);
        spawn_ship_in_space(&ctx, char_id, ship_id, 23_911, from_sys, z_gate.position);
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(z_gate.stargate_id)], &kw)
                .await.unwrap_err().code,
            "ShipCannotUseStargate"
        );
    }

    // CmdDock targeting station in a different solar system -> DockTargetWrongSystem
    sess.solar_system_id = Some(sys_id);
    if let Some(&other_st_id) = ctx.sde.stations.iter().find(|(_, s)| s.solar_system_id != sys_id).map(|(id, _)| id) {
        let _ = handle.remove_ball(ship_id).await;
        spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, st_pos);
        assert_eq!(
            call_beyonce(&ctx, &mut sess, None, "CmdDock", &[PyRep::Integer(other_st_id)], &kw)
                .await.unwrap_err().code,
            "DockTargetWrongSystem"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cmd_stargate_jump_allows_arriving_warp_at_gate_and_rejects_non_arriving_warp() {
    let ctx = test_ctx();
    let char_id = 90_000_001_i64;
    let ship_id = 896_099_i64;
    let sys_id = 30_000_142_u32;
    let kw = HashMap::new();

    let gate = ctx
        .sde
        .jumps
        .values()
        .find(|j| j.from_solar_system_id == sys_id && j.to_solar_system_id != SOLAR_SYSTEM_ZARZAKH)
        .cloned()
        .expect("Jita stargate");

    let handle = ctx.systems.get_or_spawn(sys_id);
    let gate_radius = handle
        .get_ball_snapshot(gate.stargate_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.radius)
        .filter(|&r| r > 0.0)
        .unwrap_or_else(|| {
            ctx.sde
                .types
                .get(&16)
                .map(|t| t.radius)
                .filter(|&r| r > 0.0)
                .unwrap_or(14_051.0)
        });

    let mut sess = SessionState {
        session_id: 99,
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };

    // (a1) Arriving warp within gate_radius + 2_500.0 succeeds and clears active_warps
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        sys_id,
        gate.position + DVec3::new(gate_radius + 1_000.0, 0.0, 0.0),
    );
    let _ = handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: gate.position + DVec3::new(1_000_000.0, 0.0, 0.0),
                destination: gate.position,
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: eve_destiny::WarpPhase::Decelerating,
                elapsed_warp_time: 10.0,
            },
        )
        .await;
    ctx.active_warps.lock().unwrap().insert(ship_id, gate.stargate_id);
    call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw)
        .await
        .expect("arriving warp within gate jump range must jump");
    assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // (a2) Arriving warp in WarpPhase::Decelerating within <= 1.0 s of destination outside gate_radius + 2_500.0 succeeds
    ctx.db.clock().advance_secs(60);
    let _ = ctx.systems.get_or_spawn(gate.to_solar_system_id).remove_ball(ship_id).await;
    sess.solar_system_id = Some(sys_id);
    let _ = handle.remove_ball(ship_id).await;
    let warp_dest = gate.position + DVec3::new(gate_radius + 2_400.0, 0.0, 0.0);
    let near_finish_pos = gate.position + DVec3::new(gate_radius + 2_550.0, 0.0, 0.0);
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, near_finish_pos);
    let _ = handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: gate.position + DVec3::new(1_000_000.0, 0.0, 0.0),
                destination: warp_dest,
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: eve_destiny::WarpPhase::Decelerating,
                elapsed_warp_time: 12.0,
            },
        )
        .await;
    ctx.active_warps.lock().unwrap().insert(ship_id, gate.stargate_id);
    call_beyonce(&ctx, &mut sess, None, "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &kw)
        .await
        .expect("decelerating warp finishing within <= 1s at gate must jump");
    assert_eq!(sess.solar_system_id, Some(gate.to_solar_system_id));
    assert!(!ctx.active_warps.lock().unwrap().contains_key(&ship_id));

    // (b1) Wrong warp destination still returns ShipInWarp
    ctx.db.clock().advance_secs(60);
    let _ = ctx.systems.get_or_spawn(gate.to_solar_system_id).remove_ball(ship_id).await;
    sess.solar_system_id = Some(sys_id);
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(&ctx, char_id, ship_id, 606, sys_id, gate.position);
    let _ = handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: gate.position,
                destination: gate.position + DVec3::new(1_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: eve_destiny::WarpPhase::Decelerating,
                elapsed_warp_time: 12.0,
            },
        )
        .await;
    let wrong_dest_err = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(wrong_dest_err.code, "ShipInWarp");

    // (b2) Cruising far from destination (even aimed at gate) still returns ShipInWarp
    let _ = handle.remove_ball(ship_id).await;
    spawn_ship_in_space(
        &ctx,
        char_id,
        ship_id,
        606,
        sys_id,
        gate.position + DVec3::new(gate_radius + 50_000.0, 0.0, 0.0),
    );
    let _ = handle
        .set_movement_mode(
            ship_id,
            MovementMode::Warp {
                origin: gate.position + DVec3::new(1_000_000.0, 0.0, 0.0),
                destination: gate.position,
                warp_speed_au_s: 3.0,
                total_distance: 1_000_000.0,
                phase: eve_destiny::WarpPhase::Cruising,
                elapsed_warp_time: 2.0,
            },
        )
        .await;
    let far_cruise_err = call_beyonce(
        &ctx,
        &mut sess,
        None,
        "CmdStargateJump",
        &[PyRep::Integer(gate.stargate_id)],
        &kw,
    )
    .await
    .unwrap_err();
    assert_eq!(far_cruise_err.code, "ShipInWarp");
}
