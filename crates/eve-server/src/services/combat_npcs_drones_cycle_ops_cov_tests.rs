//! Direct unit tests covering all remaining branches of `combat_npcs_drones_cycle_ops`.

use super::super::cycle_ops::*;
use super::super::*;
use super::{make_drone_test_ctx, make_fighter_item, make_fighter_type};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime, ExtendedNpcEntity};
use crate::system_actor::ActiveModuleCycle;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, TargetLock};
use glam::DVec3;
use std::sync::Arc;

fn make_drone_state(
    ball_id: i64,
    item_id: i64,
    type_id: u32,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    target_id: Option<i64>,
) -> DroneSpaceState {
    DroneSpaceState {
        ball_id,
        drone_item_id: item_id,
        type_id,
        group_id: 100,
        owner_char_id: char_id,
        original_owner_char_id: char_id,
        parent_ship_id: ship_id,
        solar_system_id: sys_id,
        target_id,
        activity_state: if target_id.is_some() { 1 } else { 0 },
        assist_char_id: None,
        guard_char_id: None,
        is_abandoned: false,
        is_sentry: false,
        bandwidth_used: 5.0,
        max_velocity: 2_400.0,
        optimal_range: 15_000.0,
        damage_vector: DamageVector::new(0.0, 20.0, 0.0, 0.0),
        shield_repair_amount: 0.0,
        armor_repair_amount: 0.0,
        hull_repair_amount: 0.0,
        web_speed_factor: 0.0,
        neut_amount: 0.0,
        ecm_strength: 0.0,
        tp_sig_bonus_pct: 0.0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ewar_immunity_ecm_taunt_victim_resolution_and_aggression_timers() {
    let (mut ctx, _session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let other_char = 90_000_888i64;
    let other_ship = 1_888_000_001i64;
    let unpiloted_item = 1_888_000_002i64;

    let mut sde = (*ctx.sde).clone();
    sde.types.insert(35_832, make_fighter_type(35_832, 1657, 65, 1_000_000.0, vec![])); // Upwell Structure
    sde.types.insert(671, make_fighter_type(671, 30, 6, 100_000.0, vec![])); // Titan
    sde.types.insert(16_213, make_fighter_type(16_213, 365, 23, 50_000.0, vec![])); // POS tower
    sde.types.insert(23_919, make_fighter_type(23_919, 659, 6, 80_000.0, vec![])); // Supercarrier
    sde.types.insert(99_104, make_fighter_type(99_104, 25, 6, 1_000.0, vec![(1045, 1.0)])); // disallowOffensiveModifiers
    sde.types.insert(33_400, make_fighter_type(33_400, 515, 7, 5.0, vec![])); // Bastion module
    sde.types.insert(99_204, make_fighter_type(99_204, 25, 6, 1_000.0, vec![(2045, 0.0)])); // 0 EWAR resistance
    sde.types.insert(99_225, make_fighter_type(99_225, 25, 6, 1_000.0, vec![(2253, 0.0)])); // 0 ECM resist multiplier
    ctx.sde = Arc::new(sde);

    ctx.db.with_mut_snapshot(|snap| {
        let mut victim = snap.characters[&char_id].clone();
        victim.character_id = other_char;
        victim.name = "Victim".into();
        victim.active_ship_id = other_ship;
        snap.characters.insert(other_char, victim);
        snap.items.insert(
            other_ship,
            make_fighter_item(other_ship, 587, other_char, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            unpiloted_item,
            make_fighter_item(unpiloted_item, 99_204, other_char, sys_id as i64, InventoryFlag::None, 1),
        );
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // 1. Immunity checks across category 65, groups 30/365/659, attr 1045, active cycle, bastion, attr 2045
    for (tid, type_id) in [
        (800_001, 35_832),
        (800_002, 671),
        (800_003, 16_213),
        (800_004, 23_919),
        (800_005, 99_104),
    ] {
        assert!(is_target_immune_to_drone_ewar(&ctx, &sys_handle, tid, type_id).await);
    }
    assert!(!is_target_immune_to_drone_ewar(&ctx, &sys_handle, other_ship, 587).await);

    // Active module cycle with self_ewar_immune
    {
        let mut st = sys_handle.actor_state.write().unwrap();
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id: other_ship,
            self_ewar_immune: true,
            ..Default::default()
        });
    }
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys_handle, other_ship, 587).await);
    sys_handle.actor_state.write().unwrap().active_module_cycles.clear();

    // Active Bastion module (33400) on ship in DB
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            800_010,
            make_fighter_item(800_010, 33_400, other_char, other_ship, InventoryFlag::HiSlot0, 1),
        );
        snap.module_active_states.insert(800_010);
    });
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys_handle, other_ship, 587).await);
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&800_010);
    });

    // Fitting with attr 2045 == 0.0 (zero EWAR resistance)
    assert!(is_target_immune_to_drone_ewar(&ctx, &sys_handle, unpiloted_item, 99_204).await);

    // 2. try_apply_drone_ecm_jam branches: ecm_strength == 0, cloaked/tethered, 0 ECM resist, taunt lock preservation
    let drone_ball = Ball {
        id: 950_001,
        type_id: 23_707,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 2_400.0,
        inertia: 1.0,
        mass: 5_000.0,
        radius: 10.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let target_ball = Ball {
        id: other_ship,
        type_id: 587,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let mut ecm_drone = make_drone_state(950_001, 950_101, 23_707, char_id, ship_id, sys_id, Some(other_ship));
    assert!(!try_apply_drone_ecm_jam(&ctx, &sys_handle, &ecm_drone, &drone_ball, other_ship, &target_ball).await);

    ecm_drone.ecm_strength = 500.0;
    sys_handle
        .spawn_ball_with_combat(
            target_ball.clone(),
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(!try_apply_drone_ecm_jam(&ctx, &sys_handle, &ecm_drone, &drone_ball, other_ship, &target_ball).await);

    // Uncloak, add lock on jamming drone + lock on parent ship, register NPC state -> taunt preserves drone lock only
    sys_handle
        .spawn_ball_with_combat(target_ball.clone(), CombatState::default())
        .await
        .unwrap();
    for tid in [ship_id, ecm_drone.ball_id] {
        sys_handle
            .add_target_lock(
                other_ship,
                TargetLock {
                    target_id: tid,
                    lock_time_ms: 1_000.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await
            .unwrap();
    }
    with_mut_combat_runtime(&ctx, |rt| {
        rt.npc_entities.insert(
            other_ship,
            ExtendedNpcEntity {
                ball_id: other_ship,
                type_id: 587,
                faction_id: 0,
                archetype: String::new(),
                solar_system_id: sys_id,
                spawn_group_id: 0,
                target_id: None,
                aggression_threat: std::collections::HashMap::new(),
                threat_table: std::collections::HashMap::new(),
                bounty_isk: 0.0,
                optimal_range: 10_000.0,
                orbit_range: 5_000.0,
                damage_vector: DamageVector::default(),
                entropic_cycles: 0,
                drifter_overshield_hp: 0.0,
                drifter_doomsday_fired: false,
                web_speed_factor: 0.0,
                web_range: 0.0,
                scram_strength: 0.0,
                scram_range: 0.0,
                neut_amount: 0.0,
                ecm_jammed: false,
                remote_shield_rep: 0.0,
                remote_armor_rep: 0.0,
                remote_rep_range: 0.0,
                tag_drop_type_id: None,
                spawn_position: DVec3::ZERO,
                sleeper_wh_class: 0,
                custom_cargo_loot: None,
            },
        );
    });
    assert!(try_apply_drone_ecm_jam(&ctx, &sys_handle, &ecm_drone, &drone_ball, other_ship, &target_ball).await);
    let locks_after = sys_handle.get_target_locks(other_ship).await.unwrap();
    assert_eq!(locks_after.len(), 1);
    assert_eq!(locks_after[0].target_id, ecm_drone.ball_id);
    assert!(with_combat_runtime(&ctx, |rt| rt.npc_entities[&other_ship].ecm_jammed));

    // Zero ECM resist (2253 == 0.0) prevents jam
    let res0_ball = Ball {
        id: 800_020,
        type_id: 99_225,
        position: DVec3::new(1_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            800_020,
            make_fighter_item(800_020, 99_225, other_char, sys_id as i64, InventoryFlag::None, 1),
        );
    });
    sys_handle
        .spawn_ball_with_combat(res0_ball.clone(), CombatState::default())
        .await
        .unwrap();
    assert!(!try_apply_drone_ecm_jam(&ctx, &sys_handle, &ecm_drone, &drone_ball, 800_020, &res0_ball).await);

    // 3. resolve_victim_character_for_ball & renew_drone_aggression_timers
    assert_eq!(resolve_victim_character_for_ball(&ctx, other_ship), Some(other_char));
    assert_eq!(resolve_victim_character_for_ball(&ctx, unpiloted_item), Some(other_char));
    assert_eq!(resolve_victim_character_for_ball(&ctx, 999_999_999), None);

    let now = ctx.now_unix();
    renew_drone_aggression_timers(&ctx, char_id, Some(char_id), now);
    renew_drone_aggression_timers(&ctx, char_id, None, now);
    assert!(!ctx.crimewatch.read().unwrap().states.get(&char_id).is_some_and(|s| s.is_weapon_timer_active(now)));
    renew_drone_aggression_timers(&ctx, char_id, Some(other_char), now);
    assert!(ctx.crimewatch.read().unwrap().states.get(&char_id).is_some_and(|s| s.is_weapon_timer_active(now)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn step_engaged_drones_and_auto_aggression_cloaked_tethered_missing_and_destroy_paths() {
    let (ctx, _session, char_id, ship_id, sys_id) = make_drone_test_ctx();
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_ok = 960_001i64;
    let target_cloaked = 960_002i64;
    let target_fragile = 960_003i64;

    // Parent tethered initially -> engaged drone disengages & auto-aggression skips
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                is_tethered: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let d1 = make_drone_state(970_001, 971_001, 2_456, char_id, ship_id, sys_id, Some(target_ok));
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(d1.ball_id, d1.clone());
    });
    step_engaged_drones(&ctx, 1_000).await;
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&d1.ball_id].activity_state), 0);

    // Untether parent; test missing target ball and cloaked target ball disengage
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: d1.ball_id,
                type_id: 2_456,
                position: DVec3::new(100.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 2_400.0,
                inertia: 1.0,
                mass: 5_000.0,
                radius: 10.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_cloaked,
                type_id: 587,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let d_missing = make_drone_state(970_001, 971_001, 2_456, char_id, ship_id, sys_id, Some(999_888_777));
    let mut d_cloak = make_drone_state(970_002, 971_002, 2_456, char_id, ship_id, sys_id, Some(target_cloaked));
    d_cloak.is_sentry = true;
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: d_cloak.ball_id,
                type_id: 23_561,
                position: DVec3::new(50.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 5_000.0,
                radius: 10.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();

    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(d_missing.ball_id, d_missing.clone());
        rt.active_drones_in_space.insert(d_cloak.ball_id, d_cloak.clone());
    });
    step_engaged_drones(&ctx, 1_000).await;
    with_combat_runtime(&ctx, |rt| {
        assert_eq!(rt.active_drones_in_space[&d_missing.ball_id].target_id, None);
        assert_eq!(rt.active_drones_in_space[&d_cloak.ball_id].target_id, None);
    });

    // ECM drone volley via step_engaged_drones & TiDi cycle scaling
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_ok,
                type_id: 587,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
        )
        .await
        .unwrap();
    let mut d_ecm = make_drone_state(970_001, 971_001, 23_707, char_id, ship_id, sys_id, Some(target_ok));
    d_ecm.damage_vector = DamageVector::default();
    d_ecm.ecm_strength = 500.0;
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.insert(d_ecm.ball_id, d_ecm.clone());
        rt.drone_next_ready_unix.insert(d_ecm.ball_id, 0);
        rt.drone_settings.entry(char_id).or_default().aggressive = true;
    });
    step_engaged_drones(&ctx, 2_000).await;
    assert!(with_combat_runtime(&ctx, |rt| rt.drone_next_ready_unix[&d_ecm.ball_id] > 2_000));

    // Fragile target destroyed by sentry drone in step_drone_auto_aggression_and_assist
    sys_handle
        .spawn_ball_with_combat(
            Ball {
                id: target_fragile,
                type_id: 587,
                position: DVec3::new(500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            CombatState {
                shield_hp: 0.0,
                armor_hp: 0.0,
                structure_hp: 5.0,
                max_structure_hp: 5.0,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let mut sentry_kill = make_drone_state(970_002, 971_002, 23_561, char_id, ship_id, sys_id, None);
    sentry_kill.is_sentry = true;
    sentry_kill.damage_vector = DamageVector::new(0.0, 100.0, 0.0, 0.0);
    with_mut_combat_runtime(&ctx, |rt| {
        rt.active_drones_in_space.clear();
        rt.active_drones_in_space.insert(sentry_kill.ball_id, sentry_kill.clone());
    });
    // Cloaked target skipped by auto-aggression
    assert!(step_drone_auto_aggression_and_assist(&ctx, sys_id, char_id, Some(target_cloaked), None, None).await.is_empty());
    // Fragile target destroyed -> sentry does not orbit and target_id reset to None
    let engaged = step_drone_auto_aggression_and_assist(&ctx, sys_id, char_id, Some(target_fragile), None, None).await;
    assert_eq!(engaged, vec![sentry_kill.ball_id]);
    assert_eq!(with_combat_runtime(&ctx, |rt| rt.active_drones_in_space[&sentry_kill.ball_id].target_id), None);
}
