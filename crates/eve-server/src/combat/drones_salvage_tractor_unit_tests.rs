//! Unit and mutation-killing tests for `combat::drones_salvage_tractor`.

use super::*;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{Ball, CombatState, MovementMode};
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

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

fn spawn_ship_ball(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
    owner_id: i64,
    type_id: u32,
    pos: DVec3,
    radius: f64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, type_id, owner_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(ship_id, [pos.x, pos.y, pos.z]);
        if let Some(ch) = snap.characters.get_mut(&owner_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let cs = ctx.build_ship_combat_state(owner_id, ship_id, type_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: ship_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_100_000.0,
            radius,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        },
        cs,
    );
}

fn spawn_wreck_ball(
    ctx: &ServerContext,
    sys_id: u32,
    wreck_id: i64,
    type_id: u32,
    owner_id: i64,
    pos: DVec3,
    radius: f64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wreck_id,
            make_item(wreck_id, type_id, owner_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.item_positions.insert(wreck_id, [pos.x, pos.y, pos.z]);
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.try_spawn_ball_with_combat(
        Ball {
            id: wreck_id,
            type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 10_000.0,
            radius,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        CombatState::default(),
    );
}

#[tokio::test]
async fn attempt_salvage_wreck_range_chance_clamp_and_empty_wreck() {
    let ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_300_001;

    // Missing ship in space -> Err
    assert!(attempt_salvage_wreck(&ctx, sys_id, char_id, 999_888, 100_300_010, 0.3, 0.1)
        .await
        .unwrap_err()
        .contains("not found in space"));

    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, 40.0);

    // Missing wreck in space -> Err
    assert!(attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, 999_777, 0.3, 0.1)
        .await
        .unwrap_err()
        .contains("not found in space"));

    // Exact 5,000.0 m boundary vs 5,000.1 m
    let w_far = 100_300_010;
    let w_edge = 100_300_011;
    spawn_wreck_ball(&ctx, sys_id, w_far, 26557, char_id, DVec3::new(5_000.1, 0.0, 0.0), 30.0);
    spawn_wreck_ball(&ctx, sys_id, w_edge, 26557, char_id, DVec3::new(5_000.0, 0.0, 0.0), 30.0);

    assert!(attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, w_far, 0.3, 0.0)
        .await
        .unwrap_err()
        .contains("maximum range is 5,000 m"));

    // Chance clamp min (0.05): bonus -0.50 -> chance = 0.05; roll 0.05 fails, 0.049 succeeds
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, w_edge, -0.50, 0.05)
            .await
            .unwrap(),
        SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials",
        }
    );

    // Mid chance (0.20 + 0.30 = 0.50): roll 0.50 fails
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, w_edge, 0.30, 0.50)
            .await
            .unwrap(),
        SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials",
        }
    );

    // Chance clamp max (0.95): bonus 1.50 -> chance = 0.95; roll 0.95 fails, 0.949 succeeds
    assert_eq!(
        attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, w_edge, 1.50, 0.95)
            .await
            .unwrap(),
        SalvageOutcome::Failed {
            reason: "Salvager cycle failed to extract materials",
        }
    );
    let ok_out = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, w_edge, 1.50, 0.949)
        .await
        .unwrap();
    assert_eq!(
        ok_out,
        SalvageOutcome::Success {
            yielded_salvage_type_ids: vec![25588, 25595, 25597],
            yielded_quantities: vec![4, 2, 3],
        }
    );
    // Empty wreck does NOT spawn a cargo container (type 23)
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&w_edge));
        assert!(snap
            .items
            .values()
            .all(|i| !(i.type_id == 23 && i.location_id == sys_id as i64)));
    });
}

#[tokio::test]
async fn validate_and_execute_wreck_salvage_cycle_all_difficulty_tiers_and_radii() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_310_001;
    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, 40.0);

    // 1. Non-existent target, wrong system, and non-wreck target (InvalidSalvageTarget)
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, 999_111, None)
            .await
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_310_002,
            make_item(100_310_002, 26557, char_id, 30_000_144, InventoryFlag::None, 1),
        );
        snap.items.insert(
            100_310_003,
            make_item(100_310_003, 587, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, 100_310_002, None)
            .await
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, 100_310_003, None)
            .await
            .unwrap_err()
            .code,
        "InvalidSalvageTarget"
    );

    // 2. Module range subtracts ship_radius (40m) + wreck_radius (30m) = 70m:
    // center distance 6,070m -> surface distance 6,000m (Ok); 6,071m -> 6,001m (TargetOutOfRange)
    spawn_wreck_ball(
        &ctx,
        sys_id,
        100_310_004,
        26468, // special wreck type_id == 26468
        char_id,
        DVec3::new(6_070.0, 0.0, 0.0),
        30.0,
    );
    spawn_wreck_ball(
        &ctx,
        sys_id,
        100_310_005,
        26557,
        char_id,
        DVec3::new(6_071.0, 0.0, 0.0),
        30.0,
    );
    assert!(validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, 100_310_004, Some(25_861))
        .await
        .is_ok());
    assert_eq!(
        validate_wreck_salvage_target(&ctx, sys_id, char_id, ship_id, 100_310_005, Some(25_861))
            .await
            .unwrap_err()
            .code,
        "TargetOutOfRange"
    );

    // 3. Create custom wreck types for the 4 difficulty tiers:
    // -15.0 (Sleeper: 30259, 30258), -5.0 (T2: 25605, 25600), 10.0 (T1 size_mult=2), 20.0 (T1 size_mult=1)
    let base_wreck = ctx.sde.types[&26557].clone();
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        for (tid, diff) in [
            (999_701u32, -15.0),
            (999_702u32, -5.0),
            (999_703u32, 10.0),
            (999_704u32, 20.0),
        ] {
            let mut wt = base_wreck.clone();
            wt.type_id = tid;
            wt.group_id = 186;
            wt.default_attributes.insert(901, diff);
            sde_mut.types.insert(tid, wt);
        }
        // Salvager module with +200% access bonus so every roll < 100% succeeds deterministically
        let mut super_salv = base_wreck;
        super_salv.type_id = 999_799;
        super_salv.default_attributes.insert(902, 200.0);
        sde_mut.types.insert(999_799, super_salv);
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            3438,
            TrainedSkillRecord {
                type_id: 3438,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });

    for (idx, (wtid, expected_pool, expected_first_qty)) in [
        (999_701u32, &ctx.sde.salvage_pools.ancient, 2i32),
        (999_702u32, &ctx.sde.salvage_pools.t2_intact, 2i32),
        (999_703u32, &ctx.sde.salvage_pools.t1_standard, 8i32),
        (999_704u32, &ctx.sde.salvage_pools.t1_standard, 4i32),
    ]
    .into_iter()
    .enumerate()
    {
        let expected_table = ctx.sde.salvage_materials_for_wreck(wtid, true);
        assert!(!expected_table.is_empty());
        let expected_first_tid = expected_table[0].0;
        assert!(expected_pool.contains(&expected_first_tid));
        assert_eq!(expected_table[0].1, expected_first_qty);

        let wid = 100_310_100 + idx as i64;
        spawn_wreck_ball(&ctx, sys_id, wid, wtid, char_id, DVec3::new(500.0, 0.0, 0.0), 30.0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items
                .retain(|_, i| !(i.location_id == ship_id && i.flag == InventoryFlag::Cargo));
        });
        let rep = execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wid, Some(999_799))
            .await
            .unwrap();
        assert_eq!(rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(true));
        let first_qty = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == InventoryFlag::Cargo
                        && i.type_id == expected_first_tid
                })
                .map(|i| i.quantity)
        });
        assert_eq!(first_qty, Some(expected_first_qty));
    }

    // 4. NotEnoughCargoSpace in execute_wreck_salvage_cycle when ship cargo is overfilled
    let w_full = 100_310_200;
    spawn_wreck_ball(&ctx, sys_id, w_full, 999_704, char_id, DVec3::new(500.0, 0.0, 0.0), 30.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            100_310_201,
            make_item(100_310_201, 1230, char_id, ship_id, InventoryFlag::Cargo, 50_000),
        );
    });
    let full_err =
        execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, w_full, Some(999_799))
            .await
            .unwrap_err();
    assert_eq!(full_err.code, "NotEnoughCargoSpace");
}

#[tokio::test]
async fn tractor_beam_drone_bandwidth_and_drone_controller_exact_checks() {
    // 1. Tractor beam exact stop_distance (1,200 m) and arrival threshold (1,201 m)
    let ship_pos = DVec3::new(100.0, 200.0, 300.0);
    let (p_at_stop, arr_stop) =
        step_tractor_beam(ship_pos + DVec3::new(1_200.0, 0.0, 0.0), ship_pos, 500.0, 1.0);
    assert!(arr_stop);
    assert_eq!(p_at_stop, ship_pos + DVec3::new(1_200.0, 0.0, 0.0));

    // Wreck at 1,701.0 m pulled by 500 m/s for 1s -> ends at 1,201.0 m (arrived == true)
    let (p_1201, arr_1201) =
        step_tractor_beam(ship_pos + DVec3::new(1_701.0, 0.0, 0.0), ship_pos, 500.0, 1.0);
    assert!(arr_1201);
    assert!((p_1201.distance(ship_pos) - 1_201.0).abs() < 1e-9);

    // Wreck at 1,702.0 m pulled by 500 m/s for 1s -> ends at 1,202.0 m (arrived == false)
    let (p_1202, arr_1202) =
        step_tractor_beam(ship_pos + DVec3::new(1_702.0, 0.0, 0.0), ship_pos, 500.0, 1.0);
    assert!(!arr_1202);
    assert!((p_1202.distance(ship_pos) - 1_202.0).abs() < 1e-9);

    // 2. Drone bandwidth exact remaining calculation & bandwidth overflow
    let mut ctx = test_ctx();
    let sys_id = 30_000_142;
    let char_id = 90_000_001;
    let ship_id = 100_320_001;
    let other_ship_id = 100_320_002;
    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, 40.0);
    spawn_ship_ball(&ctx, sys_id, other_ship_id, char_id, 587, DVec3::ZERO, 40.0);

    // Set ship 587 droneBandwidth = 25.0 and create a 10 Mbit/s medium drone type 999_750
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        if let Some(st) = sde_mut.types.get_mut(&587) {
            st.default_attributes
                .insert(eve_sde::attr_ids::DRONE_BANDWIDTH, 25.0);
        }
        let mut med_drone = sde_mut.types[&2454].clone();
        med_drone.type_id = 999_750;
        med_drone
            .default_attributes
            .insert(eve_sde::attr_ids::DRONE_BANDWIDTH_USED, 10.0);
        sde_mut.types.insert(999_750, med_drone);
    }

    // 5 light drones (5 * 5 = 25) -> remaining 0.0
    let rem_zero = validate_drone_bandwidth(&ctx, ship_id, &[2454, 2454], &[2454, 2454, 2454]).unwrap();
    assert!((rem_zero - 0.0).abs() < 1e-9);
    // 1 medium (10) + 1 light (5) = 15 -> remaining 10.0
    let rem_ten = validate_drone_bandwidth(&ctx, ship_id, &[999_750], &[2454]).unwrap();
    assert!((rem_ten - 10.0).abs() < 1e-9);
    // 3 medium drones (30 > 25) -> Err
    assert!(validate_drone_bandwidth(&ctx, ship_id, &[999_750; 3], &[])
        .unwrap_err()
        .contains("Insufficient drone bandwidth"));

    // 3. DroneController error branches & multi-ship parent_ship_id filtering
    let mut ctrl = DroneController::new();
    // Missing drone item
    assert!(ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[999_999_111])
        .await
        .unwrap_err()
        .contains("not found in inventory"));

    // Drone in wrong location
    let wrong_loc_drone = 100_320_010;
    let valid_drone_1 = 100_320_011;
    let valid_drone_2 = 100_320_012;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wrong_loc_drone,
            make_item(wrong_loc_drone, 2454, char_id, other_ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            valid_drone_1,
            make_item(valid_drone_1, 2454, char_id, ship_id, InventoryFlag::DroneBay, 1),
        );
        snap.items.insert(
            valid_drone_2,
            make_item(valid_drone_2, 2454, char_id, other_ship_id, InventoryFlag::DroneBay, 1),
        );
    });
    assert!(ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[wrong_loc_drone])
        .await
        .unwrap_err()
        .contains("not in ship's drone bay"));

    let b1 = ctrl
        .launch_drones(&ctx, sys_id, char_id, ship_id, &[valid_drone_1])
        .await
        .unwrap()[0];
    let b2 = ctrl
        .launch_drones(&ctx, sys_id, char_id, other_ship_id, &[valid_drone_2])
        .await
        .unwrap()[0];

    let target_ship = 100_320_099;
    spawn_ship_ball(&ctx, sys_id, target_ship, 90_000_002, 587, DVec3::new(500.0, 0.0, 0.0), 40.0);
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let shield_before = sys_handle
        .get_combat_state(target_ship)
        .await
        .unwrap()
        .unwrap()
        .shield_hp;

    let engaged = ctrl.engage_target(&ctx, sys_id, target_ship).await.unwrap();
    assert_eq!(engaged, 2);
    let shield_after = sys_handle
        .get_combat_state(target_ship)
        .await
        .unwrap()
        .unwrap()
        .shield_hp;
    assert!(shield_after < shield_before);

    // return_and_orbit for ship_id only clears target_id on b1, leaving b2 targeting target_ship
    let returned = ctrl.return_and_orbit(&ctx, sys_id, ship_id).await.unwrap();
    assert_eq!(returned, 1);
    assert_eq!(ctrl.active_drones[&b1].target_id, None);
    assert_eq!(ctrl.active_drones[&b2].target_id, Some(target_ship));

    // recall_drones for ship_id only recalls valid_drone_1, leaving b2 active in space
    let recalled = ctrl.recall_drones(&ctx, sys_id, ship_id).await.unwrap();
    assert_eq!(recalled, vec![valid_drone_1]);
    assert!(!ctrl.active_drones.contains_key(&b1));
    assert!(ctrl.active_drones.contains_key(&b2));
}
