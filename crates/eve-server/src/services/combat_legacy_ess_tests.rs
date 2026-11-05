//! Unit and mutation-killing tests for `combat_legacy_ess`.

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_type,
};
use eve_db::{ActiveMissionRecord, InventoryFlag};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[test]
fn step_dynamic_bounty_multiplier_decay_boost_equilibrium_and_clamp() {
    let (ctx, _session, _char_id, _ship_id, sys_id) = make_fighter_ctx_and_session();

    // Initial state starts at 1.35; 50M ratting decays by 0.10 -> 1.25
    let m1 = step_dynamic_bounty_multiplier(&ctx, sys_id, 50_000_000.0, 0.0);
    assert!((m1 - 1.25).abs() < 1e-6);

    // 25M PvP loss boosts by 0.15 -> 1.40
    let m2 = step_dynamic_bounty_multiplier(&ctx, sys_id, 0.0, 25_000_000.0);
    assert!((m2 - 1.40).abs() < 1e-6);

    // Zero ratting and zero PvP drifts 5% toward 1.35: 1.40 + (1.35 - 1.40)*0.05 = 1.3975
    let m3 = step_dynamic_bounty_multiplier(&ctx, sys_id, 0.0, 0.0);
    assert!((m3 - 1.3975).abs() < 1e-6);

    // Massive ratting clamps to 0.50 floor; massive PvP clamps to 2.00 ceiling
    let floor = step_dynamic_bounty_multiplier(&ctx, sys_id, 5_000_000_000.0, 0.0);
    assert!((floor - 0.50).abs() < 1e-6);
    let ceil = step_dynamic_bounty_multiplier(&ctx, sys_id, 0.0, 5_000_000_000.0);
    assert!((ceil - 2.00).abs() < 1e-6);
}

#[tokio::test]
async fn legacy_activate_validation_npc_kill_and_concord_highsec_enforcement() {
    let (mut ctx, mut session, char_id, ship_id, sys_id) = make_fighter_ctx_and_session();
    let kwargs = HashMap::new();

    // Deleted legacy RPCs return UnimplementedRPC
    // rpc-lint: allow testing Phase 4 deleted FireWeapon rejection
    let fw_err = handle_legacy_and_ess(&ctx, &mut session, "dogmaIM", "FireWeapon", &[], &kwargs)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(fw_err.code, "UnimplementedRPC");

    // Configure weapon (group 55 Projectile Turret, type 2905) + compatible ammo (group 83, type 192, size 1) + NPC (type 10001, cat 11)
    let mut sde = (*ctx.sde).clone();
    if let Some(ship_t) = sde.types.get_mut(&23_915) {
        ship_t.default_attributes.insert(14, 8.0);
        ship_t.default_attributes.insert(102, 8.0);
        ship_t.default_attributes.insert(11, 10_000.0);
        ship_t.default_attributes.insert(48, 10_000.0);
    }
    let mut wep_t = make_fighter_type(
        2_905,
        55,
        7,
        5.0,
        vec![
            (51, 1_000.0),   // speed 1000ms (1 tick)
            (54, 15_000.0),  // optimal range 15km
            (64, 2.0),       // damage multiplier 2x
            (128, 1.0),      // charge size 1
            (158, 10_000.0), // falloff 10km
            (160, 100.0),    // tracking speed
            (604, 83.0),     // usedWithChargeGroup 83
        ],
    );
    wep_t.effect_ids = vec![12];
    sde.types.insert(2_905, wep_t);
    sde.types.insert(
        192,
        make_fighter_type(
            192,
            83,
            8,
            0.01,
            vec![
                (128, 1.0),    // charge size 1
                (114, 500.0),  // EM damage
                (116, 500.0),  // Explosive damage
                (117, 500.0),  // Kinetic damage
                (118, 500.0),  // Thermal damage
            ],
        ),
    );
    sde.types.insert(
        10_001,
        make_fighter_type(
            10_001,
            550,
            11,
            1_000.0,
            vec![(eve_sde::attr_ids::ENTITY_KILL_BOUNTY, 100_000.0)],
        ),
    );
    ctx.sde = std::sync::Arc::new(sde);

    // 1. Docked rejection
    session.station_id = Some(60_003_760);
    let docked_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(900_001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(docked_err.code, "ShipDocked");
    session.station_id = None;

    // 2. Self/invalid target rejection
    let self_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(ship_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(self_err.code, "InvalidTarget");

    // 3. Ship not in space rejection
    let not_in_space_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(900_001)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(not_in_space_err.code, "ShipNotInSpace");

    // Spawn attacker ship and target NPC in space
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let npc_ball_id = 900_001_i64;
    for (id, tid, pos, hp) in [
        (ship_id, 23_915_u32, DVec3::ZERO, 1_000.0),
        (npc_ball_id, 10_001_u32, DVec3::new(2_000.0, 0.0, 0.0), 100.0),
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
                    shield_hp: 0.0,
                    max_shield_hp: hp,
                    armor_hp: 0.0,
                    max_armor_hp: hp,
                    structure_hp: hp,
                    max_structure_hp: hp,
                    signature_radius: 50.0,
                    ..CombatState::default()
                },
            )
            .await
            .unwrap();
    }

    // 4. No weapon fitted rejection
    let no_wep_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_ball_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_wep_err.code, "NoWeaponFitted");

    // Fit weapon in HiSlot0 (flag 27), verify NoAmmoLoaded before loading ammo
    let wep_id = 800_001_i64;
    let ammo_id = 800_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            wep_id,
            make_fighter_item(wep_id, 2_905, char_id, ship_id, InventoryFlag::HiSlot0, 1),
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let no_ammo_err = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_ball_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(no_ammo_err.code, "NoAmmoLoaded");

    // Load ammo and seed an active mission referencing `npc_ball_id`
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ammo_id,
            make_fighter_item(ammo_id, 192, char_id, ship_id, InventoryFlag::HiSlot0, 5),
        );
        snap.active_missions.insert(
            (char_id, 3_018_418),
            ActiveMissionRecord {
                character_id: char_id,
                agent_id: 3_018_418,
                mission_id: 1,
                state: "Accepted".into(),
                solar_system_id: sys_id,
                current_room_id: 0,
                npcs_remaining: 1,
                spawned_npc_ball_ids: vec![npc_ball_id],
                bookmark_id: None,
            },
        );
    });

    // 5. Destroy NPC via legacy Activate -> verify bounty payout, mission decrement, wreck & killmail
    let hit_res = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(npc_ball_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        hit_res.dict_get("destroyed"),
        Some(&PyRep::Bool(true))
    );
    assert_eq!(
        hit_res.dict_get("bountyPayoutISK"),
        Some(&PyRep::Real(90_000.0))
    );
    ctx.db.with_snapshot(|snap| {
        let m = snap.active_missions.get(&(char_id, 3_018_418)).unwrap();
        assert_eq!(m.npcs_remaining, 0);
        assert_eq!(snap.items.get(&ammo_id).unwrap().quantity, 4);
    });

    // 6. Illegal high-sec PvP attack triggers CONCORD response and kill right creation
    ctx.weapon_cycle_state.write().unwrap().clear();
    let victim_char_id = 90_000_002_i64;
    let victim_ship_id = 200_000_002_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut vic = snap.characters.get(&char_id).cloned().unwrap();
        vic.character_id = victim_char_id;
        vic.corporation_id = 2_000_002;
        vic.active_ship_id = victim_ship_id;
        vic.solar_system_id = sys_id;
        vic.security_status = 1.0;
        snap.characters.insert(victim_char_id, vic);
    });
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: victim_ship_id,
                type_id: 23_915,
                position: DVec3::new(1_500.0, 0.0, 0.0),
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
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();

    let concord_res = handle_legacy_and_ess(
        &ctx,
        &mut session,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(victim_char_id)],
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        concord_res.dict_get("criminalCharacterID"),
        Some(&PyRep::Integer(char_id))
    );
    assert_eq!(
        concord_res.dict_get("victimCharacterID"),
        Some(&PyRep::Integer(victim_char_id))
    );
    assert_eq!(
        concord_res.dict_get("criminalTimerSeconds"),
        Some(&PyRep::Integer(900))
    );
}
