//! Extra unit and mutation-killing tests for `services::combat_crimewatch::combat_fighters_abilities`
//! covering slot validation edge cases, lowsec restrictions, range formulas across all ability IDs,
//! structure surface distance math, location-change invulnerability & safety checks, damage/EWAR
//! fallback formulas, MWD/MJD untargeted abilities, and point-targeted bomb fallbacks.

use crate::router::context::ActiveFighterSquadron;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_sde_registry, make_fighter_type,
};
use crate::services::combat_crimewatch::combat_fighters_abilities::*;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, SafetyLevel};
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_proto::PyRep;
use eve_sde::{FighterAbilityCharges, FighterAbilityInfo, FighterAbilitySlot};
use glam::DVec3;
use std::sync::Arc;

fn py_key<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| matches!(k, PyRep::String(s) if s == key)).map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 0.5, mass: 10_000.0, radius, speed_fraction: 0.0, mode: MovementMode::Stop,
    }
}

fn make_cs(shield: f64, armor: f64, structure: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield, max_shield_hp: shield, armor_hp: armor, max_armor_hp: armor,
        structure_hp: structure, max_structure_hp: structure, capacitor: cap, max_capacitor: cap,
        ..CombatState::default()
    }
}

fn insert_ability_def(sde: &mut eve_sde::SdeRegistry, ability_id: u32, target_mode: &str, disallow_hi: bool, disallow_lo: bool) {
    sde.fighter_abilities.insert(
        ability_id,
        FighterAbilityInfo {
            ability_id, target_mode: target_mode.into(), disallow_in_high_sec: disallow_hi,
            disallow_in_low_sec: disallow_lo, icon_id: None, display_name_id: None,
            ability_name_id: None, tooltip_text_id: None, turret_graphic_id: None,
            raw_json: serde_json::Value::Null,
        },
    );
}

#[tokio::test]
async fn activate_and_deactivate_validation_errors_and_lowsec_restriction() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();

    // 1. Unauthenticated session returns error on activate, point-activate, and deactivate
    let mut unauth = session.clone();
    unauth.character_id = None;
    assert!(handle_activate_fighter_ability_slots(&ctx, &mut unauth, &[]).await.unwrap().is_err());
    assert!(handle_activate_fighter_ability_slots_on_point(&ctx, &mut unauth, &[]).await.unwrap().is_err());
    assert!(handle_deactivate_fighter_ability_slots(&ctx, &mut unauth, &[]).await.unwrap().is_err());

    // 2. NoSolarSystem on activate, point-activate, and deactivate
    let mut no_sys = session.clone();
    no_sys.solar_system_id = None;
    assert_eq!(handle_activate_fighter_ability_slots(&ctx, &mut no_sys, &[]).await.unwrap().unwrap_err().code, "NoSolarSystem");
    assert_eq!(handle_activate_fighter_ability_slots_on_point(&ctx, &mut no_sys, &[]).await.unwrap().unwrap_err().code, "NoSolarSystem");
    assert_eq!(handle_deactivate_fighter_ability_slots(&ctx, &mut no_sys, &[]).await.unwrap().unwrap_err().code, "NoSolarSystem");

    // 3. Empty fighter_ids -> FighterNotFound
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::List(vec![]), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "FighterNotFound"
    );

    // 4. Invalid raw_slot_id (< 0 or > 2) on activate and deactivate
    for bad_slot in [-1i64, 3i64] {
        assert_eq!(
            handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(500_001), PyRep::Integer(bad_slot)]).await.unwrap().unwrap_err().code,
            "InvalidFighterAbilitySlot"
        );
        assert_eq!(
            handle_deactivate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(500_001), PyRep::Integer(bad_slot)]).await.unwrap().unwrap_err().code,
            "InvalidFighterAbilitySlot"
        );
    }

    // 5. Abandoned fighter -> FighterNotOwned
    let f_ball = 500_001i64;
    ctx.active_fighters.write().unwrap().insert(
        f_ball,
        ActiveFighterSquadron {
            ball_id: f_ball, item_id: 600_001, type_id: 999_101, owner_char_id: char_id,
            parent_ship_id: ship_id, tube_index: 0, squadron_size: 3, target_id: None,
            is_abandoned: true, mwd_active: false,
        },
    );
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "FighterNotOwned"
    );
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "FighterNotOwned"
    );

    // Un-abandon, test FighterNotInSystem before spawning ball
    ctx.active_fighters.write().unwrap().get_mut(&f_ball).unwrap().is_abandoned = false;
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "FighterNotInSystem"
    );

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    sys_handle.spawn_ball(make_ball(f_ball, 999_101, DVec3::ZERO, 20.0)).await.unwrap();

    // 6. Fighter type has no fighter_abilities_by_type -> InvalidFighterAbilitySlot
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "InvalidFighterAbilitySlot"
    );
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "InvalidFighterAbilitySlot"
    );

    // 7. Slot index missing & ability_id missing from sde.fighter_abilities
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        sde_mut.fighter_abilities_by_type.insert(
            999_101,
            vec![FighterAbilitySlot { slot_index: 0, ability_id: 777_777, cooldown_seconds: Some(5.0), charges: None }],
        );
    }
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(1)]).await.unwrap().unwrap_err().code,
        "InvalidFighterAbilitySlot"
    );
    assert_eq!(
        handle_deactivate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(1)]).await.unwrap().unwrap_err().code,
        "InvalidFighterAbilitySlot"
    );
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "InvalidFighterAbilitySlot"
    );

    // 8. Lowsec restriction: sec = 0.25 and disallow_in_low_sec = true -> AbilityDisallowedInLowSec
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        insert_ability_def(sde_mut, 777_777, "untargeted", false, true);
        if let Some(sys) = sde_mut.solar_systems.get_mut(&sys_id) {
            sys.security_status = 0.25;
        }
    }
    assert_eq!(
        handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0)]).await.unwrap().unwrap_err().code,
        "AbilityDisallowedInLowSec"
    );
}

#[tokio::test]
async fn range_formulas_structure_surface_distance_and_damage_fallbacks() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = make_fighter_sde_registry();
    if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
        sys.security_status = -0.5;
    }

    for aid in [2u32, 7, 10, 11, 12, 14, 16, 17, 20, 33, 34, 38, 44, 90] {
        insert_ability_def(&mut sde, aid, "itemTargeted", false, false);
    }
    sde.types.insert(
        999_200,
        make_fighter_type(
            999_200, 1652, 87, 1_000.0,
            vec![
                (2175, 4_000.0), (2176, 1_000.0), // max_range for 20 = 6000
                (2236, 5_000.0), (2237, 1_500.0), // max_range for 44/7 = 8000
                (2149, 7_000.0),                  // max_range for 33..=36 = 7000
                (2330, 2_000.0),                  // max_range for 38 = 2000
                (2186, 6_000.0), (2187, 500.0),   // max_range for 2 = 7000
                (2204, 9_000.0),                  // max_range for 10 = 9000
                (2209, 4_000.0), (2210, 1_000.0), // max_range for 11 = 6000
                (2221, 5_000.0), (2222, 1_500.0), // max_range for 12 = 8000
                (76, 12_000.0),                   // max_range for 14/17 = 12000
                (2239, 8_500.0),                  // max_range for 16 = 8500
                (2178, 2.0), (2130, 1.5),
                (2131, 10.0), (2132, 20.0), (2133, 30.0), (2134, 40.0),
            ],
        ),
    );
    sde.types.insert(
        999_201,
        make_fighter_type(999_201, 1652, 87, 1_000.0, vec![(2149, 5_500.0), (2130, 1.0)]),
    );
    ctx.sde = Arc::new(sde);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let f_ball = 500_200i64;
    let target_id = 800_200i64;
    let struct_id = 1_000_000_000_500i64;
    sys_handle.spawn_ball(make_ball(ship_id, 23_911, DVec3::ZERO, 500.0)).await.unwrap();
    sys_handle.spawn_ball(make_ball(f_ball, 999_200, DVec3::ZERO, 100.0)).await.unwrap();
    sys_handle.spawn_ball_with_combat(
        make_ball(target_id, 603, DVec3::new(5_900.0, 0.0, 0.0), 50.0),
        make_cs(50_000.0, 50_000.0, 50_000.0, 1_000.0),
    ).await.unwrap();
    // Structure center is at 10,000m, radius 4,500m, fighter radius 100m -> surface distance = 5,400m (<= 6,000m)
    sys_handle.spawn_ball_with_combat(
        make_ball(struct_id, 35_832, DVec3::new(10_000.0, 0.0, 0.0), 4_500.0),
        make_cs(100_000.0, 100_000.0, 100_000.0, 1_000.0),
    ).await.unwrap();
    for tid in [target_id, struct_id] {
        sys_handle.add_target_lock(
            ship_id,
            TargetLock { target_id: tid, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 100.0, max_range_m: None },
        ).await.unwrap();
    }

    ctx.active_fighters.write().unwrap().insert(
        f_ball,
        ActiveFighterSquadron {
            ball_id: f_ball, item_id: 600_200, type_id: 999_200, owner_char_id: char_id,
            parent_ship_id: ship_id, tube_index: 0, squadron_size: 2, target_id: None,
            is_abandoned: false, mwd_active: false,
        },
    );

    let set_slot0 = |ctx: &crate::router::ServerContext, type_id: u32, aid: u32| {
        ctx.fighter_cooldown_state.write().unwrap().clear();
        let mut sde_clone = (*ctx.sde).clone();
        sde_clone.fighter_abilities_by_type.insert(
            type_id,
            vec![FighterAbilitySlot { slot_index: 0, ability_id: aid, cooldown_seconds: Some(1.0), charges: None }],
        );
        sde_clone
    };

    ctx.sde = Arc::new(set_slot0(&ctx, 999_200, 20));
    let res_struct = handle_activate_fighter_ability_slots(
        &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(struct_id)],
    ).await.unwrap();
    assert!(res_struct.is_ok(), "structure surface distance should be within 6,000m: {res_struct:?}");

    for aid in [20u32, 34, 7, 11, 44] {
        ctx.sde = Arc::new(set_slot0(&ctx, 999_200, aid));
        let r = handle_activate_fighter_ability_slots(
            &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(target_id)],
        ).await.unwrap();
        assert!(r.is_ok(), "ability {aid} at 5,900m should succeed: {r:?}");
    }

    ctx.sde = Arc::new(set_slot0(&ctx, 999_200, 38));
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(target_id)],
        ).await.unwrap().unwrap_err().code,
        "TargetOutOfRange"
    );

    ctx.active_fighters.write().unwrap().get_mut(&f_ball).unwrap().type_id = 999_201;
    ctx.sde = Arc::new(set_slot0(&ctx, 999_201, 90));
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(target_id)],
        ).await.unwrap().unwrap_err().code,
        "TargetOutOfRange"
    );

    ctx.sde = Arc::new(set_slot0(&ctx, 999_201, 16));
    let cs_before = sys_handle.get_combat_state(target_id).await.unwrap().unwrap();
    let r16 = handle_activate_fighter_ability_slots(
        &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(target_id)],
    ).await.unwrap();
    assert!(r16.is_ok());
    let cs_after = sys_handle.get_combat_state(target_id).await.unwrap().unwrap();
    assert!(cs_after.shield_hp < cs_before.shield_hp);
}

#[tokio::test]
async fn safety_levels_location_invulnerability_ecm_threshold_and_damp_locks() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = make_fighter_sde_registry();
    if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
        sys.security_status = 0.25;
    }
    insert_ability_def(&mut sde, 12, "itemTargeted", false, false);
    insert_ability_def(&mut sde, 17, "itemTargeted", false, false);
    sde.types.insert(
        999_300,
        make_fighter_type(999_300, 1537, 87, 1_000.0, vec![(2246, 0.5), (76, 50_000.0)]),
    );
    sde.fighter_abilities_by_type.insert(
        999_300,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 12, cooldown_seconds: Some(1.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 17, cooldown_seconds: Some(1.0), charges: None },
        ],
    );
    ctx.sde = Arc::new(sde);

    let victim_char = 90_009_999i64;
    let victim_ship = 800_300i64;
    let victim_pod = 800_301i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.get(&char_id).unwrap().clone();
        ch.character_id = victim_char;
        ch.active_ship_id = victim_ship;
        snap.characters.insert(victim_char, ch);
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let f_ball = 500_300i64;
    sys_handle.spawn_ball(make_ball(ship_id, 23_911, DVec3::ZERO, 500.0)).await.unwrap();
    sys_handle.spawn_ball(make_ball(f_ball, 999_300, DVec3::ZERO, 50.0)).await.unwrap();
    sys_handle.spawn_ball_with_combat(
        make_ball(victim_ship, 603, DVec3::new(2_000.0, 0.0, 0.0), 50.0),
        make_cs(5_000.0, 5_000.0, 5_000.0, 500.0),
    ).await.unwrap();
    sys_handle.spawn_ball_with_combat(
        make_ball(victim_pod, 670, DVec3::new(2_000.0, 0.0, 0.0), 20.0),
        make_cs(500.0, 500.0, 500.0, 100.0),
    ).await.unwrap();
    for tid in [victim_ship, victim_pod] {
        sys_handle.add_target_lock(
            ship_id,
            TargetLock { target_id: tid, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 100.0, max_range_m: None },
        ).await.unwrap();
    }

    ctx.active_fighters.write().unwrap().insert(
        f_ball,
        ActiveFighterSquadron {
            ball_id: f_ball, item_id: 600_300, type_id: 999_300, owner_char_id: char_id,
            parent_ship_id: ship_id, tube_index: 0, squadron_size: 2, target_id: None,
            is_abandoned: false, mwd_active: false,
        },
    );

    let mut vcs = sys_handle.get_combat_state(victim_ship).await.unwrap().unwrap();
    vcs.undock_invulnerable_remaining_ticks = 60;
    vcs.sync_invuln_with_undock_timer = true;
    sys_handle.set_combat_state(victim_ship, vcs.clone()).await.unwrap();
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(victim_ship)],
        ).await.unwrap().unwrap_err().code,
        "TargetInvulnerable"
    );
    vcs.clear_action_invuln();
    sys_handle.set_combat_state(victim_ship, vcs).await.unwrap();

    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Green); });
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(victim_ship)],
        ).await.unwrap().unwrap_err().code,
        "CantDoInCurrentSafetyLevel"
    );

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&victim_char).unwrap().active_ship_id = victim_pod);
    with_mut_combat_runtime(&ctx, |rt| { rt.safety_levels.insert(char_id, SafetyLevel::Yellow); });
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(victim_pod)],
        ).await.unwrap().unwrap_err().code,
        "CantDoInCurrentSafetyLevel"
    );

    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&victim_char).unwrap().active_ship_id = victim_ship);

    let far_ball = 800_399i64;
    sys_handle.spawn_ball(make_ball(far_ball, 603, DVec3::new(45_000.0, 0.0, 0.0), 50.0)).await.unwrap();
    for tid in [ship_id, far_ball] {
        sys_handle.add_target_lock(
            victim_ship,
            TargetLock { target_id: tid, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 100.0, max_range_m: None },
        ).await.unwrap();
    }

    let res_ecm = handle_activate_fighter_ability_slots(
        &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(victim_ship)],
    ).await.unwrap().unwrap();
    assert_eq!(py_key(&res_ecm, "ewarStrength").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(sys_handle.get_target_locks(victim_ship).await.unwrap().len(), 2);

    let res_damp = handle_activate_fighter_ability_slots(
        &ctx, &mut session, &[PyRep::Integer(f_ball), PyRep::Integer(1), PyRep::Integer(victim_ship)],
    ).await.unwrap().unwrap();
    assert!((py_key(&res_damp, "ewarStrength").and_then(|v| v.as_f64()).unwrap() - 0.30).abs() < 1e-6);
    let remaining_locks = sys_handle.get_target_locks(victim_ship).await.unwrap();
    assert_eq!(remaining_locks.len(), 1);
    assert_eq!(remaining_locks[0].target_id, ship_id);
}

#[tokio::test]
async fn untargeted_mwd_mjd_multi_fighter_batch_and_point_bomb_fallbacks() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = make_fighter_sde_registry();
    if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
        sys.security_status = -0.5;
    }
    for (aid, mode) in [(4u32, "untargeted"), (5, "untargeted"), (9, "untargeted"), (50, "pointTargeted")] {
        insert_ability_def(&mut sde, aid, mode, false, false);
    }
    sde.types.insert(999_499, make_fighter_type(999_499, 863, 8, 100.0, vec![]));
    sde.types.insert(
        999_400,
        make_fighter_type(999_400, 1653, 87, 1_000.0, vec![(2151, 300.0), (2154, 50_000.0), (2324, 999_499.0)]),
    );
    sde.fighter_abilities_by_type.insert(
        999_400,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 4, cooldown_seconds: Some(1.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 5, cooldown_seconds: Some(1.0), charges: None },
            FighterAbilitySlot {
                slot_index: 2, ability_id: 50, cooldown_seconds: Some(1.0),
                charges: Some(FighterAbilityCharges { charge_count: 2, rearm_time_seconds: 10.0 }),
            },
        ],
    );
    ctx.sde = Arc::new(sde);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (f1, f2) = (500_401i64, 500_402i64);
    sys_handle.spawn_ball(make_ball(ship_id, 23_911, DVec3::ZERO, 500.0)).await.unwrap();
    for fid in [f1, f2] {
        sys_handle.spawn_ball_with_combat(
            make_ball(fid, 999_400, DVec3::ZERO, 40.0),
            make_cs(1_000.0, 1_000.0, 1_000.0, 100.0),
        ).await.unwrap();
        ctx.active_fighters.write().unwrap().insert(
            fid,
            ActiveFighterSquadron {
                ball_id: fid, item_id: fid + 100_000, type_id: 999_400, owner_char_id: char_id,
                parent_ship_id: ship_id, tube_index: if fid == f1 { 0 } else { 1 },
                squadron_size: 3, target_id: None, is_abandoned: false, mwd_active: false,
            },
        );
    }

    let batch_ok = handle_activate_fighter_ability_slots(
        &ctx, &mut session, &[PyRep::List(vec![PyRep::Integer(f1), PyRep::Integer(f2)]), PyRep::Integer(0)],
    ).await.unwrap().unwrap();
    assert_eq!(py_key(&batch_ok, "success"), Some(&PyRep::Bool(true)));
    let (_, b1_after_mwd) = sys_handle.get_ball_snapshot(f1).await.unwrap().unwrap();
    assert!((b1_after_mwd.max_velocity - 1_200.0).abs() < 1e-6);

    let deact = handle_deactivate_fighter_ability_slots(
        &ctx, &mut session, &[PyRep::List(vec![PyRep::Integer(f1)]), PyRep::Integer(0)],
    ).await.unwrap().unwrap();
    assert_eq!(py_key(&deact, "deactivated"), Some(&PyRep::Bool(true)));
    let (_, b1_restored) = sys_handle.get_ball_snapshot(f1).await.unwrap().unwrap();
    assert!((b1_restored.max_velocity - 300.0).abs() < 1e-6);

    assert!(handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(f1), PyRep::Integer(1)]).await.unwrap().is_ok());

    let npc_ball = 900_401i64;
    sys_handle.spawn_ball_with_combat(
        make_ball(npc_ball, 10_001, DVec3::new(1_000.0, 0.0, 0.0), 50.0),
        make_cs(10_000.0, 10_000.0, 10_000.0, 100.0),
    ).await.unwrap();
    let bomb_res = handle_activate_fighter_ability_slots(
        &ctx, &mut session,
        &[PyRep::Integer(f2), PyRep::Integer(2), PyRep::Real(1_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    ).await.unwrap().unwrap();
    assert_eq!(py_key(&bomb_res, "pointTargeted"), Some(&PyRep::Bool(true)));
    let npc_cs = sys_handle.get_combat_state(npc_ball).await.unwrap().unwrap();
    assert!(npc_cs.shield_hp < 10_000.0);
}

#[tokio::test]
async fn fighter_ability_uses_surface_distance_for_non_upwell_titan_hull_1247() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = make_fighter_sde_registry();
    if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
        sys.security_status = -0.5;
    }
    for (aid, mode) in [
        (11u32, "itemTargeted"),
        (10u32, "itemTargeted"),
        (38u32, "itemTargeted"),
    ] {
        insert_ability_def(&mut sde, aid, mode, false, false);
    }
    sde.types.insert(
        999_500,
        make_fighter_type(
            999_500,
            1652,
            87,
            1_000.0,
            vec![
                (2209, 5_000.0),
                (2211, 100.0),
                (2204, 8_000.0),
                (2330, 1_000.0),
                (2325, 400.0),
            ],
        ),
    );
    sde.fighter_abilities_by_type.insert(
        999_500,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 11, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 10, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot {
                slot_index: 2,
                ability_id: 38,
                cooldown_seconds: Some(0.0),
                charges: Some(FighterAbilityCharges { charge_count: 1, rearm_time_seconds: 10.0 }),
            },
        ],
    );
    ctx.sde = Arc::new(sde);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    // Non-Upwell Titan ball ID (`< 1_000_000_000_000`) with 6_800m Avatar radius at x = 10_820m.
    let titan_ball = 800_501i64;
    let f_mid = 500_501i64;
    let f_close = 500_502i64;
    sys_handle.spawn_ball(make_ball(ship_id, 23_911, DVec3::ZERO, 500.0)).await.unwrap();
    sys_handle
        .spawn_ball_with_combat(
            make_ball(titan_ball, 11_567, DVec3::new(10_820.0, 0.0, 0.0), 6_800.0),
            make_cs(50_000.0, 50_000.0, 50_000.0, 10_000.0),
        )
        .await
        .unwrap();
    sys_handle
        .add_target_lock(
            ship_id,
            TargetLock {
                target_id: titan_ball,
                locked: true,
                remaining_lock_ms: 0.0,
                lock_time_ms: 100.0,
                max_range_m: Some(250_000.0),
            },
        )
        .await
        .unwrap();

    // Fighter `f_mid` at origin: center distance = 10_820m (> 8_000m), surface distance = 4_000m.
    // Fighter `f_close` at x = 3_500m: center distance = 7_320m (> 1_000m), surface distance = 500m (<= 1_000m).
    for (idx, (fid, x)) in [(f_mid, 0.0), (f_close, 3_500.0)].into_iter().enumerate() {
        sys_handle
            .spawn_ball_with_combat(
                make_ball(fid, 999_500, DVec3::new(x, 0.0, 0.0), 20.0),
                make_cs(1_000.0, 1_000.0, 1_000.0, 100.0),
            )
            .await
            .unwrap();
        ctx.active_fighters.write().unwrap().insert(
            fid,
            ActiveFighterSquadron {
                ball_id: fid,
                item_id: fid + 100_000,
                type_id: 999_500,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                tube_index: idx,
                squadron_size: 2,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
    }

    // Short-range Cenobite neut (5_000m) and Siren tackle (8_000m) succeed at 4_000m surface distance (10_820m center).
    assert!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_mid), PyRep::Integer(0), PyRep::Integer(titan_ball)],
        )
        .await
        .unwrap()
        .is_ok(),
        "Cenobite neut (5_000m) must succeed at 4_000m surface distance on non-Upwell Titan"
    );
    assert!(
        sys_handle.get_combat_state(titan_ball).await.unwrap().unwrap().capacitor < 10_000.0
    );
    assert!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_mid), PyRep::Integer(1), PyRep::Integer(titan_ball)],
        )
        .await
        .unwrap()
        .is_ok(),
        "Siren tackle (8_000m) must succeed at 4_000m surface distance on non-Upwell Titan"
    );

    // Shadow sacrifice (1_000m) fails TargetOutOfRange at 4_000m surface distance, succeeds at 500m surface distance (7_320m center).
    assert_eq!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_mid), PyRep::Integer(2), PyRep::Integer(titan_ball)],
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "TargetOutOfRange"
    );
    assert!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_close), PyRep::Integer(2), PyRep::Integer(titan_ball)],
        )
        .await
        .unwrap()
        .is_ok(),
        "Shadow sacrifice (1_000m) must succeed at 500m surface distance (7_320m center) on non-Upwell Titan"
    );
    assert!(
        sys_handle.get_combat_state(titan_ball).await.unwrap().unwrap().shield_hp < 50_000.0
    );
}

#[tokio::test]
async fn fighter_sensor_dampener_preserves_zero_surface_lock_on_titan_1247() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = make_fighter_sde_registry();
    if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
        sys.security_status = -0.5;
    }
    insert_ability_def(&mut sde, 17, "itemTargeted", false, false);
    sde.types.insert(
        999_501,
        make_fighter_type(999_501, 1652, 87, 1_000.0, vec![(76, 50_000.0), (2215, 3.0)]),
    );
    sde.fighter_abilities_by_type.insert(
        999_501,
        vec![FighterAbilitySlot {
            slot_index: 0,
            ability_id: 17,
            cooldown_seconds: Some(0.0),
            charges: None,
        }],
    );
    sde.types.insert(11_567, make_fighter_type(11_567, 30, 6, 10_000_000.0, vec![(76, 5_000.0)]));
    sde.types.insert(23_911, make_fighter_type(23_911, 26, 6, 100_000.0, vec![(76, 5_000.0)]));
    ctx.sde = Arc::new(sde);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let titan_ball = 800_601i64;
    let near_lock = 800_602i64;
    let far_lock = 800_603i64;
    let cruiser_ball = 800_604i64;
    let cruiser_titan_lock = 800_605i64;
    let cruiser_far_lock = 800_606i64;

    sys_handle.spawn_ball(make_ball(ship_id, 23_911, DVec3::ZERO, 200.0)).await.unwrap();
    for (bid, tid, pos, radius) in [
        // Case A: Damped Titan (radius 6_800m) holding 0m-surface lock (center 7_000m > damped_range 4_250m) & OOR lock (5_500m surface)
        (titan_ball, 11_567u32, DVec3::ZERO, 6_800.0),
        (near_lock, 23_911, DVec3::new(7_000.0, 0.0, 0.0), 200.0),
        (far_lock, 23_911, DVec3::new(12_500.0, 0.0, 0.0), 200.0),
        // Case B: Damped cruiser (radius 200m) locking 6_800m Titan at 0m surface distance (center 7_000m) & OOR cruiser (5_600m surface)
        (cruiser_ball, 23_911, DVec3::new(0.0, 20_000.0, 0.0), 200.0),
        (cruiser_titan_lock, 11_567, DVec3::new(0.0, 27_000.0, 0.0), 6_800.0),
        (cruiser_far_lock, 23_911, DVec3::new(6_000.0, 20_000.0, 0.0), 200.0),
    ] {
        sys_handle
            .spawn_ball_with_combat(make_ball(bid, tid, pos, radius), make_cs(50_000.0, 50_000.0, 50_000.0, 10_000.0))
            .await
            .unwrap();
    }

    for (src, dst) in [
        (ship_id, titan_ball),
        (ship_id, cruiser_ball),
        (titan_ball, near_lock),
        (titan_ball, far_lock),
        (cruiser_ball, cruiser_titan_lock),
        (cruiser_ball, cruiser_far_lock),
    ] {
        sys_handle
            .add_target_lock(
                src,
                TargetLock {
                    target_id: dst,
                    lock_time_ms: 100.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: Some(250_000.0),
                },
            )
            .await
            .unwrap();
    }

    let f_titan = 500_601i64;
    let f_cruiser = 500_602i64;
    for (idx, (fid, pos)) in [
        (f_titan, DVec3::new(7_500.0, 0.0, 0.0)),
        (f_cruiser, DVec3::new(0.0, 20_500.0, 0.0)),
    ]
    .into_iter()
    .enumerate()
    {
        sys_handle
            .spawn_ball_with_combat(make_ball(fid, 999_501, pos, 20.0), make_cs(1_000.0, 1_000.0, 1_000.0, 100.0))
            .await
            .unwrap();
        ctx.active_fighters.write().unwrap().insert(
            fid,
            ActiveFighterSquadron {
                ball_id: fid,
                item_id: fid + 100_000,
                type_id: 999_501,
                owner_char_id: char_id,
                parent_ship_id: ship_id,
                tube_index: idx,
                squadron_size: 1,
                target_id: None,
                is_abandoned: false,
                mwd_active: false,
            },
        );
    }

    assert!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_titan), PyRep::Integer(0), PyRep::Integer(titan_ball)],
        )
        .await
        .unwrap()
        .is_ok(),
        "Support fighter sensor dampener (ability 17) must activate on Titan"
    );
    assert!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_cruiser), PyRep::Integer(0), PyRep::Integer(cruiser_ball)],
        )
        .await
        .unwrap()
        .is_ok(),
        "Support fighter sensor dampener (ability 17) must activate on cruiser"
    );

    let titan_locks = sys_handle.get_target_locks(titan_ball).await.unwrap();
    let near_lk = titan_locks.iter().find(|l| l.target_id == near_lock).expect("0m surface lock on Titan must stay locked");
    assert!((near_lk.max_range_m.unwrap() - 11_250.0).abs() < 1e-3);
    assert!(!titan_locks.iter().any(|l| l.target_id == far_lock), "5_500m surface lock must drop past 4_250m damped range");

    let cruiser_locks = sys_handle.get_target_locks(cruiser_ball).await.unwrap();
    let cruiser_titan_lk = cruiser_locks
        .iter()
        .find(|l| l.target_id == cruiser_titan_lock)
        .expect("0m surface lock from cruiser onto Titan must stay locked");
    assert!((cruiser_titan_lk.max_range_m.unwrap() - 11_250.0).abs() < 1e-3);
    assert!(!cruiser_locks.iter().any(|l| l.target_id == cruiser_far_lock), "5_600m surface lock must drop past 4_250m damped range");

    sys_handle.step_manual(1.0).await.unwrap();
    assert!(
        sys_handle.get_target_locks(titan_ball).await.unwrap().iter().any(|l| l.target_id == near_lock),
        "0m surface lock held by damped Titan must survive Ballpark::step"
    );
    assert!(
        sys_handle.get_target_locks(cruiser_ball).await.unwrap().iter().any(|l| l.target_id == cruiser_titan_lock),
        "0m surface lock onto Titan must survive Ballpark::step"
    );
}

#[tokio::test]
async fn fighter_ability_and_bomb_protect_invuln_ship_only_not_pilot_drone_or_corp_ship() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let mut sde = make_fighter_sde_registry();
    if let Some(sys) = sde.solar_systems.get_mut(&sys_id) {
        sys.security_status = -0.5;
    }
    sde.groups.entry(100).or_insert_with(|| eve_sde::GroupInfo {
        group_id: 100,
        category_id: 18,
        name: "Combat Drone".into(),
        anchorable: false,
        anchored: false,
        fittable_non_singleton: false,
        use_base_price: false,
        published: true,
    });
    insert_ability_def(&mut sde, 17, "itemTargeted", false, false);
    insert_ability_def(&mut sde, 50, "pointTargeted", true, false);
    sde.types.insert(999_699, make_fighter_type(999_699, 863, 8, 100.0, vec![(114, 1_500.0)]));
    sde.types.insert(
        999_600,
        make_fighter_type(
            999_600,
            1653,
            87,
            1_000.0,
            vec![(2151, 300.0), (2154, 50_000.0), (2246, 0.5), (76, 50_000.0), (2324, 999_699.0)],
        ),
    );
    sde.fighter_abilities_by_type.insert(
        999_600,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 17, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot {
                slot_index: 2,
                ability_id: 50,
                cooldown_seconds: Some(1.0),
                charges: Some(FighterAbilityCharges { charge_count: 2, rearm_time_seconds: 10.0 }),
            },
        ],
    );
    sde.types.entry(587).or_insert_with(|| make_fighter_type(587, 25, 6, 500.0, vec![]));
    sde.types.entry(2454).or_insert_with(|| make_fighter_type(2454, 100, 18, 5.0, vec![]));
    ctx.sde = Arc::new(sde);

    let victim_char = 90_999_101i64;
    let unledgered_char = 90_999_102i64;
    let corp_id = 98_999_101i64;
    let protected_ship_ledger = 890_101i64;
    let protected_ship_no_ledger = 890_102i64;
    let pilot_drone = 890_103i64;
    let corp_ship = 890_104i64;
    let now_unix = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&char_id).cloned().unwrap();
        let mut vc1 = base_ch.clone();
        vc1.character_id = victim_char;
        vc1.corporation_id = corp_id;
        vc1.active_ship_id = protected_ship_ledger;
        vc1.solar_system_id = sys_id;
        snap.characters.insert(victim_char, vc1);

        let mut vc2 = base_ch;
        vc2.character_id = unledgered_char;
        vc2.corporation_id = corp_id + 1;
        vc2.active_ship_id = protected_ship_no_ledger;
        vc2.solar_system_id = sys_id;
        snap.characters.insert(unledgered_char, vc2);

        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord::new(corp_id, "Fighter CEO Corp", "FCOC", victim_char),
        );
        snap.items.insert(
            protected_ship_ledger,
            make_fighter_item(protected_ship_ledger, 587, victim_char, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            protected_ship_no_ledger,
            make_fighter_item(protected_ship_no_ledger, 587, unledgered_char, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            pilot_drone,
            make_fighter_item(pilot_drone, 2454, victim_char, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            corp_ship,
            make_fighter_item(corp_ship, 587, corp_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    crate::services::space_destiny::grant_location_change_invulnerability(&ctx, victim_char, now_unix + 60);

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let f_ball = 500_600i64;
    sys_handle.spawn_ball(make_ball(ship_id, 23_911, DVec3::ZERO, 500.0)).await.unwrap();
    sys_handle.spawn_ball(make_ball(f_ball, 999_600, DVec3::ZERO, 50.0)).await.unwrap();
    for (bid, tid, pos) in [
        (protected_ship_ledger, 587u32, DVec3::new(2_000.0, 0.0, 0.0)),
        (protected_ship_no_ledger, 587u32, DVec3::new(2_100.0, 0.0, 0.0)),
        (pilot_drone, 2454u32, DVec3::new(2_200.0, 0.0, 0.0)),
        (corp_ship, 587u32, DVec3::new(2_300.0, 0.0, 0.0)),
    ] {
        sys_handle
            .spawn_ball_with_combat(make_ball(bid, tid, pos, 50.0), make_cs(5_000.0, 5_000.0, 5_000.0, 500.0))
            .await
            .unwrap();
        sys_handle
            .add_target_lock(
                ship_id,
                TargetLock { target_id: bid, locked: true, remaining_lock_ms: 0.0, lock_time_ms: 100.0, max_range_m: None },
            )
            .await
            .unwrap();
    }
    let _ = sys_handle.modify_combat_state(protected_ship_ledger, |cs| {
        cs.undock_invulnerable_remaining_ticks = 60;
        cs.sync_invuln_with_undock_timer = true;
    }).await;
    let _ = sys_handle.modify_combat_state(protected_ship_no_ledger, |cs| {
        cs.warp_exit_invulnerable_remaining_ticks = 60;
    }).await;

    ctx.active_fighters.write().unwrap().insert(
        f_ball,
        ActiveFighterSquadron {
            ball_id: f_ball,
            item_id: 600_600,
            type_id: 999_600,
            owner_char_id: char_id,
            parent_ship_id: ship_id,
            tube_index: 0,
            squadron_size: 2,
            target_id: None,
            is_abandoned: false,
            mwd_active: false,
        },
    );

    // 1. Targeted ability rejects both protected ships via ballpark CombatState::is_protected(),
    //    even with is_cloaked=false and without a pilot ledger entry on protected_ship_no_ledger,
    //    while succeeding on pilot_drone and corp_ship.
    for target in [protected_ship_ledger, protected_ship_no_ledger] {
        assert_eq!(
            handle_activate_fighter_ability_slots(
                &ctx,
                &mut session,
                &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(target)],
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "TargetInvulnerable"
        );
    }
    assert!(
        handle_activate_fighter_ability_slots(
            &ctx,
            &mut session,
            &[PyRep::Integer(f_ball), PyRep::Integer(0), PyRep::Integer(corp_ship)],
        )
        .await
        .unwrap()
        .is_ok(),
        "targeted ability on CEO corp ship must succeed"
    );

    // 2. Fighter point-targeted AoE bomb skips both protected ships while damaging drone & corp ship.
    let bomb_res = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(f_ball), PyRep::Integer(2), PyRep::Real(2_100.0), PyRep::Real(0.0), PyRep::Real(0.0)],
    )
    .await
    .unwrap();
    assert!(bomb_res.is_ok(), "fighter bomb pointTargeted succeeds: {bomb_res:?}");
    assert_eq!(sys_handle.get_combat_state(protected_ship_ledger).await.unwrap().unwrap().shield_hp, 5_000.0);
    assert_eq!(sys_handle.get_combat_state(protected_ship_no_ledger).await.unwrap().unwrap().shield_hp, 5_000.0);
    assert!(sys_handle.get_combat_state(pilot_drone).await.unwrap().unwrap().shield_hp < 5_000.0);
    assert!(sys_handle.get_combat_state(corp_ship).await.unwrap().unwrap().shield_hp < 5_000.0);
}
