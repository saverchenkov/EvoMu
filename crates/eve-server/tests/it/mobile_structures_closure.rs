//! Integration tests for Mobile Deployable Structures (`feat:structures.mobile-depot`,
//! `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-tractor`,
//! `feat:structures.mobile-tractor-rules`, `feat:structures.mobile-cyno-beacon`,
//! `feat:structures.mobile-micro-jump-unit`, `feat:structures.mobile-deploy-restrictions`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::*;
use eve_server::services::inventory_fitting::fitting_containers_ships::{
    is_near_in_space_fitting_service, validate_single_item_can_be_opened,
};

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const SHIP_1: i64 = 100_000_001;
const SHIP_2: i64 = 100_000_002;
const TEST_SYSTEM: u32 = 30_000_142; // Jita (High-sec 0.95)

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    sess.user_id = Some(char_id as u64);
    sess.character_id = Some(char_id);
    sess.solar_system_id = Some(sys_id);
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, InventoryItemRecord {
            item_id: ship_id, type_id: 587, owner_id: char_id, location_id: sys_id as i64,
            flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let ball = Ball {
        id: ship_id, type_id: 587, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball_with_meta(
        ball,
        ctx.build_ship_combat_state(char_id, ship_id, 587),
        ctx.build_ship_slim_meta(char_id, 587),
    ).await;

    sess
}

/// Tests Mobile Depot anchoring countdown, refitting proximity, owner storage access vs non-owner rejection,
/// 48-hour reinforcement threshold at <= 25% shield HP, invulnerability while reinforced, damage retention on scoop/redeploy,
/// and 30-day lifetime refresh on interaction (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_anchoring_refit_storage_reinforce() {
    // parity: feat:structures.mobile-depot, feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&[
        "feat:structures.mobile-depot",
        "feat:structures.mobile-depot-lifetime-reinforce",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(10_000_000.0, 10_000_000.0, 10_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 101, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_item_id = 700_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_item_id, InventoryItemRecord {
            item_id: depot_item_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(depot_item_id)],
        &kw,
        None,
    ).await;
    assert!(res.is_ok(), "Mobile Depot anchor failed: {res:?}");

    let state = read_deployable_state(&ctx);
    let rec = state.deployables.get(&depot_item_id).expect("depot record");
    assert_eq!(rec.anchor_duration_secs, DEPOT_ANCHOR_TIME_SECS);
    assert!(!rec.anchor_completed);

    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    let anchor_dmg = DamageVector { em: 2000.0, thermal: 2000.0, kinetic: 2000.0, explosive: 2000.0 };
    let (dmg_done, destroyed, _) = sys.apply_damage(depot_item_id, anchor_dmg).await.expect("apply damage").expect("depot hit");
    assert!(dmg_done > 0.0);
    assert!(!destroyed);
    let rec_anchor = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().unwrap();
    assert!(rec_anchor.reinforced_until_unix.is_none(), "Depot must not reinforce during anchoring phase");

    write_deployable_state(&ctx, |st| {
        if let Some(r) = st.deployables.get_mut(&depot_item_id) {
            r.anchor_completed = true;
        }
    });

    let near = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(near, "Pilot within 2,500m of active Mobile Depot must have fitting service access");

    let _ = sys.set_ball_position(SHIP_1, ship_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let far = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(!far, "Pilot beyond 2,500m of active Mobile Depot must not have fitting service access");
    let _ = sys.set_ball_position(SHIP_1, ship_pos).await;

    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, depot_item_id).await.is_ok());

    let _sess2 = setup_pilot(&ctx, 102, PILOT_2, SHIP_2, TEST_SYSTEM, ship_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let non_owner_res = validate_single_item_can_be_opened(&ctx, PILOT_2, depot_item_id).await;
    assert!(non_owner_res.is_err());
    assert_eq!(non_owner_res.unwrap_err().code, "OwnershipViolation");

    let old_despawn = read_deployable_state(&ctx).deployables[&depot_item_id].expires_at_unix;
    touch_mobile_depot_interaction(&ctx, depot_item_id);
    let new_despawn = read_deployable_state(&ctx).deployables[&depot_item_id].expires_at_unix;
    assert!(new_despawn >= old_despawn);

    let mut test_cs = CombatState {
        shield_hp: 10_000.0,
        max_shield_hp: 10_000.0,
        armor_hp: 5_000.0,
        max_armor_hp: 5_000.0,
        structure_hp: 5_000.0,
        max_structure_hp: 5_000.0,
        ..Default::default()
    };
    let heavy_hit = DamageVector { em: 8_000.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (dmg_applied, is_reinf, is_dead) = process_mobile_depot_damage(&ctx, depot_item_id, heavy_hit, &mut test_cs);
    assert_eq!(test_cs.shield_hp, 2_500.0, "Depot shield HP must clamp to 25% threshold");
    assert!(is_reinf);
    assert!(!is_dead);
    assert!(dmg_applied > 0.0);

    let rec_reinf = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().unwrap();
    assert!(rec_reinf.reinforced_until_unix.is_some(), "Depot must enter reinforcement");
    assert!(rec_reinf.reinforcement_used);

    let reinf_hit = DamageVector { em: 1000.0, thermal: 1000.0, kinetic: 1000.0, explosive: 1000.0 };
    let (dmg_reinf, _, _) = process_mobile_depot_damage(&ctx, depot_item_id, reinf_hit, &mut test_cs);
    assert_eq!(dmg_reinf, 0.0, "Reinforced depot must take 0 damage");

    write_deployable_state(&ctx, |st| {
        if let Some(r) = st.deployables.get_mut(&depot_item_id) {
            r.reinforced_until_unix = None;
            r.reinforcement_used = true;
        }
    });
    record_scooped_deployable_damage(&ctx, depot_item_id, &test_cs);
    let damage_rec = read_deployable_state(&ctx).damaged_items.get(&depot_item_id).cloned().expect("saved damage rec");
    assert!(damage_rec.reinforcement_used);
    assert_eq!(damage_rec.shield_hp, 2_500.0);
}

/// Tests Mobile Tractor Unit 125km range, target ordering (closest first), ownership filtering (skips unowned non-abandoned wrecks),
/// idling when owner in different system, automated looting into hold, 2-day decay, and 50% loot drop on destruction
/// (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`).
#[tokio::test]
async fn test_mobile_tractor_unit_range_ordering_ownership_decay_loot() {
    // parity: feat:structures.mobile-tractor, feat:structures.mobile-tractor-rules
    eve_testkit::attest(&[
        "feat:structures.mobile-tractor",
        "feat:structures.mobile-tractor-rules",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mtu_pos = DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 201, PILOT_1, SHIP_1, TEST_SYSTEM, mtu_pos).await;

    let mtu_id = 800_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_id, InventoryItemRecord {
            item_id: mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1, location_id: SHIP_1,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(mtu_id)],
        &kw,
        None,
    ).await;
    assert!(res.is_ok(), "MTU anchor failed: {res:?}");

    let despawn = read_deployable_state(&ctx).deployables[&mtu_id].expires_at_unix;
    assert_eq!(despawn, ctx.now_unix() + MTU_LIFETIME_SECS);

    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    let wreck_far = 810_001i64;
    let wreck_near = 810_002i64;
    let wreck_out_of_range = 810_003i64;
    let wreck_other_unabandoned = 810_004i64;
    let wreck_other_abandoned = 810_005i64;

    let wrecks_data = [
        (wreck_far, mtu_pos + DVec3::new(10_000.0, 0.0, 0.0), PILOT_1, false),
        (wreck_near, mtu_pos + DVec3::new(3_000.0, 0.0, 0.0), PILOT_1, false),
        (wreck_out_of_range, mtu_pos + DVec3::new(150_000.0, 0.0, 0.0), PILOT_1, false),
        (wreck_other_unabandoned, mtu_pos + DVec3::new(4_000.0, 0.0, 0.0), PILOT_2, false),
        (wreck_other_abandoned, mtu_pos + DVec3::new(5_000.0, 0.0, 0.0), PILOT_2, true),
    ];

    for &(wid, wpos, wowner, wabandoned) in &wrecks_data {
        let ball = Ball {
            id: wid, type_id: 26526, position: wpos, velocity: DVec3::ZERO, max_velocity: 1_000.0,
            inertia: 1.0, mass: 50_000.0, radius: 20.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let _ = sys.spawn_ball(ball).await;
        ctx.db.with_mut_snapshot(|s| {
            s.items.insert(wid, InventoryItemRecord {
                item_id: wid, type_id: 26526, owner_id: if wabandoned { 0 } else { wowner },
                location_id: TEST_SYSTEM as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
            });
            s.item_positions.insert(wid, wpos.to_array());
        });
        if wabandoned {
            eve_server::services::combat_crimewatch::with_mut_combat_runtime(&ctx, |rt| {
                rt.container_lifetimes.insert(
                    wid,
                    eve_server::services::combat_crimewatch::ContainerLifetimeEntry {
                        item_id: wid,
                        solar_system_id: TEST_SYSTEM,
                        type_id: 26526,
                        owner_id: 0,
                        created_unix: ctx.now_unix(),
                        lifetime_secs: 7200,
                        is_abandoned: true,
                    },
                );
            });
        }
    }

    let target = process_mtu_tractor_tick(&ctx, &sys, mtu_id, 1.0).await;
    assert_eq!(target, Some(wreck_near), "MTU must prioritize the closest valid wreck (wreck_near at 3,000m)");

    let loot_item_id = 820_001i64;
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(
            loot_item_id,
            InventoryItemRecord {
                item_id: loot_item_id,
                type_id: 34,
                owner_id: PILOT_1,
                location_id: wreck_near,
                flag: InventoryFlag::Cargo,
                quantity: 5_000,
                singleton: false,
                ..Default::default()
            },
        );
    });

    let _ = sys.set_ball_position(wreck_near, mtu_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let _ = process_mtu_tractor_tick(&ctx, &sys, mtu_id, 1.0).await;

    let looted = ctx.db.with_snapshot(|s| s.items[&loot_item_id].location_id == mtu_id);
    assert!(looted, "MTU within 2,500m must transfer wreck items into its cargo hold");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = 30_000_144;
        }
    });
    let idle_target = process_mtu_tractor_tick(&ctx, &sys, mtu_id, 1.0).await;
    assert_eq!(idle_target, None, "MTU must idle when owner is in a different solar system");

    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = TEST_SYSTEM;
        }
    });

    let (item1, item2) = (830_001i64, 830_002i64);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(item1, InventoryItemRecord { item_id: item1, type_id: 34, owner_id: PILOT_1, location_id: mtu_id, flag: InventoryFlag::Cargo, quantity: 1000, singleton: false, ..Default::default() });
        s.items.insert(item2, InventoryItemRecord { item_id: item2, type_id: 35, owner_id: PILOT_1, location_id: mtu_id, flag: InventoryFlag::Cargo, quantity: 2000, singleton: false, ..Default::default() });
    });
    drop_mtu_loot_on_destruction(&ctx, &sys, mtu_id).await;

    let spawned_can = ctx.db.with_snapshot(|s| {
        s.items.values().find(|it| it.type_id == 23 && it.location_id == TEST_SYSTEM as i64).map(|it| it.item_id)
    });
    assert!(spawned_can.is_some(), "Destroyed MTU with loot must spawn a cargo container with remaining loot");
}

/// Tests Mobile Cynosural Beacon fleet requirements on deployment, 1-minute spool, 60/30-minute durations,
/// fleet jump visibility, cannot be scooped, and destructibility (`feat:structures.mobile-cyno-beacon`).
#[tokio::test]
async fn test_mobile_cyno_beacon_fleet_requirements_duration_destructible() {
    // parity: feat:structures.mobile-cyno-beacon
    eve_testkit::attest(&["feat:structures.mobile-cyno-beacon"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let beacon_pos = DVec3::new(20_000_000.0, 20_000_000.0, 20_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 301, PILOT_1, SHIP_1, TEST_SYSTEM, beacon_pos).await;

    let beacon_id = 900_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            beacon_id,
            InventoryItemRecord {
                item_id: beacon_id,
                type_id: TYPE_MOBILE_CYNO_BEACON,
                owner_id: PILOT_1,
                location_id: SHIP_1,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    let kw = HashMap::new();
    let skill_err = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(beacon_id)],
        &kw,
        None,
    ).await;
    assert!(skill_err.is_err());
    assert_eq!(skill_err.unwrap_err().code, "SkillMissing");

    ctx.db.with_mut_snapshot(|s| {
        let skills = s.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
    });

    let fleet_err = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(beacon_id)],
        &kw,
        None,
    ).await;
    assert!(fleet_err.is_err());
    assert_eq!(fleet_err.unwrap_err().code, "MustBeInFleet");

    sess1.fleet_id = Some(5001);

    let ok_res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(beacon_id)],
        &kw,
        None,
    ).await;
    assert!(ok_res.is_ok(), "Mobile Cyno Beacon anchor failed after meeting prerequisites: {ok_res:?}");

    let despawn = read_deployable_state(&ctx).deployables[&beacon_id].expires_at_unix;
    assert_eq!(despawn, ctx.now_unix() + CYNO_BEACON_DURATION_SECS);

    assert!(!can_deployable_be_scooped(GROUP_MOBILE_CYNO_BEACON), "Mobile Cyno Beacon must not be scoopable");

    let covert_beacon_id = 900_002i64;
    ctx.db.with_mut_snapshot(|s| {
        let skills = s.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        skills.insert(21603, eve_db::TrainedSkillRecord { type_id: 21603, level: 5, skill_points: 256_000.0 });
        s.items.insert(covert_beacon_id, InventoryItemRecord {
            item_id: covert_beacon_id, type_id: TYPE_COVERT_MOBILE_CYNO_BEACON, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let covert_res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(covert_beacon_id)],
        &kw,
        None,
    ).await;
    assert!(covert_res.is_ok(), "Covert Mobile Cyno Beacon anchor failed: {covert_res:?}");
    let covert_despawn = read_deployable_state(&ctx).deployables[&covert_beacon_id].expires_at_unix;
    assert_eq!(covert_despawn, ctx.now_unix() + COVERT_CYNO_BEACON_DURATION_SECS);
}

/// Tests Mobile Micro Jump Unit 1-minute activation spool, 2-day lifetime, cannot be scooped,
/// ship mass check (< 1,000,000,000 kg), 12s warm-up executing 100km jump in ship heading,
/// friendly/hostile usability, and zero structure cooldown (`feat:structures.mobile-micro-jump-unit`).
#[tokio::test]
async fn test_mobile_micro_jump_unit_activation_mass_jump() {
    // parity: feat:structures.mobile-micro-jump-unit
    eve_testkit::attest(&["feat:structures.mobile-micro-jump-unit"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mmju_pos = DVec3::new(30_000_000.0, 30_000_000.0, 30_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 401, PILOT_1, SHIP_1, TEST_SYSTEM, mmju_pos).await;

    let mmju_id = 950_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(mmju_id, InventoryItemRecord {
            item_id: mmju_id, type_id: TYPE_MOBILE_MJU, owner_id: PILOT_1, location_id: SHIP_1,
            flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    let res = ctx.dispatch_rpc(
        &mut sess1,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(mmju_id)],
        &kw,
        None,
    ).await;
    assert!(res.is_ok(), "MMJU anchor failed: {res:?}");

    let despawn = read_deployable_state(&ctx).deployables[&mmju_id].expires_at_unix;
    assert_eq!(despawn, ctx.now_unix() + MMJU_LIFETIME_SECS);
    assert!(!can_deployable_be_scooped(GROUP_MOBILE_MICRO_JUMP_UNIT), "MMJU must not be scoopable");

    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    let early_act = activate_mmju(&ctx, &sys, SHIP_1, mmju_id).await;
    assert!(early_act.is_err());
    assert_eq!(early_act.unwrap_err().code, "DeployableNotActive");

    write_deployable_state(&ctx, |st| {
        if let Some(r) = st.deployables.get_mut(&mmju_id) {
            r.anchor_completed = true;
        }
    });

    let titan_ship_id = 100_000_999i64;
    let _ = setup_pilot(&ctx, 402, PILOT_2, titan_ship_id, TEST_SYSTEM, mmju_pos + DVec3::new(500.0, 0.0, 0.0)).await;
    let titan_ball = Ball {
        id: titan_ship_id,
        type_id: 11567,
        position: mmju_pos + DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 100.0,
        inertia: 1.0,
        mass: 2_400_000_000.0,
        radius: 1000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(titan_ball).await;

    let heavy_err = activate_mmju(&ctx, &sys, titan_ship_id, mmju_id).await;
    assert!(heavy_err.is_err());
    assert_eq!(heavy_err.unwrap_err().code, "ShipTooHeavy");

    let initial_pos = sys.get_ball_snapshot(SHIP_1).await.unwrap().unwrap().1.position;
    let new_pos = activate_mmju(&ctx, &sys, SHIP_1, mmju_id).await.expect("activate MMJU");
    let dist_jumped = (new_pos - initial_pos).length();
    assert!((dist_jumped - MMJU_JUMP_DISTANCE_M).abs() < 1_000.0, "MMJU must jump ship exactly 100 km (jumped {dist_jumped:.1} m)");

    let hostile_ship = 100_000_003i64;
    let _ = setup_pilot(&ctx, 403, 90_000_003, hostile_ship, TEST_SYSTEM, mmju_pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
    let hostile_jump = activate_mmju(&ctx, &sys, hostile_ship, mmju_id).await;
    assert!(hostile_jump.is_ok(), "MMJU must be usable by any pilot within range regardless of ownership");
}

/// Tests deployment restrictions in starter and career-agent systems, proximity restrictions (< 50km stargate/station/structure,
/// < 5km MTU-to-MTU), and crimewatch suspect flag upon attacking deployable in highsec (`feat:structures.mobile-deploy-restrictions`).
#[tokio::test]
async fn test_mobile_deploy_restrictions_starter_career_proximity_crimewatch() {
    // parity: feat:structures.mobile-deploy-restrictions
    eve_testkit::attest(&["feat:structures.mobile-deploy-restrictions"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let starter_sys = ctx.sde.schools.values().find(|s| (11..=22).contains(&s.school_id)).map(|s| s.solar_system_id).unwrap_or(30_000_141);
    let mut sess_starter = SessionState::new(501);
    sess_starter.character_id = Some(PILOT_1);
    sess_starter.solar_system_id = Some(starter_sys);
    sess_starter.ship_id = Some(SHIP_1);

    let restr_err = check_mobile_deployment_restrictions(
        &ctx,
        &sess_starter,
        999_001,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        starter_sys,
        DVec3::new(100_000_000.0, 0.0, 0.0),
    );
    assert!(restr_err.is_err());
    assert_eq!(restr_err.unwrap_err().code, "RestrictionsInEffect");

    let mut sess_valid = SessionState::new(502);
    sess_valid.character_id = Some(PILOT_1);
    sess_valid.solar_system_id = Some(TEST_SYSTEM);
    sess_valid.ship_id = Some(SHIP_1);

    let gate_pos = ctx.sde.stargates().values().find(|j| j.from_solar_system_id == TEST_SYSTEM).map(|j| j.position).unwrap_or(DVec3::ZERO);
    let close_to_gate = check_mobile_deployment_restrictions(
        &ctx,
        &sess_valid,
        999_002,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        TEST_SYSTEM,
        gate_pos + DVec3::new(20_000.0, 0.0, 0.0),
    );
    assert!(close_to_gate.is_err());
    assert_eq!(close_to_gate.unwrap_err().code, "DeploymentTooCloseToGate");

    let station_pos = ctx.sde.solar_systems[&TEST_SYSTEM].stations.first().map(|s| s.position).unwrap_or(DVec3::ZERO);
    let close_to_station = check_mobile_deployment_restrictions(
        &ctx,
        &sess_valid,
        999_003,
        TYPE_MOBILE_DEPOT,
        GROUP_MOBILE_DEPOT,
        TEST_SYSTEM,
        station_pos + DVec3::new(30_000.0, 0.0, 0.0),
    );
    assert!(close_to_station.is_err());
    assert_eq!(close_to_station.unwrap_err().code, "DeploymentTooCloseToStation");

    let existing_mtu_id = 999_010i64;
    let mtu_anchor_pos = DVec3::new(40_000_000.0, 40_000_000.0, 40_000_000.0);
    ctx.db.with_mut_snapshot(|s| {
        s.items.insert(existing_mtu_id, InventoryItemRecord {
            item_id: existing_mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::None, quantity: 1, singleton: true, ..Default::default()
        });
        s.item_positions.insert(existing_mtu_id, mtu_anchor_pos.to_array());
    });
    write_deployable_state(&ctx, |st| {
        st.deployables.insert(
            existing_mtu_id,
            MobileDeployableRecord {
                item_id: existing_mtu_id,
                owner_id: PILOT_1,
                type_id: TYPE_MTU_STANDARD,
                group_id: GROUP_MOBILE_TRACTOR,
                solar_system_id: TEST_SYSTEM,
                position: mtu_anchor_pos.to_array(),
                anchor_start_unix: ctx.now_unix(),
                anchor_duration_secs: 10,
                anchor_completed: true,
                expires_at_unix: ctx.now_unix() + 172_800,
                last_interaction_unix: ctx.now_unix(),
                reinforced_until_unix: None,
                reinforcement_used: false,
            },
        );
    });

    let close_to_mtu = check_mobile_deployment_restrictions(
        &ctx,
        &sess_valid,
        999_011,
        TYPE_MTU_STANDARD,
        GROUP_MOBILE_TRACTOR,
        TEST_SYSTEM,
        mtu_anchor_pos + DVec3::new(2_000.0, 0.0, 0.0),
    );
    assert!(close_to_mtu.is_err());
    assert_eq!(close_to_mtu.unwrap_err().code, "DeploymentTooCloseToSameType");

    let far_safe_pos = DVec3::new(80_000_000.0, 80_000_000.0, 80_000_000.0);
    let safe_ok = check_mobile_deployment_restrictions(
        &ctx,
        &sess_valid,
        999_012,
        TYPE_MTU_STANDARD,
        GROUP_MOBILE_TRACTOR,
        TEST_SYSTEM,
        far_safe_pos,
    );
    assert!(safe_ok.is_ok(), "Deploying beyond restricted radii must succeed: {safe_ok:?}");

    let mut cw = ctx.crimewatch.write().unwrap();
    let now = ctx.now_unix();
    let effective_sec = 0.95f64.min(0.40);
    let outcome = cw.handle_combat_aggression(PILOT_2, PILOT_1, SHIP_2, TEST_SYSTEM, effective_sec, false, now);
    assert_eq!(outcome, eve_server::combat::crimewatch::AggressionEvaluation::SuspectFlagTriggered);
    let attacker_st = cw.get_or_create(PILOT_2);
    assert!(attacker_st.is_suspect(now), "Attacking mobile deployable must result in a 15-minute SUSPECT flag");
    assert!(!attacker_st.is_criminal(now), "Attacking mobile deployable in highsec must NOT flag attacker as criminal");
}
