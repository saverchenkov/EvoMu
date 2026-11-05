use super::combat_fighters::combat_fighters_tests::make_fighter_type;
use super::combat_fighters_abilities_tests::{make_ability_test_ctx, py_key, spawn_fighter_and_ship};
use super::*;
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_sde::FighterAbilitySlot;
use glam::DVec3;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fighter_abilities_scram_neut_ecm_paint_damp_and_multi_batch() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_ability_test_ctx();
    let mut sde = (*ctx.sde).clone();
    sde.fighter_abilities_by_type.insert(
        23_057,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 10, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 11, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 12, cooldown_seconds: None, charges: None },
        ],
    );
    sde.fighter_abilities_by_type.insert(
        23_059,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 14, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 17, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 38, cooldown_seconds: None, charges: None },
        ],
    );
    sde.types.insert(
        23_061,
        make_fighter_type(
            23_061,
            1653,
            87,
            1_000.0,
            vec![(2215, 3.0), (2214, 1.0), (2324, 41_549.0), (2325, 30.0), (2330, 15_000.0)],
        ),
    );
    sde.fighter_abilities_by_type.insert(
        23_061,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 7, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 44, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 99, cooldown_seconds: None, charges: None },
        ],
    );
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), ctx.db.clone());
    let f_ewar = 800_000_020i64;
    let f_supp = 800_000_021i64;
    let f_heavy = 800_000_022i64;
    let target_id = 300_000_020i64;

    spawn_fighter_and_ship(&ctx, sys_id, ship_id, f_ewar, 23_057, char_id).await;
    spawn_fighter_and_ship(&ctx, sys_id, ship_id, f_supp, 23_059, char_id).await;
    spawn_fighter_and_ship(&ctx, sys_id, ship_id, f_heavy, 23_061, char_id).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 587,
                position: DVec3::new(1_400.0, 0.0, 0.0),
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
                capacitor: 500.0,
                max_capacitor: 500.0,
                signature_radius: 40.0,
                ..CombatState::default()
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                locked: true,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                max_range_m: None,
            },
        )
        .await;
    let _ = sys
        .add_target_lock(
            target_id,
            TargetLock {
                target_id: ship_id,
                locked: true,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                max_range_m: Some(50_000.0),
            },
        )
        .await;

    // Scram (10), Neut (11), TP (14), Damp (17), ECM (12), Single Bomb (7), Entropic (38), Heavy Rocket (44)
    let r_scram = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_ewar), PyRep::Integer(0), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&r_scram, "ewarStrength").and_then(|v| v.as_f64()), Some(3.0));

    let r_neut = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_ewar), PyRep::Integer(1), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(py_key(&r_neut, "ewarStrength").and_then(|v| v.as_f64()).unwrap() >= 200.0);
    assert!(sys.get_combat_state(target_id).await.unwrap().unwrap().capacitor < 300.0);

    let r_tp = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_supp), PyRep::Integer(0), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!((py_key(&r_tp, "ewarStrength").and_then(|v| v.as_f64()).unwrap() - 1.45).abs() < 1e-6);
    assert!((sys.get_combat_state(target_id).await.unwrap().unwrap().signature_radius - 58.0).abs() < 1e-6);

    let r_damp = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_supp), PyRep::Integer(1), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!((py_key(&r_damp, "ewarStrength").and_then(|v| v.as_f64()).unwrap() - 0.45).abs() < 1e-6);

    let r_ent = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_supp), PyRep::Integer(2), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&r_ent, "abilityID").and_then(|v| v.as_i64()), Some(38));

    let r_ecm = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_ewar), PyRep::Integer(2), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(py_key(&r_ecm, "ewarStrength").and_then(|v| v.as_f64()).unwrap() >= 7.0);
    assert!(sys.get_target_locks(target_id).await.unwrap().is_empty());

    let r_bomb7 = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_heavy), PyRep::Integer(0), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&r_bomb7, "abilityID").and_then(|v| v.as_i64()), Some(7));

    let r_rocket44 = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_heavy), PyRep::Integer(1), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&r_rocket44, "abilityID").and_then(|v| v.as_i64()), Some(44));

    // Highsec disallowed ability 99
    let err_hi = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_heavy), PyRep::Integer(2)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_hi.code, "AbilityDisallowedInHighSec");

    // Multi-fighter batch call returns dict keyed by failed fighter ID
    ctx.fighter_cooldown_state.write().unwrap().clear();
    let batch = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[
            PyRep::List(vec![PyRep::Integer(f_heavy), PyRep::Integer(888_777)]),
            PyRep::Integer(0),
            PyRep::Integer(target_id),
        ],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(batch, PyRep::Dict(_)));
}
