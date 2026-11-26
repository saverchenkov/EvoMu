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

pub(in crate::services) fn enable_test_overload(
    sde: &mut eve_sde::SdeRegistry,
    ctx: &ServerContext,
    char_id: i64,
    type_ids: &[u32],
) {
    let cat5_eid = sde
        .effects
        .values()
        .find(|e| e.effect_category == 5)
        .map(|e| e.effect_id)
        .unwrap_or_else(|| {
            let mut eff = sde.effects.values().next().cloned().expect("seed effects");
            eff.effect_id = 998;
            eff.name = "testOverloadEffect".into();
            eff.effect_category = 5;
            sde.effects.insert(998, eff);
            998
        });
    for tid in type_ids {
        if let Some(t) = sde.types.get_mut(tid) {
            if !t.effect_ids.contains(&cat5_eid) {
                t.effect_ids.push(cat5_eid);
            }
        }
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord {
                type_id: 28_164,
                level: 1,
                skill_points: 250.0,
            },
        );
    });
}

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
    enable_test_overload(&mut sde, &ctx, char_id, &[53_001]);
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

    ctx.clear_all_reactivation_delays();
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
    ctx.clear_all_reactivation_delays();
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
    use crate::services::combat_crimewatch::{
        set_char_safety_level, with_mut_combat_runtime, SafetyLevel,
    };
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
    enable_test_overload(&mut sde, &ctx, char_id, &[55_001, 53_002, 19_861]);
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
    let mut suspect_cs = sys_handle.get_combat_state(pvp_suspect_id).await.unwrap().unwrap();
    suspect_cs.apply_jump_cloak_and_invuln_timers(60);
    sys_handle.set_combat_state(pvp_suspect_id, suspect_cs.clone()).await.unwrap();
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
    suspect_cs.clear_action_invuln();
    sys_handle.set_combat_state(pvp_suspect_id, suspect_cs).await.unwrap();

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
    ctx.clear_all_reactivation_delays();
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
    ctx.clear_all_reactivation_delays();
    let fac_shot2 = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(fac_laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(fac_shot2.dict_get("crystalShattered"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let rem = &snap.items[&fac_crys_id];
        assert_eq!(rem.quantity, 1);
        assert_eq!(rem.dynamic_attributes.get(&attr_ids::DAMAGE).copied(), None);
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
    ctx.clear_all_reactivation_delays();
    let ent2 = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(ent_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(ent2.dict_get("spoolCycles"), Some(&PyRep::Integer(0)));
    assert_eq!(ent2.dict_get("spoolMultiplier"), Some(&PyRep::Real(1.0)));

    // 6. Vorton Projector arc chaining under SafetyLevel::Yellow (skips cloaked & neutral pod; destroys fragile NPC; hits suspect pilot)
    set_char_safety_level(&ctx, char_id, SafetyLevel::Yellow);
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
    use crate::services::combat_crimewatch::{set_char_safety_level, SafetyLevel};

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
    enable_test_overload(&mut sde, &ctx, char_id, &[37_903, 53_003]);
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
    set_char_safety_level(&ctx, char_id, SafetyLevel::Yellow);
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
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    let _ = ctx.drain_notifications_for_char(char_id);
    let _ = ctx.drain_notifications_for_char(susp_char);
    let le_shot = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(susp_ship_id)], &kwargs)
        .await.unwrap().unwrap();
    assert!(le_shot.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);
    assert!(ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));
    assert!(ctx.drain_notifications_for_char(susp_char).iter().any(|n| n.event == "OnCrimewatchEngagementCreated"));

    // 4. Red safety shooting neutral pod in lowsec (0.0 < 0.30 < 0.45) escalates to Criminal (suspect cleared!)
    ctx.clear_all_reactivation_delays();
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
    ctx.clear_all_reactivation_delays();
    let npc_shot = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)], &kwargs)
        .await.unwrap().unwrap();
    assert_eq!(npc_shot.dict_get("targetID"), Some(&PyRep::Integer(npc_id)));
}

#[tokio::test]
async fn combat_weapons_auto_target_fof_destruction_upwell_and_salvager_edges() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{set_char_safety_level, SafetyLevel};

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
    enable_test_overload(&mut sde, &ctx, char_id, &[53_004]);
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
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
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
    ctx.clear_all_reactivation_delays();
    let can_hit = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
        .await.unwrap().unwrap();
    assert!(can_hit.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap() > 0.0);

    ctx.clear_all_reactivation_delays();
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
    ctx.clear_all_reactivation_delays();
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
    ctx.clear_all_reactivation_delays();
    assert!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id), PyRep::String("targetAttack".into()), PyRep::Integer(corp_can_id)], &kwargs)
            .await.unwrap().is_err()
    );
}

#[tokio::test]
async fn combat_weapons_ewar_victim_resolution_lowsec_pod_and_sentry_aggression() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{set_char_safety_level, with_combat_runtime, SafetyLevel};

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
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
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
    assert!(crate::services::combat_crimewatch::has_limited_engagement_with(&ctx, char_id, other_char_id, now_unix));
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

#[tokio::test]
async fn weapon_ammo_depletion_one_to_zero_emits_item_tombstone_and_sublocation_zero_gh1124() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        55_901,
        make_fighter_type(
            55_901,
            55,
            7,
            5.0,
            vec![
                (51, 1_000.0),
                (54, 15_000.0),
                (64, 1.5),
                (128, 1.0),
                (158, 5_000.0),
                (160, 100.0),
                (604, 83.0),
            ],
        ),
    );
    sde.types.insert(
        83_901,
        make_fighter_type(
            83_901,
            83,
            8,
            0.0025,
            vec![(114, 100.0), (118, 50.0), (128, 1.0)],
        ),
    );
    sde.types.insert(
        10_099,
        make_fighter_type(
            10_099,
            550,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 10_000.0)],
        ),
    );
    ctx.sde = std::sync::Arc::new(sde);

    let turret_id = 830_001_i64;
    let ammo_id = 830_002_i64;
    let npc_id = 930_099_i64;

    ctx.db
        .acquire_online_lock(session.user_id.unwrap_or(1), 999_200, Some(char_id), true, ctx.now_unix())
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            turret_id,
            make_fighter_item(turret_id, 55_901, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        let mut ammo_rec = make_fighter_item(ammo_id, 83_901, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        ammo_rec.singleton = false;
        snap.items.insert(ammo_id, ammo_rec);
        snap.module_active_charges.insert(turret_id, 83_901);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 23_915_u32, DVec3::ZERO),
        (npc_id, 10_099_u32, DVec3::new(3_000.0, 0.0, 0.0)),
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
                    shield_hp: 1_000.0,
                    max_shield_hp: 1_000.0,
                    armor_hp: 1_000.0,
                    max_armor_hp: 1_000.0,
                    structure_hp: 1_000.0,
                    max_structure_hp: 1_000.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 120.0,
                    base_signature_radius: Some(120.0),
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
                max_range_m: Some(50_000.0),
            },
        )
        .await
        .unwrap();
    let _ = ctx.drain_notifications_for_char(char_id);

    let _res = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(turret_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(npc_id),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("weapon fire consumes last round");

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&ammo_id), "last round item must be removed from snap.items");
        assert!(
            !snap.module_active_charges.contains_key(&turret_id),
            "depleted module_active_charges entry must be cleared"
        );
    });

    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("itemID") == Some(&PyRep::Integer(ammo_id))
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("flagID") == Some(&PyRep::Integer(0))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "1 -> 0 round depletion must emit OnItemChange tombstone"
    );
    assert!(
        notifs.iter().any(|n| {
            n.event == "OnModuleAttributeChanges"
                && matches!(n.args.first(), Some(PyRep::List(rows)) if rows.iter().any(|r| {
                    matches!(r, PyRep::Tuple(cols)
                        if cols.get(3) == Some(&PyRep::Integer(805))
                            && cols.get(5) == Some(&PyRep::Real(0.0))
                            && cols.get(6) == Some(&PyRep::Real(1.0)))
                }))
        }),
        "1 -> 0 round depletion must emit OnModuleAttributeChanges(805: 1.0 -> 0.0)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handle_weapons_and_fastpath_enforce_skill_prereqs_and_gated_autoload_gh1124() {
    use crate::services::combat_crimewatch::{
        set_char_safety_level, with_combat_runtime, SafetyLevel,
    };
    use crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();
    let victim_char = 90_009_124_i64;
    let victim_ship = 200_091_124_i64;
    let far_npc = 930_912_124_i64;

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        19_868,
        make_fighter_type(
            19_868,
            1986,
            7,
            5.0,
            vec![
                (6, 20.0),
                (51, 3_000.0),
                (54, 10_000.0),
                (158, 2_000.0),
                (160, 1.0),
                (604, 1987.0),
                (128, 1.0),
                (620, 40_000.0),
                (64, 1.0),
                (2733, 0.10),
                (2734, 1.50),
            ],
        ),
    );
    sde.types.insert(
        19_878,
        make_fighter_type(19_878, 1987, 8, 0.01, vec![(117, 50.0), (118, 50.0), (128, 1.0)]),
    );
    sde.types.insert(
        19_879,
        make_fighter_type(
            19_879,
            1987,
            8,
            0.01,
            vec![(117, 80.0), (118, 80.0), (128, 1.0), (182, 12_210.0), (277, 1.0)],
        ),
    );
    sde.types.insert(
        40_608,
        make_fighter_type(
            40_608,
            4060,
            7,
            5.0,
            vec![(6, 20.0), (51, 3_000.0), (54, 10_000.0), (158, 2_000.0), (604, 4061.0), (128, 1.0), (64, 1.0)],
        ),
    );
    sde.types.insert(
        40_619,
        make_fighter_type(
            40_619,
            4061,
            8,
            0.01,
            vec![(114, 60.0), (117, 60.0), (128, 1.0), (182, 12_210.0), (277, 1.0)],
        ),
    );
    sde.types.insert(
        86_208,
        make_fighter_type(
            86_208,
            862,
            7,
            75.0,
            vec![(51, 5_000.0), (604, 90.0), (128, 1.0)],
        ),
    );
    sde.types.insert(
        90_008,
        make_fighter_type(90_008, 90, 8, 75.0, vec![(114, 500.0), (37, 2_500.0), (281, 12_000.0), (99, 15_000.0), (654, 400.0)]),
    );
    sde.types.insert(
        90_009,
        make_fighter_type(
            90_009,
            90,
            8,
            75.0,
            vec![(114, 750.0), (182, 12_210.0), (277, 1.0)],
        ),
    );
    sde.types.insert(
        10_098,
        make_fighter_type(10_098, 550, 11, 1_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 10_000.0)]),
    );
    ctx.sde = std::sync::Arc::new(sde);

    ctx.db
        .acquire_online_lock(session.user_id.unwrap_or(1), 999_224, Some(char_id), true, ctx.now_unix())
        .unwrap();

    let entropic_preloaded_id = 831_001_i64;
    let entropic_t2_slot_ammo = 831_002_i64;
    let vorton_preloaded_id = 831_003_i64;
    let vorton_t2_slot_ammo = 831_004_i64;
    let entropic_auto_id = 831_005_i64;
    let cargo_t2_ammo = 831_006_i64;
    let cargo_t1_ammo = 831_007_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let tmpl = snap.characters[&char_id].clone();
        let mut vch = tmpl;
        vch.character_id = victim_char;
        vch.user_id = victim_char as u64;
        vch.active_ship_id = victim_ship;
        vch.solar_system_id = sys_id;
        vch.station_id = None;
        vch.structure_id = None;
        snap.characters.insert(victim_char, vch);
        snap.items.insert(
            victim_ship,
            make_fighter_item(victim_ship, 23_915, victim_char, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            entropic_preloaded_id,
            make_fighter_item(entropic_preloaded_id, 19_868, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        let mut e_pre = make_fighter_item(entropic_t2_slot_ammo, 19_879, char_id, ship_id, InventoryFlag::HiSlot0, 5);
        e_pre.singleton = false;
        snap.items.insert(entropic_t2_slot_ammo, e_pre);

        snap.items.insert(
            vorton_preloaded_id,
            make_fighter_item(vorton_preloaded_id, 40_608, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        let mut v_pre = make_fighter_item(vorton_t2_slot_ammo, 40_619, char_id, ship_id, InventoryFlag::HiSlot1, 5);
        v_pre.singleton = false;
        snap.items.insert(vorton_t2_slot_ammo, v_pre);

        snap.items.insert(
            entropic_auto_id,
            make_fighter_item(entropic_auto_id, 19_868, char_id, ship_id, InventoryFlag::HiSlot2, 1),
        );
        let mut c_t2 = make_fighter_item(cargo_t2_ammo, 19_879, char_id, ship_id, InventoryFlag::Cargo, 10);
        c_t2.singleton = false;
        snap.items.insert(cargo_t2_ammo, c_t2);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, cap) in [
        (ship_id, 23_915_u32, DVec3::ZERO, 500.0),
        (victim_ship, 23_915_u32, DVec3::new(3_000.0, 0.0, 0.0), 500.0),
        (far_npc, 10_098_u32, DVec3::new(50_000.0, 0.0, 0.0), 500.0),
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
                    signature_radius: 120.0,
                    base_signature_radius: Some(120.0),
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    for tid in [victim_ship, far_npc] {
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
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    let _ = ctx.drain_notifications_for_char(char_id);

    // 1. Preloaded untrained T2 charge on Entropic fast-path (`handle_module_cycles -> handle_weapons`) -> SkillPrerequisitesNotMet
    let err_ent_pre = handle_module_cycles(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_preloaded_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)],
        &kwargs,
    )
    .await
    .expect("Some")
    .expect_err("untrained preloaded Entropic charge must fail");
    assert_eq!(err_ent_pre.code, "SkillPrerequisitesNotMet");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&entropic_t2_slot_ammo].quantity), 5);
    assert!(with_combat_runtime(&ctx, |rt| !rt.entropic_spools.contains_key(&(ship_id, entropic_preloaded_id))));

    // 2. Preloaded untrained T2 charge on Vorton `handle_weapons` -> SkillPrerequisitesNotMet
    let err_vor_pre = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(vorton_preloaded_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)],
        &kwargs,
    )
    .await
    .expect("Some")
    .expect_err("untrained preloaded Vorton charge must fail");
    assert_eq!(err_vor_pre.code, "SkillPrerequisitesNotMet");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&vorton_t2_slot_ammo].quantity), 5);

    // 3. Empty Entropic gun with ONLY untrained T2 ammo in Cargo -> SkillPrerequisitesNotMet, zero mutation
    let err_ent_auto_untrained = handle_module_cycles(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_auto_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)],
        &kwargs,
    )
    .await
    .expect("Some")
    .expect_err("untrained cargo Entropic autoload must fail");
    assert_eq!(err_ent_auto_untrained.code, "SkillPrerequisitesNotMet");
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&cargo_t2_ammo].quantity), 10);

    // 4. Add trained T1 ammo alongside untrained T2 ammo; precondition failures must NEVER mutate cargo/slot/notifications
    ctx.db.with_mut_snapshot(|snap| {
        let mut c_t1 = make_fighter_item(cargo_t1_ammo, 19_878, char_id, ship_id, InventoryFlag::Cargo, 10);
        c_t1.singleton = false;
        snap.items.insert(cargo_t1_ammo, c_t1);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    let assert_clean_entropic = |label: &str| {
        ctx.db.with_snapshot(|s| {
            assert_eq!(s.items.get(&cargo_t1_ammo).map(|i| i.quantity), Some(10), "{label}: cargo T1 stack untouched");
            assert!(
                !s.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot2 && i.item_id != entropic_auto_id),
                "{label}: HiSlot2 must remain empty"
            );
            assert!(!s.module_active_charges.contains_key(&entropic_auto_id), "{label}: no active charge recorded");
        });
        assert!(with_combat_runtime(&ctx, |rt| !rt.entropic_spools.contains_key(&(ship_id, entropic_auto_id))), "{label}: no spool started");
        let notifs = ctx.drain_notifications_for_char(char_id);
        assert!(
            !notifs.iter().any(|n| matches!(n.event.as_str(), "OnItemChange" | "OnGodmaPrimeItem" | "OnModuleAttributeChanges" | "OnChargeLoadedToModule")),
            "{label}: zero reload notifications allowed, got {:?}",
            notifs.iter().map(|n| n.event.as_str()).collect::<Vec<_>>()
        );
    };

    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(entropic_auto_id), PyRep::String("targetAttack".into()), PyRep::Integer(999_888_777)], &kwargs)
            .await.expect("Some").expect_err("unlocked target").code,
        "TargetNotLocked"
    );
    assert_clean_entropic("TargetNotLocked");

    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(entropic_auto_id), PyRep::String("targetAttack".into()), PyRep::Integer(far_npc)], &kwargs)
            .await.expect("Some").expect_err("out of range").code,
        "TargetOutOfRange"
    );
    assert_clean_entropic("TargetOutOfRange");

    sys_handle.set_capacitor(ship_id, 0.0).await.unwrap();
    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(entropic_auto_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)], &kwargs)
            .await.expect("Some").expect_err("zero cap").code,
        "NotEnoughCapacitor"
    );
    assert_clean_entropic("NotEnoughCapacitor");

    sys_handle.set_capacitor(ship_id, 500.0).await.unwrap();
    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(entropic_auto_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)], &kwargs)
            .await.expect("Some").expect_err("Safety Green in Jita").code,
        "CantDoInCurrentSafetyLevel"
    );
    assert_clean_entropic("CantDoInCurrentSafetyLevel");

    // 5. All preconditions pass -> silently autoloads trained T1 stack, consumes 1 round, starts spool cleanly
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    handle_module_cycles(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_auto_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)],
        &kwargs,
    )
    .await
    .expect("Some")
    .expect("Entropic activation with valid preconditions must silently autoload and fire");

    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&cargo_t1_ammo), "trained T1 cargo stack moved into HiSlot2");
        assert_eq!(s.items[&cargo_t2_ammo].quantity, 10, "untrained T2 stack untouched");
        let slot_charge = s
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot2 && i.item_id != entropic_auto_id)
            .expect("T1 charge loaded into HiSlot2");
        assert_eq!(slot_charge.type_id, 19_878);
        assert_eq!(slot_charge.quantity, 9, "10 loaded then 1 round fired -> 9 remaining");
        assert!(with_combat_runtime(&ctx, |rt| {
            rt.entropic_spools.get(&(ship_id, entropic_auto_id)).and_then(|st| st.charge_item_id) == Some(slot_charge.item_id)
        }));
    });
    let ok_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(ok_notifs.iter().any(|n| n.event == "OnGodmaPrimeItem"));
    assert!(ok_notifs.iter().any(|n| n.event == "OnModuleAttributeChanges"));
    assert!(ok_notifs.iter().any(|n| n.event == "OnChargeLoadedToModule"));

    // 6. Bomb Launcher (group 862) fast-path: enforces skill prereqs + gates autoload behind highsec/safety preconditions
    let bomb_launcher_id = 831_008_i64;
    let bomb_untrained_cargo_id = 831_009_i64;
    let bomb_trained_cargo_id = 831_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            bomb_launcher_id,
            make_fighter_item(bomb_launcher_id, 86_208, char_id, ship_id, InventoryFlag::HiSlot3, 1),
        );
        let mut b_untrained = make_fighter_item(bomb_untrained_cargo_id, 90_009, char_id, ship_id, InventoryFlag::Cargo, 1);
        b_untrained.singleton = false;
        snap.items.insert(bomb_untrained_cargo_id, b_untrained);
    });
    ctx.invalidate_ship_fitting(ship_id);
    let _ = ctx.drain_notifications_for_char(char_id);

    // Untrained bomb in cargo rejected in lowsec with Red safety
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(sys_id, 0.2);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &kwargs)
            .await
            .expect("Some")
            .expect_err("untrained cargo bomb must fail")
            .code,
        "SkillPrerequisitesNotMet"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&bomb_untrained_cargo_id].quantity), 1);

    // Add trained bomb in cargo; highsec & Green safety must fail BEFORE autoloading or mutating cargo
    ctx.db.with_mut_snapshot(|snap| {
        let mut b_trained = make_fighter_item(bomb_trained_cargo_id, 90_008, char_id, ship_id, InventoryFlag::Cargo, 1);
        b_trained.singleton = false;
        snap.items.insert(bomb_trained_cargo_id, b_trained);
        snap.combat_crimewatch_state
            .modified_system_securities
            .remove(&sys_id);
    });
    let _ = ctx.drain_notifications_for_char(char_id);
    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &kwargs)
            .await
            .expect("Some")
            .expect_err("highsec blocks bomb before autoload")
            .code,
        "BombDisallowedInHighSec"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&bomb_trained_cargo_id].quantity), 1);

    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(sys_id, 0.2);
    });
    assert_eq!(
        handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &kwargs)
            .await
            .expect("Some")
            .expect_err("Green safety blocks bomb in lowsec before autoload")
            .code,
        "CantDoInCurrentSafetyLevel"
    );
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&bomb_trained_cargo_id].quantity), 1);
    assert!(ctx.drain_notifications_for_char(char_id).is_empty(), "precondition failures must emit zero reload notifications");

    // Lowsec + Red safety -> autoloads 1 trained bomb, launches it, consumes 1 -> 0 with tombstone
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(bomb_launcher_id)], &kwargs)
        .await
        .expect("Some")
        .expect("lowsec Red safety autoloads and launches trained bomb");
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&bomb_trained_cargo_id), "trained bomb moved and consumed");
        assert!(
            !s.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot3 && i.item_id != bomb_launcher_id),
            "1 -> 0 bomb depletion removes charge item from slot"
        );
    });
    let bomb_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        bomb_notifs.iter().any(|n| {
            n.event == "OnItemChange"
                && n.args[0].dbrow_get("locationID") == Some(&PyRep::Integer(6))
                && n.args[0].dbrow_get("quantity") == Some(&PyRep::Integer(0))
        }),
        "bomb 1 -> 0 depletion must emit OnItemChange tombstone"
    );
}

#[tokio::test]
async fn handle_weapons_uses_surface_distance_for_non_upwell_titan_vs_concord_1247() {
    use crate::services::combat_crimewatch::{set_char_safety_level, SafetyLevel};
    use eve_sde::attr_ids;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    if let Some(sys_rec) = sde.solar_systems.get_mut(&sys_id) {
        sys_rec.security_status = -0.17;
    }
    let mut avatar = make_fighter_type(
        11_567,
        30,
        6,
        10_000_000.0,
        vec![
            (attr_ids::HI_SLOTS, 8.0),
            (attr_ids::TURRET_SLOTS_LEFT, 8.0),
            (attr_ids::CPU_OUTPUT, 10_000.0),
            (attr_ids::POWER_OUTPUT, 1_000_000.0),
            (attr_ids::CAPACITOR_CAPACITY, 10_000.0),
            (attr_ids::MAX_LOCKED_TARGETS, 7.0),
            (attr_ids::MAX_TARGET_RANGE, 200_000.0),
            (attr_ids::SIGNATURE_RADIUS, 16_000.0),
        ],
    );
    avatar.radius = 6_800.0;
    avatar.mass = 2_400_000_000.0;
    sde.types.insert(11_567, avatar);

    let mut civ_laser = make_fighter_type(
        3_634,
        53,
        7,
        5.0,
        vec![
            (6, 1.0),
            (51, 2_000.0),
            (54, 3_300.0),
            (64, 1.3),
            (114, 3.0),
            (116, 0.0),
            (117, 0.0),
            (118, 2.0),
            (158, 1_000.0),
            (160, 625.0),
            (620, 40_000.0),
            (1795, 0.01),
        ],
    );
    civ_laser.capacity = 0.0;
    civ_laser.effect_ids = vec![10, 12, 16, 42];
    civ_laser.default_effect_id = Some(10);
    sde.types.insert(3_634, civ_laser);

    let mut concord_t = make_fighter_type(
        10_035,
        182,
        11,
        1_000.0,
        vec![(attr_ids::SIGNATURE_RADIUS, 25.0)],
    );
    concord_t.radius = 38.0;
    sde.types.insert(10_035, concord_t);
    ctx.sde = std::sync::Arc::new(sde);

    let civ_laser_id = 832_001_i64;
    let concord_oob_id = 932_001_i64;
    let concord_hit_id = 932_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .entry(ship_id)
            .and_modify(|it| it.type_id = 11_567)
            .or_insert_with(|| {
                make_fighter_item(ship_id, 11_567, char_id, sys_id as i64, InventoryFlag::None, 1)
            });
        snap.items.insert(
            civ_laser_id,
            make_fighter_item(civ_laser_id, 3_634, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let now_unix = ctx.now_unix();
    let _ = ctx.db.acquire_online_lock(
        session.user_id.unwrap_or(1),
        999_247,
        Some(char_id),
        true,
        now_unix,
    );
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 11_567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 60.0,
                inertia: 1.0,
                mass: 2_400_000_000.0,
                radius: 6_800.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 100_000.0,
                max_shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                max_armor_hp: 100_000.0,
                structure_hp: 100_000.0,
                max_structure_hp: 100_000.0,
                capacitor: 10_000.0,
                max_capacitor: 10_000.0,
                signature_radius: 16_000.0,
                ..CombatState::default()
            },
            eve_destiny::SlimItemMeta {
                group_id: 30,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 98_000_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for (tid, pos) in [
        (concord_oob_id, DVec3::new(13_300.0, 0.0, 0.0)), // surface = 6,450m > 6,300m
        (concord_hit_id, DVec3::new(6_855.5, 0.0, 0.0)),  // surface = 5.5m (center = 6,855.5m)
    ] {
        sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: tid,
                    type_id: 10_035,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 2_500.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 500.0,
                    max_shield_hp: 500.0,
                    armor_hp: 550.0,
                    max_armor_hp: 550.0,
                    structure_hp: 500.0,
                    max_structure_hp: 500.0,
                    signature_radius: 25.0,
                    shield_recharge_time_ms: 0.0,
                    capacitor_recharge_time_ms: 0.0,
                    ..CombatState::default()
                },
                eve_destiny::SlimItemMeta {
                    group_id: 182,
                    category_id: 11,
                    owner_id: 1_000_125,
                    char_id: None,
                    corp_id: 1_000_125,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
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

    // 1. Out-of-range beyond surface cutoff 6300m rejects with TargetOutOfRange.
    let oob_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(civ_laser_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(concord_oob_id),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(oob_err.code, "TargetOutOfRange");

    // 2. Non-Upwell CONCORD NPC at 5.5m surface distance on 6800m Titan must hit cleanly.
    let _ = ctx.drain_notifications_for_char(char_id);
    let shot = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(civ_laser_id),
            PyRep::String("targetAttack".into()),
            PyRep::Integer(concord_hit_id),
        ],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("shot against CONCORD at 5.5m surface distance must succeed");

    let hit_chance = shot.dict_get("hitChance").and_then(|v| v.as_f64()).unwrap();
    let applied_dmg = shot.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap();
    assert!(hit_chance > 0.99, "surface distance 5.5m within 3300m optimal must have full hit chance (got {hit_chance})");
    assert!(applied_dmg > 0.0, "expected positive damage against CONCORD (got {applied_dmg})");

    let hit_cs = sys_handle.get_combat_state(concord_hit_id).await.unwrap().unwrap();
    assert!(
        hit_cs.shield_hp < 500.0,
        "CONCORD target shield must drop after Civilian Pulse Laser hit (shield={})",
        hit_cs.shield_hp
    );
    let notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        notifs.iter().any(|n| n.event == "OnDamageMessage"),
        "expected OnDamageMessage notification on handle_weapons hit"
    );
    assert!(
        notifs.iter().any(|n| n.event == "OnDamageMessages"),
        "expected OnDamageMessages notification on handle_weapons hit"
    );
}

async fn setup_titan_sde_and_item_1247(
    ctx: &mut ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    extra_types: Vec<eve_sde::InvType>,
) {
    use eve_sde::attr_ids;
    let mut sde = (*ctx.sde).clone();
    if let Some(sys_rec) = sde.solar_systems.get_mut(&sys_id) {
        sys_rec.security_status = -0.17;
    }
    let mut avatar = make_fighter_type(
        11_567,
        30,
        6,
        10_000_000.0,
        vec![
            (attr_ids::MED_SLOTS, 8.0),
            (attr_ids::HI_SLOTS, 8.0),
            (attr_ids::TURRET_SLOTS_LEFT, 8.0),
            (attr_ids::LAUNCHER_SLOTS_LEFT, 8.0),
            (attr_ids::CPU_OUTPUT, 10_000.0),
            (attr_ids::POWER_OUTPUT, 1_000_000.0),
            (attr_ids::CAPACITOR_CAPACITY, 10_000.0),
            (attr_ids::MAX_LOCKED_TARGETS, 7.0),
            (attr_ids::MAX_TARGET_RANGE, 200_000.0),
            (attr_ids::SIGNATURE_RADIUS, 16_000.0),
        ],
    );
    avatar.radius = 6_800.0;
    sde.types.insert(11_567, avatar);
    for t in extra_types {
        sde.types.insert(t.type_id, t);
    }
    ctx.sde = std::sync::Arc::new(sde);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .entry(ship_id)
            .and_modify(|it| it.type_id = 11_567)
            .or_insert_with(|| {
                make_fighter_item(ship_id, 11_567, char_id, sys_id as i64, InventoryFlag::None, 1)
            });
    });
    crate::services::combat_crimewatch::set_char_safety_level(ctx, char_id, SafetyLevel::Red);
}

async fn spawn_titan_ball_1247(
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
) {
    sys_handle
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: 11_567,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 60.0,
                inertia: 1.0,
                mass: 2_400_000_000.0,
                radius: 6_800.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 100_000.0,
                max_shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                max_armor_hp: 100_000.0,
                structure_hp: 100_000.0,
                max_structure_hp: 100_000.0,
                capacitor: 1_000.0,
                max_capacitor: 1_000.0,
                signature_radius: 16_000.0,
                ..CombatState::default()
            },
            eve_destiny::SlimItemMeta {
                group_id: 30,
                category_id: 6,
                owner_id: char_id,
                char_id: Some(char_id),
                corp_id: 98_000_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();
}

async fn lock_target_1247(
    sys_handle: &crate::system_actor::SystemHandle,
    ship_id: i64,
    target_id: i64,
) {
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(100_000.0),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn handle_weapons_vorton_titan_strict_optimal_no_turret_roll_and_surface_arc_1247() {
    use eve_sde::attr_ids;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    // Vorton Projector (`4060`) intentionally omits `attr 158` (falloff) and sets near-zero turret tracking (`160 = 1e-6`).
    setup_titan_sde_and_item_1247(
        &mut ctx,
        char_id,
        ship_id,
        sys_id,
        vec![
            make_fighter_type(
                40_601,
                4060,
                7,
                5.0,
                vec![
                    (6, 20.0),
                    (51, 3_000.0),
                    (54, 10_000.0),
                    (64, 1.0),
                    (160, 1e-6),
                    (604, 4061.0),
                    (620, 40_000.0),
                    (653, 1_000.0),
                    (654, 40.0),
                    (1353, 0.40),
                    (3036, 5_000.0),
                    (3037, 4.0),
                ],
            ),
            make_fighter_type(40_611, 4061, 8, 0.01, vec![(114, 120.0), (117, 120.0)]),
            make_fighter_type(10_050, 182, 11, 1_000.0, vec![(attr_ids::SIGNATURE_RADIUS, 16_000.0)]),
        ],
    )
    .await;

    let vorton_id = 833_001_i64;
    let vorton_ammo_id = 833_002_i64;
    let npc_oob = 933_101_i64;
    let npc_primary = 933_102_i64;
    let npc_arc_hit = 933_103_i64;
    let npc_arc_oob = 933_104_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(vorton_id, make_fighter_item(vorton_id, 40_601, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(vorton_ammo_id, make_fighter_item(vorton_ammo_id, 40_611, char_id, ship_id, InventoryFlag::HiSlot0, 5));
        snap.module_online_states.insert(vorton_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_titan_ball_1247(&sys_handle, char_id, ship_id).await;

    for (tid, pos, vel, radius) in [
        // 10_150m surface distance on opposite side from primary (> 10_000m strict optimal, < 25_000m phantom falloff, well clear of primary's 5_000m arc)
        (npc_oob, DVec3::new(-17_000.0, 0.0, 0.0), DVec3::ZERO, 50.0),
        // 8_400m surface distance (22_000m center) + transverse velocity so turret tracking 1e-6 would fail
        (npc_primary, DVec3::new(22_000.0, 0.0, 0.0), DVec3::new(0.0, 150.0, 0.0), 6_800.0),
        // 2_400m surface distance from primary (16_000m center > 5_000m arc_range)
        (npc_arc_hit, DVec3::new(38_000.0, 0.0, 0.0), DVec3::ZERO, 6_800.0),
        // 5_900m surface distance from primary (> 5_000m arc_range)
        (npc_arc_oob, DVec3::new(41_500.0, 0.0, 0.0), DVec3::ZERO, 6_800.0),
    ] {
        sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: tid,
                    type_id: 10_050,
                    position: pos,
                    velocity: vel,
                    max_velocity: vel.length(),
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius,
                    speed_fraction: if vel.length() > 0.0 { 1.0 } else { 0.0 },
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 5_000.0,
                    max_shield_hp: 5_000.0,
                    armor_hp: 5_000.0,
                    max_armor_hp: 5_000.0,
                    structure_hp: 5_000.0,
                    max_structure_hp: 5_000.0,
                    signature_radius: 16_000.0,
                    base_signature_radius: Some(16_000.0),
                    ..CombatState::default()
                },
                eve_destiny::SlimItemMeta {
                    group_id: 182,
                    category_id: 11,
                    owner_id: 1_000_125,
                    corp_id: 1_000_125,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    for tid in [npc_oob, npc_primary] {
        lock_target_1247(&sys_handle, ship_id, tid).await;
    }

    // 1. Strict optimal cutoff: 10_150m surface fails without phantom 5_000m falloff, preserving cap & ammo
    let oob_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(vorton_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_oob)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(oob_err.code, "TargetOutOfRange");
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 1_000.0).abs() < 1e-6);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&vorton_ammo_id).map(|i| i.quantity)), Some(5));

    // 2. Primary at 8_400m surface skips turret tracking/wrecking roll and chains via surface distance
    let shot = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(vorton_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_primary)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("Vorton within surface optimal must hit regardless of turret tracking");
    assert_eq!(shot.dict_get("hitChance"), Some(&PyRep::Real(1.0)));
    assert_eq!(shot.dict_get("isWreckingShot"), Some(&PyRep::Bool(false)));
    assert_eq!(shot.dict_get("secondaryTargetsHit"), Some(&PyRep::Integer(1)));
    assert!(shot.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 980.0).abs() < 1e-6);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&vorton_ammo_id).map(|i| i.quantity)), Some(4));
    assert!(sys_handle.get_combat_state(npc_primary).await.unwrap().unwrap().shield_hp < 5_000.0);
    assert!(sys_handle.get_combat_state(npc_arc_hit).await.unwrap().unwrap().shield_hp < 5_000.0);
    assert!((sys_handle.get_combat_state(npc_arc_oob).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn handle_weapons_missile_and_breacher_surface_spawn_and_oor_no_ammo_burn_1247() {
    use eve_sde::attr_ids;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    setup_titan_sde_and_item_1247(
        &mut ctx,
        char_id,
        ship_id,
        sys_id,
        vec![
            make_fighter_type(50_901, 509, 7, 5.0, vec![(51, 3_000.0), (604, 384.0)]),
            make_fighter_type(38_401, 384, 8, 0.015, vec![(37, 4_000.0), (281, 2_000.0), (9, 75.0), (117, 150.0), (653, 170.0), (654, 40.0), (1353, 0.6)]),
            make_fighter_type(45_001, 4500, 7, 5.0, vec![(51, 3_000.0), (604, 4070.0)]),
            make_fighter_type(85_001, 4070, 8, 0.1, vec![(37, 4_000.0), (281, 2_000.0), (9, 75.0), (5735, 10_000.0), (5736, 250.0), (5737, 5.0)]),
            make_fighter_type(10_050, 182, 11, 1_000.0, vec![(attr_ids::SIGNATURE_RADIUS, 16_000.0)]),
        ],
    )
    .await;

    let lml_id = 834_001_i64;
    let lml_ammo_id = 834_002_i64;
    let brl_id = 834_003_i64;
    let brl_ammo_id = 834_004_i64;
    let target_oob = 934_101_i64;
    let target_hit = 934_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(lml_id, make_fighter_item(lml_id, 50_901, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(lml_ammo_id, make_fighter_item(lml_ammo_id, 38_401, char_id, ship_id, InventoryFlag::HiSlot0, 5));
        snap.items.insert(brl_id, make_fighter_item(brl_id, 45_001, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(brl_ammo_id, make_fighter_item(brl_ammo_id, 85_001, char_id, ship_id, InventoryFlag::HiSlot1, 5));
        snap.module_online_states.insert(lml_id, true);
        snap.module_online_states.insert(brl_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_titan_ball_1247(&sys_handle, char_id, ship_id).await;

    for (tid, pos) in [
        // 8_400m surface distance (> 8_000m max_flight_range)
        (target_oob, DVec3::new(22_000.0, 0.0, 0.0)),
        // 6_000m surface distance (<= 8_000m max_flight_range, center 19_600m > 8_000m)
        (target_hit, DVec3::new(19_600.0, 0.0, 0.0)),
    ] {
        sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: tid,
                    type_id: 10_050,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 2_400_000_000.0,
                    radius: 6_800.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 100_000.0,
                    max_shield_hp: 100_000.0,
                    armor_hp: 100_000.0,
                    max_armor_hp: 100_000.0,
                    structure_hp: 100_000.0,
                    max_structure_hp: 100_000.0,
                    signature_radius: 16_000.0,
                    base_signature_radius: Some(16_000.0),
                    ..CombatState::default()
                },
                eve_destiny::SlimItemMeta {
                    group_id: 182,
                    category_id: 11,
                    owner_id: 1_000_125,
                    corp_id: 1_000_125,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        lock_target_1247(&sys_handle, ship_id, tid).await;
    }

    // 1. OOR guided missile -> TargetTooFar without burning ammo
    let lml_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(lml_id), PyRep::String("useMissiles".into()), PyRep::Integer(target_oob)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(lml_err.code, "TargetTooFar");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&lml_ammo_id).map(|i| i.quantity)), Some(5));

    // 2. OOR Breacher Pod -> TargetOutOfRange without burning ammo
    let brl_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(brl_id), PyRep::String("useMissiles".into()), PyRep::Integer(target_oob)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(brl_err.code, "TargetOutOfRange");
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&brl_ammo_id).map(|i| i.quantity)), Some(5));

    // 3. In-surface-range (6_000m surface, 19_600m center) launches from hull surface & impacts cleanly
    let lml_ok = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(lml_id), PyRep::String("useMissiles".into()), PyRep::Integer(target_hit)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("guided missile at 6_000m surface distance must impact via hull-surface launch");
    assert_eq!(lml_ok.dict_get("impacted"), Some(&PyRep::Bool(true)));
    assert_eq!(lml_ok.dict_get("expired"), Some(&PyRep::Bool(false)));
    assert!(lml_ok.dict_get("appliedDamage").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&lml_ammo_id).map(|i| i.quantity)), Some(4));

    let brl_ok = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(brl_id), PyRep::String("useMissiles".into()), PyRep::Integer(target_hit)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("breacher pod at 6_000m surface distance must impact via hull-surface launch");
    assert_eq!(brl_ok.dict_get("impacted"), Some(&PyRep::Bool(true)));
    assert_eq!(brl_ok.dict_get("expired"), Some(&PyRep::Bool(false)));
    assert_eq!(brl_ok.dict_get("isBreacherPod"), Some(&PyRep::Bool(true)));
    assert!(brl_ok.dict_get("breacherTickDPS").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert_eq!(ctx.db.with_snapshot(|s| s.items.get(&brl_ammo_id).map(|i| i.quantity)), Some(4));
    assert!(with_combat_runtime(&ctx, |rt| rt.active_breacher_dots.contains_key(&target_hit)));
}

#[tokio::test]
async fn handle_weapons_special_target_ewar_surface_range_gate_1247() {
    use eve_sde::attr_ids;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    // Target Painter (`379`, cutoff 20_000m + 3 * 5_000m = 35_000m) + Secure Container (`448`) routed via is_special_target
    setup_titan_sde_and_item_1247(
        &mut ctx,
        char_id,
        ship_id,
        sys_id,
        vec![
            make_fighter_type(37_902, 379, 7, 5.0, vec![(54, 20_000.0), (158, 5_000.0), (554, 30.0)]),
            make_fighter_type(44_801, 448, 2, 100.0, vec![(9, 5_000.0), (attr_ids::SIGNATURE_RADIUS, 100.0)]),
        ],
    )
    .await;

    let tp_id = 835_001_i64;
    let other_char_id = char_id + 500;
    let cont_oob = 935_101_i64;
    let cont_in = 935_102_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut other_ch = snap.characters[&char_id].clone();
        other_ch.character_id = other_char_id;
        other_ch.user_id = other_char_id as u64;
        other_ch.corporation_id += 1;
        other_ch.solar_system_id = sys_id;
        other_ch.active_ship_id = 0;
        other_ch.station_id = None;
        snap.characters.insert(other_char_id, other_ch);

        snap.items.insert(tp_id, make_fighter_item(tp_id, 37_902, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(cont_oob, make_fighter_item(cont_oob, 44_801, other_char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.items.insert(cont_in, make_fighter_item(cont_in, 44_801, other_char_id, sys_id as i64, InventoryFlag::None, 1));
        snap.module_online_states.insert(tp_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_titan_ball_1247(&sys_handle, char_id, ship_id).await;

    for (cid, pos) in [
        // 35_650m surface distance (> 35_000m cutoff)
        (cont_oob, DVec3::new(42_500.0, 0.0, 0.0)),
        // 34_650m surface distance (<= 35_000m cutoff, despite 41_500m center distance)
        (cont_in, DVec3::new(41_500.0, 0.0, 0.0)),
    ] {
        sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: cid,
                    type_id: 44_801,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    structure_hp: 5_000.0,
                    max_structure_hp: 5_000.0,
                    signature_radius: 100.0,
                    base_signature_radius: Some(100.0),
                    ..CombatState::default()
                },
                eve_destiny::SlimItemMeta {
                    group_id: 448,
                    category_id: 2,
                    owner_id: other_char_id,
                    char_id: Some(other_char_id),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        lock_target_1247(&sys_handle, ship_id, cid).await;
    }

    // 1. Out-of-range special target rejects before projected EWAR, weapon timer, aggression, or NPC timer
    let oob_err = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(cont_oob)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(oob_err.code, "TargetOutOfRange");
    assert!((ctx.effective_target_painter_multiplier(cont_oob) - 1.0).abs() < 1e-6);
    assert_eq!(
        ctx.crimewatch.read().unwrap().states.get(&char_id).map(|s| s.weapon_timer_until_unix).unwrap_or(0),
        0
    );
    assert!(ctx.crimewatch.read().unwrap().get_aggression(char_id, other_char_id, ctx.now_unix()).is_none());
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_timers.get(&char_id).copied()).is_none());

    // 2. Within surface range (34_650m <= 35_000m, center 41_500m) succeeds through is_special_target dispatch
    let in_ok = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("targetAttack".into()), PyRep::Integer(cont_in)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("special-target EWAR within surface range must succeed");
    assert_eq!(in_ok.dict_get("ewarAssistRecorded"), Some(&PyRep::Bool(true)));
    assert!(ctx.effective_target_painter_multiplier(cont_in) > 1.0);
    let sys_h = ctx.systems.get_handle(sys_id).expect("sys handle active");
    assert!(
        sys_h
            .projected_ewar_for(cont_in)
            .iter()
            .any(|eff| (eff.distance - 34_650.0).abs() < 1e-3)
            && sys_h.projected_ewar_for(cont_in).len() == 1
    );
}

#[tokio::test]
async fn handle_weapons_burnout_thermo_delay_preoverload_and_heat_branches() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx.db
        .acquire_online_lock(session.user_id.unwrap_or(1), 999_000, Some(char_id), true, ctx.now_unix())
        .unwrap();
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        53_009,
        make_fighter_type(
            53_009,
            53,
            7,
            5.0,
            vec![
                (6, 50.0),
                (9, 40.0),
                (51, 5_000.0),
                (54, 25_000.0),
                (64, 1.0),
                (158, 5_000.0),
                (160, 1.0),
                (604, 86.0),
            ],
        ),
    );
    sde.types.insert(
        86_009,
        make_fighter_type(86_009, 86, 8, 1.0, vec![(114, 100.0), (786, 0.0)]),
    );
    sde.types.insert(
        50_909,
        make_fighter_type(
            50_909,
            509,
            7,
            5.0,
            vec![(6, 10.0), (9, 40.0), (51, 5_000.0), (604, 384.0)],
        ),
    );
    sde.types.insert(
        38_409,
        make_fighter_type(
            38_409,
            384,
            8,
            0.015,
            vec![
                (37, 4_000.0),
                (281, 2_000.0),
                (9, 75.0),
                (117, 150.0),
                (653, 170.0),
                (654, 40.0),
                (1353, 0.6),
            ],
        ),
    );
    sde.types.insert(
        37_909,
        make_fighter_type(
            37_909,
            379,
            7,
            5.0,
            vec![(6, 10.0), (9, 40.0), (54, 25_000.0), (158, 5_000.0), (554, 30.0)],
        ),
    );
    sde.types.insert(
        72_009,
        make_fighter_type(
            72_009,
            72,
            7,
            5.0,
            vec![(6, 10.0), (9, 40.0), (51, 5_000.0), (73, 5_000.0), (99, 3_000.0), (114, 50.0)],
        ),
    );
    sde.types.insert(
        45_009,
        make_fighter_type(45_009, 4500, 7, 5.0, vec![(6, 10.0), (9, 40.0), (51, 5_000.0), (604, 4070.0)]),
    );
    sde.types.insert(
        40_709,
        make_fighter_type(
            40_709,
            4070,
            8,
            0.1,
            vec![(37, 4_000.0), (281, 2_000.0), (9, 75.0), (5735, 10_000.0), (5736, 100.0), (5737, 5.0)],
        ),
    );
    sde.types.insert(
        10_050,
        make_fighter_type(10_050, 182, 11, 1_000.0, vec![(eve_sde::attr_ids::SIGNATURE_RADIUS, 200.0)]),
    );
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        for (k, v) in [
            (11, 50_000.0),
            (12, 4.0),
            (13, 4.0),
            (14, 8.0),
            (48, 50_000.0),
            (101, 8.0),
            (102, 8.0),
        ] {
            ship_t.default_attributes.insert(k, v);
        }
    }
    for (tid, effs) in [
        (53_009, vec![12_u16, 42]),
        (50_909, vec![12_u16, 40]),
        (37_909, vec![13_u16]),
        (72_009, vec![12_u16]),
        (45_009, vec![12_u16, 40]),
    ] {
        if let Some(t) = sde.types.get_mut(&tid) {
            t.effect_ids.extend(effs);
        }
    }
    enable_test_overload(&mut sde, &ctx, char_id, &[53_009, 50_909, 37_909, 72_009]);
    ctx.sde = std::sync::Arc::new(sde);

    // Clear Thermodynamics L1 so L0 rejection can be verified first.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sk) = snap.skills.get_mut(&char_id) {
            sk.remove(&28_164);
        }
    });

    let laser_id = 880_001_i64;
    let crystal_id = 880_002_i64;
    let lml_id = 880_003_i64;
    let missile_id = 880_004_i64;
    let tp_id = 880_005_i64;
    let sb_id = 880_006_i64;
    let breacher_id = 880_007_i64;
    let breacher_ammo_id = 880_008_i64;
    let npc_id = 980_001_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(laser_id, make_fighter_item(laser_id, 53_009, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(crystal_id, make_fighter_item(crystal_id, 86_009, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(lml_id, make_fighter_item(lml_id, 50_909, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(missile_id, make_fighter_item(missile_id, 38_409, char_id, ship_id, InventoryFlag::HiSlot1, 20));
        snap.items.insert(sb_id, make_fighter_item(sb_id, 72_009, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(breacher_id, make_fighter_item(breacher_id, 45_009, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.items.insert(breacher_ammo_id, make_fighter_item(breacher_ammo_id, 40_709, char_id, ship_id, InventoryFlag::HiSlot3, 10));
        snap.items.insert(tp_id, make_fighter_item(tp_id, 37_909, char_id, ship_id, InventoryFlag::MedSlot0, 1));
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, hp, cap) in [
        (ship_id, 23_915_u32, DVec3::ZERO, 5_000.0, 1_000.0),
        (npc_id, 10_050_u32, DVec3::new(2_000.0, 0.0, 0.0), 5_000.0, 0.0),
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
                    shield_hp: hp,
                    max_shield_hp: hp,
                    armor_hp: hp,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    capacitor: cap,
                    max_capacitor: cap,
                    signature_radius: 200.0,
                    base_signature_radius: Some(200.0),
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    lock_target_1247(&sys_handle, ship_id, npc_id).await;

    // 1. Non-overloadable Breacher Pod rejects Overload cleanly (both targetless and targeted)
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(breacher_id)], &kwargs)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotOverloadModule"
    );
    assert_eq!(
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(breacher_id), PyRep::Integer(npc_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotOverloadModule"
    );

    // 2. Without Thermodynamics, targetless & targeted Overload reject without mutating state or cap
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id)], &kwargs)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DontHaveThermoDynamicsSkillz"
    );
    assert_eq!(
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(laser_id), PyRep::Integer(npc_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DontHaveThermoDynamicsSkillz"
    );
    assert!((sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().capacitor - 1_000.0).abs() < 1e-6);

    // Grant Thermodynamics L1
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(char_id).or_default().insert(
            28_164,
            eve_db::TrainedSkillRecord {
                type_id: 28_164,
                level: 1,
                skill_points: 250.0,
            },
        );
    });

    // 3. Burned-out weapon rejects BEFORE ModuleOffline even when offlined
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(laser_id, false);
        snap.items
            .get_mut(&laser_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 40.0);
    });
    ctx.invalidate_ship_fitting(ship_id);
    assert_eq!(
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(laser_id), PyRep::Integer(npc_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ModuleBurnedOut"
    );
    assert_eq!(
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(laser_id), PyRep::Integer(npc_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ModuleBurnedOut"
    );
    // Repair and re-online laser_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(laser_id, true);
        snap.items
            .get_mut(&laser_id)
            .unwrap()
            .dynamic_attributes
            .insert(eve_sde::attr_ids::DAMAGE, 0.0);
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 4. Reactivation delay (in_delay && !in_cycle) rejects both targetless and targeted Overload cleanly
    ctx.record_module_reactivation_delay(ship_id, laser_id, 10.0);
    assert_eq!(
        handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id)], &kwargs)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ModuleAlreadyActive"
    );
    assert_eq!(
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Overload",
            &[PyRep::Integer(laser_id), PyRep::Integer(npc_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ModuleAlreadyActive"
    );
    ctx.clear_all_reactivation_delays();

    // 5. Pre-overloaded weapon preserves ModuleState::Overloaded across failed Activate
    handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(laser_id)], &kwargs)
        .await
        .unwrap()
        .expect("pre-overload idle weapon");
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&laser_id].state,
        eve_dogma::ModuleState::Overloaded
    );
    let not_locked_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::Integer(999_999_888)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(not_locked_err.code, "TargetNotLocked");
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&laser_id].state,
        eve_dogma::ModuleState::Overloaded,
        "Failed Activate on unlocked target must preserve ModuleState::Overloaded"
    );
    sys_handle.set_capacitor(ship_id, 1.0).await.unwrap();
    let no_cap_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_id), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_cap_err.code, "NotEnoughCapacitor");
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&laser_id].state,
        eve_dogma::ModuleState::Overloaded,
        "Failed Activate on low capacitor must preserve ModuleState::Overloaded"
    );
    assert!(
        ctx.db.with_snapshot(|s| {
            s.ship_fit_inputs
                .get(&ship_id)
                .is_some_and(|fi| fi.overloaded_modules.contains(&laser_id))
        }),
        "DB ship_fit_inputs.overloaded_modules must retain laser_id after failed Activate"
    );
    sys_handle.set_capacitor(ship_id, 1_000.0).await.unwrap();

    // 6. Missile, EWAR, and AoE Smartbomb Overload + pre-overloaded Activate all emit OnHeatAdded
    let assert_heat_added = |mid: i64, expected_heat_id: i64| {
        let notes = ctx.drain_notifications_for_char(char_id);
        assert!(
            notes.iter().any(|n| {
                n.event == "OnHeatAdded"
                    && n.args == vec![PyRep::Integer(expected_heat_id), PyRep::Integer(mid)]
            }),
            "expected OnHeatAdded([{expected_heat_id}, {mid}]), got: {notes:?}"
        );
    };
    let reset_and_preoverload = |mid: i64| {
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&mid);
            if let Some(fi) = snap.ship_fit_inputs.get_mut(&ship_id) {
                fi.overloaded_modules.remove(&mid);
            }
        });
        ctx.invalidate_ship_fitting(ship_id);
        ctx.clear_all_reactivation_delays();
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap()
    };

    // Missile Overload -> OnHeatAdded([1175, lml_id]), then reset/pre-overload & Activate -> OnHeatAdded([1175, lml_id])
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(lml_id), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("Missile Overload");
    assert_heat_added(lml_id, 1175);
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&lml_id].state,
        eve_dogma::ModuleState::Overloaded
    );

    // Pre-overload idle missile launcher via handle_active_overload_toggle, clear cooldown, Activate -> OnHeatAdded
    let lml_fit = reset_and_preoverload(lml_id);
    let _ = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
        &ctx, &sys_handle, char_id, ship_id, lml_id, lml_fit, false,
    )
    .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(lml_id), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("Pre-overloaded Missile Activate");
    assert_heat_added(lml_id, 1175);
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&lml_id].state,
        eve_dogma::ModuleState::Overloaded
    );

    // EWAR Target Painter Overload & pre-overloaded Activate -> OnHeatAdded([1176, tp_id])
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(tp_id), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("EWAR Overload");
    assert_heat_added(tp_id, 1176);

    let tp_fit = reset_and_preoverload(tp_id);
    let _ = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
        &ctx, &sys_handle, char_id, ship_id, tp_id, tp_fit, false,
    )
    .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("Pre-overloaded EWAR Activate");
    assert_heat_added(tp_id, 1176);
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&tp_id].state,
        eve_dogma::ModuleState::Overloaded
    );

    // AoE Smartbomb Overload & pre-overloaded Activate -> OnHeatAdded([1175, sb_id]) and eff_m.state == Overloaded
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(sb_id)], &kwargs)
        .await
        .unwrap()
        .expect("AoE Smartbomb Overload");
    assert_heat_added(sb_id, 1175);
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&sb_id].state,
        eve_dogma::ModuleState::Overloaded
    );

    let sb_fit = reset_and_preoverload(sb_id);
    let _ = crate::services::fitting_cycles_overheat::handle_active_overload_toggle(
        &ctx, &sys_handle, char_id, ship_id, sb_id, sb_fit, false,
    )
    .await;
    let _ = ctx.drain_notifications_for_char(char_id);
    handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kwargs)
        .await
        .unwrap()
        .expect("Pre-overloaded AoE Smartbomb Activate");
    assert_heat_added(sb_id, 1175);
    assert_eq!(
        ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap().modules[&sb_id].state,
        eve_dogma::ModuleState::Overloaded
    );
}

#[tokio::test]
async fn handle_weapons_rejects_overload_on_standup_weapons_without_firing() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let corp_id = session.corporation_id.unwrap_or(1_000_009);
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    if let Some(carrier) = sde.types.get_mut(&23_915) {
        carrier.default_attributes.insert(14, 6.0);
    }
    sde.types.insert(
        35_834,
        make_fighter_type(
            35_834,
            1657,
            65,
            10_000.0,
            vec![(14, 6.0), (263, 1_000_000.0), (265, 1_000_000.0), (9, 1_000_000.0), (482, 500_000.0)],
        ),
    );
    sde.types.insert(
        35_926,
        make_fighter_type(35_926, 1330, 66, 4_000.0, vec![(6, 50.0), (73, 5_000.0), (54, 2_500.0)]),
    );
    sde.types.insert(
        35_923,
        make_fighter_type(35_923, 1328, 66, 4_000.0, vec![(6, 50.0), (73, 10_000.0), (54, 150_000.0), (604, 86.0)]),
    );
    sde.types.insert(
        35_928,
        make_fighter_type(35_928, 1333, 66, 4_000.0, vec![(6, 100.0), (73, 600_000.0), (54, 250_000.0)]),
    );
    sde.types.insert(
        63_195,
        make_fighter_type(63_195, 86, 8, 0.1, vec![(114, 250.0)]),
    );
    sde.types.insert(
        37_850,
        make_fighter_type(37_850, 86, 8, 50.0, vec![(114, 2_000.0)]),
    );
    enable_test_overload(&mut sde, &ctx, char_id, &[35_926, 35_923, 35_928]);
    ctx.sde = std::sync::Arc::new(sde);

    let st_id = 1_000_940_001_i64;
    let pds_id = 940_101_i64;
    let gbl_id = 940_102_i64;
    let vorton_id = 940_103_i64;
    let flak_ammo_id = 940_104_i64;
    let bomb_ammo_id = 940_105_i64;
    let ship_pds_id = 940_106_i64;
    let hostile_id = 940_201_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            st_id,
            eve_db::UpwellStructureRecord {
                structure_id: st_id,
                type_id: 35_834,
                owner_corp_id: corp_id,
                solar_system_id: sys_id,
                name: "TestFortizar".into(),
                state: "ShieldVulnerable".into(),
                shield_hp: 5_000_000.0,
                armor_hp: 5_000_000.0,
                hull_hp: 5_000_000.0,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            st_id,
            make_fighter_item(st_id, 35_834, corp_id, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            pds_id,
            make_fighter_item(pds_id, 35_926, corp_id, st_id, InventoryFlag::HiSlot0, 1),
        );
        snap.items.insert(
            gbl_id,
            make_fighter_item(gbl_id, 35_923, corp_id, st_id, InventoryFlag::HiSlot1, 1),
        );
        snap.items.insert(
            vorton_id,
            make_fighter_item(vorton_id, 35_928, corp_id, st_id, InventoryFlag::HiSlot2, 1),
        );
        snap.items.insert(
            ship_pds_id,
            make_fighter_item(ship_pds_id, 35_926, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(st_id);
    ctx.invalidate_ship_fitting(ship_id);

    // 1. Unloaded Standup PDS with controlling_structure=true rejects Overload
    session.structure_id = Some(st_id);
    session.ship_id = Some(st_id);
    let unloaded_err = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(pds_id)], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(unloaded_err.code, "CannotOverloadStructureModule");
    assert_eq!(unloaded_err.message, "Standup structure modules cannot be overloaded");

    // 2. Spawn structure + hostile target + ammo loaded in SpecializedAmmoHold -> still rejects without firing or consuming ammo/cap
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, shield) in [
        (st_id, 35_834_u32, DVec3::ZERO, 5_000_000.0),
        (ship_id, 23_915_u32, DVec3::new(100.0, 0.0, 0.0), 2_000.0),
        (hostile_id, 23_915_u32, DVec3::new(1_000.0, 0.0, 0.0), 5_000.0),
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
                    radius: if tid == 35_834 { 500.0 } else { 50.0 },
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: shield,
                    max_shield_hp: shield,
                    armor_hp: 2_000.0,
                    max_armor_hp: 2_000.0,
                    structure_hp: 2_000.0,
                    max_structure_hp: 2_000.0,
                    capacitor: 5_000.0,
                    max_capacitor: 5_000.0,
                    signature_radius: 100.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    sys_handle
        .add_target_lock(
            st_id,
            TargetLock {
                target_id: hostile_id,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(300_000.0),
            },
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            flak_ammo_id,
            make_fighter_item(flak_ammo_id, 63_195, corp_id, st_id, InventoryFlag::SpecializedAmmoHold, 500),
        );
        snap.items.insert(
            bomb_ammo_id,
            make_fighter_item(bomb_ammo_id, 37_850, corp_id, st_id, InventoryFlag::SpecializedAmmoHold, 10),
        );
    });

    for args in [
        vec![PyRep::Integer(pds_id)],
        vec![PyRep::Integer(gbl_id), PyRep::String("defaultEffect".into()), PyRep::Integer(hostile_id)],
        vec![PyRep::Integer(vorton_id), PyRep::String("superWeaponVorton".into()), PyRep::Integer(hostile_id)],
    ] {
        let err = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &args, &kwargs)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, "CannotOverloadStructureModule");
    }
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&flak_ammo_id].quantity), 500);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&bomb_ammo_id].quantity), 10);
    assert!((sys_handle.get_combat_state(hostile_id).await.unwrap().unwrap().shield_hp - 5_000.0).abs() < 1e-6);
    assert!((sys_handle.get_combat_state(st_id).await.unwrap().unwrap().capacitor - 5_000.0).abs() < 1e-6);

    // 3. Direct handle_weapons with structure ship_id even when session.structure_id is None
    session.structure_id = None;
    let no_struct_sess_err = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(pds_id)], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(no_struct_sess_err.code, "CannotOverloadStructureModule");

    // 4. Fallback Standup group guard on non-structure ship_id
    session.ship_id = Some(ship_id);
    let fallback_err = handle_weapons(&ctx, &mut session, "dogmaIM", "Overload", &[PyRep::Integer(ship_pds_id)], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(fallback_err.code, "CannotOverloadStructureModule");
}

#[tokio::test]
async fn f02_weapons_and_aoe_preserve_gate_cloak_on_rejections_and_break_on_commit() {
    use crate::rng::TEST_MODE_DEFAULT_SEED;
    use crate::services::combat_crimewatch::{
        activate_aoe_or_defender_module_with_cap, set_char_safety_level, SafetyLevel,
    };
    use crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    ctx = ctx.with_seeded_rng(TEST_MODE_DEFAULT_SEED);
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    if let Some(hull) = sde.types.get_mut(&23_915) {
        hull.default_attributes.insert(14, 8.0); // hiSlots
        hull.default_attributes.insert(11, 1_000_000.0); // powerOutput
        hull.default_attributes.insert(48, 10_000.0); // cpuOutput
        hull.default_attributes.insert(482, 500.0); // capacitorCapacity
    }
    // 1. Entropic Disintegrator (1986) + plasma (1987)
    sde.types.insert(
        19_861,
        make_fighter_type(
            19_861,
            1986,
            7,
            3.0,
            vec![(6, 10.0), (51, 3_000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 1987.0), (620, 40_000.0), (64, 1.0), (2733, 0.15), (2734, 10.0)],
        ),
    );
    sde.types.insert(19_871, make_fighter_type(19_871, 1987, 8, 0.01, vec![(117, 15.0), (118, 25.0)]));
    // 2. Vorton Projector (4060) + condenser pack (4061)
    sde.types.insert(
        40_601,
        make_fighter_type(
            40_601,
            4060,
            7,
            3.0,
            vec![
                (6, 10.0), (51, 3_000.0), (54, 25_000.0), (158, 5_000.0), (604, 4061.0), (64, 1.0),
                (3036, 10_000.0), (3037, 5.0), (653, 190.0), (654, 43.0), (1353, 0.40),
            ],
        ),
    );
    sde.types.insert(40_611, make_fighter_type(40_611, 4061, 8, 0.01, vec![(114, 30.0), (117, 30.0)]));
    // 3. Breacher Pod Launcher (4500) + Breacher Pod (4070)
    sde.types.insert(
        45_001,
        make_fighter_type(45_001, 4500, 7, 5.0, vec![(6, 10.0), (51, 3_000.0), (604, 4070.0)]),
    );
    sde.types.insert(
        85_001,
        make_fighter_type(
            85_001,
            4070,
            8,
            0.1,
            vec![(37, 4_000.0), (281, 2_000.0), (9, 75.0), (5735, 10_000.0), (5736, 250.0), (5737, 5.0)],
        ),
    );
    // 4. Bomb Launcher (862) + Bomb (90)
    sde.types.insert(
        86_208,
        make_fighter_type(86_208, 862, 7, 75.0, vec![(51, 5_000.0), (604, 90.0), (128, 1.0)]),
    );
    sde.types.insert(
        90_008,
        make_fighter_type(90_008, 90, 8, 75.0, vec![(114, 500.0), (37, 2_500.0), (281, 12_000.0), (99, 15_000.0), (654, 400.0)]),
    );
    // 5. Smartbomb (72)
    let mut sb_t = make_fighter_type(72_001, 72, 7, 5.0, vec![(6, 25.0), (73, 5_000.0), (197, 3_000.0)]);
    sb_t.effect_ids = vec![12, 16];
    sde.types.insert(72_001, sb_t);
    // Target NPC type
    sde.types.insert(
        10_098,
        make_fighter_type(10_098, 550, 11, 1_000.0, vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 10_000.0)]),
    );
    ctx.sde = std::sync::Arc::new(sde);
    ctx.db
        .acquire_online_lock(session.user_id.unwrap_or(1), 999_225, Some(char_id), true, ctx.now_unix())
        .unwrap();

    let victim_char = 99_140_302_i64;
    let victim_ship = 99_140_303_i64;
    let near_npc = 99_140_304_i64;
    let far_npc = 99_140_305_i64;
    let unlocked_npc = 99_140_306_i64;

    let entropic_id = 840_001_i64;
    let entropic_ammo = 840_002_i64;
    let entropic_unloaded_id = 840_003_i64;
    let vorton_id = 840_004_i64;
    let vorton_ammo = 840_005_i64;
    let breacher_id = 840_006_i64;
    let breacher_ammo = 840_007_i64;
    let bomb_id = 840_008_i64;
    let sb_id = 840_009_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut vch = snap.characters[&char_id].clone();
        vch.character_id = victim_char;
        vch.user_id = victim_char as u64;
        vch.corporation_id = 1_000_010;
        vch.active_ship_id = victim_ship;
        vch.solar_system_id = sys_id;
        vch.station_id = None;
        vch.structure_id = None;
        snap.characters.insert(victim_char, vch);
        snap.items.insert(
            victim_ship,
            make_fighter_item(victim_ship, 23_915, victim_char, i64::from(sys_id), InventoryFlag::None, 1),
        );

        snap.items.insert(entropic_id, make_fighter_item(entropic_id, 19_861, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        let mut ea = make_fighter_item(entropic_ammo, 19_871, char_id, ship_id, InventoryFlag::HiSlot0, 10);
        ea.singleton = false;
        snap.items.insert(entropic_ammo, ea);

        snap.items.insert(entropic_unloaded_id, make_fighter_item(entropic_unloaded_id, 19_861, char_id, ship_id, InventoryFlag::HiSlot1, 1));

        snap.items.insert(vorton_id, make_fighter_item(vorton_id, 40_601, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        let mut va = make_fighter_item(vorton_ammo, 40_611, char_id, ship_id, InventoryFlag::HiSlot2, 10);
        va.singleton = false;
        snap.items.insert(vorton_ammo, va);

        snap.items.insert(breacher_id, make_fighter_item(breacher_id, 45_001, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        let mut ba = make_fighter_item(breacher_ammo, 85_001, char_id, ship_id, InventoryFlag::HiSlot3, 10);
        ba.singleton = false;
        snap.items.insert(breacher_ammo, ba);

        snap.items.insert(bomb_id, make_fighter_item(bomb_id, 86_208, char_id, ship_id, InventoryFlag::HiSlot4, 1));
        snap.items.insert(sb_id, make_fighter_item(sb_id, 72_001, char_id, ship_id, InventoryFlag::HiSlot5, 1));

        for mid in [entropic_id, entropic_unloaded_id, vorton_id, breacher_id, bomb_id, sb_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos, tethered, cloaked, gate_ticks, undock_ticks) in [
        (ship_id, 23_915_u32, DVec3::ZERO, true, true, 30_u32, 15_u32),
        (victim_ship, 23_915_u32, DVec3::new(0.0, 15_000.0, 0.0), false, false, 0_u32, 0_u32),
        (near_npc, 10_098_u32, DVec3::new(5_000.0, 0.0, 0.0), false, false, 0_u32, 0_u32),
        (far_npc, 10_098_u32, DVec3::new(200_000.0, 0.0, 0.0), false, false, 0_u32, 0_u32),
        (unlocked_npc, 10_098_u32, DVec3::new(6_000.0, 0.0, 0.0), false, false, 0_u32, 0_u32),
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
                    shield_hp: 50_000.0,
                    max_shield_hp: 50_000.0,
                    armor_hp: 50_000.0,
                    max_armor_hp: 50_000.0,
                    structure_hp: 50_000.0,
                    max_structure_hp: 50_000.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 100.0,
                    base_signature_radius: Some(100.0),
                    is_tethered: tethered,
                    is_cloaked: cloaked,
                    gate_cloak_remaining_ticks: gate_ticks,
                    undock_invulnerable_remaining_ticks: undock_ticks,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }
    for tid in [victim_ship, near_npc, far_npc] {
        sys_handle
            .add_target_lock(
                ship_id,
                TargetLock {
                    target_id: tid,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(250_000.0),
                },
            )
            .await
            .unwrap();
    }

    let reset_gate_cloak = |tethered: bool, cap: f64| {
        let sys_h = sys_handle.clone();
        async move {
            sys_h
                .set_combat_state(
                    ship_id,
                    CombatState {
                        shield_hp: 50_000.0,
                        max_shield_hp: 50_000.0,
                        armor_hp: 50_000.0,
                        max_armor_hp: 50_000.0,
                        structure_hp: 50_000.0,
                        max_structure_hp: 50_000.0,
                        capacitor: cap,
                        max_capacitor: 500.0,
                        signature_radius: 100.0,
                        base_signature_radius: Some(100.0),
                        is_tethered: tethered,
                        is_cloaked: true,
                        gate_cloak_remaining_ticks: 30,
                        undock_invulnerable_remaining_ticks: 15,
                        ..CombatState::default()
                    },
                )
                .await
                .unwrap();
        }
    };

    // Part A: Entropic Disintegrator (1986) rejected paths MUST preserve gate cloak & undock invuln.
    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    let _ = ctx.drain_notifications_for_char(char_id);
    for (args, tethered, cap, expected_code, label) in [
        (vec![PyRep::Integer(entropic_id), PyRep::String("targetAttack".into()), PyRep::Integer(near_npc)], true, 500.0, "CantActivateWhileTethered", "tethered"),
        (vec![PyRep::Integer(entropic_id)], false, 500.0, "TargetRequired", "targetless"),
        (vec![PyRep::Integer(entropic_id), PyRep::String("targetAttack".into()), PyRep::Integer(unlocked_npc)], false, 500.0, "TargetNotLocked", "unlocked"),
        (vec![PyRep::Integer(entropic_id), PyRep::String("targetAttack".into()), PyRep::Integer(victim_ship)], false, 500.0, "CantDoInCurrentSafetyLevel", "safety"),
        (vec![PyRep::Integer(entropic_id), PyRep::String("targetAttack".into()), PyRep::Integer(far_npc)], false, 500.0, "TargetOutOfRange", "range"),
        (vec![PyRep::Integer(entropic_id), PyRep::String("targetAttack".into()), PyRep::Integer(near_npc)], false, 1.0, "NotEnoughCapacitor", "capacitor"),
        (vec![PyRep::Integer(entropic_unloaded_id), PyRep::String("targetAttack".into()), PyRep::Integer(near_npc)], false, 500.0, "NoAmmoLoaded", "unloaded"),
    ] {
        reset_gate_cloak(tethered, cap).await;
        let err = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &args, &kwargs)
            .await
            .unwrap()
            .expect_err("rejected Entropic activation must return Err");
        assert_eq!(err.code, expected_code, "Entropic rejection case {label}");
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.is_cloaked, "Entropic {label} must keep is_cloaked");
        assert_eq!(cs.gate_cloak_remaining_ticks, 30, "Entropic {label} must keep gate cloak");
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 15, "Entropic {label} must keep undock invuln");
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
    }

    // Valid Entropic activation breaks gate cloak & undock invuln on commit (single OnInvulnCancelled).
    reset_gate_cloak(false, 500.0).await;
    handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(entropic_id), PyRep::String("targetAttack".into()), PyRep::Integer(near_npc)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect("valid gate-cloaked Entropic activation must succeed");
    let cs_ent_ok = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_ent_ok.is_cloaked);
    assert_eq!(cs_ent_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_ent_ok.undock_invulnerable_remaining_ticks, 0);
    let ent_notes = ctx.drain_notifications_for_char(char_id);
    assert!(ent_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert_eq!(ent_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    // Part B: Vorton Projector (4060) & Breacher Pod (4500, zero cap cost) preserve gate cloak & undock invuln on rejection, break on commit.
    let vorton_cases = [
        (false, unlocked_npc, 500.0, "TargetNotLocked"),
        (false, far_npc, 500.0, "TargetOutOfRange"),
        (false, near_npc, 1.0, "NotEnoughCapacitor"),
    ];
    let breacher_cases = [
        (true, near_npc, 500.0, "CantActivateWhileTethered"),
        (false, unlocked_npc, 500.0, "TargetNotLocked"),
        (false, far_npc, 500.0, "TargetOutOfRange"),
    ];
    for (mod_id, label, cases) in [
        (vorton_id, "Vorton", &vorton_cases[..]),
        (breacher_id, "Breacher", &breacher_cases[..]),
    ] {
        for &(tethered, target_id, cap, expected_code) in cases {
            reset_gate_cloak(tethered, cap).await;
            let err = handle_weapons(
                &ctx,
                &mut session,
                "dogmaIM",
                "Activate",
                &[PyRep::Integer(mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)],
                &kwargs,
            )
            .await
            .unwrap()
            .expect_err("rejected Vorton/Breacher activation must return Err");
            assert_eq!(err.code, expected_code, "{label} rejection");
            let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
            assert!(cs.is_cloaked);
            assert_eq!(cs.gate_cloak_remaining_ticks, 30);
            assert_eq!(cs.undock_invulnerable_remaining_ticks, 15);
            assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
        }

        reset_gate_cloak(false, 500.0).await;
        handle_weapons(
            &ctx,
            &mut session,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mod_id), PyRep::String("targetAttack".into()), PyRep::Integer(near_npc)],
            &kwargs,
        )
        .await
        .unwrap()
        .expect("valid gate-cloaked Vorton/Breacher activation must succeed");
        let cs_ok = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(!cs_ok.is_cloaked);
        assert_eq!(cs_ok.gate_cloak_remaining_ticks, 0);
        assert_eq!(cs_ok.undock_invulnerable_remaining_ticks, 0);
        let ok_notes = ctx.drain_notifications_for_char(char_id);
        assert!(ok_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
        assert_eq!(ok_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    }

    // Part C: Bomb Launcher (862) preserves gate cloak & undock invuln on highsec/safety/no-ammo rejections, breaks on launch.
    reset_gate_cloak(false, 500.0).await;
    let bomb_hs_err = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(bomb_id)], &kwargs)
        .await
        .unwrap()
        .expect_err("Bomb Launcher in highsec must reject");
    assert_eq!(bomb_hs_err.code, "BombDisallowedInHighSec");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks, 30);
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);

    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(sys_id, 0.2);
    });
    let bomb_green_err = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(bomb_id)], &kwargs)
        .await
        .unwrap()
        .expect_err("Bomb Launcher on Green safety in lowsec must reject");
    assert_eq!(bomb_green_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks, 30);
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);

    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    let bomb_ammo_err = handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(bomb_id)], &kwargs)
        .await
        .unwrap()
        .expect_err("Bomb Launcher with no bomb must reject");
    assert_eq!(bomb_ammo_err.code, "NoAmmoLoaded");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks, 30);
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));

    ctx.db.with_mut_snapshot(|snap| {
        let mut b = make_fighter_item(840_010, 90_008, char_id, ship_id, InventoryFlag::HiSlot4, 1);
        b.singleton = false;
        snap.items.insert(840_010, b);
    });
    ctx.invalidate_ship_fitting(ship_id);
    handle_weapons(&ctx, &mut session, "dogmaIM", "Activate", &[PyRep::Integer(bomb_id)], &kwargs)
        .await
        .unwrap()
        .expect("valid gate-cloaked Bomb launch must succeed");
    let cs_bomb_ok = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_bomb_ok.is_cloaked);
    assert_eq!(cs_bomb_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_bomb_ok.undock_invulnerable_remaining_ticks, 0);
    let bomb_notes = ctx.drain_notifications_for_char(char_id);
    assert!(bomb_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert_eq!(bomb_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    // Part D: Smartbomb (72) / AoE preserves gate cloak & undock invuln on safety & capacitor rejections in both direct AoE and handle_module_cycles.
    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(sys_id, 0.9);
    });
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let sb_fm = fit.modules[&sb_id].clone();
    let sb_type = ctx.sde.types[&72_001].clone();
    let attacker_ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;

    reset_gate_cloak(false, 500.0).await;
    let aoe_safety_err = activate_aoe_or_defender_module_with_cap(
        &ctx, char_id, ship_id, sys_id, 0.9, ctx.now_unix(), sb_id, &sb_fm, &sb_type, &attacker_ball, None, &kwargs, true,
    )
    .await
    .unwrap()
    .expect_err("direct AoE smartbomb in highsec on Green safety must reject");
    assert_eq!(aoe_safety_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks, 30);
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);

    let cyc_safety_err = handle_module_cycles(&ctx, &mut session, None, "dogmaIM", "Activate", &[PyRep::Integer(sb_id)], &kwargs)
        .await
        .unwrap()
        .expect_err("handle_module_cycles smartbomb in highsec on Green safety must reject");
    assert_eq!(cyc_safety_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().gate_cloak_remaining_ticks, 30);
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);

    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    reset_gate_cloak(false, 1.0).await;
    let aoe_cap_err = activate_aoe_or_defender_module_with_cap(
        &ctx, char_id, ship_id, sys_id, 0.9, ctx.now_unix(), sb_id, &sb_fm, &sb_type, &attacker_ball, None, &kwargs, true,
    )
    .await
    .unwrap()
    .expect_err("direct AoE smartbomb with insufficient cap must reject");
    assert_eq!(aoe_cap_err.code, "NotEnoughCapacitor");
    let cs_aoe_cap = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_aoe_cap.is_cloaked);
    assert_eq!(cs_aoe_cap.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_aoe_cap.undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));

    reset_gate_cloak(false, 500.0).await;
    activate_aoe_or_defender_module_with_cap(
        &ctx, char_id, ship_id, sys_id, 0.9, ctx.now_unix(), sb_id, &sb_fm, &sb_type, &attacker_ball, None, &kwargs, true,
    )
    .await
    .unwrap()
    .expect("valid gate-cloaked direct AoE smartbomb must succeed");
    let cs_sb_ok = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_sb_ok.is_cloaked);
    assert_eq!(cs_sb_ok.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_sb_ok.undock_invulnerable_remaining_ticks, 0);
    let sb_notes = ctx.drain_notifications_for_char(char_id);
    assert!(sb_notes.iter().any(|n| n.event == "OnJumpCloakCancelled"));
    assert_eq!(sb_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
}

#[tokio::test]
async fn bf3_bf4_rapid_launchers_fw_upwell_tether_and_invuln_deferral() {

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();
    let _ = ctx.db.acquire_online_lock(
        session.user_id.unwrap_or(char_id as u64),
        1,
        Some(char_id),
        true,
        ctx.now_unix(),
    );

    let rlml_tid = 12_450_u32;
    let rhml_tid = 7_710_u32;
    let light_missile_tid = 38_401_u32;
    let heavy_missile_tid = 38_501_u32;
    let turret_tid = 53_004_u32;
    let crystal_tid = 86_005_u32;
    let tp_tid = 37_901_u32;
    let sb_tid = 72_001_u32;

    setup_titan_sde_and_item_1247(
        &mut ctx,
        char_id,
        ship_id,
        sys_id,
        vec![
            make_fighter_type(rlml_tid, 1245, 7, 5.0, vec![(51, 3_000.0), (604, 384.0), (6, 20.0)]),
            make_fighter_type(rhml_tid, 771, 7, 5.0, vec![(51, 3_000.0), (604, 385.0), (6, 25.0)]),
            make_fighter_type(
                light_missile_tid,
                384,
                8,
                0.015,
                vec![(37, 4_000.0), (281, 5_000.0), (9, 75.0), (117, 150.0), (653, 170.0), (654, 40.0), (1353, 0.6)],
            ),
            make_fighter_type(
                heavy_missile_tid,
                385,
                8,
                0.03,
                vec![(37, 4_000.0), (281, 5_000.0), (9, 75.0), (117, 250.0), (653, 170.0), (654, 140.0), (1353, 0.6)],
            ),
            make_fighter_type(
                turret_tid,
                53,
                7,
                5.0,
                vec![(6, 5.0), (51, 3_000.0), (54, 20_000.0), (158, 5_000.0), (160, 1.0), (604, 86.0), (64, 1.0)],
            ),
            make_fighter_type(crystal_tid, 86, 8, 1.0, vec![(114, 200.0), (786, 0.0)]),
            make_fighter_type(
                tp_tid,
                379,
                7,
                5.0,
                vec![(6, 10.0), (51, 3_000.0), (54, 10_000.0), (554, 30.0)],
            ),
            make_fighter_type(
                sb_tid,
                72,
                7,
                5.0,
                vec![(6, 30.0), (51, 3_000.0), (99, 5_000.0), (114, 100.0)],
            ),
            make_fighter_type(
                35_832,
                1657,
                65,
                10_000.0,
                vec![(263, 1_000_000.0), (265, 1_000_000.0), (9, 1_000_000.0), (eve_sde::attr_ids::SIGNATURE_RADIUS, 16_000.0)],
            ),
        ],
    )
    .await;
    {
        let mut sde = (*ctx.sde).clone();
        sde.solar_systems.get_mut(&sys_id).unwrap().security_status = 0.30;
        enable_test_overload(&mut sde, &ctx, char_id, &[rlml_tid, turret_tid, tp_tid, sb_tid]);
        ctx.sde = std::sync::Arc::new(sde);
    }

    let rlml_id = 845_001_i64;
    let rlml_ammo_id = 845_002_i64;
    let rhml_id = 845_003_i64;
    let rhml_ammo_id = 845_004_i64;
    let turret_id = 845_005_i64;
    let crys_id = 845_006_i64;
    let tp_id = 845_007_i64;
    let sb_id = 845_008_i64;
    let enemy_char_id = char_id + 10_000;
    let enemy_ship_id = 945_001_i64;
    let structure_id = 1_000_000_945_002_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().war_faction_id = Some(500_001);
        let mut enemy_char = snap.characters.get(&char_id).unwrap().clone();
        enemy_char.character_id = enemy_char_id;
        enemy_char.active_ship_id = enemy_ship_id;
        enemy_char.war_faction_id = Some(500_002);
        snap.characters.insert(enemy_char_id, enemy_char);
        snap.fw_system_occupations.insert(
            sys_id,
            eve_db::snapshot_ext::FwSystemOccupationRecord {
                solar_system_id: sys_id,
                owner_faction_id: 500_002,
                occupier_faction_id: 500_002,
                victory_points: 0,
                victory_points_threshold: 3_000,
            },
        );

        snap.items.insert(rlml_id, make_fighter_item(rlml_id, rlml_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1));
        snap.items.insert(rlml_ammo_id, make_fighter_item(rlml_ammo_id, light_missile_tid, char_id, ship_id, InventoryFlag::HiSlot0, 20));
        snap.items.insert(rhml_id, make_fighter_item(rhml_id, rhml_tid, char_id, ship_id, InventoryFlag::HiSlot1, 1));
        snap.items.insert(rhml_ammo_id, make_fighter_item(rhml_ammo_id, heavy_missile_tid, char_id, ship_id, InventoryFlag::HiSlot1, 20));
        snap.items.insert(turret_id, make_fighter_item(turret_id, turret_tid, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(crys_id, make_fighter_item(crys_id, crystal_tid, char_id, ship_id, InventoryFlag::HiSlot2, 1));
        snap.items.insert(tp_id, make_fighter_item(tp_id, tp_tid, char_id, ship_id, InventoryFlag::MedSlot0, 1));
        snap.items.insert(sb_id, make_fighter_item(sb_id, sb_tid, char_id, ship_id, InventoryFlag::HiSlot3, 1));
        snap.items.insert(enemy_ship_id, make_fighter_item(enemy_ship_id, 11_567, enemy_char_id, i64::from(sys_id), InventoryFlag::None, 1));

        let corp_id = snap.characters.get(&char_id).unwrap().corporation_id;
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                type_id: 35_832,
                owner_corp_id: corp_id + 99,
                solar_system_id: sys_id,
                name: "Target Astrahus".into(),
                state: "ShieldVulnerable".into(),
                shield_hp: 50_000.0,
                armor_hp: 50_000.0,
                hull_hp: 50_000.0,
                quantum_core_installed: true,
                ..Default::default()
            },
        );
        for mid in [rlml_id, rhml_id, turret_id, tp_id, sb_id] {
            snap.module_online_states.insert(mid, true);
        }
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    spawn_titan_ball_1247(&sys_handle, char_id, ship_id).await;
    for (tid, pos, sig) in [
        (enemy_ship_id, DVec3::new(9_000.0, 0.0, 0.0), 16_000.0),
        (structure_id, DVec3::new(12_000.0, 0.0, 0.0), 16_000.0),
    ] {
        sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: tid,
                    type_id: if tid == structure_id { 35_832 } else { 11_567 },
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 1_000_000.0,
                    radius: 100.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                CombatState {
                    shield_hp: 50_000.0,
                    max_shield_hp: 50_000.0,
                    armor_hp: 50_000.0,
                    max_armor_hp: 50_000.0,
                    structure_hp: 50_000.0,
                    max_structure_hp: 50_000.0,
                    signature_radius: sig,
                    base_signature_radius: Some(sig),
                    ..CombatState::default()
                },
                eve_destiny::SlimItemMeta::default(),
            )
            .await
            .unwrap();
    }

    let reset_ship_state = |tethered: bool, cloaked: bool, gate_ticks: u32, undock_ticks: u32| {
        let sys_c = sys_handle.clone();
        let ctx_c = ctx.clone();
        async move {
            sys_c
                .set_combat_state(
                    ship_id,
                    CombatState {
                        is_tethered: tethered,
                        is_cloaked: cloaked,
                        gate_cloak_remaining_ticks: gate_ticks,
                        undock_invulnerable_remaining_ticks: undock_ticks,
                        capacitor: 5_000.0,
                        max_capacitor: 5_000.0,
                        ..CombatState::default()
                    },
                )
                .await
                .unwrap();
            ctx_c.clear_all_reactivation_delays();
            let _ = ctx_c.drain_notifications_for_char(char_id);
        }
    };

    // 1. BF-3 & BF-4: Tethered Rapid Light (1245) & Rapid Heavy (771) reject with CantActivateWhileTethered
    //    via handle_module_cycles (which delegates FW PvP / Upwell to handle_weapons) and preserve
    //    gate_cloak and undock_invuln with zero notifications.
    for (mid, label) in [(rlml_id, "Rapid Light 1245"), (rhml_id, "Rapid Heavy 771")] {
        reset_ship_state(true, true, 30, 15).await;
        let err = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
            &ctx,
            &mut session,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mid), PyRep::String("useMissiles".into()), PyRep::Integer(enemy_ship_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .expect_err("tethered rapid launcher must reject");
        assert_eq!(err.code, "CantActivateWhileTethered", "{label}");
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert_eq!(cs.gate_cloak_remaining_ticks, 30);
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 15);
        assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled" || n.event == "OnJumpCloakCancelled"));
    }

    // Lock FW ship & Upwell structure.
    for tid in [enemy_ship_id, structure_id] {
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

    // 2. BF-3: FW PvP & Upwell Structure Combat work for Rapid Light (1245) and Rapid Heavy (771)
    //    through handle_module_cycles -> combat_weapons::handle_weapons, charging 0 capacitor and breaking
    //    both gate_cloak and undock_invuln (even without gate cloak) on commit.
    for (mid, tid, cloaked, gate_ticks, label) in [
        (rlml_id, enemy_ship_id, true, 30_u32, "FW PvP Rapid Light"),
        (rhml_id, enemy_ship_id, false, 0_u32, "FW PvP Rapid Heavy (non-gate undock invuln)"),
        (rlml_id, structure_id, false, 0_u32, "Upwell Rapid Light"),
        (rhml_id, structure_id, true, 30_u32, "Upwell Rapid Heavy"),
    ] {
        reset_ship_state(false, cloaked, gate_ticks, 15).await;
        let ok = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
            &ctx,
            &mut session,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(mid), PyRep::String("useMissiles".into()), PyRep::Integer(tid)],
            &kwargs,
        )
        .await
        .unwrap()
        .expect("rapid launcher FW/Upwell commit must succeed (no UnimplementedRPC)");
        assert_eq!(ok.dict_get("moduleID"), Some(&PyRep::Integer(mid)), "{label}");
        assert_eq!(ok.dict_get("targetID"), Some(&PyRep::Integer(tid)), "{label}");
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!((cs.capacitor - 5_000.0).abs() < 1e-6, "{label} consumes 0 cap");
        assert_eq!(cs.gate_cloak_remaining_ticks, 0, "{label} breaks gate cloak");
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 0, "{label} breaks undock invuln");
        let notes = ctx.drain_notifications_for_char(char_id);
        assert!(notes.iter().any(|n| n.event == "OnInvulnCancelled"), "{label} emits OnInvulnCancelled");
        assert_eq!(notes.iter().any(|n| n.event == "OnJumpCloakCancelled"), gate_ticks > 0, "{label}");
    }

    // 3. BF-4: Rejected weapon, EWAR, and AoE activations in handle_weapons preserve undock_invuln
    //    (with & without gate cloak) and emit zero OnInvulnCancelled.
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: 945_999,
                type_id: 11_567,
                position: DVec3::new(90_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 100.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: 945_999,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: Some(200_000.0),
            },
        )
        .await
        .unwrap();

    // Part A Weapon rejection (TargetNotLocked)
    reset_ship_state(false, false, 0, 15).await;
    let w_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(turret_id), PyRep::String("targetAttack".into()), PyRep::Integer(999_888_777)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect_err("unlocked turret must reject");
    assert_eq!(w_err.code, "TargetNotLocked");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled"));

    // Part B EWAR rejection (TargetOutOfRange)
    reset_ship_state(false, false, 0, 15).await;
    let ew_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(tp_id), PyRep::String("activate".into()), PyRep::Integer(945_999)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect_err("out-of-range EWAR must reject");
    assert_eq!(ew_err.code, "TargetOutOfRange");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled"));

    // Part C AoE Smartbomb rejection (Overload with Green safety in >0.0 sys)
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    reset_ship_state(false, false, 0, 15).await;
    let sb_err = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Overload",
        &[PyRep::Integer(sb_id), PyRep::String("empWave".into()), PyRep::Integer(enemy_ship_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect_err("Green safety AoE smartbomb must reject");
    assert_eq!(sb_err.code, "CantDoInCurrentSafetyLevel");
    assert_eq!(sys_handle.get_combat_state(ship_id).await.unwrap().unwrap().undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx.drain_notifications_for_char(char_id).iter().any(|n| n.event == "OnInvulnCancelled"));

    // Committed turret, EWAR, and AoE Smartbomb (with Red safety & non-gate undock invuln) clear ALL invuln/cloak & emit OnInvulnCancelled.
    crate::services::combat_crimewatch::set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    for (method, mid, eff, tgt_opt, label) in [
        ("Activate", turret_id, "targetAttack", Some(enemy_ship_id), "Part A Turret commit"),
        ("Activate", tp_id, "activate", Some(enemy_ship_id), "Part B EWAR commit"),
        ("Overload", sb_id, "empWave", Some(enemy_ship_id), "Part C AoE Smartbomb Overload commit"),
    ] {
        reset_ship_state(false, false, 0, 15).await;
        let mut args = vec![PyRep::Integer(mid), PyRep::String(eff.into())];
        if let Some(tid) = tgt_opt {
            args.push(PyRep::Integer(tid));
        }
        handle_weapons(&ctx, &mut session, "dogmaIM", method, &args, &kwargs)
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{label} must commit, got {e:?}"));
        let cs = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert_eq!(cs.undock_invulnerable_remaining_ticks, 0, "{label} clears undock invuln");
        let notes = ctx.drain_notifications_for_char(char_id);
        assert_eq!(notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1, "{label} emits single OnInvulnCancelled");
    }
}

#[tokio::test]
async fn smartbomb_handle_module_cycles_commit_emits_single_invuln_cancelled_1441() {
    use crate::services::combat_crimewatch::{
        activate_aoe_or_defender_module_with_cap, activate_aoe_or_defender_module_with_options,
        set_char_safety_level, SafetyLevel,
    };
    use crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    if let Some(hull) = sde.types.get_mut(&23_915) {
        hull.default_attributes.insert(14, 8.0); // hiSlots
        hull.default_attributes.insert(11, 1_000_000.0); // powerOutput
        hull.default_attributes.insert(48, 10_000.0); // cpuOutput
        hull.default_attributes.insert(482, 500.0); // capacitorCapacity
    }
    let sb_tid = 72_001_u32;
    let mut sb_t = make_fighter_type(
        sb_tid,
        72,
        7,
        5.0,
        vec![(6, 25.0), (73, 5_000.0), (99, 5_000.0), (114, 100.0)],
    );
    sb_t.effect_ids = vec![12, 16];
    sde.types.insert(sb_tid, sb_t);
    ctx.sde = std::sync::Arc::new(sde);
    ctx.db
        .acquire_online_lock(
            session.user_id.unwrap_or(1),
            999_441,
            Some(char_id),
            true,
            ctx.now_unix(),
        )
        .unwrap();

    let sb_id = 846_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        // Ensure zero Upwell structures in `sys_id` so `is_upwell_weapon_or_aoe == false`
        // and `handle_module_cycles` takes the local cycle-commit path (`#1441`).
        snap.structures.retain(|_, st| st.solar_system_id != sys_id);
        snap.items.insert(
            sb_id,
            make_fighter_item(sb_id, sb_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_online_states.insert(sb_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 23_915,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state
            .modified_system_securities
            .insert(sys_id, 0.9);
    });

    let reset_ship_state = |cloaked: bool, gate_ticks: u32, undock_ticks: u32| {
        let sys_c = sys_handle.clone();
        let ctx_c = ctx.clone();
        async move {
            let _ = sys_c.remove_module_cycle(ship_id, sb_id).await;
            ctx_c.db.with_mut_snapshot(|snap| {
                snap.module_active_states.remove(&sb_id);
            });
            ctx_c.invalidate_ship_fitting(ship_id);
            ctx_c.clear_all_reactivation_delays();
            sys_c
                .set_combat_state(
                    ship_id,
                    CombatState {
                        shield_hp: 50_000.0,
                        max_shield_hp: 50_000.0,
                        armor_hp: 50_000.0,
                        max_armor_hp: 50_000.0,
                        structure_hp: 50_000.0,
                        max_structure_hp: 50_000.0,
                        capacitor: 500.0,
                        max_capacitor: 500.0,
                        signature_radius: 100.0,
                        base_signature_radius: Some(100.0),
                        is_tethered: false,
                        is_cloaked: cloaked,
                        gate_cloak_remaining_ticks: gate_ticks,
                        undock_invulnerable_remaining_ticks: undock_ticks,
                        ..CombatState::default()
                    },
                )
                .await
                .unwrap();
            let _ = ctx_c.drain_notifications_for_char(char_id);
        }
    };

    // 1. Rejected Smartbomb via `handle_module_cycles` (Green safety in highsec) preserves all cloak/invuln flags and emits zero cancellation notifications.
    set_char_safety_level(&ctx, char_id, SafetyLevel::Green);
    reset_ship_state(true, 30, 15).await;
    let err = handle_module_cycles(
        &ctx,
        &mut session,
        None,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(sb_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .expect_err("Green safety highsec Smartbomb via handle_module_cycles must reject");
    assert_eq!(err.code, "CantDoInCurrentSafetyLevel");
    let cs_rej = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_rej.is_cloaked);
    assert_eq!(cs_rej.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_rej.undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));

    // 2. Core #1441 regression: Smartbomb (`72`) commit via `handle_module_cycles` in a system
    //    without Upwell structures with `undock_invulnerable_remaining_ticks > 0`
    //    (both with and without gate cloak) must emit exactly ONE
    //    `OnInvulnCancelled`, clear invuln, and break gate cloak when present.
    set_char_safety_level(&ctx, char_id, SafetyLevel::Red);
    for (cloaked, gate_ticks, label) in [
        (true, 30_u32, "gate-cloaked + undock_invuln"),
        (false, 0_u32, "non-gate undock_invuln"),
    ] {
        reset_ship_state(cloaked, gate_ticks, 15).await;
        let ok = handle_module_cycles(
            &ctx,
            &mut session,
            None,
            "dogmaIM",
            "Activate",
            &[PyRep::Integer(sb_id)],
            &kwargs,
        )
        .await
        .unwrap()
        .unwrap_or_else(|e| panic!("Smartbomb commit via handle_module_cycles ({label}) must succeed, got {e:?}"));
        assert_eq!(ok.dict_get("moduleID"), Some(&PyRep::Integer(sb_id)), "{label}");
        assert!(sys_handle.has_module_cycle(ship_id, sb_id).await, "{label}: cycle must be registered locally");
        let cs_ok = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(!cs_ok.is_cloaked, "{label}: must uncloak");
        assert_eq!(cs_ok.gate_cloak_remaining_ticks, 0, "{label}: must clear gate cloak");
        assert_eq!(cs_ok.undock_invulnerable_remaining_ticks, 0, "{label}: must clear undock invuln");
        let notes = ctx.drain_notifications_for_char(char_id);
        assert_eq!(
            notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(),
            1,
            "{label}: must emit exactly one OnInvulnCancelled"
        );
        assert_eq!(
            notes.iter().filter(|n| n.event == "OnJumpCloakCancelled").count(),
            usize::from(gate_ticks > 0),
            "{label}: OnJumpCloakCancelled count"
        );
    }

    // 3. Direct `activate_aoe_or_defender_module_with_cap` caller still owns cloak/invuln break
    //    and emits a single `OnInvulnCancelled` + `OnJumpCloakCancelled`.
    let fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).unwrap();
    let sb_fm = fit.modules[&sb_id].clone();
    let sb_type = ctx.sde.types[&sb_tid].clone();
    let attacker_ball = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1;

    reset_ship_state(true, 30, 15).await;
    activate_aoe_or_defender_module_with_cap(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        0.9,
        ctx.now_unix(),
        sb_id,
        &sb_fm,
        &sb_type,
        &attacker_ball,
        None,
        &kwargs,
        true,
    )
    .await
    .unwrap()
    .expect("direct activate_aoe_or_defender_module_with_cap must succeed");
    let cs_direct = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_direct.is_cloaked);
    assert_eq!(cs_direct.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs_direct.undock_invulnerable_remaining_ticks, 0);
    let direct_notes = ctx.drain_notifications_for_char(char_id);
    assert_eq!(direct_notes.iter().filter(|n| n.event == "OnJumpCloakCancelled").count(), 1);
    assert_eq!(direct_notes.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);

    // 4. Explicit `activate_aoe_or_defender_module_with_options(..., false, false)` defers cloak/invuln
    //    break to the outer cycle commit owner (`fitting_module_cycles`).
    reset_ship_state(true, 30, 15).await;
    activate_aoe_or_defender_module_with_options(
        &ctx,
        char_id,
        ship_id,
        sys_id,
        0.9,
        ctx.now_unix(),
        sb_id,
        &sb_fm,
        &sb_type,
        &attacker_ball,
        None,
        &kwargs,
        false,
        false,
    )
    .await
    .unwrap()
    .expect("deferred-break activate_aoe_or_defender_module_with_options must succeed");
    let cs_deferred = sys_handle.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_deferred.is_cloaked);
    assert_eq!(cs_deferred.gate_cloak_remaining_ticks, 30);
    assert_eq!(cs_deferred.undock_invulnerable_remaining_ticks, 15);
    assert!(!ctx
        .drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnJumpCloakCancelled" || n.event == "OnInvulnCancelled"));
}

#[tokio::test]
async fn crystal_volatility_wear_sets_singleton_and_version_and_shatter_removes_damage_attr() {
    use eve_sde::attr_ids;

    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(
        53_099,
        make_fighter_type(
            53_099,
            53,
            7,
            5.0,
            vec![
                (6, 10.0),
                (51, 3_000.0),
                (54, 15_000.0),
                (64, 1.0),
                (128, 1.0),
                (158, 5_000.0),
                (160, 100.0),
                (604, 86.0),
            ],
        ),
    );
    sde.types.insert(
        86_099,
        make_fighter_type(
            86_099,
            86,
            8,
            1.0,
            vec![
                (9, 1.0),
                (114, 50.0),
                (128, 1.0),
                (783, 1.0),
                (784, 0.25),
                (786, 1.0),
            ],
        ),
    );
    sde.types.insert(
        10_099,
        make_fighter_type(10_099, 550, 11, 1_000.0, vec![(attr_ids::ENTITY_KILL_BOUNTY, 10_000.0)]),
    );
    ctx.sde = std::sync::Arc::new(sde);

    let laser_single_id = 890_001_i64;
    let crystal_single_id = 890_002_i64;
    let laser_stack_id = 890_003_i64;
    let crystal_stack_id = 890_004_i64;
    let npc_id = 990_099_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            laser_single_id,
            make_fighter_item(laser_single_id, 53_099, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        let mut single_crys = make_fighter_item(crystal_single_id, 86_099, char_id, ship_id, InventoryFlag::HiSlot0, 1);
        single_crys.singleton = false;
        snap.items.insert(crystal_single_id, single_crys);

        snap.items.insert(
            laser_stack_id,
            make_fighter_item(laser_stack_id, 53_099, char_id, ship_id, InventoryFlag::HiSlot1, 1),
        );
        let mut stacked_crys = make_fighter_item(crystal_stack_id, 86_099, char_id, ship_id, InventoryFlag::HiSlot1, 2);
        stacked_crys.dynamic_attributes.insert(attr_ids::DAMAGE, 0.80);
        snap.items.insert(crystal_stack_id, stacked_crys);
    });
    ctx.invalidate_ship_fitting(ship_id);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (id, tid, pos) in [
        (ship_id, 23_915_u32, DVec3::ZERO),
        (npc_id, 10_099_u32, DVec3::new(5_000.0, 0.0, 0.0)),
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
                    shield_hp: 5_000.0,
                    max_shield_hp: 5_000.0,
                    armor_hp: 5_000.0,
                    max_armor_hp: 5_000.0,
                    structure_hp: 5_000.0,
                    max_structure_hp: 5_000.0,
                    capacitor: 500.0,
                    max_capacitor: 500.0,
                    signature_radius: 100.0,
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

    // 1. Non-fatal volatility wear on quantity=1, singleton=false crystal sets singleton=true, DAMAGE, and increments version
    let wear_res = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_single_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(wear_res.dict_get("crystalShattered"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| {
        let crys = &snap.items[&crystal_single_id];
        assert!(crys.singleton);
        assert_eq!(crys.version, 2);
        assert_eq!(crys.dynamic_attributes.get(&attr_ids::DAMAGE).copied(), Some(0.25));
    });

    // 2. Multi-unit shatter (0.80 + 0.25 >= 1.0) decrements quantity 2->1 and removes DAMAGE key completely
    let shatter_res = handle_weapons(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(laser_stack_id), PyRep::String("targetAttack".into()), PyRep::Integer(npc_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(shatter_res.dict_get("crystalShattered"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let rem = &snap.items[&crystal_stack_id];
        assert_eq!(rem.quantity, 1);
        assert_eq!(rem.version, 2);
        assert!(!rem.dynamic_attributes.contains_key(&attr_ids::DAMAGE));
    });
}


