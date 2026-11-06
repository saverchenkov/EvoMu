//! Deep branch-coverage and mutation-killing unit tests for `sp6_lifecycle::drone_return`.

#![forbid(unsafe_code)]

use crate::router::context::{ActiveFighterSquadron, ActiveScannerProbe};
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneMiningState, DroneSpaceState,
};
use crate::sp6_lifecycle::drone_return::{
    abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync,
    disconnect_drones_for_ship_on_warp_sync, handle_cmd_return_bay, step_repeating_drone_mining,
    step_returning_drones,
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
    ctx.systems
        .get_or_spawn(sys_id)
        .try_spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 600.0,
                inertia: 1.2,
                mass: 4_000.0,
                radius: 12.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        );
}

fn make_drone(
    ball_id: i64,
    drone_item_id: i64,
    type_id: u32,
    owner_char_id: i64,
    parent_ship_id: i64,
    solar_system_id: u32,
    is_sentry: bool,
    activity_state: i64,
) -> DroneSpaceState {
    DroneSpaceState {
        ball_id,
        drone_item_id,
        type_id,
        group_id: 100,
        owner_char_id,
        original_owner_char_id: owner_char_id,
        parent_ship_id,
        solar_system_id,
        target_id: Some(888),
        activity_state,
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry,
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
    }
}

#[tokio::test]
async fn cmd_return_bay_sentry_out_of_range_prevalidation_and_mobile_follow_transition() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 940_000_001;
    let mobile_far = 940_000_010;
    let sentry_far = 940_000_011;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 645,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    spawn_ball(&ctx, sys_id, ship_id, 645, DVec3::ZERO);
    // Mobile drone at 3_000m (> 2_500m, <= max_ctrl_range)
    spawn_ball(&ctx, sys_id, mobile_far, 2454, DVec3::new(3_000.0, 0.0, 0.0));
    // Sentry drone at 2_500.1m (> 2_500m -> SentryDroneOutOfScoopRange)
    spawn_ball(&ctx, sys_id, sentry_far, 23561, DVec3::new(2_500.1, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            mobile_far,
            make_drone(mobile_far, mobile_far, 2454, char_id, 0, sys_id, false, 1),
        );
        rt.active_drones_in_space.insert(
            sentry_far,
            make_drone(sentry_far, sentry_far, 23561, char_id, ship_id, sys_id, true, 1),
        );
    });

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    // Calling without filter hits SentryDroneOutOfScoopRange during pre-validation before mutating mobile_far!
    let err = handle_cmd_return_bay(&ctx, &mut sess, &[], &HashMap::new())
        .await
        .unwrap_err();
    assert_eq!(err.code, "SentryDroneOutOfScoopRange");
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&mobile_far].activity_state, 1);
    });

    // Now recall only mobile_far via List filter -> transitions to returning (activity_state = 4, target = ship_id).
    handle_cmd_return_bay(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(mobile_far)])],
        &HashMap::new(),
    )
    .await
    .expect("mobile drone at 3000m begins returning");

    with_combat_runtime(&ctx, |rt| {
        let st = &rt.active_drones_in_space[&mobile_far];
        assert_eq!(st.activity_state, 4);
        assert_eq!(st.parent_ship_id, ship_id);
        assert_eq!(st.target_id, Some(ship_id));
    });

    // Move mobile_far to exact 2_500.0m boundary and run step_returning_drones -> scooped into DroneBay!
    spawn_ball(&ctx, sys_id, mobile_far, 2454, DVec3::new(2_500.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mobile_far,
            InventoryItemRecord {
                item_id: mobile_far,
                type_id: 2454,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 5,
                ..Default::default()
            },
        );
    });

    step_returning_drones(&ctx).await;

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&mobile_far));
    });
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&mobile_far];
        assert_eq!(it.location_id, ship_id);
        assert_eq!(it.flag, InventoryFlag::DroneBay);
        assert_eq!(it.version, 6);
    });
}

#[tokio::test]
async fn abandon_probes_fighters_and_repeating_drone_miner_depletion_and_full_hold() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 940_100_001;
    let probe_in_sys = 940_100_010;
    let probe_other_sys = 940_100_011;
    let fighter_ball_id = 940_100_020;
    let fighter_item_id = 940_100_021;

    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO);
    spawn_ball(
        &ctx,
        sys_id,
        fighter_ball_id,
        23057,
        DVec3::new(1_200.0, 3_400.0, 5_600.0),
    );

    ctx.active_scanner_probes.write().unwrap().insert(
        probe_in_sys,
        ActiveScannerProbe {
            probe_id: probe_in_sys,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 1,
            solar_system_id: sys_id,
            position: [0.0, 0.0, 0.0],
            destination: [0.0, 0.0, 0.0],
            range_step: 1,
            scan_range_m: 149_597_870_700.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 1_000,
            state: 2,
            warp_end_ft: None,
        },
    );
    ctx.active_scanner_probes.write().unwrap().insert(
        probe_other_sys,
        ActiveScannerProbe {
            probe_id: probe_other_sys,
            probe_type_id: 30013,
            owner_char_id: char_id,
            ship_id,
            launcher_module_id: 1,
            solar_system_id: 30_000_144,
            position: [0.0, 0.0, 0.0],
            destination: [0.0, 0.0, 0.0],
            range_step: 1,
            scan_range_m: 149_597_870_700.0,
            base_sensor_strength: 40.0,
            max_flight_time_s: 3600.0,
            launched_at_unix: 1_000,
            state: 2,
            warp_end_ft: None,
        },
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_item_id,
            InventoryItemRecord {
                item_id: fighter_item_id,
                type_id: 23057,
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                version: 3,
                ..Default::default()
            },
        );
    });
    ctx.active_fighters.write().unwrap().insert(
        fighter_ball_id,
        ActiveFighterSquadron {
            ball_id: fighter_ball_id,
            item_id: fighter_item_id,
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
    abandon_drones_on_warp_sync(&ctx, char_id, ship_id, sys_id);
    abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, sys_id);

    assert_eq!(
        ctx.active_scanner_probes.read().unwrap()[&probe_in_sys].state,
        0
    );
    assert_eq!(
        ctx.active_scanner_probes.read().unwrap()[&probe_other_sys].state,
        2
    );
    ctx.db.with_snapshot(|snap| {
        let fit_item = &snap.items[&fighter_item_id];
        assert_eq!(fit_item.location_id, sys_id as i64);
        assert_eq!(fit_item.flag, InventoryFlag::None);
        assert_eq!(fit_item.version, 4);
        assert_eq!(
            snap.item_positions.get(&fighter_item_id),
            Some(&[1_200.0, 3_400.0, 5_600.0])
        );
        assert_eq!(
            snap.item_positions.get(&fighter_ball_id),
            Some(&[1_200.0, 3_400.0, 5_600.0])
        );
        assert_eq!(
            snap.sp6_lifecycle.space_object_meta[&fighter_item_id].drone_ball_id,
            Some(fighter_ball_id)
        );
    });

    // Test repeating drone mining: asteroid depletion & full hold stop
    let depleting_drone = 940_100_030;
    let full_hold_drone = 940_100_031;
    let ast_id = 940_100_040;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = ship_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587, // Rifter
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.asteroids.insert(
            ast_id,
            AsteroidRecord {
                asteroid_id: ast_id,
                solar_system_id: sys_id,
                belt_id: 40_009_079,
                type_id: 1230,
                quantity: 10,
                initial_quantity: 10,
                position: (500.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });

    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .spawn_asteroid(
            crate::system_actor::AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_009_079,
                type_id: 1230,
                initial_units: 10,
                remaining_units: 10,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: 1230,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 100_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    spawn_ball(&ctx, sys_id, depleting_drone, 32787, DVec3::new(400.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, full_hold_drone, 32787, DVec3::new(450.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            depleting_drone,
            make_drone(depleting_drone, depleting_drone, 32787, char_id, ship_id, sys_id, false, 1),
        );
        rt.active_drone_miners.insert(
            depleting_drone,
            DroneMiningState {
                drone_id: depleting_drone,
                asteroid_id: ast_id,
                ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 10, // depletes all 10 units
                duration_secs: 60,
                next_cycle_unix: 500,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
        rt.active_drones_in_space.insert(
            full_hold_drone,
            make_drone(full_hold_drone, full_hold_drone, 32787, char_id, ship_id, sys_id, false, 1),
        );
        rt.active_drone_miners.insert(
            full_hold_drone,
            DroneMiningState {
                drone_id: full_hold_drone,
                asteroid_id: ast_id,
                ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 99_999_999, // exceeds ship cargo capacity -> stopped before mining
                duration_secs: 60,
                next_cycle_unix: 500,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
    });

    step_repeating_drone_mining(&ctx, 500).await;

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drone_miners.contains_key(&depleting_drone));
        assert_eq!(rt.active_drones_in_space[&depleting_drone].activity_state, 0);
        assert!(!rt.active_drone_miners.contains_key(&full_hold_drone));
        assert_eq!(rt.active_drones_in_space[&full_hold_drone].activity_state, 0);
    });
    assert_eq!(ctx.db.with_snapshot(|s| s.asteroids[&ast_id].quantity), 0);
}
