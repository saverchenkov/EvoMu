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

    // Give ship_id locks on friend_id and enemy_id, and give enemy_id a lock on ship_id so ECM clears it
    for tid in [friend_id, enemy_id] {
        sys_handle
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: tid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .unwrap();
    }
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

    // 1. ChangeDroneSettings / SetAggressive (bool + float 0.0/1.0 + reject non-0/1 float)
    let set_float = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "ChangeDroneSettings",
        &[PyRep::Real(0.0), PyRep::Real(1.0)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_dict_get(&set_float, "aggressive").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(py_dict_get(&set_float, "focusFire").and_then(|v| v.as_bool()), Some(true));

    let mut set_kw = HashMap::new();
    set_kw.insert("aggressive".into(), PyRep::Real(1.0));
    set_kw.insert("focusFire".into(), PyRep::Real(0.0));
    let set_agg_kw = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "SetAggressive",
        &[],
        &set_kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_dict_get(&set_agg_kw, "aggressive").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(py_dict_get(&set_agg_kw, "focusFire").and_then(|v| v.as_bool()), Some(false));

    let bad_float = handle_npcs_drones(
        &ctx,
        &mut session,
        "entity",
        "ChangeDroneSettings",
        &[PyRep::Real(0.5), PyRep::Real(1.0)],
        &kwargs,
    )
    .await
    .unwrap();
    assert_eq!(bad_float.unwrap_err().code, "InvalidArgument");

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn toctou_concurrent_launch_and_scoop_drones() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_test_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO, 2_000.0).await;

    // Limit pilot to 1 active drone (`Drones` skill 3436 = level 1) and stock 2 drones in DroneBay
    let d1 = 300_800_001i64;
    let d2 = 300_800_002i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
            ch.active_ship_id = ship_id;
        }
        snap.skills.entry(char_id).or_default().insert(
            3436,
            eve_db::TrainedSkillRecord { type_id: 3436, level: 1, skill_points: 250.0 },
        );
        for &did in &[d1, d2] {
            snap.items.insert(
                did,
                eve_db::InventoryItemRecord {
                    item_id: did,
                    type_id: 2454, // Hobgoblin I
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::DroneBay,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let mut s1 = session.clone();
    let mut s2 = session.clone();
    let l_args1 = [PyRep::List(vec![PyRep::Integer(d1)])];
    let l_args2 = [PyRep::List(vec![PyRep::Integer(d2)])];
    let (r1, r2) = tokio::join!(
        handle_npcs_drones(&ctx, &mut s1, "ship", "LaunchDrones", &l_args1, &kwargs),
        handle_npcs_drones(&ctx, &mut s2, "ship", "LaunchDrones", &l_args2, &kwargs),
    );
    let ok1 = r1.as_ref().is_some_and(|r| r.is_ok());
    let ok2 = r2.as_ref().is_some_and(|r| r.is_ok());
    assert!(ok1 ^ ok2, "expected exactly 1 LaunchDrones to succeed with Drones=1, got r1={r1:?} r2={r2:?}");
    let launched_count = with_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.values().filter(|d| d.parent_ship_id == ship_id && !d.is_abandoned).count()
    });
    assert_eq!(launched_count, 1);

    // Abandon the launched drone and race two nearby ships calling ScoopToDroneBay
    let launched_ball_id = with_combat_runtime(&ctx, |rt| *rt.active_drones_in_space.keys().next().unwrap());
    with_mut_combat_runtime(&ctx, |rt| {
        if let Some(d) = rt.active_drones_in_space.get_mut(&launched_ball_id) {
            d.is_abandoned = true;
        }
    });
    let char2 = char_id + 1;
    let ship2 = ship_id + 1;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch2 = snap.characters[&char_id].clone();
        ch2.character_id = char2;
        ch2.active_ship_id = ship2;
        snap.characters.insert(char2, ch2);
        let mut sh2 = snap.items[&ship_id].clone();
        sh2.item_id = ship2;
        sh2.owner_id = char2;
        snap.items.insert(ship2, sh2);
    });
    spawn_test_ball(&ctx, sys_id, ship2, 587, DVec3::new(100.0, 0.0, 0.0), 2_000.0).await;
    let mut scoop1 = session.clone();
    let mut scoop2 = SessionState::new(99_002);
    scoop2.user_id = Some(char2 as u64);
    scoop2.character_id = Some(char2);
    scoop2.corporation_id = session.corporation_id;
    scoop2.solar_system_id = Some(sys_id);
    scoop2.ship_id = Some(ship2);

    let sc_args1 = [PyRep::Integer(launched_ball_id)];
    let sc_args2 = [PyRep::Integer(launched_ball_id)];
    let (sc1, sc2) = tokio::join!(
        handle_npcs_drones(&ctx, &mut scoop1, "ship", "ScoopToDroneBay", &sc_args1, &kwargs),
        handle_npcs_drones(&ctx, &mut scoop2, "ship", "ScoopToDroneBay", &sc_args2, &kwargs),
    );
    let sok1 = sc1.as_ref().is_some_and(|r| r.is_ok());
    let sok2 = sc2.as_ref().is_some_and(|r| r.is_ok());
    assert!(sok1 ^ sok2, "expected exactly 1 ScoopToDroneBay winner, got sc1={sc1:?} sc2={sc2:?}");
    let _ = sys_handle;
    let _ = &mut session;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drone_math_tractor_modifiers_launch_meta_engage_and_scoop_boundary_mutants() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let kwargs = HashMap::new();
    let mut sde = (*ctx.sde).clone();

    // Equip ship hull 587 with slots + non-20km base control range hull 99_587 (attr 458 = 30_000)
    if let Some(hull) = sde.types.get_mut(&587) {
        hull.default_attributes.insert(eve_sde::attr_ids::HI_SLOTS, 8.0);
        hull.default_attributes.insert(eve_sde::attr_ids::MED_SLOTS, 8.0);
        hull.default_attributes.insert(eve_sde::attr_ids::LOW_SLOTS, 8.0);
        hull.default_attributes.insert(eve_sde::attr_ids::RIG_SLOTS, 3.0);
        hull.default_attributes.insert(eve_sde::attr_ids::CPU_OUTPUT, 500.0);
        hull.default_attributes.insert(eve_sde::attr_ids::POWER_OUTPUT, 500.0);
        hull.default_attributes.insert(eve_sde::attr_ids::UPGRADE_CAPACITY, 400.0);
        hull.default_attributes.insert(eve_sde::attr_ids::RIG_SIZE, 1.0);
    }
    let mut custom_hull = sde.types[&587].clone();
    custom_hull.type_id = 99_587;
    custom_hull.default_attributes.insert(458, 30_000.0);
    custom_hull.default_attributes.insert(1271, 50.0);
    custom_hull.default_attributes.insert(283, 100.0);
    sde.types.insert(99_587, custom_hull);

    // Non-empty drone name on 2_456 + primary skill 182 = 3436, plus no-base-160 drone 99_201 & empty-name drone 99_202
    if let Some(dt) = sde.types.get_mut(&2_456) {
        dt.name = "Hobgoblin II".into();
        dt.default_attributes.insert(160, 2.0);
        dt.default_attributes.insert(182, 3436.0);
    }
    let mut mod_only_drone = sde.types[&2_456].clone();
    mod_only_drone.type_id = 99_201;
    mod_only_drone.name = "ModOnly Drone".into();
    mod_only_drone.default_attributes.remove(&160);
    mod_only_drone.default_attributes.insert(182, 3436.0);
    sde.types.insert(99_201, mod_only_drone);

    let mut unnamed_drone = sde.types[&2_456].clone();
    unnamed_drone.type_id = 99_202;
    unnamed_drone.name = String::new();
    sde.types.insert(99_202, unnamed_drone);

    // High-TP drone (>100% pct clamped at 100% -> max multiplier 2.0)
    let mut hi_tp_drone = sde.types[&23_711].clone();
    hi_tp_drone.type_id = 99_203;
    hi_tp_drone.default_attributes.insert(554, 150.0);
    hi_tp_drone.default_attributes.insert(20, 0.0);
    hi_tp_drone.default_attributes.insert(97, 0.0);
    sde.types.insert(99_203, hi_tp_drone);

    // Effect + skills for Drone Avionics (3437) and real ADA (23566)
    sde.effects.insert(
        448,
        eve_sde::DogmaEffectType {
            effect_id: 448,
            name: "skillRangeInit".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "itemID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 459,
                modifying_attribute_id: 280,
                operator: 0,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    sde.effects.insert(
        504,
        eve_sde::DogmaEffectType {
            effect_id: 504,
            name: "skillRangeAddChar".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "charID".into(),
                func: "ItemModifier".into(),
                modified_attribute_id: 458,
                modifying_attribute_id: 459,
                operator: 2,
                group_id: None,
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    let mut da_skill = sde.types[&2_456].clone();
    da_skill.type_id = 3_437;
    da_skill.group_id = 273;
    da_skill.category_id = 16;
    da_skill.default_attributes = [(459, 5_000.0)].into_iter().collect();
    da_skill.effect_ids = vec![448, 504];
    sde.types.insert(3_437, da_skill.clone());
    let mut ada_skill = da_skill;
    ada_skill.type_id = 23_566;
    ada_skill.default_attributes = [(459, 3_000.0)].into_iter().collect();
    sde.types.insert(23_566, ada_skill);

    // Omni module effect adding +0.75 (operator 2) to attr 160 AND +50% (operator 6) to attr 54 for drones requiring 3436
    sde.effects.insert(
        64_301,
        eve_sde::DogmaEffectType {
            effect_id: 64_301,
            name: "omniTrackingAndOptimalMod".into(),
            effect_category: 0,
            modifiers: vec![
                eve_sde::EffectModifierSpec {
                    domain: "charID".into(),
                    func: "OwnerRequiredSkillModifier".into(),
                    modified_attribute_id: 160,
                    modifying_attribute_id: 767,
                    operator: 2,
                    group_id: None,
                    skill_type_id: Some(3436),
                    effect_id: None,
                },
                eve_sde::EffectModifierSpec {
                    domain: "charID".into(),
                    func: "OwnerRequiredSkillModifier".into(),
                    modified_attribute_id: 54,
                    modifying_attribute_id: 349,
                    operator: 6,
                    group_id: None,
                    skill_type_id: Some(3436),
                    effect_id: None,
                },
            ],
            ..Default::default()
        },
    );
    let mut omni_mod = sde.types[&1_222].clone();
    omni_mod.type_id = 99_401;
    omni_mod.group_id = 646;
    omni_mod.default_attributes = [(30, 1.0), (50, 1.0), (767, 0.75), (349, 50.0)].into_iter().collect();
    omni_mod.effect_ids = vec![11, 16, 64_301];
    sde.types.insert(99_401, omni_mod);

    // Tractor variants & rig modifying attr 1045 (velocity) with small base_1045 = 250 (< 1000) and zero-base 1045
    sde.effects.insert(
        64_302,
        eve_sde::DogmaEffectType {
            effect_id: 64_302,
            name: "tractorVelRigBonus".into(),
            effect_category: 0,
            modifiers: vec![eve_sde::EffectModifierSpec {
                domain: "shipID".into(),
                func: "LocationGroupModifier".into(),
                modified_attribute_id: 1045,
                modifying_attribute_id: 1670,
                operator: 6,
                group_id: Some(650),
                skill_type_id: None,
                effect_id: None,
            }],
            ..Default::default()
        },
    );
    let mut tractor_rig = sde.types[&1_222].clone();
    tractor_rig.type_id = 99_402;
    tractor_rig.group_id = 782;
    tractor_rig.default_attributes = [(1153, 50.0), (1670, 100.0), (1547, 1.0)].into_iter().collect();
    tractor_rig.effect_ids = vec![2663, 64_302];
    sde.types.insert(99_402, tractor_rig);

    // Tractor A: 54=0 (fallback 20_000), 624=0 (ignored), 73=4_000 (4s), 1045=250 -> with +100% rig => eff_1045=500, base_1045=250 => 1_000*(500/250)=2_000 m/s * 4s = 8_000m pull
    let mut tractor_a = sde.types[&1_222].clone();
    tractor_a.type_id = 99_410;
    tractor_a.default_attributes = [(30, 1.0), (50, 1.0), (54, 0.0), (624, 0.0), (73, 4_000.0), (1045, 250.0)]
        .into_iter()
        .collect();
    tractor_a.effect_ids = vec![12, 16];
    sde.types.insert(99_410, tractor_a);

    // Tractor B: 54=0, 624=0, no 1045 modifier, 73=4_000 -> pull speed fallback 1_000 m/s * 4s = 4_000m
    let mut tractor_b = sde.types[&1_222].clone();
    tractor_b.type_id = 99_411;
    tractor_b.default_attributes = [(30, 1.0), (50, 1.0), (54, 0.0), (624, 0.0), (73, 4_000.0)]
        .into_iter()
        .collect();
    tractor_b.effect_ids = vec![12, 16];
    sde.types.insert(99_411, tractor_b);

    // Tractor C: 54=15_000, 624=1_250, 73=4_000, NO rig -> uses explicit 624 = 1_250 m/s * 4s = 5_000m
    let mut tractor_c = sde.types[&1_222].clone();
    tractor_c.type_id = 99_412;
    tractor_c.default_attributes = [(30, 1.0), (50, 1.0), (54, 15_000.0), (624, 1_250.0), (73, 4_000.0)]
        .into_iter()
        .collect();
    tractor_c.effect_ids = vec![12, 16];
    sde.types.insert(99_412, tractor_c);

    // Salvager with 54=0 (fallback 5_000m) and 624=1_500 (non-650 never pulls!)
    let mut salv_zero = sde.types[&25_861].clone();
    salv_zero.type_id = 99_413;
    salv_zero.default_attributes = [(30, 1.0), (50, 1.0), (54, 0.0), (624, 1_500.0)].into_iter().collect();
    salv_zero.effect_ids = vec![12, 16];
    sde.types.insert(99_413, salv_zero);

    // Triage module (27951) zeroes bandwidth
    let mut triage = sde.types[&1_222].clone();
    triage.type_id = 27_951;
    triage.group_id = 515;
    triage.default_attributes = [(30, 1.0), (50, 1.0)].into_iter().collect();
    triage.effect_ids = vec![12, 16];
    sde.types.insert(27_951, triage);

    ctx.sde = Arc::new(sde);

    // 1. compute_drone_control_range: legacy_extra only applies when ada_real_lv == 0, AND ship_base != 20_000 shifts by (ship_base - 20_000)
    // make_drone_test_ctx seeded DA IV (3437=4 -> +20km) & legacy ADA II (23618=2 -> +6km), ada_real=0
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 46_000.0).abs() < 1e-6);

    // Switch hull to 99_587 (ship_base 30_000) AND fit omni: base 30km + DA 20km + legacy ADA 6km = 56km
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 99_587;
        snap.items.insert(810_001, make_fighter_item(810_001, 99_401, char_id, ship_id, InventoryFlag::LoSlot0, 1));
        snap.module_online_states.insert(810_001, true);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = 98_000_123;
            ch.alliance_id = Some(99_000_456);
        }
    });
    session.corporation_id = Some(98_000_123);
    session.alliance_id = Some(99_000_456);
    ctx.invalidate_ship_fitting(ship_id);
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 56_000.0).abs() < 1e-6);

    // Now seed real ADA II (23566=2) ALONGSIDE legacy ADA (23618=2): legacy MUST NOT double-count (remains 56_000, not 62_000!)
    super::combat_npcs_drones_tests::seed_pilot_skill(&ctx, char_id, 23_566, 2);
    ctx.invalidate_ship_fitting(ship_id);
    assert!((compute_drone_control_range(&ctx, char_id, ship_id) - 56_000.0).abs() < 1e-6);

    // Fallback (no ship item -> Err fitting): base 20_000 + DA 20_000 + real ADA 6_000 = 46_000
    assert!((compute_drone_control_range(&ctx, char_id, 999_888_777) - 46_000.0).abs() < 1e-6);

    // 2. evaluated_drone_attribute: modifiers.any on absent base attr 160, parent_ship_id<=0, owner_char_id<=0, dyn_override scaling, optimal modifier
    let drone_mod_id = 810_101i64;
    let drone_std_id = 810_102i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(drone_mod_id, make_fighter_item(drone_mod_id, 99_201, char_id, ship_id, InventoryFlag::DroneBay, 1));
        let mut d_std = make_fighter_item(drone_std_id, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 1);
        d_std.dynamic_attributes.insert(160, 4.0);
        snap.items.insert(drone_std_id, d_std);
    });
    let base_ds = DroneSpaceState {
        ball_id: 910_501,
        drone_item_id: drone_mod_id,
        type_id: 99_201,
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
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 2_600.0,
        optimal_range: 2_000.0,
        damage_vector: DamageVector::new(0.0, 20.0, 0.0, 0.0),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    };
    // 99_201 lacks 160 in base_attributes, so ONLY modifiers.iter().any(|m| m.target_attribute_id == 160) returns 0.75!
    assert!((base_ds.effective_tracking_speed(&ctx) - 0.75).abs() < 1e-9);
    // optimal_range == sb (2_000) -> uses +50% optimal mod => 3_000.0; custom optimal 2_001 != sb => keeps 2_001.0
    assert!((base_ds.effective_optimal_range(&ctx) - 3_000.0).abs() < 1e-9);
    let mut custom_opt = base_ds.clone();
    custom_opt.optimal_range = 2_001.0;
    assert!((custom_opt.effective_optimal_range(&ctx) - 2_001.0).abs() < 1e-9);
    // parent_ship_id <= 0 or owner_char_id <= 0 skip fitting
    let mut no_parent = base_ds.clone();
    no_parent.parent_ship_id = 0;
    assert_eq!(no_parent.evaluated_drone_attribute(&ctx, 160), None);
    let mut no_owner = base_ds.clone();
    no_owner.owner_char_id = 0;
    assert_eq!(no_owner.evaluated_drone_attribute(&ctx, 160), None);
    // dyn_override base tracking 4.0 + flat Dogma ModAdd 0.75 = 4.75
    let mut scaled_ds = base_ds.clone();
    scaled_ds.drone_item_id = drone_std_id;
    scaled_ds.type_id = 2_456;
    assert!((scaled_ds.effective_tracking_speed(&ctx) - 4.75).abs() < 1e-9);

    // 3. Tractor velocity 1045 (base_1045=250 < 1_000), fallback 1_000, 624=1_250, Salvager 54=0 fallback 5_000m (never pulls)
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let attacker_ball = Ball {
        id: ship_id,
        type_id: 99_587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let wreck_t_id = 810_201i64;
    let wreck_zero_id = 810_202i64;
    spawn_test_ball(&ctx, sys_id, ship_id, 99_587, DVec3::ZERO, 2_000.0).await;
    spawn_test_ball(&ctx, sys_id, wreck_t_id, 26_470, DVec3::new(15_000.0, 0.0, 0.0), 500.0).await;
    spawn_test_ball(&ctx, sys_id, wreck_zero_id, 26_470, DVec3::ZERO, 500.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(wreck_t_id, make_fighter_item(wreck_t_id, 26_470, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(wreck_zero_id, make_fighter_item(wreck_zero_id, 26_470, char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(810_010, make_fighter_item(810_010, 99_410, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(810_011, make_fighter_item(810_011, 99_411, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(810_012, make_fighter_item(810_012, 99_412, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(810_013, make_fighter_item(810_013, 99_402, char_id, ship_id, InventoryFlag::RigSlot0, 1));
        for mid in [810_010, 810_011, 810_012, 810_013] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    let fit_with_rig = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();

    // Tractor A with rig: 1_000 * (500/250) * 4s = 8_000m pull -> 15_000 - 8_000 = 7_000m
    let tr_a = ctx.sde.types[&99_410].clone();
    let res_a = activate_salvager_or_tractor_module(
        &ctx, char_id, ship_id, sys_id, 810_010, &fit_with_rig.modules[&810_010], &tr_a, &attacker_ball, Some(wreck_t_id), &kwargs,
    ).await.unwrap().unwrap();
    assert!((py_dict_get(&res_a, "newDistanceMeters").and_then(|v| v.as_f64()).unwrap() - 7_000.0).abs() < 1e-3);

    // Zero-distance wreck does not divide by 0 / produce NaN
    let res_zero = activate_salvager_or_tractor_module(
        &ctx, char_id, ship_id, sys_id, 810_010, &fit_with_rig.modules[&810_010], &tr_a, &attacker_ball, Some(wreck_zero_id), &kwargs,
    ).await.unwrap().unwrap();
    assert!(py_dict_get(&res_zero, "newDistanceMeters").and_then(|v| v.as_f64()).unwrap().is_finite());

    // Without rig & from 15_000m: Tractor B falls back to 1_000 m/s * 4s = 4_000m -> 11_000m
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&810_013);
        snap.module_online_states.remove(&810_013);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let fit_no_rig = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let _ = sys_handle.set_ball_position(wreck_t_id, DVec3::new(15_000.0, 0.0, 0.0)).await;
    let tr_b = ctx.sde.types[&99_411].clone();
    let res_b = activate_salvager_or_tractor_module(
        &ctx, char_id, ship_id, sys_id, 810_011, &fit_no_rig.modules[&810_011], &tr_b, &attacker_ball, Some(wreck_t_id), &kwargs,
    ).await.unwrap().unwrap();
    assert!((py_dict_get(&res_b, "newDistanceMeters").and_then(|v| v.as_f64()).unwrap() - 11_000.0).abs() < 1e-3);

    // Tractor C (624 = 1_250 > 0) pulls 1_250 * 4s = 5_000m -> 11_000 - 5_000 = 6_000m
    let tr_c = ctx.sde.types[&99_412].clone();
    let res_c = activate_salvager_or_tractor_module(
        &ctx, char_id, ship_id, sys_id, 810_012, &fit_no_rig.modules[&810_012], &tr_c, &attacker_ball, Some(wreck_t_id), &kwargs,
    ).await.unwrap().unwrap();
    assert!((py_dict_get(&res_c, "newDistanceMeters").and_then(|v| v.as_f64()).unwrap() - 6_000.0).abs() < 1e-3);

    // Salvager with 54=0 fallback 5_000m: rejects at 5_001m, succeeds at 4_999m WITHOUT moving target!
    let salv_t = ctx.sde.types[&99_413].clone();
    let mut salv_fm = fit_no_rig.modules[&810_012].clone();
    salv_fm.type_id = 99_413;
    let _ = sys_handle.set_ball_position(wreck_t_id, DVec3::new(5_001.0, 0.0, 0.0)).await;
    let err_salv = activate_salvager_or_tractor_module(
        &ctx, char_id, ship_id, sys_id, 810_014, &salv_fm, &salv_t, &attacker_ball, Some(wreck_t_id), &kwargs,
    ).await.unwrap().unwrap_err();
    assert_eq!(err_salv.code, "TargetOutOfRange");
    let _ = sys_handle.set_ball_position(wreck_t_id, DVec3::new(4_999.0, 0.0, 0.0)).await;
    let ok_salv = activate_salvager_or_tractor_module(
        &ctx, char_id, ship_id, sys_id, 810_014, &salv_fm, &salv_t, &attacker_ball, Some(wreck_t_id), &kwargs,
    ).await.unwrap().unwrap();
    assert_eq!(py_dict_get(&ok_salv, "wreckID"), Some(&PyRep::Integer(wreck_t_id)));
    assert!(py_dict_get(&ok_salv, "newDistanceMeters").is_none());
    if let Ok(Some((_, wb))) = sys_handle.get_ball_snapshot(wreck_t_id).await {
        assert!((wb.position.x - 4_999.0).abs() < 1e-6);
    }

    // 4. pilot_drone_capacity triage mode zeroes bandwidth, and LaunchDrones checks SlimItemMeta + stack split + dyn HP + Excavator + Scoop borders
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(810_020, make_fighter_item(810_020, 27_951, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.module_online_states.insert(810_020, true);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let mut fit_triage = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    fit_triage.modules.get_mut(&810_020).unwrap().state = eve_dogma::ModuleState::Active;
    let (_, triage_bw, _) = pilot_drone_capacity(&ctx, char_id, ship_id, Some(&fit_triage));
    assert_eq!(triage_bw, 0.0);

    // Remove Drones skill -> NoDroneManagementAbilities, then re-add Drones I (max 1) -> NoDroneManagementAbilitiesLeft when > 1
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.skills.get_mut(&char_id) {
            m.remove(&3_436);
        }
        let mut stack = make_fighter_item(810_301, 2_456, char_id, ship_id, InventoryFlag::DroneBay, 3);
        stack.version = 5;
        stack.dynamic_attributes.insert(9, 450.0);
        stack.dynamic_attributes.insert(263, 320.0);
        stack.dynamic_attributes.insert(265, 210.0);
        snap.items.insert(810_301, stack);
    });
    let err_no_skill = handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(810_301)], &kwargs)
        .await.unwrap().unwrap_err();
    assert_eq!(err_no_skill.code, "NoDroneManagementAbilities");

    super::combat_npcs_drones_tests::seed_pilot_skill(&ctx, char_id, 3_436, 1);
    let err_bw_over = handle_npcs_drones(
        &ctx, &mut session, "ship", "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(810_301), PyRep::Integer(drone_mod_id)])], &kwargs,
    ).await.unwrap().unwrap_err();
    assert_eq!(err_bw_over.code, "NoDroneManagementAbilitiesLeft");

    // Launch 1 from stack (qty=3, ver=5) -> check SlimItemMeta, dynamic CombatState HP/Shield/Armor, stack split version/quantity!
    let ok_launch = handle_npcs_drones(&ctx, &mut session, "ship", "LaunchDrones", &[PyRep::Integer(810_301)], &kwargs)
        .await.unwrap().unwrap();
    let PyRep::List(l_ids) = ok_launch else { panic!("expected list") };
    let launched_bid = l_ids[0].as_i64().unwrap();
    let slim = sys_handle.get_ball_slim_item(launched_bid).await.unwrap().unwrap();
    let PyRep::Object { args: slim_dict, .. } = slim else { panic!("expected SlimItem") };
    assert_eq!(py_dict_get(&slim_dict, "groupID").and_then(|v| v.as_i64()), Some(100));
    assert_eq!(py_dict_get(&slim_dict, "categoryID").and_then(|v| v.as_i64()), Some(18));
    assert_eq!(py_dict_get(&slim_dict, "ownerID").and_then(|v| v.as_i64()), Some(char_id));
    assert_eq!(py_dict_get(&slim_dict, "charID"), Some(&PyRep::None));
    assert_eq!(py_dict_get(&slim_dict, "corpID").and_then(|v| v.as_i64()), Some(98_000_123));
    assert_eq!(py_dict_get(&slim_dict, "allianceID").and_then(|v| v.as_i64()), Some(99_000_456));
    assert_eq!(py_dict_get(&slim_dict, "securityStatus").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(py_dict_get(&slim_dict, "name"), Some(&PyRep::String("Hobgoblin II".into())));
    let launched_cs = sys_handle.get_combat_state(launched_bid).await.unwrap().unwrap();
    assert!((launched_cs.structure_hp - 450.0).abs() < 1e-6);
    assert!((launched_cs.shield_hp - 320.0).abs() < 1e-6);
    assert!((launched_cs.armor_hp - 210.0).abs() < 1e-6);
    let split_item_id = with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&launched_bid].drone_item_id);
    ctx.db.with_snapshot(|snap| {
        let orig = &snap.items[&810_301];
        assert_eq!(orig.quantity, 2);
        assert_eq!(orig.version, 6);
        let split = &snap.items[&split_item_id];
        assert_eq!(split.quantity, 1);
        assert!(split.singleton);
        assert_eq!(split.version, 1);
        assert_eq!(split.location_id, sys_id as i64);
        assert_eq!(split.flag, InventoryFlag::None);
    });

    // CmdReturnHome boundary: target at exact max_ctrl_range (56_000) vs 56_001
    let _ = sys_handle.set_ball_position(launched_bid, DVec3::new(56_001.0, 0.0, 0.0)).await;
    let ret_far = handle_npcs_drones(&ctx, &mut session, "entity", "CmdReturnHome", &[], &kwargs).await.unwrap().unwrap();
    assert_eq!(ret_far.as_i64(), Some(0));
    let _ = sys_handle.set_ball_position(launched_bid, DVec3::new(56_000.0, 0.0, 0.0)).await;
    let ret_edge = handle_npcs_drones(&ctx, &mut session, "entity", "CmdReturnHome", &[], &kwargs).await.unwrap().unwrap();
    assert_eq!(ret_edge.as_i64(), Some(1));

    // ScoopToDroneBay errors + ownership + range (2_500 vs 2_501) + rollback when bay full + abandoned other owner
    let _ = sys_handle.set_ball_position(launched_bid, DVec3::new(2_500.1, 0.0, 0.0)).await;
    let scoop_far = handle_npcs_drones(&ctx, &mut session, "ship", "ScoopToDroneBay", &[PyRep::Integer(launched_bid)], &kwargs)
        .await.unwrap().unwrap_err();
    assert_eq!(scoop_far.code, "OutOfScoopRange");

    // Owned by other (not abandoned) -> DroneOwnedByOther and NOT removed from active_drones_in_space
    let _ = sys_handle.set_ball_position(launched_bid, DVec3::new(2_500.0, 0.0, 0.0)).await;
    with_mut_combat_runtime(&ctx, |rt| {
        let d = rt.active_drones_in_space.get_mut(&launched_bid).unwrap();
        d.owner_char_id = char_id + 99;
        d.is_abandoned = false;
    });
    let err_other = handle_npcs_drones(&ctx, &mut session, "ship", "ScoopToDroneBay", &[PyRep::Integer(launched_bid)], &kwargs)
        .await.unwrap().unwrap_err();
    assert_eq!(err_other.code, "DroneOwnedByOther");
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.contains_key(&launched_bid)));

    // Abandoned owned by other, but bay capacity exceeded -> InsufficientDroneBayCapacity AND active_drones_in_space rolled back!
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.get_mut(&launched_bid).unwrap().is_abandoned = true;
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(283, 1.0);
    });
    let err_cap = handle_npcs_drones(&ctx, &mut session, "ship", "ScoopToDroneBay", &[PyRep::Integer(launched_bid)], &kwargs)
        .await.unwrap().unwrap_err();
    assert_eq!(err_cap.code, "InsufficientDroneBayCapacity");
    assert!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space.contains_key(&launched_bid)));

    // Restore bay cap (remove dynamic 283) -> scoop succeeds, owner=char_id, location=ship_id, flag=DroneBay, version=2!
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.remove(&283);
    });
    let ok_scoop = handle_npcs_drones(&ctx, &mut session, "ship", "ScoopToDroneBay", &[PyRep::Integer(launched_bid)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(py_dict_get(&ok_scoop, "scoopedDroneItemID").and_then(|v| v.as_i64()), Some(split_item_id));
    assert_eq!(py_dict_get(&ok_scoop, "newOwnerID").and_then(|v| v.as_i64()), Some(char_id));
    ctx.db.with_snapshot(|snap| {
        let it = &snap.items[&split_item_id];
        assert_eq!(it.owner_id, char_id);
        assert_eq!(it.location_id, ship_id);
        assert_eq!(it.flag, InventoryFlag::DroneBay);
        assert_eq!(it.version, 2);
        assert_eq!(snap.sp6_lifecycle.space_object_meta.get(&split_item_id).and_then(|m| m.drone_ball_id), None);
    });

    // Empty-name drone fallback name + Excavator on Rorqual (mass 10_000_000, inertia 2.8, radius 50) + TP drone clamping at 100% -> tp_sig_multiplier == 2.0
    super::combat_npcs_drones_tests::seed_pilot_skill(&ctx, char_id, 3_436, 5);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().type_id = 28_352; // Rorqual: 125 bw, excavator attr 3223
        snap.items.insert(810_401, make_fighter_item(810_401, 41_030, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(810_402, make_fighter_item(810_402, 99_202, char_id, ship_id, InventoryFlag::DroneBay, 1));
        snap.items.insert(810_403, make_fighter_item(810_403, 99_203, char_id, ship_id, InventoryFlag::DroneBay, 1));
    });
    ctx.invalidate_ship_fitting(ship_id);
    let PyRep::List(ex_bids) = handle_npcs_drones(
        &ctx, &mut session, "ship", "LaunchDrones",
        &[PyRep::List(vec![PyRep::Integer(810_401), PyRep::Integer(810_402), PyRep::Integer(810_403)])],
        &kwargs,
    ).await.unwrap().unwrap() else { panic!("expected launch list") };
    let exc_bid = ex_bids[0].as_i64().unwrap();
    let unnamed_bid = ex_bids[1].as_i64().unwrap();
    let tp_bid = ex_bids[2].as_i64().unwrap();
    let (_, exc_ball) = sys_handle.get_ball_snapshot(exc_bid).await.unwrap().unwrap();
    assert_eq!(exc_ball.mass, 10_000_000.0);
    assert_eq!(exc_ball.inertia, 2.8);
    assert_eq!(exc_ball.radius, 50.0);
    let PyRep::Object { args: unnamed_dict, .. } = sys_handle.get_ball_slim_item(unnamed_bid).await.unwrap().unwrap() else { panic!("slim") };
    assert_eq!(py_dict_get(&unnamed_dict, "name"), Some(&PyRep::String(format!("Item {unnamed_bid}"))));

    // Engage high-TP drone (554 = 150.0) against locked target -> tp_sig_multiplier clamped to 2.0 (not 2.5!)
    let tp_target_id = 810_999i64;
    spawn_test_ball(&ctx, sys_id, tp_target_id, 587, DVec3::new(3_000.0, 0.0, 0.0), 2_000.0).await;
    sys_handle.add_target_lock(ship_id, eve_destiny::TargetLock {
        target_id: tp_target_id,
        lock_time_ms: 1_000.0,
        remaining_lock_ms: 0.0,
        locked: true,
        max_range_m: None,
    }).await.unwrap();
    let engage_tp = handle_npcs_drones(
        &ctx, &mut session, "entity", "CmdEngage",
        &[PyRep::List(vec![PyRep::Integer(tp_bid)]), PyRep::Integer(tp_target_id)],
        &kwargs,
    ).await.unwrap().unwrap();
    assert_eq!(py_dict_get(&engage_tp, "ewarApplied"), Some(&PyRep::Bool(true)));
    let target_cs = sys_handle.get_combat_state(tp_target_id).await.unwrap().unwrap();
    assert_eq!(target_cs.tp_sig_multiplier, Some(2.0));
}
