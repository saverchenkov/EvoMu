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
    let (vol, avail) = match full_err.to_client_error("dogmaIM", "Activate") {
        eve_proto::ClientError::User {
            key: eve_proto::UserErrorKey::NotEnoughCargoSpace,
            args,
        } => match args.as_slice() {
            [(k0, eve_proto::PyRep::Real(v)), (k1, eve_proto::PyRep::Real(a))]
                if k0 == "volume" && k1 == "available" => (*v, *a),
            other => panic!("expected [('volume', Real), ('available', Real)], got {other:?}"),
        },
        other => panic!("expected UserError::NotEnoughCargoSpace, got {other:?}"),
    };
    assert!(vol > 0.0 && avail >= 0.0 && vol > avail, "vol={vol}, avail={avail}");
    let eve_proto::PyRep::Tuple(parts) = full_err
        .to_client_error("dogmaIM", "Activate")
        .to_macho_error_body(1)
    else {
        panic!("expected ErrorResponse tuple");
    };
    let eve_proto::PyRep::Tuple(inner) = &parts[2] else {
        panic!("expected inner tuple");
    };
    let eve_proto::PyRep::Bytes(raw) = &inner[0] else {
        panic!("expected marshaled bytes");
    };
    let eve_proto::PyRep::ObjectEx { header, .. } =
        eve_proto::marshal::decode(raw).expect("decode UserError")
    else {
        panic!("expected ObjectEx");
    };
    let eve_proto::PyRep::Tuple(hdr) = *header else {
        panic!("expected header tuple");
    };
    assert_eq!(hdr[0], eve_proto::PyRep::Token("eveexceptions.UserError".into()));
    let eve_proto::PyRep::Tuple(u_args) = &hdr[1] else {
        panic!("expected UserError args tuple");
    };
    assert_eq!(u_args[0].as_str(), Some("NotEnoughCargoSpace"));
    let eve_proto::PyRep::Dict(entries) = &u_args[1] else {
        panic!("expected UserError dict");
    };
    let wire_kv: Vec<(String, f64)> = entries
        .iter()
        .map(|(k, v)| match (k.as_str(), v) {
            (Some(name), eve_proto::PyRep::Real(num)) => (name.to_string(), *num),
            other => panic!("expected (str, PyRep::Real), got {other:?}"),
        })
        .collect();
    assert_eq!(
        wire_kv,
        vec![("volume".to_string(), vol), ("available".to_string(), avail)]
    );
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

    // recall_drones for other_ship_id recalls valid_drone_2 and leaves active_drones empty
    let recalled_other = ctrl
        .recall_drones(&ctx, sys_id, other_ship_id)
        .await
        .unwrap();
    assert_eq!(recalled_other, vec![valid_drone_2]);
    assert!(ctrl.active_drones.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn salvage_emits_item_change_flushes_remove_ball_and_clears_target_locks() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_330_001_i64;
    let mod_id = 100_330_002_i64;
    let wreck_empty = 100_330_010_i64;
    let wreck_with_loot = 100_330_011_i64;
    let wreck_fail = 100_330_012_i64;
    let loot_child = 100_330_020_i64;
    let mod_tid = 999_861_u32;

    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut salv_mod = sde.types[&25_861].clone();
        salv_mod.type_id = mod_tid;
        salv_mod.default_attributes.insert(902, 200.0);
        sde.types.insert(mod_tid, salv_mod);
    }

    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, 40.0);
    spawn_wreck_ball(
        &ctx,
        sys_id,
        wreck_empty,
        26_573,
        char_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        30.0,
    );
    spawn_wreck_ball(
        &ctx,
        sys_id,
        wreck_with_loot,
        26_573,
        char_id,
        DVec3::new(1_200.0, 0.0, 0.0),
        30.0,
    );
    spawn_wreck_ball(
        &ctx,
        sys_id,
        wreck_fail,
        26_573,
        char_id,
        DVec3::new(1_400.0, 0.0, 0.0),
        30.0,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_item(mod_id, mod_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            loot_child,
            make_item(loot_child, 15_331, char_id, wreck_with_loot, InventoryFlag::Cargo, 5),
        );
    });
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))
        .expect("demo character must exist for salvage notification delivery");
    ctx.db
        .acquire_online_lock(user_id, 9_729, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Prime observer queue so `ship_id` has no pending SetState blocking `FlushPeerObserverUpdates`.
    let _ = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    let mut sys_rx = sys_handle.subscribe();

    // Negative case: failed roll leaves wreck intact and emits zero RemoveBall / OnItemChange / OnTarget.
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.target_locks.entry(ship_id).or_default().push(eve_destiny::TargetLock {
            target_id: wreck_fail,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        });
    }
    let fail_out = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_fail, 0.0, 0.99)
        .await
        .unwrap();
    assert!(matches!(fail_out, SalvageOutcome::Failed { .. }));
    assert!(sys_rx.try_recv().is_err(), "failed salvage must not emit RemoveBall");
    assert!(
        ctx.drain_notifications_for_char(char_id).is_empty(),
        "failed salvage must not emit OnItemChange or OnTarget lost"
    );
    assert!(sys_handle.ballpark.read().unwrap().balls.contains_key(&wreck_fail));

    // Positive case A: `execute_wreck_salvage_cycle` with active target lock + Guristas Small Wreck (26573).
    {
        let mut bp = sys_handle.ballpark.write().unwrap();
        bp.target_locks.entry(ship_id).or_default().push(eve_destiny::TargetLock {
            target_id: wreck_empty,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        });
    }
    let cycle_rep = execute_wreck_salvage_cycle(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_empty,
        Some(mod_tid),
    )
    .await
    .unwrap();
    assert_eq!(cycle_rep.dict_get("salvaged").and_then(|v| v.as_bool()), Some(true));

    let bcast_a = sys_rx
        .try_recv()
        .expect("execute_wreck_salvage_cycle must flush RemoveBall immediately before module deactivation");
    assert!(
        bcast_a
            .observer_events
            .get(&ship_id)
            .is_some_and(|evs| evs.iter().any(|ev| matches!(
                ev,
                eve_destiny::DestinyEvent::RemoveBall { ball_id } if *ball_id == wreck_empty
            ))),
        "flushed broadcast must include RemoveBall({wreck_empty}): {:?}",
        bcast_a.observer_events
    );
    assert!(
        !sys_handle.ballpark.read().unwrap().balls.contains_key(&wreck_empty),
        "wreck ball must be removed from ballpark"
    );
    assert!(
        !sys_handle
            .ballpark
            .read()
            .unwrap()
            .target_locks
            .get(&ship_id)
            .is_some_and(|ls| ls.iter().any(|l| l.target_id == wreck_empty)),
        "target lock on salvaged wreck must be cleared"
    );

    let notifs_a = ctx.drain_notifications_for_char(char_id);
    let cargo_inserts: Vec<_> = notifs_a
        .iter()
        .filter(|n| {
            n.event == "OnItemChange"
                && matches!(
                    n.args.get(1),
                    Some(eve_proto::PyRep::Dict(entries))
                        if entries.iter().any(|(k, v)| {
                            k == &eve_proto::PyRep::Integer(3) && v == &eve_proto::PyRep::Integer(0)
                        })
                )
        })
        .collect();
    assert!(
        !cargo_inserts.is_empty(),
        "salvage cycle must emit OnItemChange insert (ixLocationID=0) for yielded cargo items: {notifs_a:?}"
    );
    assert!(
        notifs_a.iter().any(|n| {
            n.event == "OnTarget"
                && n.args.first() == Some(&eve_proto::PyRep::Integer(wreck_empty))
                && n.args.get(1) == Some(&eve_proto::PyRep::String("lost".into()))
        }),
        "locked target wreck removal must emit OnTarget lost: {notifs_a:?}"
    );
    let cargo_salvage_count = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
            .count()
    });
    assert!(cargo_salvage_count > 0);
    assert_eq!(cargo_inserts.len(), cargo_salvage_count);

    // Positive case B: `attempt_salvage_wreck` on wreck with child loot spawns jetcan + emits moved loot OnItemChange + RemoveBall.
    let ok_loot = attempt_salvage_wreck(&ctx, sys_id, char_id, ship_id, wreck_with_loot, 1.0, 0.0)
        .await
        .unwrap();
    assert!(matches!(ok_loot, SalvageOutcome::Success { .. }));
    let can_id = ctx.db.with_snapshot(|snap| snap.items[&loot_child].location_id);
    assert_ne!(can_id, wreck_with_loot);

    let bcast_b = sys_rx
        .try_recv()
        .expect("salvage with loot must flush jetcan AddBalls2 + wreck RemoveBall immediately");
    let obs_evs_b = bcast_b.observer_events.get(&ship_id).expect("ship observer events");
    assert!(
        obs_evs_b.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == can_id)
        )),
        "flushed broadcast must include AddBalls2 for spawned jetcan {can_id}: {obs_evs_b:?}"
    );
    assert!(
        obs_evs_b.iter().any(|ev| matches!(
            ev,
            eve_destiny::DestinyEvent::RemoveBall { ball_id } if *ball_id == wreck_with_loot
        )),
        "flushed broadcast must include RemoveBall({wreck_with_loot}): {obs_evs_b:?}"
    );

    let notifs_b = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs_b.iter().any(|n| {
            n.event == "OnItemChange"
                && matches!(
                    n.args.get(1),
                    Some(eve_proto::PyRep::Dict(entries))
                        if entries.iter().any(|(k, v)| {
                            k == &eve_proto::PyRep::Integer(3)
                                && v == &eve_proto::PyRep::Integer(wreck_with_loot)
                        })
                )
        }),
        "loot moved from salvaged wreck into jetcan must emit OnItemChange from old wreck location: {notifs_b:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_salvaged_wreck_rejects_already_consumed_or_relocated_wreck() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_331_001_i64;
    let wreck_gone = 100_331_010_i64;
    let wreck_relocated = 100_331_011_i64;
    let loot_id = 100_331_020_i64;

    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, 40.0);
    spawn_wreck_ball(
        &ctx,
        sys_id,
        wreck_gone,
        26_573,
        char_id,
        DVec3::new(900.0, 0.0, 0.0),
        30.0,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            loot_id,
            make_item(loot_id, 15_331, char_id, wreck_gone, InventoryFlag::Cargo, 3),
        );
        snap.items.insert(
            wreck_relocated,
            make_item(
                wreck_relocated,
                26_573,
                char_id,
                60_003_760, // wrong location (not in solar system sys_id)
                InventoryFlag::None,
                1,
            ),
        );
    });

    let mats = vec![(25_588_u32, 2_i32)];
    let first_can = commit_salvaged_wreck_inventory_and_notify(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_gone,
        [900.0, 0.0, 0.0],
        &mats,
    )
    .expect("first salvage commit succeeds");
    assert!(first_can.is_some());
    let snap_len_after_first = ctx.db.with_snapshot(|snap| snap.items.len());
    let cargo_after_first: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::Cargo)
            .map(|i| i.item_id)
            .collect()
    });
    assert_eq!(cargo_after_first.len(), 1);

    // Second salvage commit on already-consumed wreck must fail closed with WreckMissing and mint zero items/cans.
    let second_res = commit_salvaged_wreck_inventory_and_notify(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_gone,
        [900.0, 0.0, 0.0],
        &mats,
    );
    assert!(matches!(second_res, Err(CommitSalvageError::WreckMissing)));

    // Wreck outside the solar system must likewise be rejected with WreckMissing.
    let relocated_res = commit_salvaged_wreck_inventory_and_notify(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_relocated,
        [0.0, 0.0, 0.0],
        &mats,
    );
    assert!(matches!(relocated_res, Err(CommitSalvageError::WreckMissing)));

    let err_second_cycle =
        execute_wreck_salvage_cycle(&ctx, sys_id, char_id, ship_id, wreck_gone, None)
            .await
            .expect_err("second cycle on consumed wreck must return TargetNotInSpace");
    assert_eq!(err_second_cycle.code, "TargetNotInSpace");

    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.len(), snap_len_after_first);
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeating_salvager_salvage_end_flushes_remove_ball_before_single_deactivation() {
    let mut ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char_id = 90_000_001_i64;
    let ship_id = 100_332_001_i64;
    let mod_id = 100_332_002_i64;
    let mod_second = 100_332_003_i64;
    let wreck_id = 100_332_010_i64;
    let mod_tid = 999_862_u32;

    {
        let sde = std::sync::Arc::make_mut(&mut ctx.sde);
        let mut salv_mod = sde.types[&25_861].clone();
        salv_mod.type_id = mod_tid;
        salv_mod.default_attributes.insert(902, 200.0);
        sde.types.insert(mod_tid, salv_mod);
    }

    spawn_ship_ball(&ctx, sys_id, ship_id, char_id, 587, DVec3::ZERO, 40.0);
    spawn_wreck_ball(
        &ctx,
        sys_id,
        wreck_id,
        26_573,
        char_id,
        DVec3::new(1_000.0, 0.0, 0.0),
        30.0,
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            mod_id,
            make_item(mod_id, mod_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            mod_second,
            make_item(mod_second, mod_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.module_active_states.insert(mod_id);
        snap.module_active_states.insert(mod_second);
    });
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))
        .expect("demo char exists");
    ctx.db
        .acquire_online_lock(user_id, 9_730, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_handle.drain_observer_updates(ship_id).await.unwrap();
    sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id,
            module_id: mod_id,
            module_type_id: mod_tid,
            owner_char_id: Some(char_id),
            effect_name: "salvaging".to_string(),
            target_id: Some(wreck_id),
            repeat: 1000,
            duration_s: 5.0,
            elapsed_s: 0.0,
            ..Default::default()
        })
        .await
        .unwrap();
    {
        let mut st = sys_handle.actor_state.write().unwrap();
        st.pending_cycle_actions
            .push(crate::system_actor::PendingCycleAction::SalvageEnd {
                char_id,
                ship_id,
                module_id: mod_id,
                module_type_id: mod_tid,
                wreck_id,
                repeat: 1000,
                effect_name: "salvaging".to_string(),
                next_cycle_cap_deducted: false,
                deducted_cap_gj: 0.0,
            });
    }

    let mut sys_rx = sys_handle.subscribe();
    let mut notify_rx = ctx.notify_tx.subscribe();
    let timeline = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let timeline_bg = std::sync::Arc::clone(&timeline);
    let (done_tx, mut done_rx) = tokio::sync::oneshot::channel::<()>();
    let recorder = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                msg = sys_rx.recv() => {
                    if let Ok(bcast) = msg {
                        if bcast
                            .observer_events
                            .get(&ship_id)
                            .is_some_and(|evs| evs.iter().any(|ev| matches!(
                                ev,
                                eve_destiny::DestinyEvent::RemoveBall { ball_id } if *ball_id == wreck_id
                            )))
                        {
                            timeline_bg.lock().unwrap().push("RemoveBall".to_string());
                        }
                    }
                }
                msg = notify_rx.recv() => {
                    if let Ok((cid, notif)) = msg {
                        if cid == char_id {
                            if notif.event == "OnGodmaShipEffect"
                                && notif.args.first().and_then(|v| v.as_i64()) == Some(mod_id)
                                && notif.args.get(2) == Some(&eve_proto::PyRep::Bool(false))
                            {
                                timeline_bg.lock().unwrap().push("OnGodmaShipEffect(false)".to_string());
                            } else if notif.event == "OnSpecialFX"
                                && notif.args.get(1).and_then(|v| v.as_i64()) == Some(mod_id)
                                && notif.args.get(7).and_then(|v| v.as_i64()) == Some(0)
                            {
                                timeline_bg.lock().unwrap().push("OnSpecialFX(start=0)".to_string());
                            }
                        }
                    }
                }
                _ = &mut done_rx => {
                    while let Ok(bcast) = sys_rx.try_recv() {
                        if bcast
                            .observer_events
                            .get(&ship_id)
                            .is_some_and(|evs| evs.iter().any(|ev| matches!(
                                ev,
                                eve_destiny::DestinyEvent::RemoveBall { ball_id } if *ball_id == wreck_id
                            )))
                        {
                            timeline_bg.lock().unwrap().push("RemoveBall".to_string());
                        }
                    }
                    while let Ok((cid, notif)) = notify_rx.try_recv() {
                        if cid == char_id {
                            if notif.event == "OnGodmaShipEffect"
                                && notif.args.first().and_then(|v| v.as_i64()) == Some(mod_id)
                                && notif.args.get(2) == Some(&eve_proto::PyRep::Bool(false))
                            {
                                timeline_bg.lock().unwrap().push("OnGodmaShipEffect(false)".to_string());
                            } else if notif.event == "OnSpecialFX"
                                && notif.args.get(1).and_then(|v| v.as_i64()) == Some(mod_id)
                                && notif.args.get(7).and_then(|v| v.as_i64()) == Some(0)
                            {
                                timeline_bg.lock().unwrap().push("OnSpecialFX(start=0)".to_string());
                            }
                        }
                    }
                    break;
                }
            }
        }
    });

    crate::services::inventory_fitting::execute_pending_cycle_actions(&ctx, sys_id).await;
    let _ = done_tx.send(());
    recorder.await.unwrap();

    let recorded = timeline.lock().unwrap().clone();
    let remove_pos = recorded
        .iter()
        .position(|e| e == "RemoveBall")
        .expect("RemoveBall must be broadcast");
    let godma_positions: Vec<usize> = recorded
        .iter()
        .enumerate()
        .filter_map(|(i, e)| (e == "OnGodmaShipEffect(false)").then_some(i))
        .collect();
    let sfx_positions: Vec<usize> = recorded
        .iter()
        .enumerate()
        .filter_map(|(i, e)| (e == "OnSpecialFX(start=0)").then_some(i))
        .collect();
    assert_eq!(godma_positions.len(), 1, "must emit exactly one OnGodmaShipEffect(false): {recorded:?}");
    assert_eq!(sfx_positions.len(), 1, "must emit exactly one OnSpecialFX(start=0): {recorded:?}");
    assert!(
        remove_pos < godma_positions[0] && remove_pos < sfx_positions[0],
        "RemoveBall must flush BEFORE OnGodmaShipEffect(false) and OnSpecialFX(start=0): {recorded:?}"
    );

    // If a second repeating SalvageEnd ticks after the wreck and cycle were already removed,
    // it must NOT emit a duplicate OnGodmaShipEffect(false).
    let _ = ctx.drain_notifications_for_char(char_id);
    {
        let mut st = sys_handle.actor_state.write().unwrap();
        st.pending_cycle_actions
            .push(crate::system_actor::PendingCycleAction::SalvageEnd {
                char_id,
                ship_id,
                module_id: mod_second,
                module_type_id: mod_tid,
                wreck_id,
                repeat: 1000,
                effect_name: "salvaging".to_string(),
                next_cycle_cap_deducted: false,
                deducted_cap_gj: 0.0,
            });
    }
    crate::services::inventory_fitting::execute_pending_cycle_actions(&ctx, sys_id).await;
    let dup_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        !dup_notifs.iter().any(|n| n.event == "OnGodmaShipEffect" || n.event == "OnSpecialFX"),
        "already-removed cycle must not emit a duplicate OnGodmaShipEffect/OnSpecialFX: {dup_notifs:?}"
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.module_active_states.contains(&mod_id));
        assert!(!snap.module_active_states.contains(&mod_second));
    });
}
