//! Additional edge-case and branch coverage tests for `combat_fighters_abilities`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_item, make_fighter_type,
};
use super::super::*;
use super::{make_ability_test_ctx, py_key, spawn_fighter_and_ship, squad_record};
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_db::types::PersistedCrimeWatchState;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta, TargetLock, WarpPhase};
use eve_proto::PyRep;
use eve_sde::{FighterAbilityInfo, FighterAbilitySlot};
use glam::DVec3;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn operational_state_gate_cloak_warp_tether_parent_cloak_mod_and_fighter_locks() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_ability_test_ctx();
    let fid = 410_001_i64;
    let target_id = 510_001_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    sde_mut.types.insert(
        992_001,
        make_fighter_type(992_001, 330, 7, 10.0, vec![]), // Cloaking Device
    );
    sde_mut.types.insert(
        992_002,
        make_fighter_type(
            992_002,
            1652,
            87,
            1_000.0,
            vec![(2215, 1.0)], // Zero weapon attrs -> fallback damage 20.0 * mult
        ),
    );
    sde_mut.fighter_abilities.insert(
        20,
        FighterAbilityInfo {
            ability_id: 20,
            target_mode: "itemTargeted".into(),
            disallow_in_high_sec: false,
            disallow_in_low_sec: false,
            icon_id: Some(20),
            display_name_id: None,
            ability_name_id: None,
            tooltip_text_id: None,
            turret_graphic_id: None,
            raw_json: serde_json::Value::Null,
        },
    );
    sde_mut.fighter_abilities_by_type.insert(
        992_002,
        vec![FighterAbilitySlot {
            slot_index: 0,
            ability_id: 20,
            cooldown_seconds: Some(0.0),
            charges: None,
        }],
    );

    spawn_fighter_and_ship(&ctx, sys_id, ship_id, fid, 992_002, char_id).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_meta(
            Ball {
                id: target_id,
                type_id: 587,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1.0e6,
                radius: 40.0,
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
                ..CombatState::default()
            },
            SlimItemMeta {
                char_id: Some(char_id), // self-target allowed under Green safety
                owner_id: char_id,
                group_id: 25,
                category_id: 6,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // 1. Fighter gate cloak remaining ticks -> FighterCloaked; tethered -> FighterTethered
    let mut f_cs = sys_h.get_combat_state(fid).await.unwrap().unwrap();
    f_cs.gate_cloak_remaining_ticks = 5;
    sys_h.set_combat_state(fid, f_cs.clone()).await.unwrap();
    assert_eq!(
        activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(target_id))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FighterCloaked"
    );
    f_cs.gate_cloak_remaining_ticks = 0;
    f_cs.is_tethered = true;
    sys_h.set_combat_state(fid, f_cs.clone()).await.unwrap();
    assert_eq!(
        activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(target_id))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FighterTethered"
    );
    f_cs.is_tethered = false;
    sys_h.set_combat_state(fid, f_cs).await.unwrap();

    // 2. Parent ship active cloak module (group 330) -> CannotActivateWhileCloaked
    let cloak_mod_id = 730_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            make_fighter_item(cloak_mod_id, 992_001, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    assert_eq!(
        activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(target_id))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CannotActivateWhileCloaked"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
    });

    // 3. Invalid self-target on fighter ball ID or <= 0
    assert_eq!(
        activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(fid))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidTarget"
    );
    assert_eq!(
        activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(0))
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidTarget"
    );

    // 4. Fighter holds lock on sys_h (parent does not hold lock) + self-attack under Green safety + ability 20 fallback 20.0*mult
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    sys_h
        .add_target_lock(
            fid,
            TargetLock {
                target_id,
                lock_time_ms: 1.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    let ok = activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(target_id))
        .await
        .unwrap()
        .expect("self-attack with fighter lock should succeed");
    assert_eq!(py_key(&ok, "success"), Some(&PyRep::Bool(true)));
    let post_cs = sys_h.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(post_cs.shield_hp < 1_000.0);

    // 5. Fighter in warp -> FighterInWarp
    let warp_mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    sys_h.set_movement_mode(fid, warp_mode).await.unwrap();
    let e_warp = activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(target_id))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(e_warp.code, "FighterInWarp");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn legal_green_safety_pvp_ecm_damp_ability9_and_deactivate_slots() {
    let (mut ctx, mut sess, char_id, ship_id, sys_id) = make_ability_test_ctx();
    let victim_cid = 90_000_077_i64;
    let victim_ship = 511_001_i64;
    let fid = 411_001_i64;
    let fid2 = 411_002_i64;

    let sde_mut = Arc::make_mut(&mut ctx.sde);
    for (aid, mode) in [
        (7_u32, "itemTargeted"),
        (9_u32, "untargeted"),
        (11_u32, "itemTargeted"),
        (12_u32, "itemTargeted"),
        (17_u32, "itemTargeted"),
        (18_u32, "itemTargeted"),
        (33_u32, "itemTargeted"),
        (35_u32, "itemTargeted"),
        (36_u32, "itemTargeted"),
        (45_u32, "itemTargeted"),
    ] {
        sde_mut.fighter_abilities.insert(
            aid,
            FighterAbilityInfo {
                ability_id: aid,
                target_mode: mode.into(),
                disallow_in_high_sec: false,
                disallow_in_low_sec: false,
                icon_id: Some(aid),
                display_name_id: None,
                ability_name_id: None,
                tooltip_text_id: None,
                turret_graphic_id: None,
                raw_json: serde_json::Value::Null,
            },
        );
    }

    // Victim hull type with low sensor strength (2.0 on attr 208) and 50km base lock range (attr 76)
    sde_mut.types.insert(
        992_010,
        make_fighter_type(992_010, 25, 6, 40.0, vec![(76, 50_000.0), (208, 2.0)]),
    );
    // Support fighter with ECM(12), Damp(17), Untargeted MWD(9)
    sde_mut.types.insert(
        992_011,
        make_fighter_type(
            992_011,
            1537,
            87,
            1_000.0,
            vec![
                (2215, 2.0),
                (2154, 150.0), // fallback MWD boost
                (2205, 4.0),   // heavy neut
                (2211, 50.0),  // damp pct
                (2184, -40.0), // web factor
                (2224, 30.0),  // TP sig bonus
                (2130, 5.0),
                (2131, 40.0),  // guaranteed ECM jam
            ],
        ),
    );
    sde_mut.fighter_abilities_by_type.insert(
        992_011,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 12, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 9, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 17, cooldown_seconds: None, charges: None },
        ],
    );
    // Custom ability type without charge attr 2324 on ability 7 (torpedo/salvo without valid ammo)
    sde_mut.types.insert(
        992_012,
        make_fighter_type(992_012, 1652, 87, 1_000.0, vec![(2215, 1.0)]),
    );
    sde_mut.fighter_abilities_by_type.insert(
        992_012,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 7, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 11, cooldown_seconds: None, charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 45, cooldown_seconds: None, charges: None },
        ],
    );

    spawn_fighter_and_ship(&ctx, sys_id, ship_id, fid, 992_011, char_id).await;
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: fid2,
                type_id: 992_012,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 400.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: 25.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    ctx.active_fighters.write().unwrap().insert(
        fid2,
        squad_record(fid2, 992_012, char_id, ship_id, sys_id, 2),
    );

    let far_lock = 511_099_i64;
    let near_lock = 511_098_i64;
    for (bid, pos) in [
        (victim_ship, DVec3::new(2_000.0, 0.0, 0.0)),
        (near_lock, DVec3::new(3_000.0, 0.0, 0.0)),
        (far_lock, DVec3::new(200_000.0, 0.0, 0.0)),
    ] {
        sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: 992_010,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1.0e6,
                    radius: 40.0,
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
                    signature_radius: 100.0,
                    ..CombatState::default()
                },
                SlimItemMeta {
                    char_id: Some(victim_cid),
                    owner_id: victim_cid,
                    group_id: 25,
                    category_id: 6,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    // Victim is Suspect + Safety Green -> legal attack in Highsec! And ship/victim hold locks
    let future_unix = ctx.clock.read().unwrap().now_unix_secs() + 600;
    ctx.db.with_mut_snapshot(|snap| {
        snap.crimewatch_states.insert(
            victim_cid,
            PersistedCrimeWatchState {
                character_id: victim_cid,
                suspect_flag_until_unix: future_unix,
                ..Default::default()
            },
        );
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    for (src, dst) in [
        (ship_id, victim_ship),
        (victim_ship, near_lock),
        (victim_ship, far_lock),
    ] {
        sys_h
            .add_target_lock(
                src,
                TargetLock {
                    target_id: dst,
                    lock_time_ms: 1.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .unwrap();
    }

    // ECM slot 0 -> lands jam and clears victim locks
    let ecm_ok = activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 0, Some(victim_ship))
        .await
        .unwrap()
        .expect("suspect legal under green safety + ECM lands");
    assert_eq!(py_key(&ecm_ok, "success"), Some(&PyRep::Bool(true)));
    assert!(matches!(py_key(&ecm_ok, "ewarStrength"), Some(PyRep::Real(v)) if *v > 0.0));
    let v_cs = sys_h.get_combat_state(victim_ship).await.unwrap().unwrap();
    assert!(v_cs.ecm_jammed_remaining_s > 0.0);
    assert!(sys_h.get_target_locks(victim_ship).await.unwrap().is_empty());

    // Re-seed victim target locks and activate Damp slot 2 -> keeps near_lock (1km <= damped range), drops far_lock (198km)
    for dst in [near_lock, far_lock] {
        sys_h
            .add_target_lock(
                victim_ship,
                TargetLock {
                    target_id: dst,
                    lock_time_ms: 1.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .unwrap();
    }
    activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 2, Some(victim_ship))
        .await
        .unwrap()
        .unwrap();
    let v_locks = sys_h.get_target_locks(victim_ship).await.unwrap();
    assert!(v_locks.iter().any(|l| l.target_id == near_lock));
    assert!(!v_locks.iter().any(|l| l.target_id == far_lock));

    // Ability 9 untargeted MWD: first activation scales speed, repeated activation (after cooldown) with mwd_active == true does not double
    activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 1, None)
        .await
        .unwrap()
        .unwrap();
    let v1 = sys_h.get_ball_snapshot(fid).await.unwrap().unwrap().1.max_velocity;
    ctx.fighter_cooldown_state.write().unwrap().remove(&(fid, 1));
    activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid, 1, None)
        .await
        .unwrap()
        .unwrap();
    let v2 = sys_h.get_ball_snapshot(fid).await.unwrap().unwrap().1.max_velocity;
    assert!((v1 - v2).abs() < 1e-6);

    // Ability 7 without attr 2324 deals 0 damage; Ability 36 neut drains cap; Ability 45 paints target
    let pre_hp = sys_h.get_combat_state(victim_ship).await.unwrap().unwrap().shield_hp;
    let a7 = activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid2, 0, Some(victim_ship))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(py_key(&a7, "success"), Some(&PyRep::Bool(true)));
    let post_hp = sys_h.get_combat_state(victim_ship).await.unwrap().unwrap().shield_hp;
    assert!((post_hp - pre_hp).abs() < 1e-6);

    activate_single_fighter_ability(&ctx, &mut sess, char_id, sys_id, fid2, 1, Some(victim_ship))
        .await
        .unwrap()
        .unwrap();
    assert!(sys_h.get_combat_state(victim_ship).await.unwrap().unwrap().capacitor < 500.0);

    // handle_deactivate_fighter_ability_slots: unknown fighter fails; slot 0 clears target_id; slot 1 skips already false MWD
    assert_eq!(
        handle_deactivate_fighter_ability_slots(
            &ctx,
            &mut sess,
            &[PyRep::List(vec![PyRep::Integer(999_888)]), PyRep::Integer(0)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotFound"
    );
    ctx.active_fighters.write().unwrap().get_mut(&fid).unwrap().target_id = Some(victim_ship);
    handle_deactivate_fighter_ability_slots(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(fid)]), PyRep::Integer(0)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(ctx.active_fighters.read().unwrap()[&fid].target_id, None);
    // Deactivate MWD twice -> second call exercises mwd_active == false branch cleanly
    handle_deactivate_fighter_ability_slots(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(fid)]), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
    handle_deactivate_fighter_ability_slots(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(fid)]), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activate_ability_on_point_all_error_and_aoe_filter_branches() {
    let (ctx, mut sess, char_id, ship_id, _hi_sys) = make_ability_test_ctx();
    let low_sys = 30_000_200_u32;
    sess.solar_system_id = Some(low_sys);

    let bomber_id = 412_001_i64;
    let tethered_target = 512_001_i64;
    let invuln_target = 512_002_i64;
    let illegal_pvp_target = 512_003_i64;
    let victim_cid = 90_000_088_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut rec = snap.characters.values().next().unwrap().clone();
        rec.character_id = victim_cid;
        rec.corporation_id = 1_000_099;
        rec.active_ship_id = illegal_pvp_target;
        rec.solar_system_id = low_sys;
        snap.characters.insert(victim_cid, rec);
    });

    spawn_fighter_and_ship(&ctx, low_sys, ship_id, bomber_id, 23_063, char_id).await;
    let sys_h = ctx.systems.get_or_spawn(low_sys);
    for (bid, cid, tethered, invuln) in [
        (tethered_target, victim_cid, true, false),
        (invuln_target, victim_cid, false, true),
        (illegal_pvp_target, victim_cid, false, false),
    ] {
        sys_h
            .spawn_ball_with_meta(
                Ball {
                    id: bid,
                    type_id: 587,
                    position: DVec3::new(10_000.0, 0.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1.0e6,
                    radius: 40.0,
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
                    is_tethered: tethered,
                    warp_exit_invulnerable_remaining_ticks: if invuln { 10 } else { 0 },
                    ..CombatState::default()
                },
                SlimItemMeta {
                    char_id: Some(cid),
                    owner_id: cid,
                    group_id: 25,
                    category_id: 6,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    let point = PyRep::Tuple(vec![
        PyRep::Real(10_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
    ]);

    // Error paths: FighterNotFound, InvalidFighterAbilitySlot, FighterNotOwned, FighterNotInSystem
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut sess,
            &[PyRep::Integer(999_777), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotFound"
    );
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut sess,
            &[PyRep::Integer(bomber_id), PyRep::Integer(9), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidFighterAbilitySlot"
    );

    let mut wrong_owner = sess.clone();
    wrong_owner.character_id = Some(90_999_999);
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut wrong_owner,
            &[PyRep::Integer(bomber_id), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotOwned"
    );

    let other_low_sys = ctx
        .sde
        .solar_systems
        .values()
        .find(|s| s.solar_system_id != low_sys && s.security_status < 0.45 && s.security_status > 0.0)
        .map(|s| s.solar_system_id)
        .unwrap_or(30_002_048);
    let mut wrong_sys = sess.clone();
    wrong_sys.solar_system_id = Some(other_low_sys); // Lowsec system != low_sys so FighterNotInSystem fires cleanly
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut wrong_sys,
            &[PyRep::Integer(bomber_id), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotInSystem"
    );

    // Valid bomb on point in lowsec hits ONLY illegal_pvp_target, skipping tethered & invuln, and flags CrimeWatch suspect
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let res = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut sess,
        &[PyRep::List(vec![PyRep::Integer(bomber_id)]), PyRep::Integer(2), point],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&res, "success"), Some(&PyRep::Bool(true)));
    assert!((sys_h.get_combat_state(tethered_target).await.unwrap().unwrap().shield_hp - 1_000.0).abs() < 1e-6);
    assert!((sys_h.get_combat_state(invuln_target).await.unwrap().unwrap().shield_hp - 1_000.0).abs() < 1e-6);
    assert!(sys_h.get_combat_state(illegal_pvp_target).await.unwrap().unwrap().shield_hp < 1_000.0);
    let now_u = ctx.clock.read().unwrap().now_unix_secs();
    assert!(ctx
        .crimewatch
        .read()
        .unwrap()
        .states
        .get(&char_id)
        .is_some_and(|s| s.is_suspect(now_u)));
}
