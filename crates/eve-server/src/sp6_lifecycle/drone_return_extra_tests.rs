//! Additional unit and mutation-killing tests for `drone_return`.

#![forbid(unsafe_code)]

use super::handle_cmd_return_bay;
use crate::router::ServerContext;
use crate::services::combat_crimewatch::{DroneMiningState, DroneSpaceState};
use crate::services::drone_test_helpers;
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

    drone_test_helpers::seed_active_drones(
        &ctx,
        [
            make_drone_state(sentry_2500, sentry_item, 23561, char_id, ship_id, sys_id, true, 1),
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
            make_drone_state(kwarg_drone, kwarg_drone, 2454, char_id, 0, sys_id, false, 0),
        ],
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .upsert_drone_miner(DroneMiningState {
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
        })
        .await
        .unwrap();

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
    assert!(!drone_test_helpers::has_active_drone(&ctx, sentry_2500));
    assert_eq!(sys_handle.active_drone_miner(sentry_2500), None);

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

    let out_state = drone_test_helpers::active_drone(&ctx, out_of_ctrl_drone).unwrap();
    assert_eq!(out_state.activity_state, 1);
    assert!(!drone_test_helpers::has_active_drone(&ctx, kwarg_drone));
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

    drone_test_helpers::seed_active_drones(
        &ctx,
        [
            make_drone_state(d_bay_full, d_bay_full, 2454, char_id, ship_full_bay, sys_id, false, 4),
            make_drone_state(d_item_missing, d_item_missing, 2454, char_id, ship_missing_drone, sys_id, false, 4),
            make_drone_state(d_ghost_ship, d_ghost_ship, 2454, char_id, ghost_ship, sys_id, false, 4),
        ],
    );

    // `step_returning_drones` exercises BayFull (`L179`, `196-200`) and ItemMissing (`L156`, `182`, `202-204`)
    step_returning_drones(&ctx).await;
    assert!(drone_test_helpers::has_active_drone(&ctx, d_bay_full)); // restored on BayFull
    assert!(!drone_test_helpers::has_active_drone(&ctx, d_item_missing)); // purged ItemMissing
    let abandoned_ghost = drone_test_helpers::active_drone(&ctx, d_ghost_ship)
        .expect("ghost ship drone abandoned in space rather than deleted");
    assert!(abandoned_ghost.is_abandoned);
    assert_eq!(abandoned_ghost.owner_char_id, 0);
    assert_eq!(abandoned_ghost.original_owner_char_id, char_id);
    assert_eq!(abandoned_ghost.parent_ship_id, 0);
    assert_eq!(abandoned_ghost.activity_state, 0);
    assert!(
        ctx.systems
            .get_or_spawn(sys_id)
            .get_ball_snapshot(d_ghost_ship)
            .await
            .unwrap()
            .is_some(),
        "abandoned drone ball must stay in space when parent ship disappears"
    );

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
    assert!(
        drone_test_helpers::active_drone(&ctx, d_bay_full)
            .unwrap()
            .is_abandoned
    );

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
    let mut st_far = make_drone_state(d_far_aban, d_far_aban, 2454, 0, 0, sys_id, false, 0);
    st_far.is_abandoned = true;
    st_far.original_owner_char_id = char_id;

    let mut st_heavy = make_drone_state(d_heavy_aban, d_heavy_aban, 2454, 0, 0, sys_id, false, 0);
    st_heavy.is_abandoned = true;
    st_heavy.original_owner_char_id = char_id;
    st_heavy.bandwidth_used = 500.0; // > Tristan ship_bw

    let mut st_ok = make_drone_state(d_extra_aban, d_extra_aban, 2454, 0, 0, sys_id, false, 0);
    st_ok.is_abandoned = true;
    st_ok.original_owner_char_id = char_id;
    drone_test_helpers::seed_active_drones(&ctx, [st_far, st_heavy, st_ok]);
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
    drone_test_helpers::seed_active_drones(
        &ctx,
        [943_000_023i64, 943_000_024i64].into_iter().map(|active_id| {
            let mut st = make_drone_state(active_id, active_id, 2454, char_id, ship_full_bay, sys_id, false, 1);
            st.bandwidth_used = 1.0;
            st
        }),
    );
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
    drone_test_helpers::seed_active_drones(
        &ctx,
        [
            make_drone_state(d_mine_err, d_mine_err, 32787, char_id, ship_full_bay, sys_id, false, 1),
            make_drone_state(d_mine_hold, d_mine_hold, 32787, char_id, ship_full_bay, sys_id, false, 1),
        ],
    );
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .upsert_drone_miner(DroneMiningState {
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
        })
        .await
        .unwrap();
    sys_handle
        .upsert_drone_miner(DroneMiningState {
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
        })
        .await
        .unwrap();
    step_repeating_drone_mining(&ctx, 5_000).await;
    assert_eq!(sys_handle.active_drone_miner(d_mine_err), None);
    assert_eq!(sys_handle.active_drone_miner(d_mine_hold), None);
    assert_eq!(
        drone_test_helpers::active_drone(&ctx, d_mine_err)
            .unwrap()
            .activity_state,
        0
    );
}

#[tokio::test]
async fn returning_drones_abandon_on_ship_death_dock_cross_system_and_scoop_rejects_invalid_targets() {
    use super::{handle_cmd_return_bay, step_returning_drones};

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let other_sys_id = 30_000_144u32;
    let station_id = 60_003_760i64;
    let char1 = 90_994_001i64;
    let char2 = 90_994_002i64;
    let char3 = 90_994_003i64;
    let char4 = 90_994_004i64;
    let ship1 = 944_000_001i64;
    let ship2 = 944_000_002i64;
    let ship3 = 944_000_003i64;
    let ship4 = 944_000_004i64;
    let d1 = 944_000_011i64;
    let d2 = 944_000_012i64;
    let d3 = 944_000_013i64;
    let d4 = 944_000_014i64;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid, st_opt, cur_sys) in [
            (char1, ship1, None, sys_id),
            (char2, ship2, Some(station_id), sys_id),
            (char3, ship3, None, other_sys_id),
            (char4, ship4, None, sys_id),
        ] {
            let mut c = snap.characters[&90_000_001].clone();
            c.character_id = cid;
            c.user_id = cid as u64;
            c.name = format!("DroneRetPilot-{cid}");
            c.solar_system_id = cur_sys;
            c.active_ship_id = sid;
            c.station_id = st_opt;
            c.structure_id = None;
            snap.characters.insert(cid, c);
        }
        // ship1 intentionally removed/absent from DB to simulate parent ship destruction mid-return
        snap.items.insert(
            ship2,
            InventoryItemRecord {
                item_id: ship2,
                type_id: 593,
                owner_id: char2,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship3,
            InventoryItemRecord {
                item_id: ship3,
                type_id: 593,
                owner_id: char3,
                location_id: other_sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            ship4,
            InventoryItemRecord {
                item_id: ship4,
                type_id: 593,
                owner_id: char4,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for (did, oid) in [(d1, char1), (d2, char2), (d3, char3), (d4, char4)] {
            snap.items.insert(
                did,
                InventoryItemRecord {
                    item_id: did,
                    type_id: 2454,
                    owner_id: oid,
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

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Spawn ship4 (only in-space valid ship) + all 4 returning drone balls
    spawn_ball(&ctx, sys_id, ship4, 593, DVec3::ZERO);
    spawn_ball(&ctx, sys_id, d1, 2454, DVec3::new(100.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d2, 2454, DVec3::new(200.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d3, 2454, DVec3::new(300.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d4, 2454, DVec3::new(10_000.0, 0.0, 0.0)); // > 2500m from ship4

    // CmdReturnBay when ship ball is absent rejects ShipNotInSpace without deleting d1
    let mut sess_destroyed = SessionState {
        user_id: Some(char1 as u64),
        character_id: Some(char1),
        ship_id: Some(ship1),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    drone_test_helpers::seed_active_drones(
        &ctx,
        [
            make_drone_state(d1, d1, 2454, char1, ship1, sys_id, false, 4),
            make_drone_state(d2, d2, 2454, char2, ship2, sys_id, false, 4),
            make_drone_state(d3, d3, 2454, char3, ship3, sys_id, false, 4),
            make_drone_state(d4, d4, 2454, char4, ship4, sys_id, false, 4),
        ],
    );
    assert_eq!(
        handle_cmd_return_bay(&ctx, &mut sess_destroyed, &[PyRep::Integer(d1)], &HashMap::new())
            .await
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );

    // Try atomic scoop on out-of-range returning drone d4 -> keeps d4 controlled & returning
    let d4_state = drone_test_helpers::active_drone(&ctx, d4).unwrap();
    assert!(!super::try_atomic_scoop_drone(&ctx, &sys_handle, &d4_state, ship4).await);
    let cur4 = drone_test_helpers::active_drone(&ctx, d4)
        .expect("out-of-range drone restored");
    assert!(!cur4.is_abandoned);
    assert_eq!(cur4.parent_ship_id, ship4);
    assert_eq!(cur4.activity_state, 4);
    assert_eq!(ctx.db.get_item(d4).unwrap().location_id, sys_id as i64);

    // Try atomic scoop on cross-system ship d3 -> abandons d3 in space without deleting or scooping
    let d3_state = drone_test_helpers::active_drone(&ctx, d3).unwrap();
    assert!(!super::try_atomic_scoop_drone(&ctx, &sys_handle, &d3_state, ship3).await);

    // Step returning drones -> abandons d1 (destroyed ship) and d2 (docked ship), keeps d4 controlled
    step_returning_drones(&ctx).await;
    for (did, orig_owner) in [(d1, char1), (d2, char2), (d3, char3)] {
        let st = drone_test_helpers::active_drone(&ctx, did)
            .unwrap_or_else(|| panic!("drone {did} must remain abandoned in space"));
        assert!(st.is_abandoned);
        assert_eq!(st.owner_char_id, 0);
        assert_eq!(st.original_owner_char_id, orig_owner);
        assert_eq!(st.parent_ship_id, 0);
        assert_eq!(st.activity_state, 0);
        let item = ctx.db.get_item(did).unwrap();
        assert_eq!(item.location_id, sys_id as i64);
        assert_eq!(item.flag, InventoryFlag::None);
    }
    let st4 = drone_test_helpers::active_drone(&ctx, d4)
        .expect("valid far returning drone remains controlled");
    assert!(!st4.is_abandoned);
    assert_eq!(st4.parent_ship_id, ship4);
    for did in [d1, d2, d3, d4] {
        assert!(sys_handle.get_ball_snapshot(did).await.unwrap().is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_return_and_scoop_use_surface_range_fail_closed_and_block_destroyed_resurrection() {
    use super::step_returning_drones;
    use crate::services::combat_crimewatch::combat_npcs_drones::{
        apply_damage_to_drone_in_space, handle_npcs_drones,
    };

    let ctx = test_ctx();
    let sys_id = 30_000_142u32;
    let char_id = 90_995_001i64;
    let titan_ship = 955_000_001i64;
    let d_sentry = 955_000_011i64;
    let d_mobile = 955_000_012i64;
    let d_scoop_ok = 955_000_013i64;
    let d_scoop_far = 955_000_014i64;
    let d_no_ball = 955_000_015i64;
    let d_killed_claimed_ball = 955_000_016i64;
    let d_killed_claimed_item = 955_000_116i64;
    let d_killed_direct_ball = 955_000_017i64;
    let d_killed_direct_item = 955_000_117i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut c = snap.characters[&90_000_001].clone();
        c.character_id = char_id;
        c.user_id = char_id as u64;
        c.name = "TitanDronePilot".into();
        c.solar_system_id = sys_id;
        c.active_ship_id = titan_ship;
        c.station_id = None;
        c.structure_id = None;
        snap.characters.insert(char_id, c);

        // 593 Tristan carries a 40 m3 drone bay (enough for 5 m3 test drones) while spawned with radius 3_000 m.
        snap.items.insert(
            titan_ship,
            InventoryItemRecord {
                item_id: titan_ship,
                type_id: 593,
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
            (d_sentry, 2454u32),
            (d_mobile, 2454u32),
            (d_scoop_ok, 2454u32),
            (d_scoop_far, 2454u32),
            (d_no_ball, 2454u32),
            (d_killed_claimed_item, 2454u32),
            (d_killed_direct_item, 2454u32),
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
        super::super::record_drone_ball_meta_in_snap(
            snap,
            d_killed_claimed_item,
            d_killed_claimed_ball,
        );
        super::super::record_drone_ball_meta_in_snap(
            snap,
            d_killed_direct_item,
            d_killed_direct_ball,
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // `get_or_spawn` auto-hydrates in-space `snap.items` into ballpark; remove `d_no_ball` so its ball is genuinely absent.
    let _ = sys_handle.remove_ball(d_no_ball).await;
    assert!(sys_handle.get_ball_snapshot(d_no_ball).await.unwrap().is_none());
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: titan_ship,
            type_id: 593,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.5,
            mass: 50_000_000.0,
            radius: 3_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
    // All within surface_distance <= 2_500 m (center_distance = 4_500 m > 2_500 m) except d_scoop_far (surface = 2_600 m)
    spawn_ball(&ctx, sys_id, d_sentry, 2454, DVec3::new(4_500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_mobile, 2454, DVec3::new(4_500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_scoop_ok, 2454, DVec3::new(4_500.0, 0.0, 0.0));
    spawn_ball(&ctx, sys_id, d_scoop_far, 2454, DVec3::new(5_650.0, 0.0, 0.0));
    // Give d_killed_claimed_ball and d_killed_direct_ball finite HP so apply_damage_to_drone_in_space destroys them.
    for bid in [d_killed_claimed_ball, d_killed_direct_ball] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 2454,
                position: DVec3::new(3_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 500.0,
                inertia: 1.5,
                mass: 5_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 10.0,
                max_shield_hp: 10.0,
                armor_hp: 10.0,
                max_armor_hp: 10.0,
                structure_hp: 10.0,
                max_structure_hp: 10.0,
                ..CombatState::default()
            },
        );
    }

    drone_test_helpers::seed_active_drones(
        &ctx,
        [
            make_drone_state(d_sentry, d_sentry, 2454, char_id, titan_ship, sys_id, true, 0),
            make_drone_state(d_mobile, d_mobile, 2454, char_id, titan_ship, sys_id, false, 4),
            make_drone_state(d_scoop_ok, d_scoop_ok, 2454, char_id, titan_ship, sys_id, false, 0),
            make_drone_state(d_scoop_far, d_scoop_far, 2454, char_id, titan_ship, sys_id, false, 0),
            make_drone_state(d_no_ball, d_no_ball, 2454, char_id, titan_ship, sys_id, false, 4),
            make_drone_state(
                d_killed_direct_ball,
                d_killed_direct_item,
                2454,
                char_id,
                titan_ship,
                sys_id,
                false,
                0,
            ),
        ],
    );

    let mut sess = SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        ship_id: Some(titan_ship),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };

    // 1. Capital hull surface range allows immediate sentry return + mobile step_returning_drones scoop + ScoopToDroneBay
    handle_cmd_return_bay(&ctx, &mut sess, &[PyRep::Integer(d_sentry)], &HashMap::new())
        .await
        .expect("sentry within 2500m hull surface of 3000m capital succeeds");
    step_returning_drones(&ctx).await;

    let scoop_ok_res = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(d_scoop_ok)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .expect("ScoopToDroneBay within capital hull surface range succeeds");
    assert_eq!(scoop_ok_res.dict_get("scoopedDroneItemID").and_then(PyRep::as_i64), Some(d_scoop_ok));

    let scoop_far_err = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(d_scoop_far)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(scoop_far_err.code, "OutOfScoopRange");

    ctx.db.with_snapshot(|snap| {
        for scooped in [d_sentry, d_mobile, d_scoop_ok] {
            let it = &snap.items[&scooped];
            assert_eq!(it.location_id, titan_ship);
            assert_eq!(it.flag, InventoryFlag::DroneBay);
        }
        assert_eq!(snap.items[&d_scoop_far].flag, InventoryFlag::None);
    });

    // 2. try_atomic_scoop_drone fails closed when drone_ball_opt is None (never moves to DroneBay)
    let no_ball_state = drone_test_helpers::active_drone(&ctx, d_no_ball)
        .expect("d_no_ball must remain in active space drones when drone ball is absent");
    assert!(!super::try_atomic_scoop_drone(&ctx, &sys_handle, &no_ball_state, titan_ship).await);
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&d_no_ball];
        assert_eq!(it.location_id, sys_id as i64);
        assert_eq!(it.flag, InventoryFlag::None);
    });

    // 3a. Concurrent kill after active space drone claim still deletes DB item and marks item destroyed
    let (_, claimed_destroyed) = apply_damage_to_drone_in_space(
        &ctx,
        sys_id,
        d_killed_claimed_ball,
        DamageVector::new(1_000.0, 1_000.0, 1_000.0, 1_000.0),
    )
    .await;
    assert!(claimed_destroyed);
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&d_killed_claimed_item), "destroyed drone item must be deleted even if claimed from runtime map");
        assert!(snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(d_killed_claimed_ball));
        assert!(snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(d_killed_claimed_item));
    });

    // 3b. ScoopToDroneBay and try_atomic_scoop_drone both refuse recently destroyed drones even if a stale item row remains
    let (_, direct_destroyed) = apply_damage_to_drone_in_space(
        &ctx,
        sys_id,
        d_killed_direct_ball,
        DamageVector::new(1_000.0, 1_000.0, 1_000.0, 1_000.0),
    )
    .await;
    assert!(direct_destroyed);

    // Re-seed stale ball + item row for killed IDs to prove recently_destroyed gate rejects before DB move
    spawn_ball(&ctx, sys_id, d_killed_direct_ball, 2454, DVec3::new(3_500.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            d_killed_direct_item,
            InventoryItemRecord {
                item_id: d_killed_direct_item,
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
        super::super::record_drone_ball_meta_in_snap(snap, d_killed_direct_item, d_killed_direct_ball);
    });
    let scoop_killed_err = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(d_killed_direct_ball)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(scoop_killed_err.code, "DroneNotInSpace");
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&d_killed_direct_item),
            "ScoopToDroneBay must persist destroyed drone purge rather than rolling back"
        );
        assert!(
            !snap.sp6_lifecycle.space_object_meta.contains_key(&d_killed_direct_item),
            "ScoopToDroneBay must persist destroyed drone space_object_meta purge"
        );
    });
    let fake_claimed_state = make_drone_state(
        d_killed_direct_ball,
        d_killed_direct_item,
        2454,
        char_id,
        titan_ship,
        sys_id,
        false,
        4,
    );
    assert!(!super::try_atomic_scoop_drone(&ctx, &sys_handle, &fake_claimed_state, titan_ship).await);
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.items.get(&d_killed_direct_item).is_none_or(|it| it.flag != InventoryFlag::DroneBay),
            "recently destroyed drone must never resurrect into DroneBay"
        );
    });

    // Exercise the in-transaction DestroyedPurged commit branch directly: an un-claimed/abandoned
    // drone ball whose resolved `drone_item_id` (not `drone_ball_id`) is in persisted tombstones
    // bypasses the pre-tx runtime ball check and must still commit its DB purge on `DroneNotInSpace`.
    let d_persisted_ball = d_killed_direct_ball + 10;
    let d_persisted_item = d_killed_direct_item + 10;
    spawn_ball(&ctx, sys_id, d_persisted_ball, 2454, DVec3::new(3_500.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            d_persisted_item,
            InventoryItemRecord {
                item_id: d_persisted_item,
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
        super::super::record_drone_ball_meta_in_snap(snap, d_persisted_item, d_persisted_ball);
        snap.combat_crimewatch_state
            .record_destroyed_item_id(d_persisted_item);
    });
    let persisted_scoop_err = handle_npcs_drones(
        &ctx,
        &mut sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(d_persisted_ball)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(persisted_scoop_err.code, "DroneNotInSpace");
    assert!(sys_handle.get_ball_snapshot(d_persisted_ball).await.unwrap().is_none());
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.items.contains_key(&d_persisted_item),
            "ScoopToDroneBay DestroyedPurged branch must commit destroyed drone item purge"
        );
        assert!(
            !snap.sp6_lifecycle.space_object_meta.contains_key(&d_persisted_item),
            "ScoopToDroneBay DestroyedPurged branch must commit destroyed drone meta purge"
        );
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn launched_active_drone_cannot_be_stolen_during_return_or_scoop_race() {
    use crate::services::combat_crimewatch::combat_npcs_drones::handle_npcs_drones;

    let ctx = Arc::new(test_ctx());
    let sys_id = 30_000_142u32;
    let owner_char = 90_996_001i64;
    let owner_ship = 956_000_001i64;
    let thief_char = 90_996_002i64;
    let thief_ship = 956_000_002i64;
    let drone_stack_id = 956_000_101i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&90_000_001].clone();
        for (cid, sid, name) in [
            (owner_char, owner_ship, "ActiveDroneOwner"),
            (thief_char, thief_ship, "ActiveDroneThief"),
        ] {
            let mut ch = base_char.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = name.into();
            ch.solar_system_id = sys_id;
            ch.active_ship_id = sid;
            ch.station_id = None;
            ch.structure_id = None;
            snap.characters.insert(cid, ch);

            snap.items.insert(
                sid,
                InventoryItemRecord {
                    item_id: sid,
                    type_id: 593,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.skills.entry(cid).or_default().insert(
                3436,
                eve_db::TrainedSkillRecord {
                    type_id: 3436,
                    level: 2,
                    skill_points: 1_415.0,
                },
            );
        }
        snap.items.insert(
            drone_stack_id,
            InventoryItemRecord {
                item_id: drone_stack_id,
                type_id: 2454,
                owner_id: owner_char,
                location_id: owner_ship,
                flag: InventoryFlag::DroneBay,
                quantity: 1,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for sid in [owner_ship, thief_ship] {
        spawn_ball(&ctx, sys_id, sid, 593, DVec3::ZERO);
    }

    let mut owner_sess = SessionState {
        user_id: Some(owner_char as u64),
        character_id: Some(owner_char),
        ship_id: Some(owner_ship),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let mut thief_sess = SessionState {
        user_id: Some(thief_char as u64),
        character_id: Some(thief_char),
        ship_id: Some(thief_ship),
        solar_system_id: Some(sys_id),
        ..Default::default()
    };
    let kw = HashMap::new();

    // Pilot A launches active drone (`LaunchDrones` sets `space_object_meta[drone_id].drone_ball_id`)
    let PyRep::List(launched) = handle_npcs_drones(
        &ctx,
        &mut owner_sess,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(drone_stack_id)])],
        &kw,
    )
    .await
    .unwrap()
    .expect("LaunchDrones succeeds")
    else {
        panic!("expected LaunchDrones ID list");
    };
    let drone_bid = launched[0].as_i64().unwrap();
    let drone_item_id = drone_test_helpers::active_drone(&ctx, drone_bid)
        .unwrap()
        .drone_item_id;
    let _ = sys_handle.set_ball_position(drone_bid, DVec3::new(500.0, 0.0, 0.0)).await;

    // Confirm space_object_meta has drone_ball_id populated, yet Pilot B CANNOT scoop it as "abandoned"
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.sp6_lifecycle
                .space_object_meta
                .get(&drone_item_id)
                .and_then(|m| m.drone_ball_id),
            Some(drone_bid)
        );
    });
    let thief_pre_err = handle_npcs_drones(
        &ctx,
        &mut thief_sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(drone_bid)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(thief_pre_err.code, "DroneOwnedByOther");

    // Race Pilot A `CmdReturnBay` (calls `try_atomic_scoop_drone`) against Pilot B `ScoopToDroneBay`
    // across multiple iterations (including re-launch) — Pilot B must NEVER steal Pilot A's active drone.
    let mut cur_bid = drone_bid;
    let mut cur_item = drone_item_id;
    for iter in 0..8 {
        let _ = sys_handle.set_ball_position(cur_bid, DVec3::new(500.0, 0.0, 0.0)).await;
        let c1 = Arc::clone(&ctx);
        let c2 = Arc::clone(&ctx);
        let mut s1 = owner_sess.clone();
        let mut s2 = thief_sess.clone();
        let ret_args = [PyRep::List(vec![PyRep::Integer(cur_bid)])];
        let scoop_args = [PyRep::Integer(cur_bid)];
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);
        let (ret_res, thief_res) = tokio::join!(
            async move {
                b1.wait().await;
                handle_cmd_return_bay(&c1, &mut s1, &ret_args, &HashMap::new()).await
            },
            async move {
                b2.wait().await;
                handle_npcs_drones(&c2, &mut s2, "ship", "ScoopToDroneBay", &scoop_args, &HashMap::new()).await
            }
        );
        assert!(ret_res.is_ok(), "iter {iter}: owner CmdReturnBay failed: {ret_res:?}");
        let thief_err = thief_res
            .expect("handled")
            .expect_err("thief ScoopToDroneBay must NEVER succeed against active owner return");
        assert!(
            matches!(thief_err.code.as_str(), "DroneOwnedByOther" | "DroneNotInSpace"),
            "iter {iter}: unexpected thief error {:?}",
            thief_err
        );
        ctx.db.with_snapshot(|snap| {
            let it = &snap.items[&cur_item];
            assert_eq!(it.location_id, owner_ship);
            assert_eq!(it.flag, InventoryFlag::DroneBay);
            assert_eq!(it.owner_id, owner_char);
        });

        let PyRep::List(relaunched) = handle_npcs_drones(
            &ctx,
            &mut owner_sess,
            "ship",
            "LaunchDrones",
            &[PyRep::List(vec![PyRep::Integer(cur_item)])],
            &kw,
        )
        .await
        .unwrap()
        .expect("re-launch succeeds")
        else {
            panic!("expected LaunchDrones ID list");
        };
        cur_bid = relaunched[0].as_i64().unwrap();
        cur_item = drone_test_helpers::active_drone(&ctx, cur_bid)
            .unwrap()
            .drone_item_id;
    }

    // Once Pilot A genuinely abandons the active drone (`CmdAbandonDrone`), Pilot B CAN scoop it.
    let _ = sys_handle.set_ball_position(cur_bid, DVec3::new(500.0, 0.0, 0.0)).await;
    super::handle_cmd_abandon_drone(&ctx, &mut owner_sess, &[PyRep::Integer(cur_bid)], &kw)
        .await
        .expect("CmdAbandonDrone succeeds");
    let ok_thief = handle_npcs_drones(
        &ctx,
        &mut thief_sess,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(cur_bid)],
        &kw,
    )
    .await
    .unwrap()
    .expect("ScoopToDroneBay on genuinely abandoned drone succeeds");
    assert_eq!(
        ok_thief.dict_get("scoopedDroneItemID").and_then(PyRep::as_i64),
        Some(cur_item)
    );
    assert_eq!(
        ok_thief.dict_get("newOwnerID").and_then(PyRep::as_i64),
        Some(thief_char)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scoop_to_drone_bay_and_atomic_return_handle_concurrent_races_and_docking() {
    use crate::services::combat_crimewatch::combat_npcs_drones::{
        handle_npcs_drones, move_drone_item_to_bay_in_snap,
    };

    let ctx = Arc::new(test_ctx());
    let sys_id = 30_000_142u32;
    let char_id = 90_000_001i64;
    let ship_id = 920_009_001i64;
    let d_race = 920_009_101i64;
    let d_dock = 920_009_102i64;
    let d_cargo = 920_009_103i64;

    let uid = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("demo character");
        ch.active_ship_id = ship_id;
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.structure_id = None;
        let uid = ch.user_id;
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 645, // Dominix (large drone bay + bandwidth)
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        for id in [d_race, d_dock, d_cargo] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
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
        }
        uid
    });
    ctx.db
        .acquire_online_lock_with_computer_hash(
            uid,
            1,
            Some(char_id),
            None,
            true,
            ctx.now_unix(),
        )
        .expect("register online");

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_ball(&ctx, sys_id, ship_id, 645, DVec3::ZERO);
    for id in [d_race, d_dock, d_cargo] {
        spawn_ball(&ctx, sys_id, id, 2454, DVec3::new(500.0, 0.0, 0.0));
    }

    drone_test_helpers::seed_active_drones(
        &ctx,
        [d_race, d_dock, d_cargo]
            .into_iter()
            .map(|id| make_drone_state(id, id, 2454, char_id, ship_id, sys_id, false, 1)),
    );

    let mut owner_sess = SessionState::default();
    owner_sess.user_id = Some(uid);
    owner_sess.character_id = Some(char_id);
    owner_sess.ship_id = Some(ship_id);
    owner_sess.solar_system_id = Some(sys_id);
    let kw = HashMap::new();

    // 1. Overlapping concurrent `ScoopToDroneBay` calls gated behind `db.with_mut_snapshot` lock:
    //    exactly one succeeds, one returns `DroneNotInSpace`, and NO zombie remains in active space drones.
    {
        let db_hold = Arc::clone(&ctx.db);
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_hold.with_mut_snapshot(|_snap| {
                let _ = locked_tx.send(());
                let _ = release_rx.blocking_recv();
            });
        });
        locked_rx.await.unwrap();

        let (e1_tx, e1_rx) = tokio::sync::oneshot::channel::<()>();
        let (e2_tx, e2_rx) = tokio::sync::oneshot::channel::<()>();
        let c1 = Arc::clone(&ctx);
        let c2 = Arc::clone(&ctx);
        let mut s1 = owner_sess.clone();
        let mut s2 = owner_sess.clone();
        let k1 = kw.clone();
        let k2 = kw.clone();

        let t1 = tokio::spawn(async move {
            let _ = e1_tx.send(());
            handle_npcs_drones(&c1, &mut s1, "ship", "ScoopToDroneBay", &[PyRep::Integer(d_race)], &k1)
                .await
        });
        let t2 = tokio::spawn(async move {
            let _ = e2_tx.send(());
            handle_npcs_drones(&c2, &mut s2, "ship", "ScoopToDroneBay", &[PyRep::Integer(d_race)], &k2)
                .await
        });
        e1_rx.await.unwrap();
        e2_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let _ = sys_handle.get_ball_snapshot(ship_id).await;
        let _ = release_tx.send(());
        holder.await.unwrap();

        let r1 = t1.await.unwrap().expect("t1 handled");
        let r2 = t2.await.unwrap().expect("t2 handled");
        match (r1, r2) {
            (Ok(_), Err(e)) | (Err(e), Ok(_)) => assert_eq!(e.code, "DroneNotInSpace"),
            other => panic!("expected exactly one Ok and one DroneNotInSpace, got {other:?}"),
        }
        ctx.db.with_snapshot(|snap| {
            let it = &snap.items[&d_race];
            assert_eq!(it.location_id, ship_id);
            assert_eq!(it.flag, InventoryFlag::DroneBay);
            assert_eq!(it.owner_id, char_id);
        });
        assert!(
            !drone_test_helpers::has_active_drone(&ctx, d_race),
            "losing ScoopToDroneBay must NOT re-insert zombie active drone"
        );
        assert_eq!(sys_handle.drone_next_ready_unix(d_race), None);
    }

    // 2. Direct `F-1489-R3-02` unit check on `move_drone_item_to_bay_in_snap` + `try_atomic_scoop_drone`
    //    race where the drone item is moved to ship Cargo (ball still present in space) while returning.
    {
        ctx.db.with_mut_snapshot(|snap| {
            assert!(
                !move_drone_item_to_bay_in_snap(
                    snap,
                    d_race,
                    d_race,
                    sys_id,
                    Some(char_id),
                    ship_id,
                    false,
                ),
                "item already in DroneBay must be rejected"
            );
            assert!(
                !move_drone_item_to_bay_in_snap(
                    snap,
                    d_cargo,
                    d_cargo,
                    sys_id + 1,
                    Some(char_id),
                    ship_id,
                    false,
                ),
                "item in different solar system must be rejected"
            );
            assert_eq!(snap.items[&d_cargo].flag, InventoryFlag::None);
            assert_eq!(snap.items[&d_cargo].location_id, sys_id as i64);
        });

        let state_cargo = drone_test_helpers::active_drone(&ctx, d_cargo).unwrap();
        let db_hold = Arc::clone(&ctx.db);
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_hold.with_mut_snapshot(|snap| {
                let _ = locked_tx.send(());
                let _ = release_rx.blocking_recv();
                if let Some(it) = snap.items.get_mut(&d_cargo) {
                    it.location_id = ship_id;
                    it.flag = InventoryFlag::Cargo;
                }
            });
        });
        locked_rx.await.unwrap();
        let _ = ctx.drain_notifications_for_char(char_id);

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let c_ret = Arc::clone(&ctx);
        let sys_ret = sys_handle.clone();
        let ret_task = tokio::spawn(async move {
            let _ = entered_tx.send(());
            super::try_atomic_scoop_drone(&c_ret, &sys_ret, &state_cargo, ship_id).await
        });
        entered_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let _ = release_tx.send(());
        holder.await.unwrap();

        let scooped = ret_task.await.unwrap();
        assert!(!scooped, "try_atomic_scoop_drone must return false when item moved to Cargo");
        ctx.db.with_snapshot(|snap| {
            let it = &snap.items[&d_cargo];
            assert_eq!(it.location_id, ship_id);
            assert_eq!(it.flag, InventoryFlag::Cargo);
        });
        assert!(
            !drone_test_helpers::has_active_drone(&ctx, d_cargo),
            "stale Cargo drone must be removed without zombie entry"
        );
        assert!(
            sys_handle.get_ball_snapshot(d_cargo).await.unwrap().is_some(),
            "NotInSpace outcome must not remove live ballpark ball"
        );
        let notifs_cargo = ctx.drain_notifications_for_char(char_id);
        assert!(
            notifs_cargo
                .iter()
                .all(|n| n.event != "OnDroneStateChange" && n.event != "OnDroneControlLost"),
            "NotInSpace return rejection must emit zero drone state notifications, got {notifs_cargo:?}"
        );
        assert!(
            !notifs_cargo.iter().any(|n| {
                n.event == "OnItemChange"
                    && format!("{n:?}").contains("DroneBay")
            }),
            "NotInSpace return rejection must emit zero OnItemChange into DroneBay, got {notifs_cargo:?}"
        );
    }

    // 3. Dock-during-scoop race gated behind `db.with_mut_snapshot`:
    //    ScoopToDroneBay passes initial snapshot/ball checks, pilot docks in DB while DB lock is held,
    //    and ScoopToDroneBay returns `DroneNotInSpace`, leaves item in space, abandons drone in runtime,
    //    and emits `OnDroneStateChange(0, 0) < OnDroneActivityChange(0) < OnDroneControlLost`.
    {
        let _ = ctx.drain_notifications_for_char(char_id);
        let db_hold = Arc::clone(&ctx.db);
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel::<()>();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder = tokio::task::spawn_blocking(move || {
            db_hold.with_mut_snapshot(|snap| {
                let _ = locked_tx.send(());
                let _ = release_rx.blocking_recv();
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.station_id = Some(60_003_760);
                }
                if let Some(sh) = snap.items.get_mut(&ship_id) {
                    sh.location_id = 60_003_760;
                    sh.flag = InventoryFlag::Hangar;
                }
            });
        });
        locked_rx.await.unwrap();

        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
        let c_dock = Arc::clone(&ctx);
        let mut s_dock = owner_sess.clone();
        let k_dock = kw.clone();
        let scoop_task = tokio::spawn(async move {
            let _ = entered_tx.send(());
            handle_npcs_drones(&c_dock, &mut s_dock, "ship", "ScoopToDroneBay", &[PyRep::Integer(d_dock)], &k_dock)
                .await
        });
        entered_rx.await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let _ = sys_handle.get_ball_snapshot(ship_id).await;
        let _ = release_tx.send(());
        holder.await.unwrap();

        let dock_err = scoop_task
            .await
            .unwrap()
            .expect("handled")
            .expect_err("dock during scoop must fail");
        assert_eq!(dock_err.code, "DroneNotInSpace");

        ctx.db.with_snapshot(|snap| {
            let it = &snap.items[&d_dock];
            assert_eq!(it.location_id, sys_id as i64);
            assert_eq!(it.flag, InventoryFlag::None);
        });
        let d = drone_test_helpers::active_drone(&ctx, d_dock)
            .expect("abandoned drone stays tracked in space");
        assert!(d.is_abandoned);
        assert_eq!(d.owner_char_id, 0);
        assert_eq!(d.parent_ship_id, 0);
        assert_eq!(d.activity_state, 0);

        let dock_notifs = ctx.drain_notifications_for_char(char_id);
        let idx_state = dock_notifs
            .iter()
            .position(|n| {
                n.event == "OnDroneStateChange"
                    && n.args.first().and_then(|v| v.as_i64()) == Some(d_dock)
                    && n.args.get(1).and_then(|v| v.as_i64()) == Some(0)
                    && n.args.get(2).and_then(|v| v.as_i64()) == Some(0)
            })
            .expect("dock-during-scoop abandonment emits OnDroneStateChange(0, 0)");
        let idx_act = dock_notifs
            .iter()
            .position(|n| {
                n.event == "OnDroneActivityChange"
                    && n.args.first().and_then(|v| v.as_i64()) == Some(d_dock)
                    && n.args.get(1).and_then(|v| v.as_i64()) == Some(0)
            })
            .expect("dock-during-scoop abandonment emits OnDroneActivityChange(0)");
        let idx_lost = dock_notifs
            .iter()
            .position(|n| {
                n.event == "OnDroneControlLost"
                    && n.args.first().and_then(|v| v.as_i64()) == Some(d_dock)
            })
            .expect("dock-during-scoop abandonment emits OnDroneControlLost");
        assert!(
            idx_state < idx_act && idx_act < idx_lost,
            "OnDroneStateChange(0,0) must precede OnDroneControlLost on dock-during-scoop"
        );
    }
}
