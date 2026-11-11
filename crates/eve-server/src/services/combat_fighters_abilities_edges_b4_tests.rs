//! Behavioral edge-case tests for `combat_fighters_abilities.rs`.

#![forbid(unsafe_code)]

use crate::router::context::ActiveFighterSquadron;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_sde_registry, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_fighters_abilities::*;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_db::{InventoryFlag, ServerClock, VirtualServerClock};
use eve_destiny::{Ball, CombatState, MovementMode, WarpPhase};
use eve_proto::PyRep;
use eve_sde::{FighterAbilityCharges, FighterAbilityInfo, FighterAbilitySlot};
use glam::DVec3;
use std::sync::Arc;

fn make_ball(id: i64, type_id: u32, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 0.5,
        mass: 10_000.0,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn make_cs(shield: f64, armor: f64, structure: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield,
        max_shield_hp: shield,
        armor_hp: armor,
        max_armor_hp: armor,
        structure_hp: structure,
        max_structure_hp: structure,
        capacitor: cap,
        max_capacitor: cap,
        ..CombatState::default()
    }
}

fn insert_ability(
    sde: &mut eve_sde::SdeRegistry,
    ability_id: u32,
    target_mode: &str,
    disallow_hi: bool,
    disallow_lo: bool,
) {
    sde.fighter_abilities.insert(
        ability_id,
        FighterAbilityInfo {
            ability_id,
            target_mode: target_mode.into(),
            disallow_in_high_sec: disallow_hi,
            disallow_in_low_sec: disallow_lo,
            icon_id: None,
            display_name_id: Some(500),
            ability_name_id: Some(501),
            tooltip_text_id: Some(502),
            turret_graphic_id: Some(503),
            raw_json: serde_json::Value::Null,
        },
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn operational_state_warp_cloak_tether_and_active_cloak_module_block_activation() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000));
    ctx.set_clock(clock);

    let fighter_tid = 23057_u32;
    let cloak_tid = 11370_u32;
    let cloak_iid = 77_113_700_i64;
    let fighter_id = 5_001_001_i64;

    let mut sde = make_fighter_sde_registry();
    insert_ability(&mut sde, 13, "untargeted", false, false);
    sde.fighter_abilities_by_type.insert(
        fighter_tid,
        vec![FighterAbilitySlot {
            slot_index: 1,
            ability_id: 13,
            cooldown_seconds: Some(10.0),
            charges: None,
        }],
    );
    sde.types.insert(
        cloak_tid,
        make_fighter_type(cloak_tid, 330, 7, 5.0, vec![]), // group 330 = Cloaking Device
    );
    ctx.sde = Arc::new(sde);

    ctx.active_fighters.write().unwrap().insert(
        fighter_id,
        ActiveFighterSquadron {
            ball_id: fighter_id,
            item_id: fighter_id,
            type_id: fighter_tid,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 9,
            target_id: None,
            mwd_active: false,
            is_abandoned: false,
        },
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 23915, DVec3::ZERO, 1_500.0),
            make_cs(10_000.0, 10_000.0, 10_000.0, 10_000.0),
        )
        .await
        .unwrap();

    // 1. Fighter in Warp -> FighterInWarp
    let mut f_warp = make_ball(fighter_id, fighter_tid, DVec3::new(2_000.0, 0.0, 0.0), 50.0);
    f_warp.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    sys_handle
        .spawn_ball_with_combat(f_warp, make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0))
        .await
        .unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterInWarp"
    );

    // 2. Fighter cloaked -> FighterCloaked; fighter tethered -> FighterTethered
    let f_stop = make_ball(fighter_id, fighter_tid, DVec3::new(2_000.0, 0.0, 0.0), 50.0);
    let mut f_cloak_cs = make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0);
    f_cloak_cs.is_cloaked = true;
    sys_handle
        .spawn_ball_with_combat(f_stop.clone(), f_cloak_cs)
        .await
        .unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterCloaked"
    );

    let mut f_tether_cs = make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0);
    f_tether_cs.is_tethered = true;
    sys_handle
        .spawn_ball_with_combat(f_stop.clone(), f_tether_cs)
        .await
        .unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterTethered"
    );

    // Restore normal fighter
    sys_handle
        .spawn_ball_with_combat(f_stop, make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0))
        .await
        .unwrap();

    // 3. Parent carrier in Warp -> ShipInWarp
    let mut p_warp = make_ball(ship_id, 23915, DVec3::ZERO, 1_500.0);
    p_warp.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 1.0,
    };
    sys_handle
        .spawn_ball_with_combat(p_warp, make_cs(10_000.0, 10_000.0, 10_000.0, 10_000.0))
        .await
        .unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "ShipInWarp"
    );

    // 4. Parent carrier cloaked -> CannotActivateWhileCloaked; tethered -> CannotActivateWhileTethered
    let p_stop = make_ball(ship_id, 23915, DVec3::ZERO, 1_500.0);
    let mut p_cloak_cs = make_cs(10_000.0, 10_000.0, 10_000.0, 10_000.0);
    p_cloak_cs.gate_cloak_remaining_ticks = 5;
    sys_handle
        .spawn_ball_with_combat(p_stop.clone(), p_cloak_cs)
        .await
        .unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotActivateWhileCloaked"
    );

    let mut p_tether_cs = make_cs(10_000.0, 10_000.0, 10_000.0, 10_000.0);
    p_tether_cs.is_tethered = true;
    sys_handle
        .spawn_ball_with_combat(p_stop.clone(), p_tether_cs)
        .await
        .unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotActivateWhileTethered"
    );

    // 5. Parent has active cloaking device module in snap.module_active_states -> CannotActivateWhileCloaked
    sys_handle
        .spawn_ball_with_combat(p_stop, make_cs(10_000.0, 10_000.0, 10_000.0, 10_000.0))
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_iid,
            make_fighter_item(cloak_iid, cloak_tid, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(cloak_iid);
    });
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotActivateWhileCloaked"
    );

    // 6. Remove active cloak, activate Evasive Maneuvers (ability 13), then deactivate and verify velocity + sig restored
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_iid);
    });
    let mut f_sig_cs = make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0);
    f_sig_cs.signature_radius = 40.0;
    sys_handle
        .spawn_ball_with_combat(
            make_ball(fighter_id, fighter_tid, DVec3::new(2_000.0, 0.0, 0.0), 50.0),
            f_sig_cs,
        )
        .await
        .unwrap();

    handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx.active_fighters.read().unwrap().get(&fighter_id).unwrap().mwd_active);

    let deact = handle_deactivate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fighter_id), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(deact.dict_get("deactivated").and_then(|v| v.as_bool()), Some(true));
    assert!(!ctx.active_fighters.read().unwrap().get(&fighter_id).unwrap().mwd_active);
    let restored_ball = sys_handle.get_ball_snapshot(fighter_id).await.unwrap().unwrap().1;
    assert!((restored_ball.max_velocity - 400.0).abs() < 1e-6);
    let restored_cs = sys_handle.get_combat_state(fighter_id).await.unwrap().unwrap();
    assert!((restored_cs.signature_radius - 40.0).abs() < 1e-6);
    assert!(session
        .notifications
        .iter()
        .any(|(ev, _)| ev == "OnFighterAbilitySlotDeactivated"));
}

#[tokio::test(flavor = "multi_thread")]
async fn point_targeted_bomb_security_range_cooldown_charges_and_aoe_filtering() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_750_000_000));
    ctx.set_clock(clock.clone());

    let heavy_tid = 40574_u32;
    let bomb_type_tid = 41549_u32;
    let f1 = 5_002_001_i64;
    let f2 = 5_002_002_i64;
    let target_ship_id = 6_002_001_i64;
    let cloaked_ship_id = 6_002_002_i64;
    let pod_ship_id = 6_002_003_i64;

    let mut sde = make_fighter_sde_registry();
    insert_ability(&mut sde, 22, "pointTargeted", true, false);
    insert_ability(&mut sde, 1, "itemTargeted", false, false);
    sde.fighter_abilities_by_type.insert(
        heavy_tid,
        vec![
            FighterAbilitySlot {
                slot_index: 0,
                ability_id: 1,
                cooldown_seconds: None,
                charges: None,
            },
            FighterAbilitySlot {
                slot_index: 2,
                ability_id: 22,
                cooldown_seconds: Some(30.0),
                charges: Some(FighterAbilityCharges {
                    charge_count: 1,
                    rearm_time_seconds: 60.0,
                }),
            },
        ],
    );
    // Heavy fighter type has attribute 2324 pointing at bomb type
    sde.types.insert(
        heavy_tid,
        make_fighter_type(heavy_tid, 1653, 87, 50.0, vec![(2324, bomb_type_tid as f64)]),
    );
    sde.types.insert(
        bomb_type_tid,
        make_fighter_type(
            bomb_type_tid,
            90,
            8,
            1.0,
            vec![(114, 1_000.0), (107, 15_000.0)], // EM dmg = 1000, AoE radius = 15km
        ),
    );
    sde.types
        .insert(587, make_fighter_type(587, 25, 6, 35.0, vec![]));
    sde.types
        .insert(670, make_fighter_type(670, 29, 6, 20.0, vec![])); // Capsule / pod
    ctx.sde = Arc::new(sde);

    for fid in [f1, f2] {
        ctx.active_fighters.write().unwrap().insert(
            fid,
            ActiveFighterSquadron {
                ball_id: fid,
                item_id: fid,
                type_id: heavy_tid,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                tube_index: 0,
                squadron_size: 3,
                target_id: None,
                mwd_active: false,
                is_abandoned: false,
            },
        );
    }

    let point = PyRep::Tuple(vec![
        PyRep::Real(10_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
    ]);

    // 1. Missing solar_system_id -> NoSolarSystem
    let mut no_sys = session.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut no_sys,
            &[PyRep::Integer(f1), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "NoSolarSystem"
    );

    // 2. Non-pointTargeted slot 0 -> InvalidTargetMode
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(0), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTargetMode"
    );

    // 3. Highsec (default seed sys 30000142 has sec >= 0.45) -> BombForbiddenInHighSec
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BombForbiddenInHighSec"
    );

    // Switch system to lowsec (0.2) and test SafetyLevel::Green -> CantDoInCurrentSafetyLevel
    {
        let mut reg = (*ctx.sde).clone();
        if let Some(sys) = reg.solar_systems.get_mut(&sys_id) {
            sys.security_status = 0.2;
        }
        ctx.sde = Arc::new(reg);
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .message,
        "CantDoInCurrentSafetyLevel"
    );

    // Set SafetyLevel::Yellow (blocks pods in lowsec, allows normal ships)
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Yellow);
    });

    // 4. FighterNotInSystem before ball spawned
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FighterNotInSystem"
    );

    // Spawn carrier, f1, f2, normal target, cloaked target, and pod target
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle
        .spawn_ball_with_combat(
            make_ball(ship_id, 23915, DVec3::ZERO, 1_500.0),
            make_cs(10_000.0, 10_000.0, 10_000.0, 10_000.0),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            make_ball(f1, heavy_tid, DVec3::new(-30_000.0, 0.0, 0.0), 50.0),
            make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            make_ball(f2, heavy_tid, DVec3::new(-40_000.0, 0.0, 0.0), 50.0),
            make_cs(1_000.0, 1_000.0, 1_000.0, 1_000.0),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            make_ball(target_ship_id, 587, DVec3::new(12_000.0, 0.0, 0.0), 35.0),
            make_cs(5_000.0, 5_000.0, 5_000.0, 500.0),
        )
        .await
        .unwrap();
    let mut cloaked_cs = make_cs(5_000.0, 5_000.0, 5_000.0, 500.0);
    cloaked_cs.is_cloaked = true;
    sys_handle
        .spawn_ball_with_combat(
            make_ball(cloaked_ship_id, 587, DVec3::new(11_000.0, 0.0, 0.0), 35.0),
            cloaked_cs,
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_ship_id,
            make_fighter_item(pod_ship_id, 670, 90_000_002, i64::from(sys_id), InventoryFlag::None, 1),
        );
    });
    sys_handle
        .spawn_ball_with_combat(
            make_ball(pod_ship_id, 670, DVec3::new(10_500.0, 0.0, 0.0), 20.0),
            make_cs(1_000.0, 1_000.0, 1_000.0, 100.0),
        )
        .await
        .unwrap();

    // 5. TargetOutOfRange when > 100_000 m from fighter
    let far_point = PyRep::Tuple(vec![
        PyRep::Real(250_000.0),
        PyRep::Real(0.0),
        PyRep::Real(0.0),
    ]);
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(2), far_point],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );

    // 6. Multi-fighter point activation returns PyRep::List of 2 results, damages target_ship_id,
    // skips cloaked_ship_id and pod_ship_id (Yellow safety)
    let multi = handle_activate_fighter_ability_slots_on_point(
        &ctx,
        &mut session,
        &[
            PyRep::List(vec![PyRep::Integer(f1), PyRep::Integer(f2)]),
            PyRep::Integer(2),
            point.clone(),
        ],
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(results) = multi else {
        panic!("expected List for multi-fighter point activation");
    };
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].dict_get("remainingCharges").and_then(|v| v.as_i64()),
        Some(0)
    );
    assert_eq!(
        results[0].dict_get("pointTargeted").and_then(|v| v.as_bool()),
        Some(true)
    );

    assert!(sys_handle.get_combat_state(target_ship_id).await.unwrap().unwrap().shield_hp < 5_000.0);
    assert_eq!(
        sys_handle.get_combat_state(cloaked_ship_id).await.unwrap().unwrap().shield_hp,
        5_000.0
    );
    assert_eq!(
        sys_handle.get_combat_state(pod_ship_id).await.unwrap().unwrap().shield_hp,
        1_000.0
    );

    // 7. Immediate re-activation -> AbilityOnCooldown; after 30s cooldown -> OutOfCharges
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(2), point.clone()],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AbilityOnCooldown"
    );
    clock.advance_secs(31);
    assert_eq!(
        handle_activate_fighter_ability_slots_on_point(
            &ctx,
            &mut session,
            &[PyRep::Integer(f1), PyRep::Integer(2), point],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "OutOfCharges"
    );

    // 8. Deactivate error branches: NoSolarSystem, InvalidFighterAbilitySlot, FighterNotFound, FighterNotOwned
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut no_sys, &[PyRep::Integer(f1), PyRep::Integer(0)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f1), PyRep::Integer(5)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidFighterAbilitySlot"
    );
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(999_999), PyRep::Integer(0)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FighterNotFound"
    );
    let mut other_sess = session.clone();
    other_sess.character_id = Some(90_000_099);
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut other_sess, &[PyRep::Integer(f1), PyRep::Integer(0)])
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FighterNotOwned"
    );
}
