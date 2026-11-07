//! Semantic unit tests split from `drone_return_extra_tests`.

#![forbid(unsafe_code)]

use super::*;

use super::super::{
    abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync,
    disconnect_drones_for_ship_on_warp_sync, step_repeating_drone_mining,
    step_returning_drones,
};
use crate::router::context::ActiveFighterSquadron;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneMiningState,
};
use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use glam::DVec3;

#[tokio::test]
async fn step_returning_drones_and_abandon_fighters_cover_capacity_and_tube_cleanup() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let other_sys = 30_000_144;
    let char_id = 90_000_001;
    let ship_id = 920_100_001;
    let ret_far = 920_100_010;
    let ret_missing_ship = 920_100_011;
    let other_sys_drone = 920_100_012;
    let fighter_in_ship = 920_100_020;
    let fighter_other_sys = 920_100_021;

    spawn_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO);
    // Exact 2501m (> 2500m) stays returning
    spawn_ball(&ctx, sys_id, ret_far, 2454, DVec3::new(2_501.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, ret_missing_ship, 2454, DVec3::new(500.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            ret_far,
            make_drone_state(ret_far, ret_far, 2454, char_id, ship_id, sys_id, false, 4),
        );
        // parent_ship_id 999_999_888 has no ball/item -> skipped cleanly
        rt.active_drones_in_space.insert(
            ret_missing_ship,
            make_drone_state(
                ret_missing_ship,
                ret_missing_ship,
                2454,
                char_id,
                999_999_888,
                sys_id,
                false,
                4,
            ),
        );
        // Drone in other_sys must not be abandoned when warping in sys_id
        rt.active_drones_in_space.insert(
            other_sys_drone,
            make_drone_state(
                other_sys_drone,
                other_sys_drone,
                2454,
                0, // owner_char_id == 0, matched via parent_ship_id == ship_id
                ship_id,
                other_sys,
                false,
                1,
            ),
        );
    });

    step_returning_drones(&ctx).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space.contains_key(&ret_far));
        assert!(rt.active_drones_in_space.contains_key(&ret_missing_ship));
    });

    // abandon_drones_on_warp_sync in sys_id does NOT touch other_sys_drone.
    abandon_drones_on_warp_sync(&ctx, char_id, ship_id, sys_id);
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space[&ret_far].is_abandoned);
        assert!(!rt.active_drones_in_space[&other_sys_drone].is_abandoned);
    });

    // disconnect_drones_for_ship_on_warp_sync matches via parent_ship_id == ship_id even when owner_char_id == 0.
    disconnect_drones_for_ship_on_warp_sync(&ctx, char_id, ship_id, other_sys);
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&other_sys_drone].parent_ship_id, 0);
    });

    // Fighter tube loads and system filtering in abandon_drones_and_fighters_for_ship_sync.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            fighter_in_ship,
            InventoryItemRecord {
                item_id: fighter_in_ship,
                type_id: 23057,
                owner_id: char_id,
                location_id: ship_id, // no ball in space, matched via location_id == ship_id
                flag: InventoryFlag::FighterBay,
                quantity: 9,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            fighter_other_sys,
            InventoryItemRecord {
                item_id: fighter_other_sys,
                type_id: 23057,
                owner_id: char_id,
                location_id: other_sys as i64,
                flag: InventoryFlag::None,
                quantity: 9,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    ctx.fighter_tube_loads
        .write()
        .unwrap()
        .insert((ship_id, 2), fighter_in_ship);
    ctx.active_fighters.write().unwrap().insert(
        fighter_in_ship,
        ActiveFighterSquadron {
            ball_id: fighter_in_ship,
            item_id: fighter_in_ship,
            type_id: 23057,
            owner_char_id: 0, // matched via parent_ship_id == ship_id
            parent_ship_id: ship_id,
            tube_index: 2,
            squadron_size: 9,
            target_id: Some(123),
            is_abandoned: false,
            mwd_active: false,
        },
    );
    ctx.active_fighters.write().unwrap().insert(
        fighter_other_sys,
        ActiveFighterSquadron {
            ball_id: fighter_other_sys,
            item_id: fighter_other_sys,
            type_id: 23057,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 3,
            squadron_size: 9,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    abandon_drones_and_fighters_for_ship_sync(&ctx, char_id, ship_id, sys_id);
    assert!(ctx.active_fighters.read().unwrap()[&fighter_in_ship].is_abandoned);
    assert_eq!(
        ctx.active_fighters.read().unwrap()[&fighter_in_ship].parent_ship_id,
        0
    );
    assert!(!ctx.active_fighters.read().unwrap()[&fighter_other_sys].is_abandoned);
    assert!(!ctx
        .fighter_tube_loads
        .read()
        .unwrap()
        .contains_key(&(ship_id, 2)));
}

#[tokio::test]
async fn step_repeating_drone_mining_covers_specialized_holds_waste_pct_and_invalid_states() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let venture_ship_id = 920_200_001;
    let orphan_miner_drone = 920_200_010;
    let missing_ast_drone = 920_200_011;
    let ore_hold_drone = 920_200_012;
    let future_drone = 920_200_013;
    let ast_id = 920_200_050;

    // Find a ship type with attribute 1556 (generalMiningHoldCapacity / SpecializedOreHold), e.g. Venture (32880).
    let venture_tid = if ctx
        .sde
        .types
        .get(&32880)
        .is_some_and(|t| t.default_attributes.contains_key(&1556) || t.default_attributes.contains_key(&3227))
    {
        32880
    } else {
        587
    };

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char_id) {
            c.active_ship_id = venture_ship_id;
            c.solar_system_id = sys_id;
            c.station_id = None;
            c.structure_id = None;
        }
        snap.items.insert(
            venture_ship_id,
            InventoryItemRecord {
                item_id: venture_ship_id,
                type_id: venture_tid,
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
                quantity: 500,
                initial_quantity: 500,
                position: (1_000.0, 0.0, 0.0),
                radius: 200.0,
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
                initial_units: 500,
                remaining_units: 500,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: 1230,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await;

    spawn_ball(&ctx, sys_id, venture_ship_id, venture_tid, DVec3::ZERO);
    spawn_ball(&ctx, sys_id, missing_ast_drone, 32787, DVec3::new(500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, ore_hold_drone, 32787, DVec3::new(600.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, future_drone, 32787, DVec3::new(700.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        // 1) orphan_miner_drone has no entry in active_drones_in_space -> removed
        rt.active_drone_miners.insert(
            orphan_miner_drone,
            DroneMiningState {
                drone_id: orphan_miner_drone,
                asteroid_id: ast_id,
                ship_id: venture_ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 20,
                duration_secs: 60,
                next_cycle_unix: 1_000,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );

        // 2) missing_ast_drone targets non-existent asteroid 999_999_999 -> mine_asteroid Err branch
        rt.active_drones_in_space.insert(
            missing_ast_drone,
            make_drone_state(
                missing_ast_drone,
                missing_ast_drone,
                32787,
                char_id,
                venture_ship_id,
                sys_id,
                false,
                1,
            ),
        );
        rt.active_drone_miners.insert(
            missing_ast_drone,
            DroneMiningState {
                drone_id: missing_ast_drone,
                asteroid_id: 999_999_999,
                ship_id: venture_ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 20,
                duration_secs: 60,
                next_cycle_unix: 1_000,
                waste_prob: 50.0, // > 1.0 percentage branch (50.0 / 100.0)
                ore_type_id: 1230,
                destination_flag: InventoryFlag::SpecializedGasHold, // flag 135 branch
            },
        );

        // 3) ore_hold_drone mines into SpecializedOreHold (flag 134) with waste_prob = 100.0 (100% waste)
        rt.active_drones_in_space.insert(
            ore_hold_drone,
            make_drone_state(
                ore_hold_drone,
                ore_hold_drone,
                32787,
                char_id,
                venture_ship_id,
                sys_id,
                false,
                1,
            ),
        );
        rt.active_drone_miners.insert(
            ore_hold_drone,
            DroneMiningState {
                drone_id: ore_hold_drone,
                asteroid_id: ast_id,
                ship_id: venture_ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 25,
                duration_secs: 45,
                next_cycle_unix: 1_000,
                waste_prob: 100.0, // > 1.0 -> eff_waste = 1.0
                ore_type_id: 1230,
                destination_flag: InventoryFlag::SpecializedOreHold, // flag 134 branch
            },
        );

        // 4) future_drone has next_cycle_unix > now_unix -> untouched
        rt.active_drones_in_space.insert(
            future_drone,
            make_drone_state(
                future_drone,
                future_drone,
                32787,
                char_id,
                venture_ship_id,
                sys_id,
                false,
                1,
            ),
        );
        rt.active_drone_miners.insert(
            future_drone,
            DroneMiningState {
                drone_id: future_drone,
                asteroid_id: ast_id,
                ship_id: venture_ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 10,
                duration_secs: 60,
                next_cycle_unix: 2_000,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::SpecializedIceHold, // flag 181
            },
        );
    });

    step_repeating_drone_mining(&ctx, 1_000).await;

    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drone_miners.contains_key(&orphan_miner_drone));
        assert!(!rt.active_drone_miners.contains_key(&missing_ast_drone));
        assert_eq!(rt.active_drones_in_space[&missing_ast_drone].activity_state, 0);
        assert!(rt.active_drones_in_space[&missing_ast_drone].target_id.is_none());

        assert_eq!(rt.active_drone_miners[&ore_hold_drone].next_cycle_unix, 1_045);
        assert_eq!(rt.active_drone_miners[&future_drone].next_cycle_unix, 2_000);
    });
    // 25 mined + 25 wasted (from eff_waste = 1.0) -> 450 remaining
    assert_eq!(ctx.db.with_snapshot(|s| s.asteroids[&ast_id].quantity), 450);
}
