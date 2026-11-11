//! Additional unit and mutation-killing tests for `drone_return`.

#![forbid(unsafe_code)]

use super::handle_cmd_return_bay;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{
    with_combat_runtime, with_mut_combat_runtime, DroneMiningState, DroneSpaceState,
};
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

#[cfg(test)]
#[path = "drone_returning_and_mining_holds_tests.rs"]
mod drone_returning_and_mining_holds_tests;

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn test_ctx() -> ServerContext {
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

pub(super) fn spawn_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3) {
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

pub(super) fn make_drone_state(
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
async fn drone_scoop_bay_full_missing_items_abandon_reconnect_limits_and_mining_holds() {
    use super::{step_repeating_drone_mining, step_returning_drones};
    use crate::sp6_lifecycle::{handle_cmd_abandon_drone, handle_cmd_reconnect_to_drones};

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_993_001i64;
    let ship_full_bay = 943_000_001i64;
    let ship_missing_drone = 943_000_002i64;
    let ghost_ship = 943_000_003i64;
    let d_bay_full = 943_000_010i64;
    let d_item_missing = 943_000_011i64;
    let d_ghost_ship = 943_000_012i64;
    let filler_drones = 943_000_013i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters[&90_000_001].clone();
        c.character_id = char_id;
        c.user_id = char_id as u64;
        c.name = "DroneExtra".into();
        c.solar_system_id = sys_id;
        c.active_ship_id = ship_full_bay;
        c.station_id = None;
        c.structure_id = None;
        snap.characters.insert(char_id, c);
        snap.skills.entry(char_id).or_default().insert(
            3436,
            eve_db::TrainedSkillRecord {
                type_id: 3436,
                level: 2,
                skill_points: 1_415.0,
            },
        );
        // Rifter (587) has small/zero drone capacity or Ibis (606) / Tristan (593) with 10m3 drone bay
        for sid in [ship_full_bay, ship_missing_drone] {
            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 593, // Tristan (40m3 drone bay)
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
        // Fill ship_full_bay drone bay past 40m3 (10x 5m3 drones = 50m3)
        snap.items.insert(
            filler_drones,
            InventoryItemRecord {
                item_id: filler_drones,
                type_id: 2454,
                owner_id: char_id,
                location_id: ship_full_bay,
                flag: InventoryFlag::DroneBay,
                quantity: 10,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            d_bay_full,
            InventoryItemRecord {
                item_id: d_bay_full,
                type_id: 2454,
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

    spawn_ball(&ctx, sys_id, ship_full_bay, 593, DVec3::ZERO);
    spawn_ball(&ctx, sys_id, ship_missing_drone, 593, DVec3::new(100.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, ghost_ship, 593, DVec3::new(200.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_bay_full, 2454, DVec3::new(500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_item_missing, 2454, DVec3::new(550.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_ghost_ship, 2454, DVec3::new(600.0, 0.0, 0.0));

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            d_bay_full,
            make_drone_state(d_bay_full, d_bay_full, 2454, char_id, ship_full_bay, sys_id, false, 4),
        );
        rt.active_drones_in_space.insert(
            d_item_missing,
            make_drone_state(d_item_missing, d_item_missing, 2454, char_id, ship_missing_drone, sys_id, false, 4),
        );
        rt.active_drones_in_space.insert(
            d_ghost_ship,
            make_drone_state(d_ghost_ship, d_ghost_ship, 2454, char_id, ghost_ship, sys_id, false, 4),
        );
    });

    // `step_returning_drones` exercises BayFull (`L179`, `196-200`) and ItemMissing (`L156`, `182`, `202-204`)
    step_returning_drones(&ctx).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(rt.active_drones_in_space.contains_key(&d_bay_full)); // restored on BayFull
        assert!(!rt.active_drones_in_space.contains_key(&d_item_missing)); // purged ItemMissing
        assert!(!rt.active_drones_in_space.contains_key(&d_ghost_ship)); // purged missing ship
    });

    // `handle_cmd_abandon_drone` with PyRep::Integer single ID & `handle_cmd_reconnect_to_drones`
    // error branches (NoActiveShip, NoSolarSystem, empty reconnected -> finish_reconnect_drones(&[]),
    // OutOfRange > 250km, MaxDronesExceeded, InsufficientBandwidth)
    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_full_bay),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let _ = handle_cmd_abandon_drone(&ctx, &mut sess, &[PyRep::Integer(d_bay_full)], &HashMap::new())
        .await
        .unwrap();
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&d_bay_full].is_abandoned));

    // Reconnect error guards
    let mut sess_no_ship = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    assert_eq!(
        handle_cmd_reconnect_to_drones(&ctx, &mut sess_no_ship, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NoActiveShip"
    );
    let mut sess_no_sys = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(ship_full_bay),
        ..Default::default()
    };
    assert_eq!(
        handle_cmd_reconnect_to_drones(&ctx, &mut sess_no_sys, &[], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // Empty candidate reconnect (`drone_reconnect_finish.rs:23`)
    let empty_rep = handle_cmd_reconnect_to_drones(
        &ctx,
        &mut sess,
        &[PyRep::Integer(999_888_777)],
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(matches!(empty_rep, PyRep::Dict(_)));

    // Seed far abandoned drone (>250km) and active drones up to limit -> OutOfRange & MaxDronesExceeded / InsufficientBandwidth
    let d_far_aban = 943_000_020i64;
    let d_heavy_aban = 943_000_021i64;
    let d_extra_aban = 943_000_022i64;
    spawn_ball(&ctx, sys_id, d_far_aban, 2454, DVec3::new(300_000.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_heavy_aban, 2454, DVec3::new(500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_extra_aban, 2454, DVec3::new(600.0, 0.0, 0.0));
    with_mut_combat_runtime(&ctx, |rt| {
        let mut st_far = make_drone_state(d_far_aban, d_far_aban, 2454, 0, 0, sys_id, false, 0);
        st_far.is_abandoned = true;
        st_far.original_owner_char_id = char_id;
        rt.active_drones_in_space.insert(d_far_aban, st_far);

        let mut st_heavy = make_drone_state(d_heavy_aban, d_heavy_aban, 2454, 0, 0, sys_id, false, 0);
        st_heavy.is_abandoned = true;
        st_heavy.original_owner_char_id = char_id;
        st_heavy.bandwidth_used = 500.0; // > Tristan ship_bw
        rt.active_drones_in_space.insert(d_heavy_aban, st_heavy);

        let mut st_ok = make_drone_state(d_extra_aban, d_extra_aban, 2454, 0, 0, sys_id, false, 0);
        st_ok.is_abandoned = true;
        st_ok.original_owner_char_id = char_id;
        rt.active_drones_in_space.insert(d_extra_aban, st_ok);
    });
    let rec_errs = handle_cmd_reconnect_to_drones(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![
            PyRep::Integer(d_far_aban),
            PyRep::Integer(d_heavy_aban),
        ])],
        &HashMap::new(),
    )
    .await
    .unwrap();
    let rec_str = format!("{rec_errs:?}");
    assert!(
        rec_str.contains("OutOfRange") && rec_str.contains("InsufficientBandwidth"),
        "unexpected reconnect errs: {rec_str}"
    );

    // Saturate max_drones_allowed (2) then reconnect d_extra_aban -> MaxDronesExceeded
    with_mut_combat_runtime(&ctx, |rt| {
        for active_id in [943_000_023i64, 943_000_024i64] {
            let mut st = make_drone_state(active_id, active_id, 2454, char_id, ship_full_bay, sys_id, false, 1);
            st.bandwidth_used = 1.0;
            rt.active_drones_in_space.insert(active_id, st);
        }
    });
    let max_err = handle_cmd_reconnect_to_drones(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(d_extra_aban)])],
        &HashMap::new(),
    )
    .await
    .unwrap();
    assert!(format!("{max_err:?}").contains("MaxDronesExceeded"));

    // Repeating drone mining missing asteroid error branch + specialized hold flags (134, 135, 181) (`drone_return.rs:506-549`)
    let d_mine_err = 943_000_030i64;
    let d_mine_hold = 943_000_031i64;
    spawn_ball(&ctx, sys_id, d_mine_err, 32787, DVec3::new(500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_mine_hold, 32787, DVec3::new(600.0, 0.0, 0.0));
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(
            d_mine_err,
            make_drone_state(d_mine_err, d_mine_err, 32787, char_id, ship_full_bay, sys_id, false, 1),
        );
        rt.active_drone_miners.insert(
            d_mine_err,
            DroneMiningState {
                drone_id: d_mine_err,
                asteroid_id: 999_777_666, // missing asteroid triggers mine_asteroid Err (`L536-549`)
                ship_id: ship_full_bay,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 1,
                duration_secs: 60,
                next_cycle_unix: 1,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::Cargo,
            },
        );
        rt.active_drones_in_space.insert(
            d_mine_hold,
            make_drone_state(d_mine_hold, d_mine_hold, 32787, char_id, ship_full_bay, sys_id, false, 1),
        );
        rt.active_drone_miners.insert(
            d_mine_hold,
            DroneMiningState {
                drone_id: d_mine_hold,
                asteroid_id: 999_777_666,
                ship_id: ship_full_bay,
                owner_char_id: char_id,
                solar_system_id: sys_id,
                yield_units: 1,
                duration_secs: 60,
                next_cycle_unix: 1,
                waste_prob: 0.0,
                ore_type_id: 1230,
                destination_flag: InventoryFlag::from_u16(181),
            },
        );
    });
    step_repeating_drone_mining(&ctx, 5_000).await;
    with_combat_runtime(&ctx, |rt| {
        assert!(!rt.active_drone_miners.contains_key(&d_mine_err));
        assert_eq!(rt.active_drones_in_space[&d_mine_err].activity_state, 0);
        assert!(!rt.active_drone_miners.contains_key(&d_mine_hold));
    });
}

