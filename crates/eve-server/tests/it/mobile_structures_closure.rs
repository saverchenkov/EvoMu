//! Integration tests for Mobile Deployable Structures (`feat:structures.mobile-depot`,
//! `feat:structures.mobile-depot-lifetime-reinforce`, `feat:structures.mobile-deploy-restrictions`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
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

/// Tests Mobile Depot anchoring countdown, refitting proximity & owner gating, storage access,
/// 48-hour reinforcement threshold at <= 25% shield HP, invulnerability while reinforced,
/// and damage retention on scoop/redeploy (`feat:structures.mobile-depot`, `feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_anchoring_refit_storage_reinforce() {
    // parity: feat:structures.mobile-depot, feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&[
        "feat:structures.mobile-depot",
        "feat:structures.mobile-depot-lifetime-reinforce",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let _start_unix = ctx.now_unix();
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

    let rec = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().expect("depot record");
    assert_eq!(rec.anchor_duration_secs, DEPOT_ANCHOR_TIME_SECS);
    assert!(!rec.anchor_completed);

    // 1. During anchoring phase: refit service MUST be unavailable
    let near_while_anchoring = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(!near_while_anchoring, "Depot must not provide refit service while anchoring");

    // 2. During anchoring phase: damage does NOT trigger reinforcement
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    let anchor_dmg = DamageVector { em: 100.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    let (dmg_done, destroyed, _) = sys.apply_damage(depot_item_id, anchor_dmg).await.expect("apply damage").expect("depot hit");
    assert!(dmg_done > 0.0);
    assert!(!destroyed);
    let rec_anchor = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().unwrap();
    assert!(rec_anchor.reinforced_until_unix.is_none(), "Depot must not reinforce during anchoring phase");

    // Advance clock past the 60s anchoring timer
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);

    // 3. Anchoring complete: owner can refit within 2,500m
    let near_owner = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(near_owner, "Pilot 1 (owner) within 2,500m of active Mobile Depot must have fitting service access");

    // Non-owner cannot use depot for refitting
    let mut _sess2 = setup_pilot(&ctx, 102, PILOT_2, SHIP_2, TEST_SYSTEM, ship_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let near_non_owner = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_2).await;
    assert!(!near_non_owner, "Pilot 2 (non-owner) must NOT have fitting service access from Pilot 1's depot");

    // Owner beyond 2,500m cannot refit
    let _ = sys.set_ball_position(SHIP_1, ship_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let far_owner = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(!far_owner, "Owner beyond 2,500m must NOT have fitting service access");
    let _ = sys.set_ball_position(SHIP_1, ship_pos).await;

    // 4. Storage access: only owner can open depot
    assert!(validate_single_item_can_be_opened(&ctx, PILOT_1, depot_item_id).await.is_ok());
    let non_owner_res = validate_single_item_can_be_opened(&ctx, PILOT_2, depot_item_id).await;
    assert!(non_owner_res.is_err());
    assert_eq!(non_owner_res.unwrap_err().code, "OwnershipViolation");

    // Test moving an item in and out of the depot via real invbroker.Add RPC
    let stored_item_id = 800_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(stored_item_id, InventoryItemRecord {
            item_id: stored_item_id, type_id: 34, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });
    ctx.dispatch_rpc(
        &mut sess1,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(stored_item_id), PyRep::Integer(depot_item_id), PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw,
        None,
    ).await.expect("invbroker.Add to depot");
    let has_item = ctx.db.with_snapshot(|snap| snap.items.get(&stored_item_id).is_some_and(|i| i.location_id == depot_item_id));
    assert!(has_item, "Item must be stored inside Mobile Depot");

    ctx.dispatch_rpc(
        &mut sess1,
        "invbroker",
        None,
        "Add",
        &[PyRep::Integer(stored_item_id), PyRep::Integer(SHIP_1), PyRep::Integer(InventoryFlag::Cargo as i64)],
        &kw,
        None,
    ).await.expect("invbroker.Add back to ship");
    let back_in_ship = ctx.db.with_snapshot(|snap| snap.items.get(&stored_item_id).is_some_and(|i| i.location_id == SHIP_1));
    assert!(back_in_ship, "Item must be moved back to ship cargo");

    // 5. Reinforcement threshold at <= 25% shield HP
    let depot_cs = sys.get_combat_state(depot_item_id).await.unwrap().expect("depot combat state");
    let shield_threshold = depot_cs.max_shield_hp * DEPOT_REINFORCE_SHIELD_THRESHOLD;
    let current_shield = depot_cs.shield_hp;
    assert!(current_shield > shield_threshold);

    // Apply damage to bring shields to <= 25% threshold
    let damage_to_trigger = DamageVector {
        em: current_shield - shield_threshold + 500.0,
        thermal: 0.0, kinetic: 0.0, explosive: 0.0,
    };
    let now_before_hit = ctx.now_unix();
    let (dmg_applied, destroyed_reinf, _) = sys.apply_damage(depot_item_id, damage_to_trigger).await.unwrap().expect("hit");
    assert!(!destroyed_reinf);
    assert_eq!(dmg_applied, current_shield - shield_threshold, "Damage applied must stop exactly at 25% shield threshold");

    let rec_reinf = read_deployable_state(&ctx).deployables.get(&depot_item_id).cloned().expect("rec");
    let expected_reinf_until = now_before_hit.saturating_add(DEPOT_REINFORCE_DURATION_SECS);
    assert_eq!(rec_reinf.reinforced_until_unix, Some(expected_reinf_until), "Reinforce duration must be exactly 48 hours (172,800s)");

    // 6. Invulnerability while reinforced
    let follow_up_damage = DamageVector { em: 500.0, thermal: 500.0, kinetic: 500.0, explosive: 500.0 };
    let (reinf_dmg_done, reinf_destroyed, _) = sys.apply_damage(depot_item_id, follow_up_damage).await.unwrap().expect("hit");
    assert_eq!(reinf_dmg_done, 0.0, "Reinforced depot must take 0 damage (invulnerable)");
    assert!(!reinf_destroyed);

    // 7. Damage retention across scoop and redeploy
    let depot_2_pos = ship_pos + DVec3::new(10_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_1, depot_2_pos.to_array()));
    let _ = sys.set_ball_position(SHIP_1, depot_2_pos).await;

    let depot_2_id = 700_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot_2_id, InventoryItemRecord {
            item_id: depot_2_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap();
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);

    // Attempt re-anchor exploit (MD-P1-2): re-calling AnchorStructure on already anchored depot
    let reanchor_err = ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap_err();
    assert_eq!(reanchor_err.code, "StructureAlreadyAnchored", "Re-anchoring already anchored structure must fail");

    // Deal 50% shield damage to depot 2
    let d2_cs = sys.get_combat_state(depot_2_id).await.unwrap().unwrap();
    let dmg_half = DamageVector { em: d2_cs.max_shield_hp * 0.50, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    sys.apply_damage(depot_2_id, dmg_half).await.unwrap().unwrap();
    let d2_post_cs = sys.get_combat_state(depot_2_id).await.unwrap().unwrap();
    let expected_damaged_shields = d2_post_cs.shield_hp;

    // Unanchor depot 2 via real entity.CmdUnanchor RPC
    ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("CmdUnanchor depot 2");

    // Non-owner scoop negative test (MD-P1-3)
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_2, (depot_2_pos + DVec3::new(100.0, 0.0, 0.0)).to_array()));
    let _ = sys.set_ball_position(SHIP_2, depot_2_pos + DVec3::new(100.0, 0.0, 0.0)).await;
    let non_owner_scoop = ctx.dispatch_rpc(&mut _sess2, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap_err();
    assert_eq!(non_owner_scoop.code, "OwnershipViolation", "Non-owner scoop must fail with OwnershipViolation");

    // Loaded depot scoop negative test (MD-R2): cannot scoop non-empty container
    let inside_item_id = 800_005i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(inside_item_id, InventoryItemRecord {
            item_id: inside_item_id, type_id: 34, owner_id: PILOT_1,
            location_id: depot_2_id, flag: InventoryFlag::Cargo, quantity: 10, singleton: false, ..Default::default()
        });
    });
    let loaded_scoop = ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap_err();
    assert_eq!(loaded_scoop.code, "CannotPutContainerInContainer", "Scooping loaded deployable must fail");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&inside_item_id).map(|i| i.location_id)), Some(depot_2_id));
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&depot_2_id).map(|i| i.location_id)), Some(TEST_SYSTEM as i64));
    ctx.db.with_mut_snapshot(|snap| { snap.items.remove(&inside_item_id); });

    // Scoop depot 2 via real ship.Scoop RPC
    ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("Scoop depot 2");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&depot_2_id).map(|i| i.location_id)), Some(SHIP_1));
    assert!(ctx.db.with_snapshot(|s| s.items.get(&depot_2_id).is_some_and(|i| i.singleton)), "Scooped deployable must retain singleton=true");

    // Redeploy depot 2
    let _redeploy_pos = ship_pos + DVec3::new(100.0, 0.0, 0.0);
    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.unwrap();
    let redeployed_cs = sys.get_combat_state(depot_2_id).await.unwrap().expect("redeployed combat state");
    assert!((redeployed_cs.shield_hp - expected_damaged_shields).abs() < 1.0, "Redeployed depot must retain pre-scoop damage state");

    // Redeploy below 25% threshold asserting destruction instead of reinforcement (MD-P1-3)
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);
    let dmg_to_reinf = DamageVector { em: redeployed_cs.shield_hp + 500.0, thermal: 0.0, kinetic: 0.0, explosive: 0.0 };
    sys.apply_damage(depot_2_id, dmg_to_reinf).await.unwrap().unwrap();
    ctx.advance_secs(DEPOT_REINFORCE_DURATION_SECS + 1);
    sys.step_manual(1.0).await.unwrap();

    ctx.dispatch_rpc(&mut sess1, "entity", None, "CmdUnanchor", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("CmdUnanchor depot 2 post-reinf");
    ctx.dispatch_rpc(&mut sess1, "ship", None, "Scoop", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("Scoop depot 2 post-reinf");

    ctx.dispatch_rpc(&mut sess1, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await.expect("redeploy post-reinf");
    ctx.advance_secs(DEPOT_ANCHOR_TIME_SECS + 1);
    sys.step_manual(1.0).await.unwrap();

    // Attack while shields <= 25% asserting destruction instead of reinforcement (MD-R3)
    let mut cs_depleted = sys.get_combat_state(depot_2_id).await.unwrap().unwrap();
    cs_depleted.shield_hp = cs_depleted.max_shield_hp * 0.20;
    sys.set_combat_state(depot_2_id, cs_depleted).await.unwrap();

    let fatal_dmg = DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 };
    let (_, destroyed_after_redeploy, _) = sys.apply_damage(depot_2_id, fatal_dmg).await.unwrap().expect("hit");
    assert!(destroyed_after_redeploy, "Depot attacked while shields <= 25% must be destroyed without reinforcement");
}

/// Tests Mobile Depot 30-day lifetime enforcement, interaction refresh, and live expiry sweep (`feat:structures.mobile-depot-lifetime-reinforce`).
#[tokio::test]
async fn test_mobile_depot_lifetime_and_interaction_refresh() {
    // parity: feat:structures.mobile-depot-lifetime-reinforce
    eve_testkit::attest(&["feat:structures.mobile-depot-lifetime-reinforce"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(20_000_000.0, 20_000_000.0, 20_000_000.0);
    let mut sess = setup_pilot(&ctx, 201, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let depot_id = 700_010i64;
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

    // Advance clock by 15 days
    ctx.advance_secs(15 * 86_400);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(depot_id).await.unwrap().is_some(), "Depot must remain alive at day 15");

    // Pilot interacts with Mobile Depot via in-space refitting service -> resets the 30-day lifetime from current time
    let fitting_available = is_near_in_space_fitting_service(&ctx, TEST_SYSTEM, SHIP_1).await;
    assert!(fitting_available, "Pilot must have in-space fitting service near depot");
    let rec1 = read_deployable_state(&ctx).deployables.get(&depot_id).cloned().expect("depot record");
    let expected_refreshed_expiry = start_unix + 15 * 86_400 + DEPOT_LIFETIME_SECS;
    assert_eq!(rec1.expires_at_unix, expected_refreshed_expiry, "Interaction must refresh 30-day lifetime from current timestamp");

    // Advance clock another 20 days (total 35 days elapsed from deploy; would have expired without refresh)
    ctx.advance_secs(20 * 86_400);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(depot_id).await.unwrap().is_some(), "Depot must remain alive at day 35 due to refreshed lifetime");

    // Advance clock another 15 days (total 50 days elapsed, exceeding the refreshed 45-day expiry)
    ctx.advance_secs(15 * 86_400 + 1);
    sys.step_manual(1.0).await.unwrap();

    // Verify live expiry sweep removed the depot ball from ballpark and cleaned up the item from DB
    assert!(sys.get_combat_state(depot_id).await.unwrap().is_none(), "Expired depot must be removed from ballpark");
    let item_in_db = ctx.db.with_snapshot(|snap| snap.items.contains_key(&depot_id));
    assert!(!item_in_db, "Expired depot must be removed from database");
    let rec_in_state = read_deployable_state(&ctx).deployables.contains_key(&depot_id);
    assert!(!rec_in_state, "Expired depot must be removed from deployable runtime state");
}

/// Tests deployment restrictions in starter/career systems, 50km proximity rules,
/// Mobile Warp Disruptor (group 361) nullsec gate regression, and Crimewatch suspect flag (`feat:structures.mobile-deploy-restrictions`).
#[tokio::test]
async fn test_mobile_deploy_restrictions_and_gate_bubble_regression() {
    // parity: feat:structures.mobile-deploy-restrictions
    eve_testkit::attest(&["feat:structures.mobile-deploy-restrictions"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(10_000_000.0, 10_000_000.0, 10_000_000.0);
    let _sess = setup_pilot(&ctx, 301, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    // 1. Gating in starter / career agent systems
    let starter_sys = ctx.sde.schools.values().find(|s| (11..=22).contains(&s.school_id)).map(|s| s.solar_system_id).unwrap_or(30000140);
    let depot_item_id = 700_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(depot_item_id, InventoryItemRecord {
            item_id: depot_item_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let mut starter_sess = setup_pilot(&ctx, 302, PILOT_1, SHIP_1, starter_sys, ship_pos).await;
    let kw = HashMap::new();
    let starter_res = ctx.dispatch_rpc(
        &mut starter_sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(depot_item_id)],
        &kw,
        None,
    ).await;
    assert!(starter_res.is_err());
    assert_eq!(starter_res.unwrap_err().code, "RestrictionsInEffect", "Must block mobile structures in starter systems");

    // 2. Proximity to Stargate (>= 50km)
    let gate_pos = ctx.sde.stargates().values().find(|g| g.from_solar_system_id == TEST_SYSTEM).map(|g| g.position).unwrap_or(DVec3::ZERO);
    let mut gate_sess = setup_pilot(&ctx, 303, PILOT_1, SHIP_1, TEST_SYSTEM, gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let gate_res = ctx.dispatch_rpc(
        &mut gate_sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(depot_item_id)],
        &kw,
        None,
    ).await;
    assert!(gate_res.is_err());
    assert_eq!(gate_res.unwrap_err().code, "DeploymentTooCloseToGate", "Must block mobile deployable within 50km of stargates");

    // 3. Regression test (MD-03): Mobile Warp Disruptor (group 361) at Stargate in nullsec
    let (nullsec_sys, nullsec_gate_pos) = ctx.sde.stargates().values()
        .filter_map(|g| {
            let sys = ctx.sde.solar_systems.get(&g.from_solar_system_id)?;
            if sys.security_status <= 0.0 && sys.solar_system_id < 31_000_000 {
                Some((sys.solar_system_id, g.position))
            } else {
                None
            }
        })
        .next()
        .expect("nullsec system with stargate");
    let mwd_item_id = 700_021i64;
    let mwd_type_id = 12198; // Mobile Small Warp Disruptor I (group 361)
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(3435, eve_db::TrainedSkillRecord { type_id: 3435, level: 5, skill_points: 256_000.0 });
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 5, skill_points: 256_000.0 });
        snap.items.insert(mwd_item_id, InventoryItemRecord {
            item_id: mwd_item_id, type_id: mwd_type_id, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let mut nullsec_sess = setup_pilot(&ctx, 304, PILOT_1, SHIP_1, nullsec_sys, nullsec_gate_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let mwd_res = ctx.dispatch_rpc(
        &mut nullsec_sess,
        "posMgr",
        None,
        "AnchorStructure",
        &[PyRep::Integer(mwd_item_id)],
        &kw,
        None,
    ).await;
    assert!(mwd_res.is_ok(), "Nullsec Mobile Warp Disruptor MUST be allowed to anchor within 50km of stargate: {mwd_res:?}");

    // 4. Proximity between two Mobile Depots (>= 5km)
    let valid_pos = DVec3::new(30_000_000.0, 30_000_000.0, 30_000_000.0);
    let mut sess_valid = setup_pilot(&ctx, 305, PILOT_1, SHIP_1, TEST_SYSTEM, valid_pos).await;
    let depot_1_id = 700_022i64;
    let depot_2_id = 700_023i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot_1_id, InventoryItemRecord {
            item_id: depot_1_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(depot_2_id, InventoryItemRecord {
            item_id: depot_2_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut sess_valid, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_1_id)], &kw, None).await.unwrap();
    let depot_2_res = ctx.dispatch_rpc(&mut sess_valid, "posMgr", None, "AnchorStructure", &[PyRep::Integer(depot_2_id)], &kw, None).await;
    assert!(depot_2_res.is_err());
    assert_eq!(depot_2_res.unwrap_err().code, "DeploymentTooCloseToSameType", "Cannot anchor second depot within 5km of another depot");

    // Proximity to Station (>= 50km) (MD-P1-3)
    let st_pos = ctx.sde.stations.values().find(|s| s.solar_system_id == TEST_SYSTEM).map(|s| s.position).unwrap_or(DVec3::new(100_000.0, 0.0, 0.0));
    let mut station_sess = setup_pilot(&ctx, 307, PILOT_1, SHIP_1, TEST_SYSTEM, st_pos + DVec3::new(10_000.0, 0.0, 0.0)).await;
    let st_depot_id = 700_024i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(st_depot_id, InventoryItemRecord {
            item_id: st_depot_id, type_id: TYPE_MOBILE_DEPOT, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let st_res = ctx.dispatch_rpc(&mut station_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(st_depot_id)], &kw, None).await;
    assert_eq!(st_res.unwrap_err().code, "DeploymentTooCloseToStation", "Cannot deploy mobile structure within 50km of station");

    // Proximity between two MTUs (>= 5km) (MD-P1-3)
    let mtu_pos1 = valid_pos + DVec3::new(200_000.0, 0.0, 0.0);
    let mut mtu_sess = setup_pilot(&ctx, 308, PILOT_1, SHIP_1, TEST_SYSTEM, mtu_pos1).await;
    let mtu_1_id = 700_025i64;
    let mtu_2_id = 700_026i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_1_id, InventoryItemRecord {
            item_id: mtu_1_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(mtu_2_id, InventoryItemRecord {
            item_id: mtu_2_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    ctx.dispatch_rpc(&mut mtu_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_1_id)], &kw, None).await.unwrap();
    let mtu_2_res = ctx.dispatch_rpc(&mut mtu_sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_2_id)], &kw, None).await;
    assert_eq!(mtu_2_res.unwrap_err().code, "DeploymentTooCloseToSameType", "Cannot deploy second MTU within 5km of another MTU");

    // 5. Crimewatch Suspect Flag (MD-07): Pilot 2 attacks Pilot 1's anchored depot in highsec through real weapons dispatch
    let mut sess_p2 = setup_pilot(&ctx, 306, PILOT_2, SHIP_2, TEST_SYSTEM, valid_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
    let turret_mod_id = 800_100i64;
    let ammo_mod_id = 800_101i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(turret_mod_id, InventoryItemRecord {
            item_id: turret_mod_id, type_id: 47272, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, ..Default::default()
        });
        snap.items.insert(ammo_mod_id, InventoryItemRecord {
            item_id: ammo_mod_id, type_id: 47925, owner_id: PILOT_2,
            location_id: SHIP_2, flag: eve_db::InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });

    let sys_handle = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys_handle.add_target_lock(SHIP_2, eve_destiny::TargetLock { target_id: depot_1_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(50_000.0) }).await.unwrap();
    let now_attack = ctx.now_unix();
    let fire_res = ctx.dispatch_rpc(
        &mut sess_p2,
        "dogmaIM",
        None,
        "Activate",
        &[PyRep::Integer(turret_mod_id), PyRep::Integer(depot_1_id)],
        &kw,
        None,
    ).await;
    assert!(fire_res.is_ok(), "Firing on mobile deployable in highsec must succeed: {fire_res:?}");

    // Verify Pilot 2 received a SUSPECT flag, NOT a CRIMINAL flag
    let p2_state = ctx.crimewatch.read().unwrap().states.get(&PILOT_2).cloned().expect("p2 cw state");
    assert!(p2_state.is_suspect(now_attack), "Attacking mobile deployable in highsec must trigger suspect flag");
    assert!(!p2_state.is_criminal(now_attack), "Attacking mobile deployable must NOT trigger criminal flag");
}
