use super::combat_fighters::combat_fighters_tests::{
    make_fighter_ctx_and_session, make_fighter_item, make_fighter_sde_registry, make_fighter_type,
};
use super::*;
use eve_destiny::{Ball, CombatState, MovementMode, TargetLock};
use eve_sde::{FighterAbilityCharges, FighterAbilityInfo, FighterAbilitySlot};
use glam::DVec3;
use std::sync::Arc;

#[path = "combat_fighters_bomb_tests.rs"]
mod combat_fighters_bomb_tests;
#[path = "combat_fighters_ewar_tests.rs"]
mod combat_fighters_ewar_tests;

pub(super) fn py_key<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
            .map(|(_, v)| v),
        PyRep::KeyVal(entries) => entries.get(key),
        _ => None,
    }
}

pub(super) fn make_ability_test_ctx() -> (ServerContext, SessionState, i64, i64, u32) {
    let mut sde = make_fighter_sde_registry();
    let ability_defs: &[(u32, &str, bool, bool)] = &[
        (2, "itemTargeted", false, false),
        (7, "itemTargeted", false, false),
        (10, "itemTargeted", false, false),
        (11, "itemTargeted", false, false),
        (12, "itemTargeted", false, false),
        (13, "untargeted", false, false),
        (14, "itemTargeted", false, false),
        (16, "itemTargeted", false, false),
        (17, "itemTargeted", false, false),
        (20, "itemTargeted", false, false),
        (34, "itemTargeted", false, false),
        (38, "itemTargeted", false, false),
        (44, "itemTargeted", false, false),
        (50, "pointTargeted", true, false),
        (98, "untargeted", false, true),
        (99, "untargeted", true, false),
    ];
    for &(aid, mode, no_hi, no_lo) in ability_defs {
        sde.fighter_abilities.insert(
            aid,
            FighterAbilityInfo {
                ability_id: aid,
                target_mode: mode.into(),
                disallow_in_high_sec: no_hi,
                disallow_in_low_sec: no_lo,
                icon_id: Some(aid),
                display_name_id: Some(aid + 1000),
                ability_name_id: Some(aid + 2000),
                tooltip_text_id: Some(aid + 3000),
                turret_graphic_id: Some(aid + 4000),
                raw_json: serde_json::Value::Null,
            },
        );
    }
    // Bomb charge item 41_549
    sde.types.insert(
        41_549,
        make_fighter_type(
            41_549,
            863,
            8,
            100.0,
            vec![(114, 150.0), (116, 100.0), (117, 50.0), (118, 25.0), (99, 12_000.0)],
        ),
    );
    // Support fighter (Siren/Dromi) type 23_059 with EWAR slots
    sde.types.insert(
        23_059,
        make_fighter_type(
            23_059,
            1537,
            87,
            1_000.0,
            vec![
                (2215, 3.0),
                (2213, 1.0),
                (2184, -30.0),
                (2205, 2.0),
                (2211, 80.0),
                (2246, 4.0),
                (2224, 250.0),
                (2225, -75.0),
                (2130, 2.0),
                (2131, 40.0),
                (2132, 30.0),
                (2133, 20.0),
                (2134, 10.0),
            ],
        ),
    );
    sde.fighter_abilities_by_type.insert(
        23_059,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 2, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 13, cooldown_seconds: Some(15.0), charges: None },
            FighterAbilitySlot { slot_index: 2, ability_id: 16, cooldown_seconds: Some(12.0), charges: None },
        ],
    );
    // Bomber fighter type 23_063 for point-targeted bomb ability 50
    sde.types.insert(
        23_063,
        make_fighter_type(23_063, 1653, 87, 1_000.0, vec![(2215, 6.0), (2214, 1.0), (2324, 41_549.0)]),
    );
    sde.fighter_abilities_by_type.insert(
        23_063,
        vec![
            FighterAbilitySlot { slot_index: 0, ability_id: 34, cooldown_seconds: Some(0.0), charges: None },
            FighterAbilitySlot { slot_index: 1, ability_id: 4, cooldown_seconds: Some(10.0), charges: None },
            FighterAbilitySlot {
                slot_index: 2,
                ability_id: 50,
                cooldown_seconds: Some(20.0),
                charges: Some(FighterAbilityCharges { charge_count: 2, rearm_time_seconds: 30.0 }),
            },
        ],
    );
    // Lowsec test system 30_000_200
    sde.solar_systems.insert(
        30_000_200,
        eve_sde::SolarSystem {
            solar_system_id: 30_000_200,
            region_id: 10_000_002,
            constellation_id: 20_000_020,
            name: "LowsecAbilityTest".into(),
            security_status: 0.25,
            radius: 1.0e12,
            galactic_pos_ly: [0.0, 0.0, 0.0],
            planet_ids: Vec::new(),
            stations: Vec::new(),
            wormhole_class_id: None,
        },
    );

    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(Arc::new(sde), db.clone());
    let char_id = 90_000_001i64;
    let ship_id = 200_000_001i64;
    let sys_id = 30_000_142u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_fighter_item(ship_id, 23_915, char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    let mut session = SessionState::default();
    session.character_id = Some(char_id);
    session.solar_system_id = Some(sys_id);
    session.ship_id = Some(ship_id);
    session.user_id = Some(1);
    (ctx, session, char_id, ship_id, sys_id)
}

pub(super) fn squad_record(
    ball_id: i64,
    type_id: u32,
    owner_char_id: i64,
    parent_ship_id: i64,
    _sys_id: u32,
    size: i32,
) -> crate::router::context::ActiveFighterSquadron {
    crate::router::context::ActiveFighterSquadron {
        ball_id,
        item_id: ball_id,
        type_id,
        owner_char_id,
        parent_ship_id,
        tube_index: 0,
        squadron_size: size.max(1) as u32,
        target_id: None,
        is_abandoned: false,
        mwd_active: false,
    }
}

pub(super) async fn spawn_fighter_and_ship(
    ctx: &ServerContext,
    sys_id: u32,
    parent_ship_id: i64,
    fighter_id: i64,
    fighter_type: u32,
    owner_char_id: i64,
) {
    ctx.active_fighters.write().unwrap().insert(
        fighter_id,
        squad_record(fighter_id, fighter_type, owner_char_id, parent_ship_id, sys_id, 3),
    );
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: parent_ship_id,
                type_id: 23_915,
                position: DVec3::new(0.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 120.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 500.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await;
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: fighter_id,
                type_id: fighter_type,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 1_000.0,
                inertia: 1.0,
                mass: 5_000.0,
                radius: 25.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 200.0,
                max_armor_hp: 200.0,
                structure_hp: 100.0,
                max_structure_hp: 100.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fighter_abilities_validation_errors_and_security_gates() {
    let (_base_ctx, _, _, _, _) = make_fighter_ctx_and_session();
    let (ctx, mut session, char_id, ship_id, sys_id) = make_ability_test_ctx();
    let fid = 800_000_001i64;
    let target_id = 300_000_001i64;

    // 1. NoSolarSystem & empty fighter IDs & invalid slot
    let mut bad_sess = session.clone();
    bad_sess.solar_system_id = None;
    let err = handle_activate_fighter_ability_slots(&ctx, &mut bad_sess, &[PyRep::Integer(fid), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NoSolarSystem");

    let err = handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::List(vec![]), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "FighterNotFound");

    let err = handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(fid), PyRep::Integer(5)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "InvalidFighterAbilitySlot");

    // 2. Unlaunched fighter, wrong owner, missing ball in system
    let err = handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(fid), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "FighterNotFound");

    ctx.active_fighters
        .write()
        .unwrap()
        .insert(fid, squad_record(fid, 23_057, 999_999, ship_id, sys_id, 3));
    let err = handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(fid), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "FighterNotOwned");

    ctx.active_fighters.write().unwrap().get_mut(&fid).unwrap().owner_char_id = char_id;
    let err = handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(fid), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "FighterNotInSystem");

    // Spawn fighter & target in system
    spawn_fighter_and_ship(&ctx, sys_id, ship_id, fid, 23_057, char_id).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: target_id,
                type_id: 587,
                position: DVec3::new(3_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 1_000.0,
                max_shield_hp: 1_000.0,
                armor_hp: 1_000.0,
                max_armor_hp: 1_000.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                capacitor: 400.0,
                max_capacitor: 400.0,
                signature_radius: 45.0,
                ..CombatState::default()
            },
        )
        .await;

    // 3. Target validation: missing target, invalid target (self), not in space, not locked, out of range
    let err = handle_activate_fighter_ability_slots(&ctx, &mut session, &[PyRep::Integer(fid), PyRep::Integer(0)])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "TargetRequired");

    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(0), PyRep::Integer(ship_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "InvalidTarget");

    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(0), PyRep::Integer(999_888)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetNotInSpace");

    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(0), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetNotLocked");

    // Spawn out-of-range target first, then lock it so range check fires
    let _ = sys
        .spawn_ball(Ball {
            id: target_id,
            type_id: 587,
            position: DVec3::new(90_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 1.0,
            mode: MovementMode::Stop,
        })
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
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(0), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "TargetOutOfRange");

    // 4. UnexpectedTarget on untargeted MWD slot 1
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(1), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "UnexpectedTarget");

    // 5. Exhausted charges on slot 2 -> OutOfCharges
    ctx.fighter_charge_state.write().unwrap().insert((fid, 2), 6);
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(2), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "OutOfCharges");
    ctx.fighter_charge_state.write().unwrap().clear();

    // 6. Active cooldown -> AbilityOnCooldown
    ctx.fighter_cooldown_state
        .write()
        .unwrap()
        .insert((fid, 0), ctx.now_unix() + 120);
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(fid), PyRep::Integer(0), PyRep::Integer(target_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "AbilityOnCooldown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fighter_abilities_damage_antifighter_ewar_and_deactivation() {
    let (ctx, mut session, char_id, ship_id, sys_id) = make_ability_test_ctx();
    let attacker_fid = 800_000_010i64;
    let enemy_fid = 800_000_011i64;
    let enemy_ship_id = 300_000_010i64;
    let victim_char_id = 90_000_002i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_char_id) {
            ch.active_ship_id = enemy_ship_id;
        }
        snap.items.insert(
            enemy_ship_id,
            make_fighter_item(enemy_ship_id, 587, victim_char_id, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    spawn_fighter_and_ship(&ctx, sys_id, ship_id, attacker_fid, 23_059, char_id).await;
    spawn_fighter_and_ship(&ctx, sys_id, enemy_ship_id, enemy_fid, 23_057, victim_char_id).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_combat(
            Ball {
                id: enemy_ship_id,
                type_id: 587,
                position: DVec3::new(3_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 2.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 2_000.0,
                max_shield_hp: 2_000.0,
                armor_hp: 1_500.0,
                max_armor_hp: 1_500.0,
                structure_hp: 1_000.0,
                max_structure_hp: 1_000.0,
                capacitor: 600.0,
                max_capacitor: 600.0,
                signature_radius: 50.0,
                ..CombatState::default()
            },
        )
        .await;
    for &tid in &[enemy_ship_id, enemy_fid] {
        let _ = sys
            .add_target_lock(
                ship_id,
                TargetLock {
                    target_id: tid,
                    locked: true,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    max_range_m: None,
                },
            )
            .await;
    }

    // Green safety in highsec blocks illegal aggression
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Green);
    });
    let err = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(attacker_fid), PyRep::Integer(2), PyRep::Integer(enemy_ship_id)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err.code, "CantDoInCurrentSafetyLevel");

    // Set Red safety and fire Anti-Fighter Missile Volley (slot 2 -> ability 16) at ship vs at fighter
    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
    });
    let before_ship_shield = sys.get_combat_state(enemy_ship_id).await.unwrap().unwrap().shield_hp;
    let res = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(attacker_fid), PyRep::Integer(2), PyRep::Integer(enemy_ship_id)],
    )
    .await
    .unwrap()
    .unwrap();
    let ship_dmg_taken = before_ship_shield - sys.get_combat_state(enemy_ship_id).await.unwrap().unwrap().shield_hp;
    assert!(ship_dmg_taken > 100.0, "ship took {ship_dmg_taken} damage");
    assert_eq!(py_key(&res, "abilityID").and_then(|v| v.as_i64()), Some(16));

    // Clear cooldown and shoot enemy fighter squadron: anti-fighter ability 16 deals +100% (2x) bonus and wipes it!
    ctx.fighter_cooldown_state.write().unwrap().clear();
    let _ = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(attacker_fid), PyRep::Integer(2), PyRep::Integer(enemy_fid)],
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        sys.get_combat_state(enemy_fid).await.unwrap().is_none(),
        "anti-fighter bonus should destroy enemy fighter squadron"
    );

    // Activate Stasis Web (slot 0 -> ability 2) -> lowers target speed_fraction & records projected EWAR
    let web_res = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(attacker_fid), PyRep::Integer(0), PyRep::Integer(enemy_ship_id)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&web_res, "abilityID").and_then(|v| v.as_i64()), Some(2));
    assert!((py_key(&web_res, "ewarStrength").and_then(|v| v.as_f64()).unwrap() - 0.90).abs() < 1e-6);
    assert!(ctx.projected_ewar.read().unwrap().contains_key(&enemy_ship_id));

    // Deactivate web (slot 0) -> removes projected EWAR & clears fighter.target_id
    let deact_web = handle_deactivate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(attacker_fid), PyRep::Integer(0)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&deact_web, "deactivated").and_then(|v| v.as_bool()), Some(true));
    assert!(!ctx.projected_ewar_sources.read().unwrap().contains_key(&enemy_ship_id));

    // Activate Evasive Maneuvers (slot 1 -> ability 13) -> boosts speed and lowers signature_radius, then deactivate
    let before_sig = sys.get_combat_state(attacker_fid).await.unwrap().unwrap().signature_radius;
    let before_vel = sys.get_ball_snapshot(attacker_fid).await.unwrap().unwrap().1.max_velocity;
    let ev_res = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(attacker_fid), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&ev_res, "abilityID").and_then(|v| v.as_i64()), Some(13));
    let mid_sig = sys.get_combat_state(attacker_fid).await.unwrap().unwrap().signature_radius;
    let mid_vel = sys.get_ball_snapshot(attacker_fid).await.unwrap().unwrap().1.max_velocity;
    assert!(mid_sig < before_sig && mid_vel > before_vel);

    let deact_ev = handle_deactivate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::List(vec![PyRep::Integer(attacker_fid)]), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&deact_ev, "deactivated").and_then(|v| v.as_bool()), Some(true));
    let after_sig = sys.get_combat_state(attacker_fid).await.unwrap().unwrap().signature_radius;
    let after_vel = sys.get_ball_snapshot(attacker_fid).await.unwrap().unwrap().1.max_velocity;
    assert!((after_sig - before_sig).abs() < 1e-6);
    assert!((after_vel - before_vel).abs() < 1e-6);

    // Activate MJD (ability 5 on heavy fighter 23_061) -> jumps 100 km forward
    let heavy_fid = 800_000_012i64;
    spawn_fighter_and_ship(&ctx, sys_id, ship_id, heavy_fid, 23_061, char_id).await;
    let before_x = sys.get_ball_snapshot(heavy_fid).await.unwrap().unwrap().1.position.x;
    let mjd_res = handle_activate_fighter_ability_slots(
        &ctx,
        &mut session,
        &[PyRep::Integer(heavy_fid), PyRep::Integer(1)],
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(py_key(&mjd_res, "abilityID").and_then(|v| v.as_i64()), Some(5));
    let after_x = sys.get_ball_snapshot(heavy_fid).await.unwrap().unwrap().1.position.x;
    assert!((after_x - before_x).abs() > 10_000.0);
}
