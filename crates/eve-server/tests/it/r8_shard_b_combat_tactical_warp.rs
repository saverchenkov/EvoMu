//! Round-8 Shard-B integration regression tests for `#494`, `#495`, `#496`, `#497`, and `#520`.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode, SlimItemMeta, TargetLock, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::step_combat_system;
use eve_server::services::pve_dungeon_cache::registry_instance_id;
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_server::services::tactical_modules::{
    activate_superweapon_or_lance, check_ship_navigation_lock,
};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn seed_item(
    ctx: &ServerContext,
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
    singleton: bool,
) {
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity,
                singleton,
                version: 1,
                ..Default::default()
            },
        );
    });
}

#[allow(clippy::too_many_arguments)]
async fn spawn_ship(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    type_id: u32,
    group_id: u32,
    owner_id: i64,
    pos: DVec3,
    cap: f64,
) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut cs = ctx.build_type_combat_state(type_id);
    cs.capacitor = cap;
    cs.max_capacitor = cap;
    sys.spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 250.0,
            inertia: 1.5,
            mass: 10_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        cs,
        SlimItemMeta {
            group_id,
            category_id: 6,
            owner_id,
            char_id: Some(owner_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn r8b_494_495_cloak_warp_combat_guards_and_drone_locks_ecm_death_pipeline() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let (sys_id, c1, c2) = (30_004_759_u32, 90_000_001_i64, 90_000_002_i64);
    let (ship1, ship2, dread_id, sb_mod, drone_item, ecm_drone_item) = (
        880_494_001, 880_494_002, 880_494_003, 880_494_010, 880_494_020, 880_494_021,
    );
    ctx.db.with_mut_snapshot(|s| {
        for (cid, sid) in [(c1, ship1), (c2, ship2)] {
            let ch = s.characters.get_mut(&cid).unwrap();
            ch.station_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid;
        }
    });
    seed_item(&ctx, ship1, 645, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, ship2, 587, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, dread_id, 19_720, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, sb_mod, 4_393, c1, ship1, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, drone_item, 2_454, c1, ship1, InventoryFlag::DroneBay, 1, false);
    seed_item(&ctx, ecm_drone_item, 21_638, c1, ship1, InventoryFlag::DroneBay, 1, false);

    spawn_ship(&ctx, sys_id, ship1, 645, 27, c1, DVec3::ZERO, 5_000.0).await;
    spawn_ship(&ctx, sys_id, ship2, 587, 25, c2, DVec3::new(1_000.0, 0.0, 0.0), 500.0).await;
    spawn_ship(&ctx, sys_id, dread_id, 19_720, 485, c2, DVec3::new(2_000.0, 0.0, 0.0), 50_000.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut s1 = SessionState::new(8501);
    s1.user_id = Some(1);
    s1.character_id = Some(c1);
    s1.solar_system_id = Some(sys_id);
    s1.ship_id = Some(ship1);

    // #494: Cloaked or mid-warp ship cannot activate AoE weapons or launch drones.
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship1).unwrap().is_cloaked = true;
    assert!(call(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod)]).await.is_err());
    assert!(call(&ctx, &mut s1, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_item)])]).await.is_err());

    {
        let mut bp = sys.ballpark.write().unwrap();
        bp.combat_states.get_mut(&ship1).unwrap().is_cloaked = false;
        bp.balls.get_mut(&ship1).unwrap().mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 100_000_000.0,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        };
    }
    assert!(call(&ctx, &mut s1, "dogmaIM", "Activate", &[PyRep::Integer(sb_mod)]).await.is_err());
    assert!(call(&ctx, &mut s1, "ship", "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_item)])]).await.is_err());

    sys.ballpark.write().unwrap().balls.get_mut(&ship1).unwrap().mode = MovementMode::Stop;
    let launch_rep = call(
        &ctx,
        &mut s1,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_item), PyRep::Integer(ecm_drone_item)])],
    )
    .await
    .expect("LaunchDrones");
    let PyRep::List(launched_ids) = launch_rep else { panic!("expected list") };
    let (combat_drone_bid, ecm_drone_bid) = (launched_ids[0].as_i64().unwrap(), launched_ids[1].as_i64().unwrap());

    // #495: Unlocked target rejected with `TargetNotLocked`.
    let unl_err = call(&ctx, &mut s1, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(combat_drone_bid)]), PyRep::Integer(ship2)])
        .await
        .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(unl_err.code, "TargetNotLocked");

    for tid in [ship2, dread_id] {
        sys.add_target_lock(
            ship1,
            TargetLock {
                target_id: tid,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .unwrap();
    }

    // #495: Cloaked or tethered target rejected by `CmdEngage`.
    sys.ballpark.write().unwrap().combat_states.get_mut(&ship2).unwrap().is_cloaked = true;
    assert!(call(&ctx, &mut s1, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(combat_drone_bid)]), PyRep::Integer(ship2)]).await.is_err());
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs2 = bp.combat_states.get_mut(&ship2).unwrap();
        cs2.is_cloaked = false;
        cs2.is_tethered = true;
    }
    assert!(call(&ctx, &mut s1, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(combat_drone_bid)]), PyRep::Integer(ship2)]).await.is_err());

    // #495: EWAR-immune capital (`dread_id`) never loses locks to ECM drones.
    sys.add_target_lock(
        dread_id,
        TargetLock {
            target_id: ship1,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(100_000.0),
        },
    )
    .await
    .unwrap();
    call(&ctx, &mut s1, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(ecm_drone_bid)]), PyRep::Integer(dread_id)])
        .await
        .expect("engage dread with ECM drone");
    for _ in 0..6 {
        step_combat_system(&ctx, sys_id, 5.0).await;
    }
    assert_eq!(sys.get_target_locks(dread_id).await.unwrap().len(), 1);

    // #495: Drone kill runs full death pipeline (removes ship ball) and sets PvP weapons timer.
    {
        let mut bp = sys.ballpark.write().unwrap();
        let cs2 = bp.combat_states.get_mut(&ship2).unwrap();
        cs2.is_tethered = false;
        cs2.shield_hp = 0.0;
        cs2.armor_hp = 0.0;
        cs2.structure_hp = 1.0;
    }
    call(&ctx, &mut s1, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(combat_drone_bid)]), PyRep::Integer(ship2)])
        .await
        .expect("engage 1-HP ship2");
    for _ in 0..5 {
        if sys.get_ball_snapshot(ship2).await.unwrap().is_none() {
            break;
        }
        step_combat_system(&ctx, sys_id, 5.0).await;
    }
    assert!(sys.get_ball_snapshot(ship2).await.unwrap().is_none());
    assert!(ctx.crimewatch.read().unwrap().states.get(&c1).is_some_and(|st| st.is_weapon_timer_active(ctx.now_unix())));
}

#[tokio::test]
async fn r8b_496_single_target_doomsday_locks_eligibility_immobilization_and_pvp_timer() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let (null_sys, c_titan, c_vic) = (30_004_759_u32, 90_000_001_i64, 90_000_002_i64);
    let (titan_id, dd_mod_id, fuel_id, subcap_id, dummy_cap_id, target_cap_id) = (
        880_496_001, 880_496_002, 880_496_003, 880_496_010, 880_496_011, 880_496_012,
    );
    ctx.db.with_mut_snapshot(|s| {
        for (cid, sid) in [(c_titan, titan_id), (c_vic, target_cap_id)] {
            let ch = s.characters.get_mut(&cid).unwrap();
            ch.station_id = None;
            ch.solar_system_id = null_sys;
            ch.active_ship_id = sid;
        }
    });
    seed_item(&ctx, titan_id, 11_567, c_titan, null_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, dd_mod_id, 24_550, c_titan, titan_id, InventoryFlag::HiSlot0, 1, true);
    seed_item(&ctx, fuel_id, 16_274, c_titan, titan_id, InventoryFlag::Cargo, 100_000, false);
    seed_item(&ctx, subcap_id, 587, c_vic, null_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, dummy_cap_id, 19_720, c_vic, null_sys as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, target_cap_id, 19_720, c_vic, null_sys as i64, InventoryFlag::None, 1, true);

    spawn_ship(&ctx, null_sys, titan_id, 11_567, 30, c_titan, DVec3::ZERO, 100_000.0).await;
    spawn_ship(&ctx, null_sys, subcap_id, 587, 25, c_vic, DVec3::new(5_000.0, 0.0, 0.0), 500.0).await;
    spawn_ship(&ctx, null_sys, dummy_cap_id, 19_720, 485, c_vic, DVec3::new(8_000.0, 0.0, 0.0), 50_000.0).await;
    spawn_ship(&ctx, null_sys, target_cap_id, 19_720, 485, c_vic, DVec3::new(10_000.0, 0.0, 0.0), 50_000.0).await;

    let sys = ctx.systems.get_or_spawn(null_sys);
    for tid in [dummy_cap_id, subcap_id] {
        sys.add_target_lock(
            titan_id,
            TargetLock {
                target_id: tid,
                lock_time_ms: 1000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(250_000.0),
            },
        )
        .await
        .unwrap();
    }

    assert_eq!(
        activate_superweapon_or_lance(&ctx, c_titan, titan_id, null_sys, dd_mod_id, "Activate", Some(target_cap_id))
            .await
            .unwrap_err()
            .code,
        "TargetNotLocked"
    );
    assert_eq!(
        activate_superweapon_or_lance(&ctx, c_titan, titan_id, null_sys, dd_mod_id, "Activate", Some(subcap_id))
            .await
            .unwrap_err()
            .code,
        "InvalidTarget"
    );

    sys.ballpark.write().unwrap().combat_states.get_mut(&titan_id).unwrap().is_tethered = true;
    assert_eq!(
        activate_superweapon_or_lance(&ctx, c_titan, titan_id, null_sys, dd_mod_id, "Activate", Some(dummy_cap_id))
            .await
            .unwrap_err()
            .code,
        "CantActivateWhileTethered"
    );
    sys.ballpark.write().unwrap().combat_states.get_mut(&titan_id).unwrap().is_tethered = false;

    sys.add_target_lock(
        titan_id,
        TargetLock {
            target_id: target_cap_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(250_000.0),
        },
    )
    .await
    .unwrap();
    activate_superweapon_or_lance(&ctx, c_titan, titan_id, null_sys, dd_mod_id, "Activate", Some(target_cap_id))
        .await
        .expect("single-target Doomsday against locked Dreadnought");

    assert!(check_ship_navigation_lock(&ctx, titan_id, "dock").is_err());
    assert!(sys.has_active_module_cycle_sync(titan_id, dd_mod_id));
    assert!(sys.is_ball_scrambled(titan_id).await.unwrap());
}

#[tokio::test]
async fn r8b_497_520_store_vessel_guards_kinematics_and_agency_warp_interdiction() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let (sys_id, c1, c2) = (30_004_759_u32, 90_000_001_i64, 90_000_002_i64);
    let (orca_id, rifter_id, tackler_id, active_mod_id) = (880_497_001, 880_497_002, 880_497_003, 880_497_004);

    ctx.db.with_mut_snapshot(|s| {
        let ch1 = s.characters.get_mut(&c1).unwrap();
        ch1.station_id = None;
        ch1.solar_system_id = sys_id;
        ch1.active_ship_id = rifter_id;
        for (&tid, ti) in &ctx.sde.types {
            if ti.category_id == 16 {
                s.skills.entry(c1).or_default().insert(
                    tid,
                    eve_db::TrainedSkillRecord {
                        type_id: tid,
                        level: 5,
                        skill_points: 256_000.0,
                    },
                );
            }
        }
    });
    seed_item(&ctx, orca_id, 28_606, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, rifter_id, 587, c1, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, tackler_id, 587, c2, sys_id as i64, InventoryFlag::None, 1, true);
    seed_item(&ctx, active_mod_id, 440, c1, rifter_id, InventoryFlag::MedSlot0, 1, true);

    spawn_ship(&ctx, sys_id, orca_id, 28_606, 941, c1, DVec3::ZERO, 5_000.0).await;
    spawn_ship(&ctx, sys_id, rifter_id, 587, 25, c1, DVec3::new(500.0, 0.0, 0.0), 500.0).await;
    spawn_ship(&ctx, sys_id, tackler_id, 587, 25, c2, DVec3::new(1_500.0, 0.0, 0.0), 500.0).await;

    let sys = ctx.systems.get_or_spawn(sys_id);
    let mut sess = SessionState::new(8601);
    sess.user_id = Some(1);
    sess.character_id = Some(c1);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(rifter_id);

    // #497: Scrambled, targeted, or active-module ship cannot StoreVessel into SMA.
    sys.ballpark.write().unwrap().combat_states.get_mut(&rifter_id).unwrap().warp_scramble_strength = 2;
    assert_eq!(call(&ctx, &mut sess, "ship", "StoreVessel", &[PyRep::Integer(orca_id)]).await.unwrap_err().code, "WarpScrambled");
    sys.ballpark.write().unwrap().combat_states.get_mut(&rifter_id).unwrap().warp_scramble_strength = 0;

    sys.add_target_lock(
        tackler_id,
        TargetLock {
            target_id: rifter_id,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(25_000.0),
        },
    )
    .await
    .unwrap();
    assert_eq!(call(&ctx, &mut sess, "ship", "StoreVessel", &[PyRep::Integer(orca_id)]).await.unwrap_err().code, "CannotStoreTargetedShip");
    sys.clear_target_locks(tackler_id).await.unwrap();

    ctx.db.with_mut_snapshot(|s| { s.module_active_states.insert(active_mod_id); });
    assert_eq!(call(&ctx, &mut sess, "ship", "StoreVessel", &[PyRep::Integer(orca_id)]).await.unwrap_err().code, "CannotStoreShipWithActiveModules");
    ctx.db.with_mut_snapshot(|s| { s.module_active_states.remove(&active_mod_id); });

    let store_ok = call(&ctx, &mut sess, "ship", "StoreVessel", &[PyRep::Integer(orca_id)]).await.expect("clean StoreVessel");
    assert_eq!(store_ok.dict_get("storedShipID"), Some(&PyRep::Integer(rifter_id)));

    call(&ctx, &mut sess, "ship", "BoardStoredShip", &[PyRep::Integer(orca_id), PyRep::Integer(rifter_id)])
        .await
        .expect("BoardStoredShip");
    let (_, boarded_ball) = sys.get_ball_snapshot(rifter_id).await.unwrap().expect("boarded ball");
    let sde_rifter_vel = ctx.sde.types[&587].default_attributes[&eve_sde::attr_ids::MAX_VELOCITY];
    assert!(boarded_ball.max_velocity >= sde_rifter_vel - 1e-3 && (boarded_ball.max_velocity - 100.0).abs() > 1.0);

    // #520: Seed real exploration sites in `sys_id` and select a `probe_required` site (`signal_strength == 0.0`).
    ensure_system_pve_sites_seeded(&ctx, sys_id).await;
    let (sig_id, site_did) = with_pve_exploration_reg(&ctx, |reg| {
        reg.sites_by_system[&sys_id]
            .iter()
            .find(|s| s.probe_required && !s.cleared)
            .map(|s| (s.site_id.clone(), s.dungeon_id))
            .expect("seeded probe_required site")
    });
    let site_iid = registry_instance_id(&sig_id);
    ctx.db.with_mut_snapshot(|s| {
        if let Some(sigs) = s.cosmic_signatures.get_mut(&sys_id) {
            if let Some(sig) = sigs.iter_mut().find(|sg| sg.signature_id == sig_id) {
                sig.signal_strength = 0.0;
            }
        }
    });

    let content_unscanned = call(&ctx, &mut sess, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("get_content_data");
    let PyRep::List(pieces_before) = content_unscanned else { panic!("expected List") };
    assert!(!pieces_before.iter().any(|p| p.dict_get("instanceID") == Some(&PyRep::Integer(site_iid))));

    let site_arg = PyRep::keyval([
        ("instanceID", PyRep::Integer(site_iid)),
        ("dungeonID", PyRep::Integer(site_did as i64)),
    ]);
    let unscanned_err = call(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[site_arg.clone()])
        .await
        .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(unscanned_err.code, "SignatureNotScanned");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(sigs) = s.cosmic_signatures.get_mut(&sys_id) {
            if let Some(sig) = sigs.iter_mut().find(|sg| sg.signature_id == sig_id) {
                sig.signal_strength = 1.0;
            }
        }
    });
    sys.ballpark.write().unwrap().combat_states.get_mut(&rifter_id).unwrap().warp_scramble_strength = 1;
    let scram_err = call(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[site_arg.clone()])
        .await
        .unwrap_err();
    // rpc-lint: allow error code check
    assert_eq!(scram_err.code, "WarpScrambled");
    sys.ballpark.write().unwrap().combat_states.get_mut(&rifter_id).unwrap().warp_scramble_strength = 0;

    call(&ctx, &mut sess, "custom_agency_provider", "warp_to_content", &[site_arg])
        .await
        .expect("warp_to_content after scanning and clearing tackle");
    let (_, w_ball) = sys.get_ball_snapshot(rifter_id).await.unwrap().unwrap();
    assert!(matches!(w_ball.mode, MovementMode::Warp { .. }));
}
