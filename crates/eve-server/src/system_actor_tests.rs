use super::*;
use eve_destiny::{
    Ball, CombatState, DamageVector, DestinyEvent, MovementMode, SlimItemMeta, TargetLock,
};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::Arc;

fn make_actor_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_500_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn next_downtime_unix_secs_and_registry_node_management() {
    // 10:00 UTC (36_000s) is 1 hour (3_600s) before 11:00 UTC downtime (39_600s)
    assert_eq!(next_downtime_unix_secs(36_000, 86_400, 39_600), 39_600);
    // 12:00 UTC (43_200s) wraps to next day's 11:00 UTC
    assert_eq!(
        next_downtime_unix_secs(43_200, 86_400, 39_600),
        86_400 + 39_600
    );

    let reg = SystemRegistry::with_sde(Arc::new(SdeRegistry::with_canonical_seed()));
    let mut nodes = reg.cluster_nodes.write().unwrap();
    assert_eq!(nodes.worker_nodes, vec![1, 2, 3, 4]);
    nodes.system_node_overrides.insert(30_000_142, 2);
    nodes.manual_tidi_overrides.insert(30_000_142, 0.5);
    assert_eq!(nodes.system_node_overrides.get(&30_000_142), Some(&2));
}

#[tokio::test]
async fn system_handle_commands_cover_ball_lifecycle_movement_combat_and_cycles() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let reg = SystemRegistry::with_sde_and_db(sde, db.clone());

    let death_log = Arc::new(std::sync::Mutex::new(Vec::<(u32, i64, Option<i64>)>::new()));
    let death_clone = Arc::clone(&death_log);
    reg.set_npc_death_hook(Arc::new(move |sys_id, bid, kid| {
        let dl = Arc::clone(&death_clone);
        Box::pin(async move {
            dl.lock().unwrap().push((sys_id, bid, kid));
        })
    }));

    let handle = reg.get_or_spawn(30_000_142);

    // 1. Spawn balls (basic, with combat state, with slim meta), wormhole stages, and slim queries
    assert!(handle
        .spawn_ball(make_actor_ball(101, 587, DVec3::ZERO))
        .await
        .is_ok());
    assert!(handle
        .spawn_ball_with_combat(
            make_actor_ball(102, 587, DVec3::new(5_000.0, 0.0, 0.0)),
            CombatState {
                shield_hp: 200.0,
                max_shield_hp: 500.0,
                capacitor: 300.0,
                max_capacitor: 1_000.0,
                ..CombatState::default()
            }
        )
        .await
        .is_ok());
    assert!(handle
        .spawn_ball_with_meta(
            make_actor_ball(103, 587, DVec3::new(10_000.0, 0.0, 0.0)),
            CombatState {
                shield_hp: 1.0,
                max_shield_hp: 10.0,
                armor_hp: 1.0,
                max_armor_hp: 10.0,
                structure_hp: 1.0,
                max_structure_hp: 10.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_001,
                char_id: Some(90_000_001),
                ..Default::default()
            }
        )
        .await
        .is_ok());

    assert!(handle
        .change_slim_item(
            101,
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_002,
                ..Default::default()
            }
        )
        .await
        .unwrap());
    assert!(handle.set_ball_wormhole_stages(101, 2, 0.8).await.unwrap());
    assert!(handle.get_ball_slim_item(101).await.unwrap().is_some());

    // 2. Asteroids & mining via SystemHandle
    let ast = AsteroidState {
        asteroid_id: 9501,
        belt_id: 40_000_001,
        type_id: 1230,
        initial_units: 500,
        remaining_units: 500,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };
    handle
        .spawn_asteroid(ast, make_actor_ball(9501, 1230, DVec3::new(2_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    let mine_res = handle.mine_asteroid(9501, 120, 0.0).await.unwrap();
    assert_eq!(mine_res.mined_units, 120);
    assert_eq!(mine_res.remaining_units, 380);
    assert_eq!(
        handle.get_asteroid_state(9501).await.unwrap().unwrap().remaining_units,
        380
    );

    // 3. Movement commands & Destiny observer updates / SetState
    assert!(handle
        .set_movement_mode(101, MovementMode::GotoDirection { direction: DVec3::X })
        .await
        .is_ok());
    assert!(handle.goto_point(101, DVec3::new(20_000.0, 0.0, 0.0)).await.is_ok());
    assert!(handle.set_speed_fraction(101, 0.75).await.is_ok());
    assert!(handle.set_ball_position(101, DVec3::new(1_000.0, 0.0, 0.0)).await);
    assert!(handle
        .emit_destiny_event(
            101,
            DestinyEvent::SetBallRadius {
                ball_id: 101,
                radius: 90.0
            }
        )
        .await);
    assert!(handle.push_set_state(101).await.is_ok());
    assert!(handle.get_set_state(101).await.is_ok());
    assert!(handle.get_bubble_id_for_ball(101).await.unwrap().is_some());
    assert!(handle.get_ball_snapshot(101).await.unwrap().is_some());
    assert_eq!(handle.get_ball_group_id(101).await.unwrap(), Some(25));
    assert!(handle.list_balls().await.unwrap().len() >= 3);
    assert!(handle.list_balls_in_bubble(101).await.unwrap().len() >= 3);
    assert!(handle.list_dynamic_balls_in_bubble(101).await.unwrap().len() >= 3);
    let _ = handle.drain_observer_updates(101).await.unwrap();

    // 4. Target locks, combat state, damage & death hook, module cycles, nanite repair, ECM, disruptors
    assert!(handle
        .add_target_lock(
            102,
            TargetLock {
                target_id: 101,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            }
        )
        .await
        .is_ok());
    assert_eq!(handle.get_target_locks(102).await.unwrap().len(), 1);
    assert!(handle.remove_target_lock(102, 101).await.unwrap());
    assert_eq!(handle.clear_target_locks(102).await.unwrap(), 0);

    handle.set_capacitor(102, 600.0).await.unwrap();
    let healed = handle.heal_combat_state(102).await.unwrap();
    assert!((healed.shield_hp - healed.max_shield_hp).abs() < 1e-6);

    assert!(handle.set_warp_scramble_strength(102, 3).await.is_ok());
    assert!(handle.is_ball_scrambled(102).await.unwrap());
    assert!(handle.set_warp_core_strength(102, 5).await.is_ok());
    assert!(!handle.is_ball_scrambled(102).await.unwrap());
    assert!(handle.set_interdiction_nullified(102, true).await.unwrap());

    assert!(handle.add_warp_bubble(8801, DVec3::ZERO, 20_000.0).await.is_ok());
    assert!(handle.remove_warp_bubble(8801).await.unwrap());
    assert!(handle
        .schedule_mobile_warp_disruptor(8802, 12_198, DVec3::ZERO, 20_000.0, 5.0)
        .await
        .unwrap());
    assert!(handle.apply_ecm_jam(102, 10.0).await.unwrap());
    assert!(handle
        .apply_ecm_burst(102, 25_000.0, [20.0, 20.0, 20.0, 20.0], 10.0, 0.1)
        .await
        .is_ok());

    assert!(handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id: 102,
            owner_char_id: Some(90_000_001),
            module_id: 6101,
            duration_s: 5.0,
            repeat: -1,
            ..ActiveModuleCycle::default()
        })
        .await
        .is_ok());
    assert!(handle.has_module_cycle(102, 6101).await);
    assert!(handle.update_module_overload(102, 6101, true).await.unwrap());
    let _ = handle.step_manual(5.0).await.unwrap();
    assert!(handle.has_active_overloaded_cycle_sync(102));
    assert!(handle.remove_module_cycle(102, 6101).await.unwrap().is_some());
    assert!(handle.stop_non_warp_module_cycles(102).await.is_ok());

    handle
        .start_nanite_repair(ActiveNaniteRepair {
            char_id: 90_000_001,
            ship_id: 102,
            module_id: 6102,
            initial_damage_hp: 10.0,
            paste_total_needed: 2,
            paste_consumed_so_far: 2,
            duration_s: 5.0,
            elapsed_s: 2.5,
        })
        .await
        .unwrap();
    assert!(handle.has_any_active_nanite_repair_sync(102));
    assert!(handle.is_module_being_nanite_repaired_sync(102, 6102));
    assert!(handle.stop_nanite_repair(102, 6102).await.unwrap().is_some());

    // MicroJump + MicroJumpField
    assert!(handle.micro_jump(102, 100_000.0).await.is_ok());
    assert!(handle.micro_jump_field(102, 50_000.0, 6_000.0, 25).await.is_ok());

    // Lethal damage on 103 triggers npc_death_hook
    let dmg_res = handle
        .apply_damage_with_attacker(103, DamageVector::new(100.0, 100.0, 100.0, 100.0), Some(90_000_002))
        .await
        .unwrap();
    assert!(matches!(dmg_res, Some((_, true, _))));
    assert!(death_log
        .lock()
        .unwrap()
        .iter()
        .any(|&(sys, bid, killer)| sys == 30_000_142 && bid == 103 && killer == Some(90_000_002)));

    // Step manual & tick + DB ball removal
    assert!(handle.step_manual(1.0).await.is_ok());
    assert!(handle.tick().await.is_ok());
    assert!(handle.active_entity_and_cycle_load() >= 1);
    assert!(handle.remove_ball(101).await.is_some());
    try_remove_ball_for_db(&db, 30_000_142, 102);
    assert!(handle.get_ball_position_sync(102).is_none());
}
