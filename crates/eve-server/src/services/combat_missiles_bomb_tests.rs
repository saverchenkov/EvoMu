use super::combat_missiles_aoe_tests::{make_fighter_item, make_missile_and_aoe_ctx, py_val, spawn_missile_target_ball};
use super::*;
use eve_destiny::{Ball, DamageVector, MovementMode, TargetLock};
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smartbomb_hits_missiles_probes_drones_ships_and_triggers_concord() {
    let (ctx, _, char_id, ship_id, sys_id) = make_missile_and_aoe_ctx();
    let kwargs = HashMap::new();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    spawn_missile_target_ball(&ctx, sys_id, ship_id, 587, DVec3::ZERO, 500.0, 500.0, 500.0, 20.0).await;
    let (_, attacker_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(720_001, make_fighter_item(720_001, 12_219, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    let sb_mod = eve_dogma::FittedModule {
        item_id: 720_001,
        type_id: 12_219,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let sb_type = ctx.sde.types.get(&12_219).unwrap();
    let now_unix = ctx.now_unix();

    // 1. Highsec Green safety blocks smartbomb -> CantDoInCurrentSafetyLevel
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let err_safety = activate_aoe_or_defender_module_with_cap(
        &ctx, char_id, ship_id, sys_id, 0.9, now_unix, 720_001, &sb_mod, sb_type, &attacker_ball, None, &kwargs, true,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_safety.code, "CantDoInCurrentSafetyLevel");

    // 2. Red safety with only 20 GJ cap vs 30 GJ cost -> NotEnoughCapacitor
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let err_cap = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, sys_id, 0.9, now_unix, 720_001, &sb_mod, sb_type, &attacker_ball, None, &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cap.code, "NotEnoughCapacitor");

    // Give ship 200 GJ capacitor and place missile, probe, and enemy ship inside 5 km smartbomb range
    let mut ship_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs.capacitor = 200.0;
    sys_handle.set_combat_state(ship_id, ship_cs).await.unwrap();

    let missile_id = 880_000_001i64;
    let probe_id = 880_000_002i64;
    let victim_char = 90_000_002i64;
    let victim_ship = 200_000_222i64;

    spawn_missile_target_ball(&ctx, sys_id, missile_id, 209, DVec3::new(1_000.0, 0.0, 0.0), 0.0, 0.0, 50.0, 0.0).await;
    spawn_missile_target_ball(&ctx, sys_id, probe_id, 30_013, DVec3::new(1_500.0, 0.0, 0.0), 0.0, 0.0, 40.0, 0.0).await;
    spawn_missile_target_ball(&ctx, sys_id, victim_ship, 587, DVec3::new(2_000.0, 0.0, 0.0), 2_000.0, 500.0, 500.0, 500.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(victim_ship, make_fighter_item(victim_ship, 587, victim_char, sys_id as i64, InventoryFlag::None, 1));
        if let Some(vc) = snap.characters.get_mut(&victim_char) {
            vc.active_ship_id = victim_ship;
        }
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.insert(
            missile_id,
            InFlightMissileState {
                missile_ball_id: missile_id,
                solar_system_id: sys_id,
                attacker_char_id: victim_char,
                attacker_ship_id: victim_ship,
                launcher_module_id: 900_001,
                launcher_type_id: 2_410,
                missile_type_id: 209,
                target_ball_id: ship_id,
                max_velocity: 3_750.0,
                flight_time_s: 6.0,
                remaining_flight_s: 5.0,
                hp: 50.0,
                max_hp: 50.0,
                base_damage: DamageVector::new(0.0, 0.0, 100.0, 0.0),
                explosion_radius: 40.0,
                explosion_velocity: 170.0,
                drf: 0.604,
                is_fof: false,
                is_breacher_pod: false,
                breacher_dot_duration_s: 0.0,
                breacher_flat_cap: 0.0,
                breacher_pct_cap: 0.0,
                destroyed: false,
                expired: false,
                impacted: false,
                applied_damage: 0.0,
            },
        );
    });

    let res = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, sys_id, 0.9, now_unix, 720_001, &sb_mod, sb_type, &attacker_ball, None, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_val(&res, "concordTriggered").and_then(|v| v.as_bool()), Some(true));
    assert!(sys_handle.get_ball_snapshot(missile_id).await.unwrap().is_none(), "smartbomb destroys missile");
    assert!(sys_handle.get_ball_snapshot(probe_id).await.unwrap().is_none(), "smartbomb destroys probe");
    assert!(sys_handle.get_combat_state(victim_ship).await.unwrap().unwrap().shield_hp < 2_000.0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bomb_launcher_damage_void_lockbreaker_and_defender_interception() {
    let (ctx, _, char_id, ship_id, sys_id) = make_missile_and_aoe_ctx();
    let kwargs = HashMap::new();
    let low_sys = 30_000_200u32;
    let sys_handle = ctx.systems.get_or_spawn(low_sys);

    spawn_missile_target_ball(&ctx, low_sys, ship_id, 587, DVec3::ZERO, 500.0, 500.0, 500.0, 500.0).await;
    let attacker_ball = Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::X,
        max_velocity: 250.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };

    let bomb_mod = eve_dogma::FittedModule {
        item_id: 730_001,
        type_id: 27_912,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let def_mod = eve_dogma::FittedModule {
        item_id: 730_010,
        type_id: 1_599,
        flag_id: 28,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let bomb_mod_t = ctx.sde.types.get(&27_912).unwrap();
    let def_mod_t = ctx.sde.types.get(&1_599).unwrap();
    let now_unix = ctx.now_unix();

    // 1. Highsec check -> BombDisallowedInHighSec
    let err_hi = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, sys_id, 0.9, now_unix, 730_001, &bomb_mod, bomb_mod_t, &attacker_ball, None, &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_hi.code, "BombDisallowedInHighSec");

    // 2. Empty bomb launcher & empty defender launcher -> NoAmmoLoaded
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    assert_eq!(
        activate_aoe_or_defender_module(
            &ctx, char_id, ship_id, low_sys, 0.2, now_unix, 730_001, &bomb_mod, bomb_mod_t, &attacker_ball, None, &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoAmmoLoaded"
    );
    assert_eq!(
        activate_aoe_or_defender_module(
            &ctx, char_id, ship_id, low_sys, 0.2, now_unix, 730_010, &def_mod, def_mod_t, &attacker_ball, None, &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoAmmoLoaded"
    );

    // Spawn target at predicted detonation point (6_000 m along +X) with target locks & full capacitor
    let target_ship = 300_000_555i64;
    spawn_missile_target_ball(&ctx, low_sys, target_ship, 587, DVec3::new(6_000.0, 0.0, 0.0), 5_000.0, 500.0, 500.0, 3_000.0).await;
    sys_handle
        .add_target_lock(
            target_ship,
            TargetLock {
                target_id: ship_id,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();

    // 3. Fire Lockbreaker Bomb (27924) -> breaks target_ship's locks
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(730_002, make_fighter_item(730_002, 27_924, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    let lb_res = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, low_sys, 0.2, now_unix, 730_001, &bomb_mod, bomb_mod_t, &attacker_ball, None, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_val(&lb_res, "bombTypeID").and_then(|v| v.as_i64()), Some(27_924));
    assert!(sys_handle.get_target_locks(target_ship).await.unwrap().is_empty());

    // 4. Fire Void Bomb (27920) -> drains target_ship capacitor
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(730_003, make_fighter_item(730_003, 27_920, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    let void_res = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, low_sys, 0.2, now_unix, 730_001, &bomb_mod, bomb_mod_t, &attacker_ball, None, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(py_val(&void_res, "totalCapNeutralized").and_then(|v| v.as_f64()).unwrap() > 100.0);

    // 5. Fire Damage Bomb (27916) -> damages target_ship
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(730_004, make_fighter_item(730_004, 27_916, char_id, ship_id, InventoryFlag::HiSlot0, 1));
    });
    let dmg_res = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, low_sys, 0.2, now_unix, 730_001, &bomb_mod, bomb_mod_t, &attacker_ball, None, &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(py_val(&dmg_res, "totalDamageApplied").and_then(|v| v.as_f64()).unwrap() > 100.0);

    // 6. Defender Launcher intercepts active missile
    let mid = 880_000_009i64;
    spawn_missile_target_ball(&ctx, low_sys, mid, 209, DVec3::new(2_000.0, 0.0, 0.0), 0.0, 0.0, 50.0, 0.0).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.insert(
            mid,
            InFlightMissileState {
                missile_ball_id: mid,
                solar_system_id: low_sys,
                attacker_char_id: 90_000_002,
                attacker_ship_id: target_ship,
                launcher_module_id: 900_001,
                launcher_type_id: 2_410,
                missile_type_id: 209,
                target_ball_id: ship_id,
                max_velocity: 3_750.0,
                flight_time_s: 6.0,
                remaining_flight_s: 5.0,
                hp: 50.0,
                max_hp: 50.0,
                base_damage: DamageVector::new(0.0, 0.0, 100.0, 0.0),
                explosion_radius: 40.0,
                explosion_velocity: 170.0,
                drf: 0.604,
                is_fof: false,
                is_breacher_pod: false,
                breacher_dot_duration_s: 0.0,
                breacher_flat_cap: 0.0,
                breacher_pct_cap: 0.0,
                destroyed: false,
                expired: false,
                impacted: false,
                applied_damage: 0.0,
            },
        );
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(730_011, make_fighter_item(730_011, 267, char_id, ship_id, InventoryFlag::HiSlot1, 2));
    });
    let def_res = activate_aoe_or_defender_module(
        &ctx, char_id, ship_id, low_sys, 0.2, now_unix, 730_010, &def_mod, def_mod_t, &attacker_ball, Some(mid), &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_val(&def_res, "interceptedMissileID").and_then(|v| v.as_i64()), Some(mid));
}
