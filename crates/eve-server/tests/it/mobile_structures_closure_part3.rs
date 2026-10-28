//! Integration tests for Mobile Deployable lifecycles, cluster tick decay, multi-reinforcement,
//! and Cyno Inhibitor restrictions (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-deploy-restrictions`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::inventory_fitting::fitting_containers_ships::validate_single_item_can_be_opened;
use eve_server::services::mobile_deployables::*;
use eve_server::sp6_lifecycle::tick_background_cluster;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const SHIP_1: i64 = 100_000_001;
const SHIP_2: i64 = 100_000_002;
const TEST_SYSTEM: u32 = 30_000_142; // Jita

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
        if !snap.characters.contains_key(&char_id) {
            if let Some(mut tmpl) = snap.characters.values().next().cloned() {
                tmpl.character_id = char_id;
                tmpl.corporation_id = 1_000_002;
                tmpl.security_status = 5.0;
                snap.characters.insert(char_id, tmpl);
            }
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.security_status = 5.0;
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

/// Tests Mobile Depot cluster tick decay vs 30-day lifetime refresh and expiry (`feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_cluster_tick_decay_vs_30day_lifetime() {
    // parity: feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&["feat:structures.mobile-depot-lifetime-reinforce"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(60_000_000.0, 60_000_000.0, 60_000_000.0);
    let mut sess = setup_pilot(&ctx, 601, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_id = 700_040i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_id, InventoryItemRecord {
            item_id: depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap();

    let rec0 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("depot record");
    assert_eq!(rec0.expires_at_unix, start_unix + DEPOT_LIFETIME_SECS);

    // Advance clock past 2 hours (7,300s > legacy 7,200s secure container decay window)
    ctx.advance_secs(7_300);
    tick_background_cluster(&ctx).await;

    // Verify depot is STILL alive in space (NOT deleted by legacy 2-hour container decay sweep)
    assert!(ctx.db.with_snapshot(|s| s.items.contains_key(&depot_id)), "Mobile Depot must NOT be deleted by legacy 2h container decay sweep");
    assert!(read_deployable_state(&ctx).deployables.contains_key(&depot_id), "Mobile Depot record must persist");

    // Interact with depot -> resets lifetime to 30 days from now
    validate_single_item_can_be_opened(&ctx, PILOT_1, depot_id).await.expect("open depot");
    let rec_refreshed = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("rec");
    let now_interact = ctx.now_unix();
    assert_eq!(rec_refreshed.expires_at_unix, now_interact.saturating_add(DEPOT_LIFETIME_SECS));

    // Advance cluster tick past 30 days
    ctx.advance_secs(DEPOT_LIFETIME_SECS + 1);
    tick_background_cluster(&ctx).await;

    // Verify it cleanly expires
    assert!(!ctx.db.with_snapshot(|s| s.items.contains_key(&depot_id)), "Mobile Depot must cleanly expire after 30 days");
    assert!(!read_deployable_state(&ctx).deployables.contains_key(&depot_id), "Mobile Depot record must be removed on expiry");
}

/// Tests Mobile Depot multi-reinforcement upon shield regeneration above 25% (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_multi_reinforcement_after_shield_regen() {
    // parity: feat:structures.mobile-depot, feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&[
        "feat:structures.mobile-depot",
        "feat:structures.mobile-depot-lifetime-reinforce",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(65_000_000.0, 65_000_000.0, 65_000_000.0);
    let mut sess = setup_pilot(&ctx, 602, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_id = 700_050i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_id, InventoryItemRecord {
            item_id: depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_id)], &kw, None).await.unwrap();
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();

    let d_cs = sys.get_combat_state(depot_id).await.unwrap().expect("combat state");
    let threshold = d_cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD;

    // Attack 1: bring shields across 25% threshold -> enters 1st reinforcement
    let dmg1 = DamageVector { em: d_cs.shield_hp - threshold + 500.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (applied1, destroyed1, _) = sys.apply_damage(depot_id, dmg1).await.unwrap().unwrap();
    assert!(!destroyed1);
    assert_eq!(applied1, d_cs.shield_hp - threshold);
    let rec1 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().unwrap();
    assert!(rec1.reinforced_until_unix.is_some(), "Depot must enter reinforcement");

    // Advance clock past 48-hour reinforcement duration
    ctx.advance_secs(DEPOT_REINFORCE_DURATION_SECS + 1);
    sys.step_manual(1.0).await.unwrap();

    // Verify reinforcement timer is cleared
    let rec_post = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().unwrap();
    assert!(rec_post.reinforced_until_unix.is_none(), "Reinforcement timer must be cleared after duration");

    // Regenerate shields above 25% threshold
    let mut cs_to_regen = sys.get_combat_state(depot_id).await.unwrap().unwrap();
    cs_to_regen.shield_hp = cs_to_regen.max_shield_hp * 0.75;
    sys.set_combat_state(depot_id, cs_to_regen).await.unwrap();

    let cs_regen = sys.get_combat_state(depot_id).await.unwrap().unwrap();
    assert!(cs_regen.shield_hp > threshold, "Shields must be above 25% threshold");

    // Attack 2: bring shields across 25% threshold a second time -> enters 2nd reinforcement!
    let dmg2 = DamageVector { em: cs_regen.shield_hp - threshold + 300.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (applied2, destroyed2, _) = sys.apply_damage(depot_id, dmg2).await.unwrap().unwrap();
    assert!(!destroyed2);
    assert_eq!(applied2, cs_regen.shield_hp - threshold);
    let rec2 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().unwrap();
    assert!(rec2.reinforced_until_unix.is_some(), "Depot must enter reinforcement a SECOND time upon shield regeneration");
}

/// Tests Mobile Cyno Inhibitor (group 1249, type 33476) deploy restrictions and suspect-only crimewatch rules (`feat:structures.mobile-deploy-restrictions`).
#[tokio::test]
async fn test_mobile_cyno_inhibitor_deploy_restrictions_and_suspect_flag() {
    // parity: feat:structures.mobile-deploy-restrictions
    eve_testkit::attest(&["feat:structures.mobile-deploy-restrictions"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(70_000_000.0, 70_000_000.0, 70_000_000.0);
    let kw = HashMap::new();

    // 1. Starter / Career agent system ban (RestrictionsInEffect)
    let starter_sys = ctx.sde.schools.values().find(|s| (11..=22).contains(&s.school_id)).map(|s| s.solar_system_id).unwrap_or(30000140);
    let mci_starter_id = 700_060i64;
    let mci_type_id = 33476; // Mobile Cynosural Inhibitor (group 1249)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        snap.items.insert(mci_starter_id, InventoryItemRecord {
            item_id: mci_starter_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut starter_sess = setup_pilot(&ctx, 603, PILOT_1, SHIP_1, starter_sys, ship_pos).await;
    let starter_res = ctx.dispatch_rpc(&mut starter_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_starter_id)], &kw, None).await;
    assert_eq!(starter_res.unwrap_err().code, "RestrictionsInEffect", "Mobile Cyno Inhibitor must be blocked in starter systems");

    // 2. Proximity to Stargate (>= 50km)
    let gate_pos = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == TEST_SYSTEM).map(|g| g.position).unwrap_or(DVec3::ZERO);
    let mci_gate_id = 700_061i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_gate_id, InventoryItemRecord {
            item_id: mci_gate_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut gate_sess = setup_pilot(&ctx, 604, PILOT_1, SHIP_1, TEST_SYSTEM, gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let gate_res = ctx.dispatch_rpc(&mut gate_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_gate_id)], &kw, None).await;
    assert_eq!(gate_res.unwrap_err().code, "DeploymentTooCloseToGate", "Mobile Cyno Inhibitor must be blocked within 50km of stargates");

    // 3. Proximity to Station (>= 50km)
    let st_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == TEST_SYSTEM).map(|s| s.position).unwrap_or(DVec3::new(100_000.0, 0.0, 0.0));
    let mci_station_id = 700_062i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_station_id, InventoryItemRecord {
            item_id: mci_station_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut station_sess = setup_pilot(&ctx, 605, PILOT_1, SHIP_1, TEST_SYSTEM, st_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let station_res = ctx.dispatch_rpc(&mut station_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_station_id)], &kw, None).await;
    assert_eq!(station_res.unwrap_err().code, "DeploymentTooCloseToStation", "Mobile Cyno Inhibitor must be blocked within 50km of stations");

    // 4. Valid deployment & Crimewatch suspect flag upon attack in highsec
    let valid_pos = DVec3::new(80_000_000.0, 80_000_000.0, 80_000_000.0);
    let mci_valid_id = 700_063i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mci_valid_id, InventoryItemRecord {
            item_id: mci_valid_id, type_id: mci_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut valid_sess = setup_pilot(&ctx, 606, PILOT_1, SHIP_1, TEST_SYSTEM, valid_pos).await;
    let anchor_res = ctx.dispatch_rpc(&mut valid_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mci_valid_id)], &kw, None).await;
    assert!(anchor_res.is_ok(), "Mobile Cyno Inhibitor anchoring in valid space must succeed: {anchor_res:?}");

    // Pilot 2 attacks Pilot 1's Mobile Cyno Inhibitor
    let mut sess_p2 = setup_pilot(&ctx, 607, PILOT_2, SHIP_2, TEST_SYSTEM, valid_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let turret_id = 800_200i64;
    let ammo_id = 800_201i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_id, InventoryItemRecord {
            item_id: turret_id, type_id: 47272, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(ammo_id, InventoryItemRecord {
            item_id: ammo_id, type_id: 47925, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys_handle.add_target_lock(SHIP_2, eve_destiny::TargetLock { target_id: mci_valid_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    let now_attack = ctx.now_unix();
    let fire_res = ctx.dispatch_rpc(
        &mut sess_p2,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::Integer(mci_valid_id)],
        &kw,
        None,
    ).await;
    assert!(fire_res.is_ok(), "Attacking mobile deployable must succeed: {fire_res:?}");

    // Verify Pilot 2 received SUSPECT flag, NOT CRIMINAL flag
    let p2_state = ctx.crimewatch.read().unwrap().states.get(&PILOT_2).cloned().expect("p2 cw state");
    assert!(p2_state.is_suspect(now_attack), "Attacking Mobile Cyno Inhibitor must trigger suspect flag");
    assert!(!p2_state.is_criminal(now_attack), "Attacking Mobile Cyno Inhibitor must NOT trigger criminal flag");
}
