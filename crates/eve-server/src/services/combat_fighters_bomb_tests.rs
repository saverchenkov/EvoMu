use super::combat_fighters_abilities_tests::{make_ability_test_ctx, py_key, spawn_fighter_and_ship};
use super::*;
use eve_destiny::{Ball, CombatState, MovementMode};
use glam::DVec3;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fighter_point_bomb_validations_and_security_rules() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_ability_test_ctx();
    let bomber_id = 800_000_030i64;
    let light_id = 800_000_031i64;

    spawn_fighter_and_ship(&ctx, sys_id, ship_id, bomber_id, 23_063, char_id).await;
    spawn_fighter_and_ship(&ctx, sys_id, ship_id, light_id, 23_057, char_id).await;

    // 1. Wrong target_mode or no charges on slot 2 of light_id -> InvalidTargetMode
    let err = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(light_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InvalidTargetMode");

    // 2. Highsec (30_000_142) forbids point-targeted bombs -> BombForbiddenInHighSec
    let err = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "BombForbiddenInHighSec");

    // 3. Move session & bomber into lowsec (30_000_200, sec 0.25) with Green safety -> CantDoInCurrentSafetyLevel
    let low_sys = 30_000_200u32;
    session.solar_system_id = Some(low_sys);
    spawn_fighter_and_ship(&ctx, low_sys, ship_id, bomber_id, 23_063, char_id).await;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let err = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(2_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CantDoInCurrentSafetyLevel");

    // 4. Out-of-range detonation point (> 100 km) -> TargetOutOfRange
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let err = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(250_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetOutOfRange");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fighter_point_bomb_detonation_aoe_pod_safety_charges_and_kill() {
    let (ctx, mut session, char_id, ship_id, _) = make_ability_test_ctx();
    let low_sys = 30_000_200u32;
    session.solar_system_id = Some(low_sys);
    let bomber_id = 800_000_040i64;
    let enemy_ship_id = 300_000_040i64;
    let weak_ship_id = 300_000_041i64;
    let pod_id = 300_000_042i64;
    let victim_char_id = 90_000_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_char_id) {
            ch.active_ship_id = enemy_ship_id;
        }
    });
    spawn_fighter_and_ship(&ctx, low_sys, ship_id, bomber_id, 23_063, char_id).await;
    let sys = ctx.systems.get_or_spawn(low_sys);

    // Spawn durable enemy ship inside 12 km blast radius (at x=5000)
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: enemy_ship_id,
                type_id: 587,
                position: DVec3::new(5_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 2_000.0,
                max_shield_hp: 2_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                ..CombatState::default()
            },
        )
        .await;

    // Spawn weak fragile target at detonation center that will be destroyed by the bomb
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: weak_ship_id,
                type_id: 587,
                position: DVec3::new(4_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1.0,
                max_shield_hp: 50.0,
                armor_hp: 1.0,
                max_armor_hp: 50.0,
                structure_hp: 5.0,
                max_structure_hp: 50.0,
                ..CombatState::default()
            },
        )
        .await;

    // Spawn capsule (type_id 670) owned by victim_char_id -> Yellow safety must skip damaging pods in lowsec!
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: pod_id,
                type_id: 670,
                position: DVec3::new(4_100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                max_shield_hp: 0.0,
                armor_hp: 0.0,
                max_armor_hp: 0.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                ..CombatState::default()
            },
        )
        .await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&victim_char_id).unwrap().active_ship_id = pod_id;
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Yellow);
    });

    // Launch point-targeted bomb at (4000, 0, 0) via 5-arg CmdActivateAbilitySlots dispatch!
    let res = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Real(4_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(py_key(&res, "pointTargeted").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(py_key(&res, "remainingCharges").and_then(|v| v.as_i64()), Some(1));
    assert!(sys.get_combat_state(enemy_ship_id).await.unwrap().unwrap().shield_hp < 2_000.0);
    assert!(sys.get_ball_snapshot(weak_ship_id).await.unwrap().is_none(), "fragile target should be destroyed");
    assert_eq!(
        sys.get_combat_state(pod_id).await.unwrap().unwrap().structure_hp,
        500.0,
        "Yellow safety should spare capsules in lowsec AoE blast"
    );
    assert!(
        session.notifications.iter().any(|n| n.0 == "OnFighterAbilitySlotActivated"),
        "expected OnFighterAbilitySlotActivated notification"
    );

    // Cooldown check immediately after launch -> AbilityOnCooldown
    let err_cd = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(4_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cd.code, "AbilityOnCooldown");

    // Exhaust 2nd charge then trigger OutOfCharges on 3rd attempt
    ctx.fighter_cooldown_state.write().unwrap().clear();
    let res2 = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(4_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&res2, "remainingCharges").and_then(|v| v.as_i64()), Some(0));

    ctx.fighter_cooldown_state.write().unwrap().clear();
    let err_empty = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(4_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_empty.code, "OutOfCharges");
}
