//! Integration tests for Mobile Tractor Units (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`).

#![forbid(unsafe_code)]

use std::collections::HashMap;
use glam::DVec3;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::mobile_deployables::*;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const SHIP_1: i64 = 100_000_001;
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
                snap.characters.insert(char_id, tmpl);
            }
        }
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

/// Tests Mobile Tractor Unit automated tractoring within 125km, owner-only wreck targeting,
/// closest-first ordering, hold capacity gating (27,000 m³), and cargo looting into MTU hold
/// (`feat:structures.mobile-tractor`, `feat:structures.mobile-tractor-rules`).
#[tokio::test]
async fn test_mobile_tractor_unit_tractor_and_looting() {
    // parity: feat:structures.mobile-tractor, feat:structures.mobile-tractor-rules
    eve_testkit::attest(&[
        "feat:structures.mobile-tractor",
        "feat:structures.mobile-tractor-rules",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let ship_pos = DVec3::new(40_000_000.0, 40_000_000.0, 40_000_000.0);
    let mut sess1 = setup_pilot(&ctx, 401, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let mtu_id = 710_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(mtu_id, InventoryItemRecord {
            item_id: mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
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

    // Advance clock past the MTU anchoring delay
    ctx.advance_secs(15);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();

    // Spawn Pilot 1's wreck at 40km from MTU
    let wreck_owner_id = 720_001i64;
    let wreck_owner_pos = ship_pos + DVec3::new(40_000.0, 0.0, 0.0);
    let item_a_id = 730_001i64;
    let item_b_id = 730_002i64;

    // Spawn Pilot 1's second wreck at 60km from MTU (MD-P1-1)
    let wreck_owner_2_id = 720_004i64;
    let wreck_owner_2_pos = ship_pos + DVec3::new(60_000.0, 0.0, 0.0);
    let item_c_id = 730_004i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_owner_id, InventoryItemRecord {
            item_id: wreck_owner_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(wreck_owner_id, wreck_owner_pos.to_array());
        // Items in wreck 1: Tritanium (0.01 m3) and Pyerite (0.01 m3)
        snap.items.insert(item_a_id, InventoryItemRecord {
            item_id: item_a_id, type_id: 34, owner_id: PILOT_1,
            location_id: wreck_owner_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, ..Default::default()
        });
        snap.items.insert(item_b_id, InventoryItemRecord {
            item_id: item_b_id, type_id: 35, owner_id: PILOT_1,
            location_id: wreck_owner_id, flag: InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });

        // Items in wreck 2: Tritanium
        snap.items.insert(wreck_owner_2_id, InventoryItemRecord {
            item_id: wreck_owner_2_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(wreck_owner_2_id, wreck_owner_2_pos.to_array());
        snap.items.insert(item_c_id, InventoryItemRecord {
            item_id: item_c_id, type_id: 34, owner_id: PILOT_1,
            location_id: wreck_owner_2_id, flag: InventoryFlag::Cargo, quantity: 75, singleton: false, ..Default::default()
        });
    });
    let _ = sys.spawn_ball(Ball {
        id: wreck_owner_id, type_id: 26557, position: wreck_owner_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;
    let _ = sys.spawn_ball(Ball {
        id: wreck_owner_2_id, type_id: 26557, position: wreck_owner_2_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Spawn Pilot 2's wreck at 30km from MTU (closer, but not owned by MTU owner)
    let wreck_other_id = 720_002i64;
    let wreck_other_pos = ship_pos + DVec3::new(30_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_other_id, InventoryItemRecord {
            item_id: wreck_other_id, type_id: 26557, owner_id: PILOT_2,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(wreck_other_id, wreck_other_pos.to_array());
    });
    let _ = sys.spawn_ball(Ball {
        id: wreck_other_id, type_id: 26557, position: wreck_other_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Step 1: MTU ticks
    sys.step_manual(1.0).await.unwrap();

    // 1. Verify Pilot 2's wreck is NOT tractored (non-owner wrecks are ignored)
    let other_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_other_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(other_pos), wreck_other_pos, "MTU must NOT tractor wrecks belonging to other pilots");

    // 2. Verify Pilot 1's wreck 1 (closest, 40km) was pulled towards MTU by ~1,000m (tractoring speed: 1,000 m/s)
    let owner_pos_step1 = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_id).copied()).unwrap();
    let initial_dist = (wreck_owner_pos - ship_pos).length();
    let post_dist = (DVec3::from_array(owner_pos_step1) - ship_pos).length();
    assert!((initial_dist - post_dist - 1_000.0).abs() < 100.0, "Wreck 1 must be pulled ~1,000m per 1.0s tick");

    // Verify wreck 2 (60km) has not been pulled yet
    let owner_2_pos_step1 = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_2_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(owner_2_pos_step1), wreck_owner_2_pos, "Wreck 2 must not be tractored while Wreck 1 is active");

    // Step until wreck 1 arrives within 2,500m of MTU
    for _ in 0..40 {
        sys.step_manual(1.0).await.unwrap();
    }

    // 3. Verify wreck 1 items are transferred into the MTU cargo hold
    let item_a_loc = ctx.db.with_snapshot(|snap| snap.items.get(&item_a_id).map(|i| i.location_id));
    let item_b_loc = ctx.db.with_snapshot(|snap| snap.items.get(&item_b_id).map(|i| i.location_id));
    assert_eq!(item_a_loc, Some(mtu_id), "Item A must be looted into MTU cargo hold");
    assert_eq!(item_b_loc, Some(mtu_id), "Item B must be looted into MTU cargo hold");

    // 4. MTU now moves on to tractor wreck 2 (60km) (MD-P1-1)
    let pre_w2_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_2_id).copied()).unwrap();
    sys.step_manual(1.0).await.unwrap();
    let post_w2_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&wreck_owner_2_id).copied()).unwrap();
    let w2_dist_pre = (DVec3::from_array(pre_w2_pos) - ship_pos).length();
    let w2_dist_post = (DVec3::from_array(post_w2_pos) - ship_pos).length();
    assert!((w2_dist_pre - w2_dist_post - 1_000.0).abs() < 100.0, "Wreck 2 must now be pulled ~1,000m per tick");

    // Step until wreck 2 arrives within 2,500m of MTU and is looted
    for _ in 0..60 {
        sys.step_manual(1.0).await.unwrap();
    }
    let item_c_loc = ctx.db.with_snapshot(|snap| snap.items.get(&item_c_id).map(|i| i.location_id));
    assert_eq!(item_c_loc, Some(mtu_id), "Item C must be looted into MTU cargo hold");

    // 4. Hold Capacity check: spawn an item that exceeds MTU capacity (27,000 m³)
    let huge_wreck_id = 720_003i64;
    let huge_item_id = 730_003i64;
    let near_wreck_pos = ship_pos + DVec3::new(1_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(huge_wreck_id, InventoryItemRecord {
            item_id: huge_wreck_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(huge_wreck_id, near_wreck_pos.to_array());
        // Type 20185 (Freighter hull) volume is ~1,000,000 m3
        snap.items.insert(huge_item_id, InventoryItemRecord {
            item_id: huge_item_id, type_id: 20185, owner_id: PILOT_1,
            location_id: huge_wreck_id, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    sys.step_manual(1.0).await.unwrap();

    let huge_loc = ctx.db.with_snapshot(|snap| snap.items.get(&huge_item_id).map(|i| i.location_id));
    assert_eq!(huge_loc, Some(huge_wreck_id), "Item exceeding MTU capacity must NOT be looted into MTU hold");
}

/// Tests Mobile Tractor Unit idle state when owner is absent, 2-day decay lifetime,
/// and 50% random cargo loot drop on destruction (`feat:structures.mobile-tractor-rules`).
#[tokio::test]
async fn test_mobile_tractor_unit_rules_and_destruction() {
    // parity: feat:structures.mobile-tractor-rules
    eve_testkit::attest(&["feat:structures.mobile-tractor-rules"]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let _start_unix = ctx.now_unix();
    let ship_pos = DVec3::new(50_000_000.0, 50_000_000.0, 50_000_000.0);
    let mut sess = setup_pilot(&ctx, 501, PILOT_1, SHIP_1, TEST_SYSTEM, ship_pos).await;

    let mtu_id = 710_010i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(PILOT_1).or_default();
        skills.insert(11584, eve_db::TrainedSkillRecord { type_id: 11584, level: 3, skill_points: 40_000.0 });
        snap.items.insert(mtu_id, InventoryItemRecord {
            item_id: mtu_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });

    let kw = HashMap::new();
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_id)], &kw, None).await.unwrap();
    ctx.advance_secs(15);
    let sys = ctx.systems.get_or_spawn(TEST_SYSTEM);
    sys.step_manual(1.0).await.unwrap();

    // Spawn a wreck at 20km with items
    let test_wreck_id = 720_010i64;
    let test_wreck_pos = ship_pos + DVec3::new(20_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(test_wreck_id, InventoryItemRecord {
            item_id: test_wreck_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(test_wreck_id, test_wreck_pos.to_array());
        snap.items.insert(730_010i64, InventoryItemRecord {
            item_id: 730_010i64, type_id: 34, owner_id: PILOT_1,
            location_id: test_wreck_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, ..Default::default()
        });
    });
    let _ = sys.spawn_ball(Ball {
        id: test_wreck_id, type_id: 26557, position: test_wreck_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // 1. Owner leaves system (docks in station)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = Some(60003760);
        }
    });
    sys.step_manual(1.0).await.unwrap();
    let pos_docked = ctx.db.with_snapshot(|snap| snap.item_positions.get(&test_wreck_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(pos_docked), test_wreck_pos, "MTU must be idle when owner is docked or not in space");

    // Owner undocks back to system
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.station_id = None;
        }
    });
    sys.step_manual(1.0).await.unwrap();
    let pos_undocked = ctx.db.with_snapshot(|snap| snap.item_positions.get(&test_wreck_id).copied()).unwrap();
    assert_ne!(DVec3::from_array(pos_undocked), test_wreck_pos, "MTU must resume tractoring when owner is active in system");

    // 2. MTU 2-day decay lifetime
    let mtu_decay_id = 710_020i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_decay_id, InventoryItemRecord {
            item_id: mtu_decay_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let decay_deploy_pos = ship_pos + DVec3::new(10_000.0, 10_000.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_1, decay_deploy_pos.to_array()));
    let _ = sys.set_ball_position(SHIP_1, decay_deploy_pos).await;
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_decay_id)], &kw, None).await.unwrap();
    ctx.advance_secs(15);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(mtu_decay_id).await.unwrap().is_some(), "MTU must be active after anchoring");

    // Advance clock past 2 days (172,800s)
    ctx.advance_secs(MTU_LIFETIME_SECS + 1);
    sys.step_manual(1.0).await.unwrap();
    assert!(sys.get_combat_state(mtu_decay_id).await.unwrap().is_none(), "MTU must decay and despawn after 2 days");
    let decay_in_db = ctx.db.with_snapshot(|snap| snap.items.contains_key(&mtu_decay_id));
    assert!(!decay_in_db, "Decayed MTU item must be removed from DB");

    // 3. MTU Destruction Loot Drop: drops 50% random loot into a cargo container
    let mtu_loot_id = 710_030i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(mtu_loot_id, InventoryItemRecord {
            item_id: mtu_loot_id, type_id: TYPE_MTU_STANDARD, owner_id: PILOT_1,
            location_id: SHIP_1, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
    });
    let loot_deploy_pos = ship_pos + DVec3::new(20_000.0, 20_000.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| snap.item_positions.insert(SHIP_1, loot_deploy_pos.to_array()));
    let _ = sys.set_ball_position(SHIP_1, loot_deploy_pos).await;
    ctx.dispatch_rpc(&mut sess, "posMgr", None, "AnchorStructure", &[PyRep::Integer(mtu_loot_id)], &kw, None).await.unwrap();
    ctx.advance_secs(15);
    sys.step_manual(1.0).await.unwrap();

    // Populate 6 cargo items inside the MTU hold
    let loot_ids: Vec<i64> = (1..=6).map(|i| 740_000 + i).collect();
    ctx.db.with_mut_snapshot(|snap| {
        for &lid in &loot_ids {
            snap.items.insert(lid, InventoryItemRecord {
                item_id: lid, type_id: 34, owner_id: PILOT_1,
                location_id: mtu_loot_id, flag: InventoryFlag::Cargo, quantity: 100, singleton: false, ..Default::default()
            });
        }
    });

    // Destroy MTU with heavy damage
    let fatal_dmg = DamageVector { em: 50_000.0, thermal: 50_000.0, kinetic: 50_000.0, explosive: 50_000.0 };
    let (_, destroyed, _) = sys.apply_damage(mtu_loot_id, fatal_dmg).await.unwrap().expect("hit");
    assert!(destroyed, "MTU must be destroyed by lethal damage");

    // Verify container spawned at MTU position holding dropped loot
    let (container_found, dropped_count) = ctx.db.with_snapshot(|snap| {
        let cans: Vec<i64> = snap.items.values()
            .filter(|i| i.type_id == 23 && i.location_id == TEST_SYSTEM as i64)
            .map(|i| i.item_id)
            .collect();
        let mut drop_total = 0;
        for &cid in &cans {
            let inside = snap.items.values().filter(|i| i.location_id == cid).count();
            if inside > 0 {
                drop_total += inside;
            }
        }
        (!cans.is_empty(), drop_total)
    });
    assert!(container_found, "Cargo container must spawn upon MTU destruction");
    assert!(dropped_count > 0, "Some items must drop into the container per the 50% roll");

    // MD-P0-1 Regression: verify record removed from st.deployables upon MTU destruction
    let dep_st = read_deployable_state(&ctx);
    assert!(!dep_st.deployables.contains_key(&mtu_loot_id), "Destroyed MTU must be removed from deployable runtime state");

    // Spawn an owner wreck with contents at 20km from destroyed MTU
    let after_wreck_id = 720_099i64;
    let after_wreck_pos = loot_deploy_pos + DVec3::new(20_000.0, 0.0, 0.0);
    let after_item_id = 730_099i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(after_wreck_id, InventoryItemRecord {
            item_id: after_wreck_id, type_id: 26557, owner_id: PILOT_1,
            location_id: TEST_SYSTEM as i64, flag: InventoryFlag::Cargo, quantity: 1, singleton: true, ..Default::default()
        });
        snap.item_positions.insert(after_wreck_id, after_wreck_pos.to_array());
        snap.items.insert(after_item_id, InventoryItemRecord {
            item_id: after_item_id, type_id: 34, owner_id: PILOT_1,
            location_id: after_wreck_id, flag: InventoryFlag::Cargo, quantity: 50, singleton: false, ..Default::default()
        });
    });
    let _ = sys.spawn_ball(Ball {
        id: after_wreck_id, type_id: 26557, position: after_wreck_pos, velocity: DVec3::ZERO,
        max_velocity: 1000.0, inertia: 1.0, mass: 10_000.0, radius: 30.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }).await;

    // Step system for 5 ticks
    for _ in 0..5 {
        sys.step_manual(1.0).await.unwrap();
    }

    // Assert wreck position does NOT change and contents are not looted
    let cur_pos = ctx.db.with_snapshot(|snap| snap.item_positions.get(&after_wreck_id).copied()).unwrap();
    assert_eq!(DVec3::from_array(cur_pos), after_wreck_pos, "Destroyed MTU must not tractor any wrecks");
    let cur_loc = ctx.db.with_snapshot(|snap| snap.items.get(&after_item_id).map(|i| i.location_id));
    assert_eq!(cur_loc, Some(after_wreck_id), "Contents must never be looted into destroyed MTU");
}
