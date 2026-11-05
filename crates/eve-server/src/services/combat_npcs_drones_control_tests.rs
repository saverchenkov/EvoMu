use super::combat_npcs_drones_tests::{make_drone_test_ctx, make_fighter_item, py_dict_get};
use super::*;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

async fn spawn_test_ball(ctx: &ServerContext, sys_id: u32, id: i64, type_id: u32, pos: DVec3, hp: f64) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id,
                type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: hp * 0.5,
                max_shield_hp: hp,
                armor_hp: hp * 0.5,
                max_armor_hp: hp,
                structure_hp: hp,
                max_structure_hp: hp,
                capacitor: 500.0,
                max_capacitor: 500.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_engage_combat_logi_ewar_and_step_engaged_drones() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let friend_id = 300_000_101i64;
    let enemy_id = 300_000_102i64;
    let far_id = 300_000_103i64;

    spawn_test_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO, 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, friend_id, 587, DVec3::new(3_000.0, 0.0, 0.0), 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, enemy_id, 587, DVec3::new(4_000.0, 0.0, 0.0), 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, far_id, 587, DVec3::new(90_000.0, 0.0, 0.0), 2_000.0).await;

    // Give enemy a lock so ECM clears it
    sys_handle
        .add_target_lock(
            enemy_id,
            eve_destiny::TargetLock {
                target_id: ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    // Seed drones in bay: combat (700_010), logi (700_011), ECM (700_012), Web/Neut/TP (700_013)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(700_010, make_fighter_item(700_010, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_011, make_fighter_item(700_011, 3_186, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_012, make_fighter_item(700_012, 23_707, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_013, make_fighter_item(700_013, 23_711, char_id, ship_id, InventoryFlag::DroneBay, 1));
    });

    let res = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![
            PyRep::Integer(700_010),
            PyRep::Integer(700_011),
            PyRep::Integer(700_012),
            PyRep::Integer(700_013),
        ])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(bids) = res else { panic!("expected launched list") };
    let combat_bid = bids[0].as_i64().unwrap();
    let logi_bid = bids[1].as_i64().unwrap();
    let ecm_bid = bids[2].as_i64().unwrap();
    let web_bid = bids[3].as_i64().unwrap();

    // 1. Out of control range target -> OutOfDroneControlRange
    let err_range = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[PyRep::Integer(far_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_range.code, "OutOfDroneControlRange");

    // 2. Engage logi drone on damaged friend -> repairs shield + armor
    let before_sh = sys_handle.get_combat_state(friend_id).await.unwrap().unwrap().shield_hp;
    let rep_res = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(logi_bid)]), PyRep::Integer(friend_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    let repaired = py_dict_get(&rep_res, "totalRepairedHP").and_then(|v| v.as_f64()).unwrap();
    assert!(repaired > 40.0);
    assert!(sys_handle.get_combat_state(friend_id).await.unwrap().unwrap().shield_hp > before_sh);

    // 3. Engage combat + EWAR drones on enemy -> deals damage, clears locks, drains cap, blooms sig, lowers speed
    let atk_res = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdEngage",
        &[
            PyRep::Integer(enemy_id),
            PyRep::List(vec![PyRep::Integer(combat_bid), PyRep::Integer(ecm_bid), PyRep::Integer(web_bid)]),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(py_dict_get(&atk_res, "totalDamageDealt").and_then(|v| v.as_f64()).unwrap() > 10.0);
    assert_eq!(py_dict_get(&atk_res, "ewarApplied").and_then(|v| v.as_bool()), Some(true));
    assert!(sys_handle.get_target_locks(enemy_id).await.unwrap().is_empty());
    let enemy_cs = sys_handle.get_combat_state(enemy_id).await.unwrap().unwrap();
    assert!(enemy_cs.capacitor < 500.0 && enemy_cs.signature_radius > 40.0);
    assert!(sys_handle.get_ball_snapshot(enemy_id).await.unwrap().unwrap().1.speed_fraction < 1.0);

    // 4. Advance time & step_engaged_drones -> fires next cycle & updates drone_next_ready_unix
    let future_ts = ctx.now_unix() + 100;
    let sh_before_tick = sys_handle.get_combat_state(enemy_id).await.unwrap().unwrap().shield_hp;
    step_engaged_drones(&ctx, future_ts).await;
    assert!(sys_handle.get_combat_state(enemy_id).await.unwrap().unwrap().shield_hp < sh_before_tick);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_assist_guard_auto_aggression_return_scoop_damage_and_salvage() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_a = 300_000_201i64;
    let target_b = 300_000_202i64;
    let target_c = 300_000_203i64;

    spawn_test_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO, 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, target_a, 587, DVec3::new(2_500.0, 0.0, 0.0), 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, target_b, 587, DVec3::new(3_500.0, 0.0, 0.0), 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, target_c, 587, DVec3::new(4_500.0, 0.0, 0.0), 2_000.0).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(700_020, make_fighter_item(700_020, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(700_021, make_fighter_item(700_021, 32_787, char_id, ship_id, InventoryFlag::DroneBay, 1));
    });
    let PyRep::List(bids) = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(700_020), PyRep::Integer(700_021)])],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!("expected list")
    };
    let combat_bid = bids[0].as_i64().unwrap();
    let salvage_bid = bids[1].as_i64().unwrap();

    // 1. ChangeDroneSettings / SetAggressive
    let set_res = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "ChangeDroneSettings",
        &[PyRep::Bool(true), PyRep::Bool(true)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_dict_get(&set_res, "aggressive").and_then(|v| v.as_bool()), Some(true));

    // 2. CmdAssist & step_drone_auto_aggression_and_assist
    let fleetmate = 90_000_777i64;
    let assist_res = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdAssist",
        &[PyRep::Integer(fleetmate)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_dict_get(&assist_res, "droneCount").and_then(|v| v.as_i64()), Some(2));
    let engaged = step_drone_auto_aggression_and_assist(
        &ctx,
        sys_id,
        char_id,
        None,
        Some((fleetmate, target_a)),
        None,
    )
    .await;
    assert_eq!(engaged.len(), 2);

    // 3. CmdGuard & step_drone_auto_aggression_and_assist (guard branch)
    let _ = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdGuard",
        &[PyRep::Integer(fleetmate)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    let guarded = step_drone_auto_aggression_and_assist(
        &ctx,
        sys_id,
        char_id,
        None,
        None,
        Some((fleetmate, target_b)),
    )
    .await;
    assert_eq!(guarded.len(), 2);

    // 4. Aggressive self-defense branch when assist/guard clear
    with_mut_combat_runtime(&ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            d.assist_char_id = None;
            d.guard_char_id = None;
        }
    });
    let aggro = step_drone_auto_aggression_and_assist(&ctx, sys_id, char_id, Some(target_c), None, None).await;
    assert_eq!(aggro.len(), 2);

    // 5. CmdReturnHome orbits drones back around ship
    let ret = handle_npcs_drones(&ctx, &mut session, "entity", "CmdReturnHome", &[], &kwargs)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(ret.as_i64(), Some(2));

    // 6. ScoopToDroneBay scoops combat_bid back into ship DroneBay (700_020)
    let scoop = handle_npcs_drones(
        &ctx,
        &mut session,
        "ship",
        "ScoopToDroneBay",
        &[PyRep::Integer(combat_bid)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_dict_get(&scoop, "scoopedDroneItemID").and_then(|v| v.as_i64()), Some(700_020));

    // 7. apply_damage_to_drone_in_space destroys remaining salvage drone ball + item
    let (applied, destroyed) =
        apply_damage_to_drone_in_space(&ctx, sys_id, salvage_bid, DamageVector::new(0.0, 5_000.0, 0.0, 0.0)).await;
    assert!(applied > 0.0 && destroyed);
    assert!(sys_handle.get_ball_snapshot(salvage_bid).await.unwrap().is_none());

    // CmdSalvage with no active salvage drones -> NoActiveSalvageDrones
    let err_salv = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "CmdSalvage",
        &[PyRep::Integer(999_001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_salv.code, "NoActiveSalvageDrones");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tractor_beam_and_salvager_ownership_range_and_pull() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let attacker_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(700_030, make_fighter_item(700_030, 1_222, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    let fitted_mod = eve_dogma::FittedModule {
        item_id: 700_030,
        type_id: 1_222,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let tractor_t = ctx.sde.types.get(&1_222).unwrap();
    let salvager_t = ctx.sde.types.get(&25_861).unwrap();

    // 1. Missing target -> TargetRequired
    let err = activate_salvager_or_tractor_module(&ctx, char_id, ship_id, sys_id, 700_030, &fitted_mod, tractor_t, &attacker_ball, None, &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "TargetRequired");

    // 2. Wreck owned by stranger (88_000_001) -> TractorBeamOwnershipRestricted
    let stranger = 88_000_001i64;
    let wreck_id = 550_000_001i64;
    spawn_test_ball(&ctx, sys_id, wreck_id, 26_470, DVec3::new(10_000.0, 0.0, 0.0), 500.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_id, make_fighter_item(wreck_id, 26_470, stranger, sys_id as i64, InventoryFlag::None, 1));
    });
    let err_own = activate_salvager_or_tractor_module(&ctx, char_id, ship_id, sys_id, 700_030, &fitted_mod, tractor_t, &attacker_ball, Some(wreck_id), &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_own.code, "TractorBeamOwnershipRestricted");

    // 3. Salvager out of 6km range -> TargetOutOfRange
    let err_rng = activate_salvager_or_tractor_module(&ctx, char_id, ship_id, sys_id, 700_031, &fitted_mod, salvager_t, &attacker_ball, Some(wreck_id), &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_rng.code, "TargetOutOfRange");

    // 4. Mark wreck abandoned -> tractor beam pulls wreck closer!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.container_lifetimes.insert(
            wreck_id,
            ContainerLifetimeEntry {
                item_id: wreck_id,
                solar_system_id: sys_id,
                type_id: 26_470,
                owner_id: stranger,
                created_unix: ctx.now_unix(),
                lifetime_secs: 7_200,
                is_abandoned: true,
            },
        );
    });
    let pull_res = activate_salvager_or_tractor_module(&ctx, char_id, ship_id, sys_id, 700_030, &fitted_mod, tractor_t, &attacker_ball, Some(wreck_id), &kwargs)
        .await
        .unwrap()
        .unwrap();
    let new_dist = py_dict_get(&pull_res, "newDistanceMeters").and_then(|v| v.as_f64()).unwrap();
    assert!(new_dist < 10_000.0, "expected wreck pulled closer than 10km, got {new_dist}");

    // 5. Salvager in range succeeds
    let _ = ctx
        .systems
        .get_or_spawn(sys_id)
        .set_ball_position(wreck_id, DVec3::new(1_500.0, 0.0, 0.0))
        .await;
    let salv_ok = activate_salvager_or_tractor_module(&ctx, char_id, ship_id, sys_id, 700_031, &fitted_mod, salvager_t, &attacker_ball, Some(wreck_id), &kwargs)
        .await
        .unwrap();
    assert!(salv_ok.is_ok());
}
