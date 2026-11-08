//! Extra branch-coverage and mutation-killing unit tests for `services::space_warp_jump`.

#![forbid(unsafe_code)]

use crate::router::{BoundObjectEntry, ServerContext};
use crate::services::space_warp_jump::*;
use eve_db::{
    BookmarkRecord, CorporationRecord, CosmicAnomalyRecord, FleetRecord, InventoryFlag,
    InventoryItemRecord, UpwellStructureRecord, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
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

#[tokio::test]
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

#[tokio::test]
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
        .find(|t| t.group_id == 485)
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

#[tokio::test]
async fn dock_and_undock_enforce_crimewatch_upwell_structure_states_acl_and_ship_size() {
    let ctx = test_ctx();
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
        .find(|t| t.group_id == 485)
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
