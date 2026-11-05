//! Additional unit and mutation-killing tests for `drone_return`.

#![forbid(unsafe_code)]

use super::{
    abandon_drones_and_fighters_for_ship_sync, abandon_drones_on_warp_sync,
    disconnect_drones_for_ship_on_warp_sync, handle_cmd_return_bay, step_repeating_drone_mining,
    step_returning_drones,
};
use crate::router::context::ActiveFighterSquadron;
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

fn make_drone_state(
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
        target_id: Some(777),
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
async fn cmd_return_bay_error_guards_filter_variants_exact_2500m_and_capacity_check() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 920_000_001;

    // 1. Unauthenticated session rejected
    let mut unauth = SessionState::default();
    assert!(handle_cmd_return_bay(&ctx, &mut unauth, &[], &HashMap::new()).await.is_err());

    // 2. Character not in DB and session.ship_id is None -> NoActiveShip
    let mut no_ship_sess = SessionState {
        user_id: Some(1),
        character_id: Some(999_999_999),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    assert_eq!(
        handle_cmd_return_bay(&ctx, &mut no_ship_sess, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );

    // 3. Docked in station or structure -> ShipNotInSpace
    let mut docked_station = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        station_id: Some(60_003_760),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    assert_eq!(
        handle_cmd_return_bay(&ctx, &mut docked_station, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    let mut docked_struct = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        structure_id: Some(1_000_000_000_001),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    assert_eq!(
        handle_cmd_return_bay(&ctx, &mut docked_struct, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // 4. Missing solar_system_id -> NoSolarSystem
    let mut no_sys = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_id),
        ..Default::default()
    };
    assert_eq!(
        handle_cmd_return_bay(&ctx, &mut no_sys, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // Prepare ship in space and fallback session with ship_id = None (resolved from snap.characters).
    let sentry_2500 = 920_000_010;
    let sentry_item = 920_000_011;
    let out_of_ctrl_drone = 920_000_012;
    let kwarg_drone = 920_000_013;

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
                type_id: 645, // Dominix with large drone bay
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (iid, tid) in [
            (sentry_item, 23561u32),
            (out_of_ctrl_drone, 2454u32),
            (kwarg_drone, 2454u32),
        ] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: tid,
                    owner_id: char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    spawn_ball(&ctx, sys_id, ship_id, 645, DVec3::ZERO);
    // Exact 2_500.0m boundary for sentry drone (must be scooped, NOT error!)
    spawn_ball(&ctx, sys_id, sentry_2500, 23561, DVec3::new(2_500.0, 0.0, 0.0));
    // Beyond max_ctrl_range -> skipped
    spawn_ball(&ctx, sys_id, out_of_ctrl_drone, 2454, DVec3::new(500_000.0, 0.0, 0.0));
    // Disconnected drone (parent_ship_id == 0, owner_char_id == char_id) at 1_000m
    spawn_ball(&ctx, sys_id, kwarg_drone, 2454, DVec3::new(1_000.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            sentry_2500,
            make_drone_state(sentry_2500, sentry_item, 23561, char_id, ship_id, sys_id, true, 1),
        );
        rt.active_drone_miners.insert(
            sentry_2500,
            DroneMiningState {
                drone_id: sentry_2500,
                asteroid_id: 1,
                ship_id,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 10,
                duration_secs: 60,
                next_cycle_unix: 99_999,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
        rt.active_drones_in_space.insert(
            out_of_ctrl_drone,
            make_drone_state(
                out_of_ctrl_drone,
                out_of_ctrl_drone,
                2454,
                char_id,
                ship_id,
                sys_id,
                false,
                1,
            ),
        );
        rt.active_drones_in_space.insert(
            kwarg_drone,
            make_drone_state(kwarg_drone, kwarg_drone, 2454, char_id, 0, sys_id, false, 0),
        );
    });

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: None, // resolved from character active_ship_id
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    // Single PyRep::Integer filter matching drone_item_id (`sentry_item != sentry_2500`) at exact 2500m.
    handle_cmd_return_bay(&ctx, &mut sess, &[PyRep::Integer(sentry_item)], &HashMap::new())
        .await
        .expect("sentry at exact 2500m scooped");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&sentry_item].location_id, ship_id);
        assert_eq!(snap.items[&sentry_item].flag, InventoryFlag::DroneBay);
        assert_eq!(snap.items[&sentry_item].version, 2);
    });
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drones_in_space.contains_key(&sentry_2500));
        assert!(!rt.active_drone_miners.contains_key(&sentry_2500));
    });

    // Recall via kwargs["droneIDs"] with Tuple filter; out_of_ctrl_drone is skipped, kwarg_drone scooped.
    let mut kw = HashMap::new();
    kw.insert(
        "droneIDs".to_string(),
        PyRep::Tuple(vec![
            PyRep::Integer(out_of_ctrl_drone),
            PyRep::Integer(kwarg_drone),
        ]),
    );
    handle_cmd_return_bay(&ctx, &mut sess, &[], &kw)
        .await
        .expect("recall via kwargs Tuple");

    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space.contains_key(&out_of_ctrl_drone));
        assert_eq!(rt.active_drones_in_space[&out_of_ctrl_drone].activity_state, 1);
        assert!(!rt.active_drones_in_space.contains_key(&kwarg_drone));
    });
}

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
