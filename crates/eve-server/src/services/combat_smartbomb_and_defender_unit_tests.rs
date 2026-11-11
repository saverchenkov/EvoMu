//! Semantic unit tests split from `combat_missiles_aoe_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_item;
use eve_db::InventoryFlag;
use eve_destiny::DamageVector;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missile_multi_step_flight_destroyed_ball_missing_target_and_assisted_self_destruct() {
    let (ctx, _, char_id, ship_id, sys_id) = setup_aoe_extra_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Multi-step missile flight: missile at (0,0,0), target at (3000,0,0), vel 2000 m/s
    let m_multi = 881_000_001_i64;
    let t_multi = 301_000_001_i64;
    // 2. Missile whose ball has 0 structure HP in ballpark -> marked destroyed
    let m_dead_ball = 881_000_002_i64;
    // 3. Missile whose target ball does not exist -> marked expired
    let m_no_target = 881_000_003_i64;

    spawn_ball_with_hp(&ctx, sys_id, m_multi, 209, DVec3::ZERO, 0.0, 0.0, 75.0, 0.0).await;
    spawn_ball_with_hp(&ctx, sys_id, t_multi, 587, DVec3::new(3_000.0, 0.0, 0.0), 0.0, 0.0, 50.0, 100.0).await;
    spawn_ball_with_hp(&ctx, sys_id, m_dead_ball, 209, DVec3::ZERO, 0.0, 0.0, 0.0, 0.0).await;
    spawn_ball_with_hp(&ctx, sys_id, m_no_target, 209, DVec3::ZERO, 0.0, 0.0, 75.0, 0.0).await;

    let make_mstate = |mid: i64, tid: i64| InFlightMissileState {
        missile_ball_id: mid,
        solar_system_id: sys_id,
        attacker_char_id: char_id,
        attacker_ship_id: ship_id,
        launcher_module_id: 710_001,
        launcher_type_id: 2_410,
        missile_type_id: 209,
        target_ball_id: tid,
        max_velocity: 2_000.0,
        flight_time_s: 6.0,
        remaining_flight_s: 6.0,
        hp: 75.0,
        max_hp: 75.0,
        base_damage: DamageVector::new(0.0, 0.0, 200.0, 0.0),
        explosion_radius: 40.0,
        explosion_velocity: 170.0,
        drf: 0.604,
        is_fof: false,
        is_breacher_pod: false,
        breacher_dot_duration_s: 0.0,
        breacher_flat_cap: 0.0,
        breacher_pct_cap: 0.0,
        destroyed: false,
        expired: false,
        impacted: false,
        applied_damage: 0.0,
    };

    with_mut_combat_runtime(&ctx, |rt| {
        rt.in_flight_missiles.insert(m_multi, make_mstate(m_multi, t_multi));
        rt.in_flight_missiles.insert(m_dead_ball, make_mstate(m_dead_ball, t_multi));
        rt.in_flight_missiles.insert(m_no_target, make_mstate(m_no_target, 999_999_999));
    });

    // Step 1 (dt = 1.0s): m_multi advances 2000m (remains in flight in rt.in_flight_missiles), m_dead_ball destroyed, m_no_target expired
    let step1 = step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    assert!(!step1.iter().any(|m| m.missile_ball_id == m_multi));
    let s_multi_1 = with_combat_runtime(&ctx, |rt| rt.in_flight_missiles[&m_multi].clone());
    assert!(!s_multi_1.impacted && !s_multi_1.expired && !s_multi_1.destroyed);
    assert!((s_multi_1.remaining_flight_s - 5.0).abs() < 1e-6);
    assert!(step1.iter().find(|m| m.missile_ball_id == m_dead_ball).unwrap().destroyed);
    assert!(step1.iter().find(|m| m.missile_ball_id == m_no_target).unwrap().expired);

    // Step 2 (dt = 1.0s): m_multi covers remaining 1000m, impacts t_multi, and destroys it!
    let step2 = step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    let s_multi_2 = step2.iter().find(|m| m.missile_ball_id == m_multi).unwrap();
    assert!(s_multi_2.impacted);
    assert!(s_multi_2.applied_damage > 0.0);
    assert!(sys_handle.get_ball_snapshot(t_multi).await.unwrap().is_none());

    // Self-destruct countdown with prior attacker assist in aggression_assist_log and fitted modules
    let sd_char = 90_000_002_i64;
    let sd_ship = 220_000_222_i64;
    let now_unix = ctx.now_unix();
    spawn_ball_with_hp(&ctx, sys_id, sd_ship, 587, DVec3::ZERO, 100.0, 100.0, 100.0, 100.0).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            sd_ship,
            make_fighter_item(sd_ship, 587, sd_char, sys_id as i64, InventoryFlag::Hangar, 1),
        );
        snap.items.insert(
            777_001,
            make_fighter_item(777_001, 2_410, sd_char, sd_ship, InventoryFlag::HiSlot0, 1),
        );
        if let Some(ch) = snap.characters.get_mut(&sd_char) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = sd_ship;
        }
    });
    with_mut_combat_runtime(&ctx, |rt| {
        rt.aggression_assist_log.insert(
            sd_ship,
            vec![AssistLogEntry {
                attacker_char_id: char_id,
                corporation_id: 1_000_009,
                alliance_id: None,
                ship_type_id: 587,
                weapon_type_id: 2_410,
                damage_done: 450.0,
                is_ewar: false,
                last_action_unix: now_unix.saturating_sub(10),
            }],
        );
        rt.self_destruct_countdowns.insert(
            sd_ship,
            SelfDestructCountdown {
                ship_id: sd_ship,
                char_id: sd_char,
                solar_system_id: sys_id,
                started_unix: now_unix.saturating_sub(120),
                detonate_at_unix: now_unix,
            },
        );
    });

    let _ = step_missiles_and_breachers(&ctx, sys_id, 1.0).await;
    let has_assisted_km = with_combat_runtime(&ctx, |rt| {
        rt.detailed_killmails.values().any(|km| {
            km.victim_character_id == sd_char
                && km.attackers.iter().any(|a| a.character_id == char_id && a.final_blow)
        })
    });
    assert!(has_assisted_km, "self-destruct with recent assist must credit attacker on killmail");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn smartbomb_partial_missile_drone_pod_and_defender_auto_target_branches() {
    let (ctx, _, char_id, ship_id, _sys_id) = setup_aoe_extra_ctx();
    let kwargs = HashMap::new();
    let low_sys = 30_000_205_u32;
    let sys_handle = ctx.systems.get_or_spawn(low_sys);
    let now_unix = ctx.now_unix();

    // Spawn attacker ship with 0 capacitor to test deduct_capacitor = false
    spawn_ball_with_hp(&ctx, low_sys, ship_id, 587, DVec3::ZERO, 500.0, 500.0, 500.0, 0.0).await;
    let (_, attacker_ball) = sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();

    // Unrecognized module group (group 509 launcher) returns None
    let launcher_mod = eve_dogma::FittedModule {
        item_id: 740_000,
        type_id: 2_410,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let launcher_t = ctx.sde.types.get(&2_410).unwrap();
    assert!(activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        low_sys,
        0.2,
        now_unix,
        740_000,
        &launcher_mod,
        launcher_t,
        &attacker_ball,
        None,
        &kwargs,
    )
    .await
    .is_none());

    // Smartbomb setup:
    // - Tough missile (hp = 500 > 250 smartbomb damage) -> survives in damagedMissileIDs
    // - Enemy active drone (hp = 50) -> destroyed in destroyedDroneIDs
    // - Cloaked ship -> skipped
    // - Victim pod (type 670) in lowsec -> triggers criminal flag!
    let tough_mid = 882_000_001_i64;
    let drone_id = 882_000_002_i64;
    let cloaked_ship = 882_000_003_i64;
    let victim_char = 90_000_002_i64;
    let victim_pod = 882_000_004_i64;

    spawn_ball_with_hp(&ctx, low_sys, tough_mid, 209, DVec3::new(1_000.0, 0.0, 0.0), 0.0, 0.0, 500.0, 0.0).await;
    spawn_ball_with_hp(&ctx, low_sys, drone_id, 2_488, DVec3::new(1_200.0, 0.0, 0.0), 10.0, 10.0, 10.0, 0.0).await;
    spawn_ball_with_hp(&ctx, low_sys, cloaked_ship, 587, DVec3::new(1_500.0, 0.0, 0.0), 500.0, 500.0, 500.0, 0.0).await;
    if let Ok(Some(mut cs)) = sys_handle.get_combat_state(cloaked_ship).await {
        cs.is_cloaked = true;
        let _ = sys_handle.set_combat_state(cloaked_ship, cs).await;
    }
    spawn_ball_with_hp(&ctx, low_sys, victim_pod, 670, DVec3::new(2_000.0, 0.0, 0.0), 500.0, 500.0, 500.0, 0.0).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            victim_pod,
            make_fighter_item(victim_pod, 670, victim_char, low_sys as i64, InventoryFlag::Hangar, 1),
        );
        if let Some(vc) = snap.characters.get_mut(&victim_char) {
            vc.active_ship_id = victim_pod;
        }
    });

    with_mut_combat_runtime(&ctx, |rt| {
        rt.safety_levels.insert(char_id, SafetyLevel::Red);
        rt.in_flight_missiles.insert(
            tough_mid,
            InFlightMissileState {
                missile_ball_id: tough_mid,
                solar_system_id: low_sys,
                attacker_char_id: victim_char,
                attacker_ship_id: victim_pod,
                launcher_module_id: 900_001,
                launcher_type_id: 2_410,
                missile_type_id: 209,
                target_ball_id: ship_id,
                max_velocity: 2_000.0,
                flight_time_s: 6.0,
                remaining_flight_s: 5.0,
                hp: 500.0,
                max_hp: 500.0,
                base_damage: DamageVector::new(0.0, 0.0, 100.0, 0.0),
                explosion_radius: 40.0,
                explosion_velocity: 170.0,
                drf: 0.604,
                is_fof: false,
                is_breacher_pod: false,
                breacher_dot_duration_s: 0.0,
                breacher_flat_cap: 0.0,
                breacher_pct_cap: 0.0,
                destroyed: false,
                expired: false,
                impacted: false,
                applied_damage: 0.0,
            },
        );
        rt.active_drones_in_space.insert(
            drone_id,
            DroneSpaceState {
                ball_id: drone_id,
                drone_item_id: drone_id,
                type_id: 2_488,
                group_id: 100,
                owner_char_id: victim_char,
                original_owner_char_id: victim_char,
                parent_ship_id: victim_pod,
                solar_system_id: low_sys,
                target_id: None,
                activity_state: 0,
                assist_char_id: None,
                guard_char_id: None,
                is_abandoned: false,
                is_sentry: false,
                bandwidth_used: 5.0,
                max_velocity: 3_000.0,
                optimal_range: 1_500.0,
                damage_vector: DamageVector::default(),
                shield_repair_amount: 0.0,
                armor_repair_amount: 0.0,
                hull_repair_amount: 0.0,
                web_speed_factor: 1.0,
                neut_amount: 0.0,
                ecm_strength: 0.0,
                tp_sig_bonus_pct: 0.0,
            },
        );
    });

    let sb_mod = eve_dogma::FittedModule {
        item_id: 740_001,
        type_id: 12_219,
        flag_id: 27,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let sb_t = ctx.sde.types.get(&12_219).unwrap();

    // Activate smartbomb with deduct_capacitor = false
    let sb_res = activate_aoe_or_defender_module_with_cap(
        &ctx,
        char_id,
        ship_id,
        low_sys,
        0.2,
        now_unix,
        740_001,
        &sb_mod,
        sb_t,
        &attacker_ball,
        None,
        &kwargs,
        false,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(py_val(&sb_res, "concordTriggered").and_then(|v| v.as_bool()), Some(false));
    assert!(matches!(py_val(&sb_res, "destroyedMissileIDs"), Some(PyRep::List(l)) if l.is_empty()));
    assert_eq!(
        with_combat_runtime(&ctx, |rt| rt.in_flight_missiles[&tough_mid].hp),
        250.0
    );
    assert!(matches!(py_val(&sb_res, "destroyedDroneIDs"), Some(PyRep::List(l)) if l.len() == 1));
    // Cloaked ship took 0 damage
    assert_eq!(
        sys_handle.get_combat_state(cloaked_ship).await.unwrap().unwrap().shield_hp,
        500.0
    );
    // Lowsec pod hit gave attacker a criminal flag
    assert!(
        ctx.crimewatch
            .write()
            .unwrap()
            .get_or_create(char_id)
            .criminal_flag_until_unix
            > now_unix
    );

    // Defender Launcher with target_id_opt = None auto-targets tough_mid!
    let def_mod = eve_dogma::FittedModule {
        item_id: 740_010,
        type_id: 1_599,
        flag_id: 28,
        state: eve_dogma::ModuleState::Active,
        damage_hp: 0.0,
        max_hp: 40.0,
        charge_type_id: None,
        charge_quantity: 0,
        dynamic_attribute_overrides: HashMap::new(),
    };
    let def_t = ctx.sde.types.get(&1_599).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            740_011,
            make_fighter_item(740_011, 267, char_id, ship_id, InventoryFlag::HiSlot1, 2),
        );
    });
    let def_res = activate_aoe_or_defender_module(
        &ctx,
        char_id,
        ship_id,
        low_sys,
        0.2,
        now_unix,
        740_010,
        &def_mod,
        def_t,
        &attacker_ball,
        None,
        &kwargs,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        py_val(&def_res, "interceptedMissileID").and_then(|v| v.as_i64()),
        Some(tough_mid)
    );
}
