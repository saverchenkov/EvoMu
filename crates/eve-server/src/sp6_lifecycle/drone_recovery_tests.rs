//! Unit tests for drone bay return, warp disconnect/abandonment, and drone mining.

use super::{
    abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync,
    disconnect_drones_for_ship_on_warp_sync, handle_cmd_return_bay, step_repeating_drone_mining,
    step_returning_drones,
};
use crate::router::context::{ActiveFighterSquadron, ActiveScannerProbe};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneMiningState, DroneSpaceState,
};
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
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
            Arc::new(eve_db::WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn spawn_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 500.0,
            inertia: 1.5,
            mass: 5_000.0,
            radius: 15.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
}

#[tokio::test]
async fn cmd_return_bay_and_step_returning_drones_cover_scoop_and_return() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_090_001;
    let close_drone = 100_090_010;
    let far_drone = 100_090_011;
    let sentry_drone = 100_090_012;

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
        for did in [close_drone, far_drone, sentry_drone] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 2454,
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
        }
    });

    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO);
    spawn_ball(&ctx, sys_id, close_drone, 2454, DVec3::new(1_000.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, far_drone, 2454, DVec3::new(5_000.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, sentry_drone, 23561, DVec3::new(4_000.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        for (bid, is_sentry) in [(close_drone, false), (far_drone, false), (sentry_drone, true)] {
            rt.active_drones_in_space.insert(
                bid,
                DroneSpaceState {
                    ball_id: bid,
                    drone_item_id: bid,
                    type_id: if is_sentry { 23561 } else { 2454 },
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: None,
                    activity_state: 0,
                    assist_char_id: None,
                    guard_char_id: None,
                    is_abandoned: false,
                    is_sentry,
                    bandwidth_used: 5.0,
                    max_velocity: 1_500.0,
                    optimal_range: 5_000.0,
                    damage_vector: DamageVector::new(10.0, 0.0, 0.0, 0.0),
                    shield_repair_amount: 0.0,
                    armor_repair_amount: 0.0,
                    hull_repair_amount: 0.0,
                    web_speed_factor: 1.0,
                    neut_amount: 0.0,
                    ecm_strength: 0.0,
                    tp_sig_bonus_pct: 0.0,
                },
            );
        }
    });

    let mut session = SessionState::default();
    session.user_id = Some(1);
    session.character_id = Some(char_id);
    session.ship_id = Some(ship_id);
    session.solar_system_id = Some(sys_id);

    // Out-of-range sentry returns explicit SentryDroneOutOfScoopRange error
    let err = handle_cmd_return_bay(
        &ctx,
        &mut session,
        &[PyRep::List(vec![PyRep::Integer(sentry_drone)])],
        &HashMap::new(),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, "SentryDroneOutOfScoopRange");

    // Recall close_drone & far_drone
    handle_cmd_return_bay(
        &ctx,
        &mut session,
        &[PyRep::List(vec![PyRep::Integer(close_drone), PyRep::Integer(far_drone)])],
        &HashMap::new(),
    )
    .await
    .unwrap();

    // close_drone scooped immediately into DroneBay; far_drone marked returning (4)
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&close_drone].flag, InventoryFlag::DroneBay);
        assert_eq!(snap.items[&close_drone].location_id, ship_id);
    });
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&close_drone));
        assert_eq!(rt.active_drones_in_space[&far_drone].activity_state, 4);
    });

    // Move far_drone within 2500m and run step_returning_drones
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.set_ball_position(far_drone, DVec3::new(800.0, 0.0, 0.0)).await;
    step_returning_drones(&ctx).await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&far_drone].flag, InventoryFlag::DroneBay);
    });
}

#[tokio::test]
async fn warp_disconnect_and_abandon_drones_probes_and_fighters() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_091_001;
    let d1 = 100_091_010;
    let d2 = 100_091_011;
    let fighter_id = 100_091_020;

    spawn_ball(&ctx, sys_id, d1, 2454, DVec3::new(500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d2, 2454, DVec3::new(600.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, fighter_id, 23057, DVec3::new(700.0, 0.0, 0.0));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_id,
            InventoryItemRecord {
                item_id: fighter_id,
                type_id: 23057,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 9,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    with_mut_combat_runtime(&ctx, |rt| {
        for bid in [d1, d2] {
            rt.active_drones_in_space.insert(
                bid,
                DroneSpaceState {
                    ball_id: bid,
                    drone_item_id: bid,
                    type_id: 2454,
                    group_id: 100,
                    owner_char_id: char_id,
                    original_owner_char_id: char_id,
                    parent_ship_id: ship_id,
                    solar_system_id: sys_id,
                    target_id: Some(999),
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
        }
    });

    ctx.active_scanner_probes.write().unwrap().insert(
        555_001,
        ActiveScannerProbe {
            probe_id: 555_001,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 555_000,
            solar_system_id: sys_id,
            position: [0.0; 3],
            destination: [0.0; 3],
            range_step: 2,
            scan_range_m: 1_000_000.0,
            base_sensor_strength: 10.0,
            max_flight_time_s: 300.0,
            launched_at_unix: 1_000,
            state: 1,
            warp_end_ft: None,
        },
    );
    ctx.active_fighters.write().unwrap().insert(
        fighter_id,
        ActiveFighterSquadron {
            ball_id: fighter_id,
            item_id: fighter_id,
            type_id: 23057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: Some(999),
            is_abandoned: false,
            mwd_active: true,
        },
    );

    disconnect_drones_for_ship_on_warp_sync(&ctx, char_id, ship_id, sys_id);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&d1].parent_ship_id, 0);
        assert!(!rt.active_drones_in_space[&d1].is_abandoned);
    });

    abandon_drones_on_warp_sync(&ctx, char_id, ship_id, sys_id);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space[&d1].is_abandoned);
        assert_eq!(rt.active_drones_in_space[&d1].owner_char_id, 0);
    });

    abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, sys_id);
    assert_eq!(ctx.active_scanner_probes.read().unwrap()[&555_001].state, 0);
    assert!(ctx.active_fighters.read().unwrap()[&fighter_id].is_abandoned);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&fighter_id].location_id, sys_id as i64);
    });
}

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

