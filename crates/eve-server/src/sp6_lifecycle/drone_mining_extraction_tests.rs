//! Semantic unit tests split from `drone_recovery_tests`.

use super::*;
use super::super::step_repeating_drone_mining;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneMiningState, DroneSpaceState,
};
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn repeating_drone_mining_extracts_ore_and_stops_on_depletion_or_full_hold() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_092_001;
    let drone_id = 100_092_010;
    let asteroid_id = 70_092_001;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.station_id = None;
            c.structure_id = None;
            c.solar_system_id = sys_id;
            c.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
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
        snap.asteroids.insert(
            asteroid_id,
            AsteroidRecord {
                asteroid_id,
                solar_system_id: sys_id,
                belt_id: 40_009_079,
                type_id: 1230, // Veldspar
                quantity: 100,
                initial_quantity: 100,
                position: (2_000.0, 0.0, 0.0),
                radius: 250.0,
            },
        );
    });

    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_asteroid(
            crate::system_actor::AsteroidState {
                asteroid_id,
                belt_id: 40_009_079,
                type_id: 1230,
                initial_units: 100,
                remaining_units: 100,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: asteroid_id,
                type_id: 1230,
                position: DVec3::new(2_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO);
    spawn_ball(&ctx, sys_id, drone_id, 32787, DVec3::new(1_800.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 32787,
                group_id: 101,
                owner_char_id: char_id,
                original_owner_char_id: char_id,
                parent_ship_id: ship_id,
                solar_system_id: sys_id,
                target_id: Some(asteroid_id),
                activity_state: 1,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 1_500.0,
                optimal_range: 5_000.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
        rt.active_drone_miners.insert(
            drone_id,
            DroneMiningState {
                drone_id,
                asteroid_id,
                ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 60,
                duration_secs: 60,
                next_cycle_unix: 1_000,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
    });

    // Cycle 1: extracts 60 units (40 remaining) and reschedules next cycle
    step_repeating_drone_mining(&ctx, 1_000).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drone_miners[&drone_id].next_cycle_unix, 1_060);
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.asteroids[&asteroid_id].quantity), 40);

    // Cycle 2: depletes remaining 40 units and stops miner
    step_repeating_drone_mining(&ctx, 1_060).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drone_miners.contains_key(&drone_id));
        assert_eq!(rt.active_drones_in_space[&drone_id].activity_state, 0);
    });
}

#[tokio::test]
async fn step_repeating_drone_mining_handles_out_of_range_and_full_cargo() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 900_310_001;
    let ast_id = 900_310_002;
    let far_drone_id = 900_310_003;
    let full_drone_id = 900_310_004;

    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, loc, flag, qty) in [
            (ship_id, 587u32, sys_id as i64, InventoryFlag::None, 1),
            (far_drone_id, 32787u32, sys_id as i64, InventoryFlag::None, 1),
            (full_drone_id, 32787u32, sys_id as i64, InventoryFlag::None, 1),
            (900_310_005i64, 1230u32, ship_id, InventoryFlag::Cargo, 50_000),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: loc,
                    flag,
                    quantity: qty,
                    singleton: qty == 1,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
        }
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
        }
    });

    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_asteroid(
            crate::system_actor::AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_009_079,
                type_id: 1230,
                initial_units: 200,
                remaining_units: 200,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: 1230,
                position: DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;
    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO);
    spawn_ball(&ctx, sys_id, far_drone_id, 32787, DVec3::new(500_000.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, full_drone_id, 32787, DVec3::new(1_000.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        for (did, yield_u) in [(far_drone_id, 50u32), (full_drone_id, 10_000u32)] {
            rt.active_drones_in_space.insert(
                did,
                DroneSpaceState {
                    ball_id: did,
                    drone_item_id: did,
                    type_id: 32787,
                    group_id: 101,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: Some(ast_id),
                    activity_state: 1,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry: false,
                    bandwidth_used: 5.0,
                    max_velocity: 1_500.0,
                    optimal_range: 5_000.0,
                    damage_vector: DamageVector::default(),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 1.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
            rt.active_drone_miners.insert(
                did,
                DroneMiningState {
                    drone_id: did,
                    asteroid_id: ast_id,
                    ship_id,
                    owner_char_id: char_id,
                    solar_system_id: sys_id,
                    yield_units: yield_u as u64,
                    duration_secs: 60,
                    next_cycle_unix: 1_000,
                    waste_prob: 0.0,
                    ore_type_id: 1230,
                    destination_flag: InventoryFlag::Cargo,
                },
            );
        }
    });

    super::step_repeating_drone_mining(&ctx, 1_000).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drone_miners.contains_key(&far_drone_id));
        assert!(rt.active_drones_in_space[&far_drone_id].target_id.is_none());
        assert!(!rt.active_drone_miners.contains_key(&full_drone_id));
        assert_eq!(rt.active_drones_in_space[&full_drone_id].activity_state, 0);
    });
}
