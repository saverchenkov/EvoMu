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
    ctx.set_clock(Arc::new(eve_db::clock::VirtualServerClock::with_fixed_base(1_800_000_000)));
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fighter_ability_r9_carrier_fighter_target_pos_and_ewar_guards() {
    let (mut ctx, mut session, char_id, ship_id, _hs_sys) = make_ability_test_ctx();
    let ls_sys = 30_000_200u32;
    session.solar_system_id = Some(ls_sys);

    // Ensure Control Tower type (33475, group 365) and Upwell Astrahus (35832, category 65) exist in SDE
    {
        let reg = Arc::make_mut(&mut ctx.sde);
        reg.types.insert(
            33_475,
            crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type(
                33_475,
                365,
                23,
                10_000_000.0,
                vec![(680, 30_000.0)],
            ),
        );
        reg.types.insert(
            35_832,
            crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_type(
                35_832,
                1657,
                65,
                10_000_000.0,
                vec![],
            ),
        );
    }

    let support_id = 800_000_050i64;
    let bomber_id = 800_000_051i64;
    spawn_fighter_and_ship(&ctx, ls_sys, ship_id, support_id, 23_059, char_id).await;
    spawn_fighter_and_ship(&ctx, ls_sys, ship_id, bomber_id, 23_063, char_id).await;

    let sys_h = ctx.systems.get_or_spawn(ls_sys);
    let target_ship = 700_000_050i64;
    let struct_id = 700_000_051i64;
    let tower_id = 700_000_052i64;
    let bomb_skip_cloaked = 700_000_053i64;
    let bomb_skip_pos = 700_000_054i64;
    let bomb_hit_open = 700_000_055i64;

    let spawn_ship = |id: i64, type_id: u32, pos: DVec3, cs: CombatState| {
        let sys_h = sys_h.clone();
        async move {
            let _ = sys_h
                .spawn_ball_with_combat(
                    Ball {
                        id,
                        type_id,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 300.0,
                        inertia: 2.0,
                        mass: 1_000_000.0,
                        radius: 100.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    cs,
                )
                .await;
        }
    };

    // Spawn carrier parent ship, target ship, structure, and bomb targets
    spawn_ship(ship_id, 587, DVec3::ZERO, CombatState::new()).await;
    let mut normal_cs = CombatState::new();
    normal_cs.shield_hp = 5_000.0;
    normal_cs.max_shield_hp = 5_000.0;
    spawn_ship(target_ship, 587, DVec3::new(3_000.0, 0.0, 0.0), normal_cs.clone()).await;
    spawn_ship(struct_id, 35_832, DVec3::new(4_000.0, 0.0, 0.0), normal_cs.clone()).await;
    for locked_tid in [target_ship, struct_id] {
        let _ = sys_h
            .add_target_lock(
                ship_id,
                eve_destiny::TargetLock {
                    target_id: locked_tid,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
    }

    // 1. Target cloaked -> CannotTargetCloaked
    let _ = sys_h
        .set_combat_state(
            target_ship,
            CombatState {
                is_cloaked: true,
                ..normal_cs.clone()
            },
        )
        .await;
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CannotTargetCloaked");

    // 2. Target tethered / undock invulnerable -> TargetInvulnerable
    let _ = sys_h
        .set_combat_state(
            target_ship,
            CombatState {
                is_tethered: true,
                ..normal_cs.clone()
            },
        )
        .await;
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetInvulnerable");

    let _ = sys_h
        .set_combat_state(
            target_ship,
            CombatState {
                undock_invulnerable_remaining_ticks: 15,
                ..normal_cs.clone()
            },
        )
        .await;
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetInvulnerable");
    let _ = sys_h.set_combat_state(target_ship, normal_cs.clone()).await;

    // 3. Support fighter EWAR vs Upwell structure -> StructureImmuneToEwar
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(struct_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "StructureImmuneToEwar");

    // 4. Online POS tower at (100_000, 0, 0) protects targets inside 30km forcefield (target & bomb skip)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            tower_id,
            eve_db::InventoryItemRecord {
                item_id: tower_id,
                type_id: 33_475,
                owner_id: 98_999_001,
                location_id: ls_sys as i64,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.starbase_settings.insert(tower_id, Default::default());
        snap.item_positions.insert(tower_id, [100_000.0, 0.0, 0.0]);
    });
    let _ = sys_h
        .set_ball_position(target_ship, DVec3::new(72_000.0, 0.0, 0.0))
        .await;
    let _ = sys_h
        .set_ball_position(support_id, DVec3::new(68_000.0, 0.0, 0.0))
        .await;
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetInsideForcefield");

    // 5. Fighter inside POS forcefield or carrier cloaked / tethered / in warp rejects both targeted and point bomb
    let _ = sys_h.set_ball_position(support_id, DVec3::new(105_000.0, 0.0, 0.0)).await;
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CannotActivateInForcefield");
    let _ = sys_h.set_ball_position(support_id, DVec3::new(500.0, 0.0, 0.0)).await;
    let _ = sys_h.set_ball_position(target_ship, DVec3::new(3_000.0, 0.0, 0.0)).await;

    let _ = sys_h
        .set_combat_state(
            ship_id,
            CombatState {
                is_cloaked: true,
                ..CombatState::new()
            },
        )
        .await;
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CannotActivateWhileCloaked");
    let err_bomb = handle_activate_fighter_ability_slots_on_point(
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
    assert_eq!(err_bomb.code, "CannotActivateWhileCloaked");

    let _ = sys_h
        .set_combat_state(
            ship_id,
            CombatState {
                is_tethered: true,
                ..CombatState::new()
            },
        )
        .await;
    let err_teth = handle_activate_fighter_ability_slots_on_point(
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
    assert_eq!(err_teth.code, "CannotActivateWhileTethered");
    let _ = sys_h.set_combat_state(ship_id, CombatState::new()).await;

    // 5. AoE bomb skips cloaked and POS-protected targets while damaging unshielded open targets
    let _ = sys_h.set_ball_position(bomber_id, DVec3::new(65_000.0, 0.0, 0.0)).await;
    spawn_ship(
        bomb_skip_cloaked,
        587,
        DVec3::new(75_000.0, 0.0, 0.0),
        CombatState {
            is_cloaked: true,
            ..normal_cs.clone()
        },
    )
    .await;
    spawn_ship(bomb_skip_pos, 587, DVec3::new(82_000.0, 0.0, 0.0), normal_cs.clone()).await;
    spawn_ship(bomb_hit_open, 587, DVec3::new(68_000.0, 0.0, 0.0), normal_cs.clone()).await;

    let _ = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(75_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .expect("bomb point activation outside POS must succeed");

    assert_eq!(sys_h.get_combat_state(bomb_skip_cloaked).await.unwrap().unwrap().shield_hp, 5_000.0);
    assert_eq!(sys_h.get_combat_state(bomb_skip_pos).await.unwrap().unwrap().shield_hp, 5_000.0);
    assert!(sys_h.get_combat_state(bomb_hit_open).await.unwrap().unwrap().shield_hp < 5_000.0);

    // 6. Carrier in warp rejects fighter ability activation (ShipInWarp)
    let mut warp_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { phase, .. } = &mut warp_mode {
        *phase = eve_destiny::WarpPhase::Cruising;
    }
    let _ = sys_h.set_movement_mode(ship_id, warp_mode).await;
    let err_warp = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(support_id), PyRep::Integer(0), PyRep::Integer(target_ship)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_warp.code, "ShipInWarp");
    ctx.fighter_cooldown_state.write().unwrap().clear();
    let err_warp_bomb = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::Integer(bomber_id),
            PyRep::Integer(2),
            PyRep::Tuple(vec![PyRep::Real(75_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
        ],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_warp_bomb.code, "ShipInWarp");
}
