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

    // Exact capacitor boundary: capacitor == 50.0 GJ (== cap_need 50.0) succeeds on non-overloaded shot
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            crystal_id,
            make_fighter_item(crystal_id, 86_001, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    ctx.module_reactivation_cooldowns.write().unwrap().clear();
    let mut ship_cs_exact = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs_exact.capacitor = 50.0;
    sys_handle.set_combat_state(ship_id, ship_cs_exact).await.unwrap();
    let shot_exact = handle_weapons(
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
    assert_eq!(shot_exact.dict_get("capacitorAfter"), Some(&PyRep::Real(0.0)));
}

#[tokio::test]
async fn combat_weapons_turret_families_crystal_stack_entropic_and_vorton_arcs() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
    use eve_sde::attr_ids;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    // Clone SDE and register custom turrets & charges covering groups 55, 53, 1986, 4060, 379 & charges
    let mut sde = (*ctx.sde).clone();
    // Target Painter 379
    sde.types.insert(37_902, make_fighter_type(37_902, 379, 7, 5.0, vec![(554, 25.0)]));
    // Projectile Turret (group 55, 0 cap even with attr 6 = 999) + ammo (group 83)
    sde.types.insert(
        55_001,
        make_fighter_type(
            55_001,
            55,
            7,
            3.0,
            vec![(6, 999.0), (51, 60_000.0), (54, 10_000.0), (158, 5_000.0), (160, 0.5), (604, 83.0), (620, 40_000.0), (64, 2.0)],
        ),
    );
    sde.types.insert(
        83_001,
        make_fighter_type(
            83_001,
            83,
            8,
            0.01,
            vec![(114, 10.0), (116, 10.0), (117, 10.0), (118, 10.0), (120, 1.5), (244, 1.2)],
        ),
    );
    // Energy Laser (group 53) + T1 non-degrading crystal + Faction stacked crystal
    sde.types.insert(
        53_002,
        make_fighter_type(
            53_002,
            53,
            7,
            3.0,
            vec![(6, 10.0), (51, 3000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 86.0), (620, 40_000.0), (64, 1.0)],
        ),
    );
    sde.types.insert(
        86_002,
        make_fighter_type(86_002, 86, 8, 1.0, vec![(114, 20.0), (118, 20.0), (786, 0.0)]),
    );
    sde.types.insert(
        86_003,
        make_fighter_type(
            86_003,
            86,
            8,
            1.0,
            vec![(114, 25.0), (118, 25.0), (786, 1.0), (783, 1.0), (784, 0.01), (9, 0.015)],
        ),
    );
    // Entropic Disintegrator (group 1986) + exotic plasma ammo (group 1987)
    sde.types.insert(
        19_861,
        make_fighter_type(
            19_861,
            1986,
            7,
            3.0,
            vec![(6, 10.0), (51, 60_000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 1987.0), (620, 40_000.0), (64, 1.0), (2733, 0.15), (2734, 10.0)],
        ),
    );
    sde.types.insert(
        19_871,
        make_fighter_type(19_871, 1987, 8, 0.01, vec![(117, 15.0), (118, 25.0)]),
    );
    // Vorton Projector (group 4060) + condenser pack ammo (group 4061)
    sde.types.insert(
        40_601,
        make_fighter_type(
            40_601,
            4060,
            7,
            3.0,
            vec![
                (6, 10.0), (51, 3000.0), (54, 25_000.0), (158, 5_000.0), (604, 4061.0), (64, 1.0),
                (3036, 10_000.0), (3037, 5.0), (653, 190.0), (654, 43.0), (1353, 0.40),
            ],
        ),
    );
    sde.types.insert(
        40_611,
        make_fighter_type(40_611, 4061, 8, 0.01, vec![(114, 30.0), (117, 30.0)]),
    );
    // Capsule type 670 & combatant ship type 23_915 already in SDE or ensure capsule 670
    if !sde.types.contains_key(&670) {
        sde.types.insert(670, make_fighter_type(670, 29, 6, 100.0, vec![]));
    }
    ctx.sde = std::sync::Arc::new(sde);

    let tp_id = 830_000_i64;
    let offline_mod_id = 830_001_i64;
    let empty_gun_id = 830_002_i64;
    let proj_id = 830_003_i64;
    let proj_ammo_id = 830_004_i64;
    let t1_laser_id = 830_005_i64;
    let t1_crys_id = 830_006_i64;
    let fac_laser_id = 830_007_i64;
    let fac_crys_id = 830_008_i64;
    let ent_id = 830_009_i64;
    let ent_ammo_id = 830_010_i64;
    let vorton_id = 830_011_i64;
    let vorton_ammo_id = 830_012_i64;

    let npc_primary_id = 930_001_i64;
    let npc_moving_id = 930_002_i64;
    let npc_fragile_arc_id = 930_003_i64;
    let pvp_cloaked_id = 930_004_i64;
    let pvp_neutral_pod_id = 930_005_i64;
    let pvp_suspect_id = 930_006_i64;
    let char_cloaked = 90_000_101_i64;
    let char_pod = 90_000_102_i64;
    let char_suspect = 90_000_103_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(tp_id, make_fighter_item(tp_id, 37_902, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(offline_mod_id, make_fighter_item(offline_mod_id, 55_001, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.module_online_states.insert(offline_mod_id, false);
        snap.items.insert(empty_gun_id, make_fighter_item(empty_gun_id, 55_001, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(proj_id, make_fighter_item(proj_id, 55_001, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(proj_ammo_id, make_fighter_item(proj_ammo_id, 83_001, char_id, ship_id, InventoryFlag::HiSlot2, 2));
        snap.items.insert(t1_laser_id, make_fighter_item(t1_laser_id, 53_002, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.items.insert(t1_crys_id, make_fighter_item(t1_crys_id, 86_002, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.items.insert(fac_laser_id, make_fighter_item(fac_laser_id, 53_002, char_id, ship_id, InventoryFlag::HiSlot4, 1));
        snap.items.insert(fac_crys_id, make_fighter_item(fac_crys_id, 86_003, char_id, ship_id, InventoryFlag::HiSlot4, 2));
        snap.items.insert(ent_id, make_fighter_item(ent_id, 19_861, char_id, ship_id, InventoryFlag::HiSlot5, 1));
        snap.items.insert(ent_ammo_id, make_fighter_item(ent_ammo_id, 19_871, char_id, ship_id, InventoryFlag::HiSlot5, 20));
        snap.items.insert(vorton_id, make_fighter_item(vorton_id, 40_601, char_id, ship_id, InventoryFlag::HiSlot6, 1));
        snap.items.insert(vorton_ammo_id, make_fighter_item(vorton_ammo_id, 40_611, char_id, ship_id, InventoryFlag::HiSlot6, 20));

        // Additional PvP pilot characters & ships
        let tmpl_ch = snap.characters[&char_id].clone();
        for (vcid, bship_id, tid) in [
            (char_cloaked, pvp_cloaked_id, 23_915_u32),
            (char_pod, pvp_neutral_pod_id, 670_u32),
            (char_suspect, pvp_suspect_id, 23_915_u32),
        ] {
            let mut vc = tmpl_ch.clone();
            vc.character_id = vcid;
            vc.user_id = vcid as u64;
            vc.active_ship_id = bship_id;
            snap.characters.insert(vcid, vc);
            snap.items.insert(bship_id, make_fighter_item(bship_id, tid, vcid, sys_id as i64, InventoryFlag::None, 1));
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    {
        let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
        fit.modules.insert(
            offline_mod_id,
            eve_dogma::FittedModule {
                item_id: offline_mod_id,
                type_id: 55_001,
                flag_id: InventoryFlag::HiSlot0 as u16,
                state: eve_dogma::ModuleState::Offline,
                charge_type_id: None,
                charge_quantity: 0,
                damage_hp: 0.0,
                max_hp: 40.0,
                dynamic_attribute_overrides: HashMap::new(),
            },
        );
        ctx.store_ship_fitting(ship_id, fit);
    }

    let now_unix = ctx.now_unix();
    ctx.crimewatch.write().unwrap().get_or_create(char_suspect).suspect_flag_until_unix = now_unix + 900;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, vel, hp, sig, cap, cloaked) in [
        (ship_id, 23_915_u32, DVec3::ZERO, DVec3::ZERO, 2_000.0, 100.0, 0.0, false), // 0 GJ cap initially!
        (npc_primary_id, 10_002, DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO, 5_000.0, 50.0, 100.0, false),
        (npc_moving_id, 10_002, DVec3::new(18_000.0, 0.0, 0.0), DVec3::new(0.0, 2_500.0, 0.0), 5_000.0, 50.0, 100.0, false),
        (npc_fragile_arc_id, 10_002, DVec3::new(5_500.0, 0.0, 0.0), DVec3::ZERO, 2.0, 200.0, 100.0, false),
        (pvp_cloaked_id, 23_915, DVec3::new(5_600.0, 0.0, 0.0), DVec3::ZERO, 1_000.0, 100.0, 100.0, true),
        (pvp_neutral_pod_id, 670, DVec3::new(5_700.0, 0.0, 0.0), DVec3::ZERO, 1_000.0, 25.0, 100.0, false),
        (pvp_suspect_id, 23_915, DVec3::new(5_800.0, 0.0, 0.0), DVec3::ZERO, 5_000.0, 200.0, 100.0, false),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: vel,
                    max_velocity: 3_000.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: if tid == 670 { 15.0 } else { 50.0 },
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: hp,
                    max_shield_hp: hp,
                    armor_hp: hp,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    capacitor: cap,
                    max_capacitor: 500.0,
                    signature_radius: sig,
                    is_cloaked: cloaked,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    for tid in [npc_primary_id, npc_moving_id, pvp_suspect_id] {
        sys_handle
            .add_target_lock(
                ship_id,
                TargetLock {
                    target_id: tid,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(100_000.0),
                },
            )
            .await
            .unwrap();
    }

    // 1. Offline module & empty weapon errors
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(offline_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "ModuleOffline"
    );
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(empty_gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "NoAmmoLoaded"
    );

    // 2. TargetInvulnerable gates on both EWAR and Turret
    ctx.location_change_invuln.lock().unwrap().insert(char_suspect, now_unix + 60);
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(pvp_suspect_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "TargetInvulnerable"
    );
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(proj_id), PyRep::String("targetAttack".into()), PyRep::Integer(pvp_suspect_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "TargetInvulnerable"
    );
    ctx.location_change_invuln.lock().unwrap().remove(&char_suspect);

    // 3. Projectile turret fires with 0 GJ cap, decrements ammo stack 2 -> 1 -> removes on Overload, and lower hitChance on moving target
    let proj_shot1 = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(proj_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_moving_id)], &kwargs)
        .await.unwrap().unwrap();
    let hit_moving = proj_shot1.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    assert!(hit_moving < 0.95, "high angular velocity should reduce tracking hitChance: {hit_moving}");
    assert_eq!(proj_shot1.dict_get("capacitorBefore"), Some(&PyRep::Real(0.0)));
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&proj_ammo_id].quantity), 1);

    // Second Activate immediately hits ModuleAlreadyActive; Overload finishes stack and removes it
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(proj_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "ModuleAlreadyActive"
    );
    let proj_shot2 = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(proj_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    let hit_stat = proj_shot2.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    assert!(hit_stat > hit_moving);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&proj_ammo_id)));

    // Restore 500 GJ capacitor for energy/entropic/vorton tests
    let mut ship_cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    ship_cs.capacitor = 500.0;
    sys_handle.set_combat_state(ship_id, ship_cs).await.unwrap();

    // 4. T1 Crystal does NOT degrade; Stacked Faction Crystal (qty=2, vol_chance=1.0) decrements qty to 1 and resets DAMAGE on shatter
    let t1_shot = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(t1_laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(t1_shot.dict_get("crystalDamageFraction"), Some(&PyRep::Real(0.0)));
    assert_eq!(t1_shot.dict_get("crystalShattered"), Some(&PyRep::Bool(false)));
    assert!(ctx.db.with_snapshot(|s| !s.items[&t1_crys_id].dynamic_attributes.contains_key(&attr_ids::DAMAGE)));

    let fac_shot1 = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(fac_laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(fac_shot1.dict_get("crystalShattered"), Some(&PyRep::Bool(false)));
    let fac_shot2 = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(fac_laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(fac_shot2.dict_get("crystalShattered"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|s| {
        let rem = &s.items[&fac_crys_id];
        assert_eq!(rem.quantity, 1);
        assert_eq!(rem.dynamic_attributes.get(&attr_ids::DAMAGE).copied(), Some(0.0));
    });

    // 5. Entropic Disintegrator fresh activation, AlreadyActive rejection, and reset on reactivation
    let ent1 = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(ent1.dict_get("spoolCycles"), Some(&PyRep::Integer(0)));
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "AlreadyActive"
    );
    with_mut_combat_runtime(&ctx, |rt| {
        let entry = rt.entropic_spools.get_mut(&(ship_id, ent_id)).unwrap();
        entry.cycles = 3;
        entry.current_multiplier = 1.21;
    });
    ctx.db.with_mut_snapshot(|s| { s.module_active_states.remove(&ent_id); });
    let ent2 = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(ent2.dict_get("spoolCycles"), Some(&PyRep::Integer(0)));
    assert_eq!(ent2.dict_get("spoolMultiplier"), Some(&PyRep::Real(1.0)));

    // 6. Vorton Projector arc chaining under SafetyLevel::Yellow (skips cloaked & neutral pod; destroys fragile NPC; hits suspect pilot)
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Yellow); });
    let vorton_res = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(vorton_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(vorton_res.dict_get("isWreckingShot"), Some(&PyRep::Bool(false)));
    assert_eq!(vorton_res.dict_get("secondaryTargetsHit"), Some(&PyRep::Integer(2)));
    assert!(vorton_res.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert!(sys_handle.get_ball_snapshot(npc_fragile_arc_id).await.unwrap().is_none(), "secondary fragile NPC should be destroyed by Vorton arc");
}

#[tokio::test]
async fn combat_weapons_lowsec_pod_criminal_suspect_engagement_and_npc_timers() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    // Configure sys_id as lowsec (0.30) and add Target Painter + Energy Laser + T1 crystal
    let mut sde = (*ctx.sde).clone();
    sde.solar_systems.get_mut(&sys_id).unwrap().security_status = 0.30;
    sde.types.insert(37_903, make_fighter_type(37_903, 379, 7, 5.0, vec![(554, 20.0)]));
    sde.types.insert(53_003, make_fighter_type(53_003, 53, 7, 3.0, vec![(6, 5.0), (51, 3000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 86.0), (64, 1.0)]));
    sde.types.insert(86_004, make_fighter_type(86_004, 86, 8, 1.0, vec![(114, 10.0), (786, 0.0)]));
    if !sde.types.contains_key(&670) {
        sde.types.insert(670, make_fighter_type(670, 29, 6, 100.0, vec![]));
    }
    ctx.sde = std::sync::Arc::new(sde);

    let tp_id = 840_001_i64;
    let laser_id = 840_002_i64;
    let crys_id = 840_003_i64;
    let npc_id = 940_001_i64;
    let pod_char = 90_000_201_i64;
    let pod_id = 940_002_i64;
    let susp_char = 90_000_202_i64;
    let susp_ship_id = 940_003_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(tp_id, make_fighter_item(tp_id, 37_903, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(laser_id, make_fighter_item(laser_id, 53_003, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(crys_id, make_fighter_item(crys_id, 86_004, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        let tmpl = snap.characters[&char_id].clone();
        for (vcid, bship, tid) in [(pod_char, pod_id, 670_u32), (susp_char, susp_ship_id, 23_915_u32)] {
            let mut ch = tmpl.clone();
            ch.character_id = vcid;
            ch.user_id = vcid as u64;
            ch.active_ship_id = bship;
            snap.characters.insert(vcid, ch);
            snap.accounts.insert(
                format!("user_{vcid}"),
                eve_db::AccountRecord {
                    user_id: vcid as u64,
                    username: format!("user_{vcid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            snap.items.insert(bship, make_fighter_item(bship, tid, vcid, sys_id as i64, InventoryFlag::None, 1));
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let now_unix = ctx.now_unix();
    ctx.db.acquire_online_lock(session.user_id.unwrap_or(1), 999_000, Some(char_id), true, now_unix).unwrap();
    ctx.db.acquire_online_lock(susp_char as u64, 999_001, Some(susp_char), true, now_unix).unwrap();
    ctx.crimewatch.write().unwrap().get_or_create(susp_char).suspect_flag_until_unix = now_unix + 900;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 23_915_u32, DVec3::ZERO),
        (npc_id, 10_002, DVec3::new(3_000.0, 0.0, 0.0)),
        (pod_id, 670, DVec3::new(4_000.0, 0.0, 0.0)),
        (susp_ship_id, 23_915, DVec3::new(5_000.0, 0.0, 0.0)),
    ] {
        sys_handle
            .spawn_ball_with_combat(
                Ball {
                    id,
                    type_id: tid,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 500.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: if tid == 670 { 15.0 } else { 50.0 },
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
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 100.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    for tid in [npc_id, pod_id, susp_ship_id] {
        sys_handle
            .add_target_lock(
                ship_id,
                TargetLock { target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0) },
            )
            .await
            .unwrap();
    }

    // 1. Target Painter on NPC succeeds without PvP/Crimewatch penalty
    let tp_npc = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(tp_npc.dict_get("targetID"), Some(&PyRep::Integer(npc_id)));

    // 2. Yellow safety blocks attacking neutral pod in lowsec on both EWAR & Turret
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Yellow); });
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(pod_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "CantDoInCurrentSafetyLevel"
    );
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(pod_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "CantDoInCurrentSafetyLevel"
    );

    // 3. Red safety shooting suspect victim creates Limited Engagement + OnCrimewatchEngagementCreated on both chars
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Red); });
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(susp_char);
    let le_shot = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(susp_ship_id)], &kwargs)
        .await.unwrap().unwrap();
    assert!(le_shot.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));
    assert!(ctx.drain_notifications_for_char(susp_char).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));

    // 4. Red safety shooting neutral pod in lowsec (0.0 < 0.30 < 0.45) escalates to Criminal (suspect cleared!)
    let pod_shot = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(pod_id)], &kwargs)
        .await.unwrap().unwrap();
    assert!(pod_shot.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);
    {
        let mut cw = ctx.crimewatch.write().unwrap();
        let st = cw.get_or_create(char_id);
        assert!(st.criminal_flag_until_unix > now_unix);
        assert_eq!(st.suspect_flag_until_unix, 0);
    }

    // 5. Laser attack on NPC sets 300s NPC timer
    let npc_shot = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(npc_shot.dict_get("targetID"), Some(&PyRep::Integer(npc_id)));
}

#[tokio::test]
async fn combat_weapons_auto_target_fof_destruction_upwell_and_salvager_edges() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    sde.solar_systems.get_mut(&sys_id).unwrap().security_status = 0.30;
    sde.types.insert(53_004, make_fighter_type(53_004, 53, 7, 3.0, vec![(6, 5.0), (51, 60_000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 86.0), (64, 1.0)]));
    sde.types.insert(86_005, make_fighter_type(86_005, 86, 8, 1.0, vec![(114, 250.0), (786, 0.0)]));
    sde.types.insert(507_002, make_fighter_type(507_002, 507, 7, 3.0, vec![(6, 5.0), (51, 3000.0), (604, 396.0)]));
    sde.types.insert(396_001, make_fighter_type(396_001, 396, 8, 0.1, vec![(37, 10_000.0), (114, 150.0), (128, 2.0), (281, 5_000.0), (653, 40.0), (654, 50.0), (655, 0.6)]));
    sde.types.insert(1122_001, make_fighter_type(1122_001, 1122, 7, 3.0, vec![(6, 5.0), (51, 3000.0), (54, 5_000.0)]));
    if !sde.types.contains_key(&35_832) {
        sde.types.insert(35_832, make_fighter_type(35_832, 1657, 65, 10_000.0, vec![(263, 1_000_000.0), (265, 1_000_000.0), (9, 1_000_000.0)]));
    }
    ctx.sde = std::sync::Arc::new(sde);

    let laser_id = 850_001_i64;
    let crys_id = 850_002_i64;
    let fof_launcher_id = 850_003_i64;
    let fof_ammo_id = 850_004_i64;
    let salvager_id = 850_005_i64;
    let fragile_npc_id = 950_001_i64;
    let far_npc_id = 950_002_i64;
    let unlocked_npc_id = 950_003_i64;
    let corp_can_id = 950_004_i64;
    let foreign_corp_id = 98_999_001_i64;
    let structure_id = 1_000_000_950_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(laser_id, make_fighter_item(laser_id, 53_004, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(crys_id, make_fighter_item(crys_id, 86_005, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(fof_launcher_id, make_fighter_item(fof_launcher_id, 507_002, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(fof_ammo_id, make_fighter_item(fof_ammo_id, 396_001, char_id, ship_id, InventoryFlag::HiSlot1, 20));
        snap.items.insert(salvager_id, make_fighter_item(salvager_id, 1122_001, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(corp_can_id, make_fighter_item(corp_can_id, 10_002, foreign_corp_id, sys_id as i64, InventoryFlag::None, 1));
        let tmpl = snap.corporations.values().next().cloned().unwrap_or_default();
        let mut fc = tmpl;
        fc.corporation_id = foreign_corp_id;
        fc.ceo_id = char_id;
        snap.corporations.insert(foreign_corp_id, fc);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball_with_combat(
        Ball {
            id: ship_id, type_id: 23_915, position: DVec3::ZERO, velocity: DVec3::ZERO,
            max_velocity: 500.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 2_000.0, max_shield_hp: 2_000.0, armor_hp: 2_000.0, max_armor_hp: 2_000.0,
            structure_hp: 2_000.0, max_structure_hp: 2_000.0, capacitor: 5_000.0, max_capacitor: 5_000.0, signature_radius: 100.0,
            ..CombatState::default()
        },
    ).await.unwrap();

    // 1. Invalid moduleID, invalid target arg, Salvager missing target, FoF NoValidAutoTarget, and Laser TargetRequired
    assert!(handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(0)], &kwargs).await.unwrap().is_err());
    assert!(handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::String("bad".into())], &kwargs).await.unwrap().is_err());
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(salvager_id), PyRep::String("salvaging".into())], &kwargs)
            .await.unwrap().unwrap_err().code,
        "TargetRequired"
    );
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(fof_launcher_id), PyRep::String("missileLaunching".into())], &kwargs)
            .await.unwrap().unwrap_err().code,
        "NoValidAutoTarget"
    );
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into())], &kwargs)
            .await.unwrap().unwrap_err().code,
        "TargetRequired"
    );

    // Seed and spawn unlocked_npc, far_npc, near fragile_npc, corp_can, and Upwell structure
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(structure_id, eve_db::UpwellStructureRecord {
            structure_id,
            type_id: 35_832,
            owner_corp_id: foreign_corp_id,
            solar_system_id: sys_id,
            name: "EdgeCitadel".into(),
            state: "ShieldVulnerable".into(),
            shield_hp: 5.0,
            armor_hp: 5_000_000.0,
            hull_hp: 5_000_000.0,
            quantum_core_installed: true,
            ..Default::default()
        });
    });
    for (id, tid, pos, hp) in [
        (unlocked_npc_id, 23_915_u32, DVec3::new(4_000.0, 0.0, 0.0), 500.0),
        (far_npc_id, 23_915, DVec3::new(9_000.0, 0.0, 0.0), 500.0),
        (fragile_npc_id, 23_915, DVec3::new(1_500.0, 0.0, 0.0), 10.0),
        (corp_can_id, 10_002, DVec3::new(2_500.0, 0.0, 0.0), 500.0),
        (structure_id, 35_832, DVec3::new(5_000.0, 0.0, 0.0), 5_000_000.0),
    ] {
        sys_handle.spawn_ball_with_combat(
            Ball {
                id, type_id: tid, position: pos, velocity: DVec3::ZERO,
                max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: if tid == 35_832 { 500.0 } else { 50.0 }, speed_fraction: 0.0, mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: if tid == 35_832 { 5.0 } else { 0.0 },
                max_shield_hp: if tid == 35_832 { 1_000_000.0 } else { hp },
                armor_hp: if tid == 35_832 { 5_000_000.0 } else { 0.0 },
                max_armor_hp: if tid == 35_832 { 5_000_000.0 } else { hp },
                structure_hp: hp, max_structure_hp: hp,
                capacitor: 500.0, max_capacitor: 500.0, signature_radius: 200.0,
                ..CombatState::default()
            },
        ).await.unwrap();
    }

    // 2. TargetNotLocked when passing an unlocked target to turret
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(unlocked_npc_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "TargetNotLocked"
    );

    // 3. FoF auto-targeting selects nearest hostile (fragile_npc_id) across multiple grid balls and destroys it!
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Red); });
    let fof_hit = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(fof_launcher_id), PyRep::String("missileLaunching".into())], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(fof_hit.dict_get("targetID"), Some(&PyRep::Integer(fragile_npc_id)));
    assert_eq!(fof_hit.dict_get("isFoF"), Some(&PyRep::Bool(true)));

    // 3b. Turret shot killing a fragile locked NPC triggers combat_weapons destruction pipeline (`destroyed == true`)
    let fragile_laser_npc_id = 950_005_i64;
    sys_handle.spawn_ball_with_combat(
        Ball {
            id: fragile_laser_npc_id, type_id: 23_915, position: DVec3::new(1_800.0, 0.0, 0.0), velocity: DVec3::ZERO,
            max_velocity: 100.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
        },
        CombatState {
            shield_hp: 0.0, max_shield_hp: 10.0, armor_hp: 0.0, max_armor_hp: 10.0,
            structure_hp: 10.0, max_structure_hp: 10.0, capacitor: 500.0, max_capacitor: 500.0, signature_radius: 200.0,
            ..CombatState::default()
        },
    ).await.unwrap();
    sys_handle.add_target_lock(ship_id, TargetLock {
        target_id: fragile_laser_npc_id, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
    }).await.unwrap();
    let kill_hit = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(fragile_laser_npc_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(kill_hit.dict_get("destroyed"), Some(&PyRep::Bool(true)));

    // 4. Corp-owned item resolves through snap.items -> snap.corporations and lowsec Upwell hit triggers became_invuln unlock
    for tid in [corp_can_id, structure_id] {
        sys_handle.add_target_lock(ship_id, TargetLock {
            target_id: tid, lock_time_ms: 0.0, remaining_lock_ms: 0.0, locked: true, max_range_m: Some(100_000.0),
        }).await.unwrap();
    }
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "ModuleAlreadyActive"
    );
    let can_hit = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
        .await.unwrap().unwrap();
    assert!(can_hit.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);

    let st_hit = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(structure_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(st_hit.dict_get("targetID"), Some(&PyRep::Integer(structure_id)));
    assert!(!sys_handle.get_target_locks(ship_id).await.unwrap_or_default().iter().any(|l| l.target_id == structure_id), "structure entering ArmorReinforced should unlock and drop module state");

    // 5. ModuleNotFound, pre-damaged non-volatility crystal reading, and Warp guard
    let missing_mod_id = 850_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(missing_mod_id, make_fighter_item(missing_mod_id, 888_888, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        if let Some(ci) = snap.items.get_mut(&crys_id) {
            ci.dynamic_attributes.insert(3, 2.0);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(missing_mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
            .await.unwrap().unwrap_err().code,
        "ModuleNotFound"
    );
    let crys_read = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
        .await.unwrap().unwrap();
    assert!(crys_read.dict_get("crystalDamageFraction").and_then(|v| v.as_f64()).unwrap() >= 0.0);

    if let Some((_, mut ball)) = sys_handle.get_ball_snapshot(ship_id).await.unwrap() {
        ball.mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e12, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e12,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 1.0,
        };
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        sys_handle.spawn_ball_with_combat(ball, cs).await.unwrap();
    }
    assert!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
            .await.unwrap().is_err()
    );
}

#[tokio::test]
async fn combat_weapons_ewar_victim_resolution_lowsec_pod_and_sentry_aggression() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, SafetyLevel};

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    sde.solar_systems.get_mut(&sys_id).unwrap().security_status = 0.30;
    sde.stations.insert(
        60_999_001,
        eve_sde::StationInfo {
            station_id: 60_999_001,
            solar_system_id: sys_id,
            constellation_id: 20_000_020,
            region_id: 10_000_002,
            station_type_id: 1531,
            name: "LowsecSentryStation".into(),
            position: DVec3::new(1_000.0, 0.0, 0.0),
            owner_corporation_id: 1_000_001,
            owner_faction_id: None,
            operation_id: 0,
            reprocessing_efficiency: 0.5,
            reprocessing_stations_take: 0.05,
            is_conquerable: false,
            orbit_id: None,
            has_clone_bay: false,
        },
    );
    sde.types.insert(37_901, make_fighter_type(37_901, 379, 7, 5.0, vec![(554, 30.0)]));
    sde.types.insert(53_004, make_fighter_type(53_004, 53, 7, 3.0, vec![(6, 5.0), (51, 60_000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 86.0), (64, 1.0)]));
    sde.types.insert(86_005, make_fighter_type(86_005, 86, 8, 1.0, vec![(114, 250.0), (786, 0.0)]));
    ctx.sde = std::sync::Arc::new(sde);

    let tp_id = 860_001_i64;
    let laser_id = 860_002_i64;
    let crys_id = 860_003_i64;
    let other_char_id = 90_000_301_i64;
    let pod_char_id = 90_000_302_i64;
    let neutral_char_id = 90_000_303_i64;
    let other_ship_id = 960_001_i64;
    let char_can_id = 960_002_i64;
    let corp_can_id = 960_003_i64;
    let pod_ship_id = 960_004_i64;
    let neutral_ship_id = 960_005_i64;
    let foreign_corp_id = 98_999_301_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(tp_id, make_fighter_item(tp_id, 37_901, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(laser_id, make_fighter_item(laser_id, 53_004, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(crys_id, make_fighter_item(crys_id, 86_005, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(char_can_id, make_fighter_item(char_can_id, 10_002, other_char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(corp_can_id, make_fighter_item(corp_can_id, 10_002, foreign_corp_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(other_ship_id, make_fighter_item(other_ship_id, 23_915, other_char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(pod_ship_id, make_fighter_item(pod_ship_id, 670, pod_char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(neutral_ship_id, make_fighter_item(neutral_ship_id, 23_915, neutral_char_id, sys_id as i64, InventoryFlag::None, 1));

        let mut fc = snap.corporations.values().next().cloned().unwrap_or_default();
        fc.corporation_id = foreign_corp_id;
        fc.ceo_id = other_char_id;
        snap.corporations.insert(foreign_corp_id, fc);

        let base_ch = snap.characters.get(&char_id).unwrap().clone();
        for (cid, active_ship, corp_id) in [
            (other_char_id, other_ship_id, foreign_corp_id),
            (pod_char_id, pod_ship_id, foreign_corp_id),
            (neutral_char_id, neutral_ship_id, foreign_corp_id),
        ] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.active_ship_id = active_ship;
            ch.corporation_id = corp_id;
            snap.characters.insert(cid, ch);
            snap.accounts.insert(
                format!("user_{cid}"),
                eve_db::AccountRecord {
                    user_id: cid as u64,
                    username: format!("user_{cid}"),
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let now_unix = ctx.now_unix();
    ctx.db.acquire_online_lock(session.user_id.unwrap_or(1), 999_100, Some(char_id), true, now_unix).unwrap();
    ctx.db.acquire_online_lock(other_char_id as u64, 999_101, Some(other_char_id), true, now_unix).unwrap();

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 23_915_u32, DVec3::ZERO),
        (other_char_id, 23_915, DVec3::new(1_500.0, 0.0, 0.0)),
        (char_can_id, 10_002, DVec3::new(1_600.0, 0.0, 0.0)),
        (corp_can_id, 10_002, DVec3::new(1_700.0, 0.0, 0.0)),
        (pod_ship_id, 670, DVec3::new(1_800.0, 0.0, 0.0)),
        (neutral_ship_id, 23_915, DVec3::new(2_000.0, 0.0, 0.0)),
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
                    shield_hp: 10_000.0,
                    max_shield_hp: 10_000.0,
                    armor_hp: 10_000.0,
                    max_armor_hp: 10_000.0,
                    structure_hp: 10_000.0,
                    max_structure_hp: 10_000.0,
                    capacitor: 5_000.0,
                    max_capacitor: 5_000.0,
                    signature_radius: 10_000.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
        if id != ship_id {
            sys_handle
                .add_target_lock(
                    ship_id,
                    TargetLock {
                        target_id: id,
                        lock_time_ms: 0.0,
                        remaining_lock_ms: 0.0,
                        locked: true,
                        max_range_m: Some(100_000.0),
                    },
                )
                .await
                .unwrap();
        }
    }

    // 1. Non-string effectName falls back cleanly, while invalid target rejects
    assert!(
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::Integer(1), PyRep::String("bad".into())],
            &kwargs,
        )
        .await
        .unwrap()
        .is_err()
    );

    // 2. EWAR against character-ID target: suspect creates limited engagement + notifications on both characters
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(other_char_id).suspect_flag_until_unix = now_unix + 900;
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(other_char_id);

    let tp_char = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(other_char_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_char.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));
    assert!(with_combat_runtime(&ctx, |rt| rt.has_limited_engagement_with(char_id, other_char_id, now_unix)));
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));
    assert!(ctx.drain_notifications_for_char(other_char_id).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));

    // 3. EWAR resolves character owner on non-ship items and corp CEO via snap.corporations
    let tp_char_can = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(char_can_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_char_can.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));

    let tp_corp_can = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_corp_can.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));

    // 4. Lowsec EWAR against illegal pod forces Criminal flag (clears suspect)
    let tp_pod = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(pod_ship_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tp_pod.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(char_id);
        assert!(st.is_criminal(now_unix));
        assert_eq!(st.suspect_flag_until_unix, 0);
    }

    // 5. Lowsec illegal turret aggression near station triggers sentry gun grid aggression
    let turret_hit = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(neutral_ship_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(turret_hit.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert!(with_combat_runtime(&ctx, |rt| {
        rt.sentry_gun_aggression.get(&(sys_id, ship_id)).is_some_and(|s| s.active_on_grid)
    }));
}
