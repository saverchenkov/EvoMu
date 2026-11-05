//! Unit and mutation-killing tests for `combat_weapons`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test]
async fn ewar_target_painter_safety_gates_and_signature_bloom() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    // Target Painter (group 379, type 37901)
    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        37_901,
        make_fighter_type(37_901, 379, 7, 5.0, vec![(554, 30.0)]),
    );
    ctx.sde = std::sync::Arc::new(sde);

    let tp_id = 810_001_i64;
    let target_npc_id = 910_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            tp_id,
            make_fighter_item(tp_id, 37_901, char_id, ship_id, InventoryFlag::MedSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, tethered) in [
        (ship_id, 23_915_u32, DVec3::ZERO, true),
        (target_npc_id, 10_001_u32, DVec3::new(5_000.0, 0.0, 0.0), false),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 500.0,
                    max_shield_hp: 500.0,
                    armor_hp: 500.0,
                    max_armor_hp: 500.0,
                    structure_hp: 500.0,
                    max_structure_hp: 500.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 100.0,
                    base_signature_radius: Some(100.0),
                    is_tethered: tethered,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    // 1. Tethered ship cannot activate offensive/EWAR module
    let teth_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(teth_err.code, "CantActivateWhileTethered");

    // Untether attacker ship
    let mut cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    cs.is_tethered = false;
    sys_handle.set_combat_state(ship_id, cs).await.unwrap();

    // 2. Missing target returns TargetRequired; unlocked target returns TargetNotLocked
    let req_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(req_err.code, "TargetRequired");

    let lock_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(lock_err.code, "TargetNotLocked");

    // Lock target and activate Target Painter -> records EWAR assist
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: target_npc_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();

    let tp_ok = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_ok.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));
    assert_eq!(tp_ok.dict_get("damageDealt"), Some(&PyRep::Real(0.0)));
}

#[tokio::test]
async fn laser_capacitor_crystal_volatility_shatter_and_fof_auto_target() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    // Energy Laser Turret (group 53, type 53001): cap need attr 6=50.0, optimal 54=15km, falloff 158=5km, charge size 128=1, usedWithChargeGroup 604=86
    sde.types.insert(
        53_001,
        make_fighter_type(
            53_001,
            53,
            7,
            5.0,
            vec![
                (6, 50.0),
                (51, 1_000.0),
                (54, 15_000.0),
                (64, 2.0),
                (128, 1.0),
                (158, 5_000.0),
                (160, 100.0),
                (604, 86.0),
            ],
        ),
    );
    // Volatile Frequency Crystal (group 86, type 86001): crystalsGetDamaged 786=1, chance 783=1.0, volDmg 784=0.5, hp 9=1.0
    sde.types.insert(
        86_001,
        make_fighter_type(
            86_001,
            86,
            8,
            1.0,
            vec![
                (9, 1.0),
                (114, 100.0),
                (118, 100.0),
                (128, 1.0),
                (783, 1.0),
                (784, 0.5),
                (786, 1.0),
            ],
        ),
    );
    // Missile Launcher (group 509, type 50901) + FoF Auto-Targeting Missile (group 395, type 39501)
    sde.types.insert(
        50_901,
        make_fighter_type(50_901, 509, 7, 5.0, vec![(51, 1_000.0), (128, 2.0), (604, 395.0)]),
    );
    sde.types.insert(
        39_501,
        make_fighter_type(
            39_501,
            395,
            8,
            0.02,
            vec![(37, 4_000.0), (117, 150.0), (128, 2.0), (281, 5_000.0)],
        ),
    );
    // Target NPC (cat 11, type 10002)
    sde.types.insert(
        10_002,
        make_fighter_type(
            10_002,
            550,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 50_000.0)],
        ),
    );
    ctx.sde = std::sync::Arc::new(sde);

    let laser_id = 820_001_i64;
    let crystal_id = 820_002_i64;
    let launcher_id = 820_003_i64;
    let fof_ammo_id = 820_004_i64;
    let npc_id = 920_099_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            laser_id,
            make_fighter_item(laser_id, 53_001, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            crystal_id,
            make_fighter_item(crystal_id, 86_001, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            launcher_id,
            make_fighter_item(launcher_id, 50_901, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            fof_ammo_id,
            make_fighter_item(fof_ammo_id, 39_501, char_id, ship_id, InventoryFlag::HiSlot1, 10),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, cap) in [
        (ship_id, 23_915_u32, DVec3::ZERO, 20.0), // Only 20 GJ cap initially (< 50 GJ needed!)
        (npc_id, 10_002_u32, DVec3::new(40_000.0, 0.0, 0.0), 100.0), // Out of range (40km > 15 + 3*5 = 30km)
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 2_000.0,
                    max_shield_hp: 2_000.0,
                    armor_hp: 2_000.0,
                    max_armor_hp: 2_000.0,
                    structure_hp: 2_000.0,
                    max_structure_hp: 2_000.0,
                    capacitor: cap,
                    max_capacitor: 500.0,
                    signature_radius: 50.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: npc_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .unwrap();

    // 1. Out of range fails BEFORE capacitor check
    let oor_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(oor_err.code, "TargetOutOfRange");

    // Move NPC into range (5km) -> now fails with NotEnoughCapacitor (20 < 50 GJ)
    assert!(
        sys_handle
            .set_ball_position(npc_id, DVec3::new(5_000.0, 0.0, 0.0))
            .await
    );

    let cap_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(cap_err.code, "NotEnoughCapacitor");

    // Restore 200 GJ capacitor -> first laser shot damages crystal to 50%; Overload second shot shatters crystal!
    let mut ship_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs.capacitor = 200.0;
    sys_handle.set_combat_state(ship_id, ship_cs).await.unwrap();

    let shot1 = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(shot1.dict_get("capacitorBefore"), Some(&PyRep::Real(200.0)));
    assert_eq!(shot1.dict_get("capacitorAfter"), Some(&PyRep::Real(150.0)));
    assert_eq!(
        shot1.dict_get("crystalDamageFraction"),
        Some(&PyRep::Real(0.5))
    );
    assert_eq!(
        shot1.dict_get("crystalShattered"),
        Some(&PyRep::Bool(false))
    );

    let shot2 = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        shot2.dict_get("crystalDamageFraction"),
        Some(&PyRep::Real(1.0))
    );
    assert_eq!(shot2.dict_get("crystalShattered"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&crystal_id));
    });

    // FoF auto-targeting missile fires without an explicit target argument
    let fof_shot = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(launcher_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fof_shot.dict_get("targetID"), Some(&PyRep::Integer(npc_id)));
}
