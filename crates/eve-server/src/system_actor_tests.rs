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

#[tokio::test]
async fn system_registry_builders_sync_queries_hooks_and_nanite_refunds() {
    // 1. next_downtime_unix_secs edge cases (zero period clamps to 1; exact phase wraps next interval)
    assert_eq!(next_downtime_unix_secs(100, 0, 50), 101);
    assert_eq!(next_downtime_unix_secs(39_600, 86_400, 39_600), 86_400 + 39_600);

    // 2. Registry builders, intervals, claim guards, sync queries
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let (notify_tx, _rx) = tokio::sync::broadcast::channel(16);
    let pending_notifs = Arc::new(eve_db::lock_rank::RankedRwLock::new(
        std::collections::HashMap::new(),
    ));
    let tactical = Arc::new(eve_db::lock_rank::RankedRwLock::new(
        crate::services::tactical_modules::TacticalModulesState::default(),
    ));
    let rng = Arc::new(crate::rng::MechanicRng::seeded(
        crate::rng::TEST_MODE_DEFAULT_SEED,
    ));

    let reg = SystemRegistry::with_sde_and_db(sde, db.clone())
        .with_notifications(pending_notifs, notify_tx)
        .with_tactical_state(tactical)
        .with_mechanic_rng(rng);

    let default_reg = SystemRegistry::new();
    assert_eq!(default_reg.active_systems_count(), 0);
    assert!(default_reg.active_system_handles().is_empty());

    reg.set_belt_respawn_interval_secs(0);
    assert_eq!(reg.belt_respawn_interval_secs(), 1);
    reg.set_belt_respawn_interval_secs(999_999);
    assert_eq!(reg.belt_respawn_interval_secs(), 604_800);

    reg.set_downtime_utc_secs_of_day(90_000);
    assert_eq!(reg.downtime_utc_secs_of_day(), 3_600);

    // Claim guards: first claim returns true, duplicate returns false
    assert!(reg.claim_special_site_ball(30_000_142, 700_001));
    assert!(!reg.claim_special_site_ball(30_000_142, 700_001));
    assert!(reg.claim_wh_ore_anomaly_hydration(31_000_005, "anom-alpha"));
    assert!(!reg.claim_wh_ore_anomaly_hydration(31_000_005, "anom-alpha"));

    // Hooks: step, target_lock_completed, module_deactivation
    let step_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let step_hits_c = Arc::clone(&step_hits);
    reg.set_system_step_hook(Arc::new(move |_sys_id, _dt, _entities| {
        let c = Arc::clone(&step_hits_c);
        Box::pin(async move {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })
    }));

    let locks_completed = Arc::new(std::sync::Mutex::new(Vec::<(i64, i64)>::new()));
    let lc_c = Arc::clone(&locks_completed);
    reg.set_target_lock_completed_hook(Arc::new(move |char_id, tgt| {
        lc_c.lock().unwrap().push((char_id, tgt));
    }));

    let deactivated = Arc::new(std::sync::Mutex::new(Vec::<(i64, i64)>::new()));
    let deact_c = Arc::clone(&deactivated);
    reg.set_module_deactivation_hook(Arc::new(move |char_id, cycle| {
        deact_c.lock().unwrap().push((char_id, cycle.module_id));
    }));

    let sys_id = 30_000_142u32;
    let handle = reg.get_or_spawn(sys_id);
    assert_eq!(reg.active_systems_count(), 1);
    assert_eq!(reg.active_system_handles().len(), 1);

    // Spawn attacker (201) + target (202) + gate-cloaked ship (203) + NPC (204)
    handle
        .spawn_ball_with_meta(
            make_actor_ball(201, 587, DVec3::new(1_000.0, 2_000.0, 3_000.0)),
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                armor_hp: 500.0,
                max_armor_hp: 500.0,
                structure_hp: 500.0,
                max_structure_hp: 500.0,
                is_tethered: true,
                undock_invulnerable_remaining_ticks: 10,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_010,
                char_id: Some(90_000_010),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    // Preserving slim meta on overwrite via spawn_ball
    let mut b201_updated = make_actor_ball(201, 587, DVec3::new(1_500.0, 2_000.0, 3_000.0));
    b201_updated.max_velocity = 420.0;
    assert!(handle.spawn_ball(b201_updated).await.is_ok());
    let slim_201 = handle.get_ball_slim_item(201).await.unwrap().unwrap();
    assert!(matches!(
        slim_201,
        eve_proto::PyRep::Object { .. } | eve_proto::PyRep::KeyVal(_)
    ));

    // Max velocity & cap mutations + missing ball false branch
    assert!(handle.set_ball_max_velocity(201, 510.0).await.unwrap());
    assert!(!handle.set_ball_max_velocity(999_999, 510.0).await.unwrap());
    assert!(handle.set_ball_max_velocity_cap(201, Some(300.0)).await.unwrap());
    assert!(handle.set_ball_max_velocity_cap(201, None).await.unwrap());
    assert!(!handle.set_ball_max_velocity_cap(999_999, Some(100.0)).await.unwrap());

    // ChangeSlimItem / GetBallSlimItem / SetBallWormholeStages / BreakGateCloak missing & happy paths
    let mut new_slim = SlimItemMeta {
        group_id: 25,
        category_id: 6,
        owner_id: 90_000_010,
        char_id: Some(90_000_010),
        name: Some("UpdatedRifter".into()),
        ..Default::default()
    };
    assert!(handle.change_slim_item(201, new_slim.clone()).await.unwrap());
    new_slim.name = None;
    assert!(!handle.change_slim_item(999_999, new_slim).await.unwrap());
    assert!(handle.get_ball_slim_item(201).await.unwrap().is_some());
    assert!(handle.get_ball_slim_item(999_999).await.unwrap().is_none());
    assert!(handle.set_ball_wormhole_stages(999_999, 1, 0.5).await.is_ok());

    handle
        .spawn_ball_with_meta(
            make_actor_ball(203, 587, DVec3::new(4_000.0, 0.0, 0.0)),
            CombatState {
                is_cloaked: true,
                gate_cloak_remaining_ticks: 5,
                ..CombatState::default()
            },
            SlimItemMeta {
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(reg.has_active_modules_or_cloak_sync(sys_id, 203));
    assert!(handle.break_gate_cloak(203).await.unwrap());
    assert!(!handle.break_gate_cloak(999_999).await.unwrap());
    assert!(!reg.has_active_modules_or_cloak_sync(sys_id, 203));

    // Global & per-system sync queries
    assert!(reg.find_ball_position_sync(201).is_some());
    assert!(reg.find_ball_position_sync(999_999).is_none());
    assert_eq!(reg.find_ball_system_sync(201), Some(sys_id));
    assert_eq!(reg.find_ball_system_sync(999_999), None);
    assert!(reg.get_ball_position_sync(sys_id, 201).is_some());
    assert!(reg.get_ball_position_sync(30_000_001, 201).is_none());
    assert!(!reg.is_ball_scrambled_sync(sys_id, 201));
    assert!(!reg.is_ball_scrambled_sync(30_000_001, 201));
    assert!(reg.take_pending_cycle_actions(30_000_001).is_empty());

    // Target lock rejections (cloaked / tethered) and clearing undock invulnerability on source
    handle
        .spawn_ball_with_meta(
            make_actor_ball(202, 587, DVec3::new(6_000.0, 0.0, 0.0)),
            CombatState {
                is_tethered: true,
                ..CombatState::default()
            },
            SlimItemMeta::default(),
        )
        .await
        .unwrap();
    let pending_lock = TargetLock {
        target_id: 202,
        lock_time_ms: 1_000.0,
        remaining_lock_ms: 400.0,
        locked: false,
        max_range_m: Some(50_000.0),
    };
    assert!(handle.add_target_lock(201, pending_lock.clone()).await.is_err());

    // Untether 202, mark cloaked -> still rejected
    handle
        .set_combat_state(
            202,
            CombatState {
                is_cloaked: true,
                is_tethered: false,
                ..CombatState::default()
            },
        )
        .await
        .unwrap();
    assert!(handle.add_target_lock(201, pending_lock.clone()).await.is_err());

    // Clear cloak on 202 -> lock succeeds, clears tether & undock invulnerability on 201
    handle
        .set_combat_state(202, CombatState::default())
        .await
        .unwrap();
    assert!(handle.add_target_lock(201, pending_lock).await.is_ok());
    let cs_201 = handle.get_combat_state(201).await.unwrap().unwrap();
    assert!(!cs_201.is_tethered);
    assert_eq!(cs_201.undock_invulnerable_remaining_ticks, 0);

    // StepManual invokes system_step_hook AND completes lock -> target_lock_completed_hook
    handle.step_manual(1.0).await.unwrap();
    assert!(step_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    assert!(reg.get_system_sim_elapsed(sys_id) >= 1.0);
    assert!(locks_completed
        .lock()
        .unwrap()
        .iter()
        .any(|&(ch, tgt)| ch == 90_000_010 && tgt == 202));

    // Remove & clear target lock branches
    assert!(handle.remove_target_lock(201, 202).await.unwrap());
    assert!(!handle.remove_target_lock(201, 202).await.unwrap());
    assert!(handle
        .add_target_lock(
            201,
            TargetLock {
                target_id: 202,
                lock_time_ms: 0.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .is_ok());
    assert_eq!(handle.clear_target_locks(201).await.unwrap(), 1);
    assert_eq!(handle.clear_target_locks(201).await.unwrap(), 0);

    // Nanite repair refunds: full refund at elapsed=0 vs partial refund + ship-wide cancel_nanite_repairs_for_ship_sync
    handle
        .start_nanite_repair(ActiveNaniteRepair {
            char_id: 90_000_010,
            ship_id: 201,
            module_id: 7001,
            initial_damage_hp: 20.0,
            paste_total_needed: 10,
            paste_consumed_so_far: 10,
            duration_s: 10.0,
            elapsed_s: 0.0,
        })
        .await
        .unwrap();
    let (_, full_refund) = handle.stop_nanite_repair(201, 7001).await.unwrap().unwrap();
    assert_eq!(full_refund, 10);

    handle
        .start_nanite_repair(ActiveNaniteRepair {
            char_id: 90_000_010,
            ship_id: 201,
            module_id: 7002,
            initial_damage_hp: 20.0,
            paste_total_needed: 10,
            paste_consumed_so_far: 10,
            duration_s: 10.0,
            elapsed_s: 5.0,
        })
        .await
        .unwrap();
    let canceled = reg.cancel_nanite_repairs_for_ship_sync(201);
    assert_eq!(canceled.len(), 1);
    assert!(handle.stop_nanite_repair(201, 7002).await.unwrap().is_none());

    // Error paths on missing balls for movement/combat/state commands
    assert!(handle.set_movement_mode(999_999, MovementMode::Stop).await.is_err());
    assert!(handle.set_speed_fraction(999_999, 0.5).await.is_err());
    assert!(handle.set_warp_scramble_strength(999_999, 1).await.is_err());
    assert!(handle.set_warp_core_strength(999_999, 1).await.is_err());
    assert!(!handle.set_ball_position(999_999, DVec3::ZERO).await);
    assert!(handle.push_set_state(999_999).await.is_err());
    assert!(handle.get_set_state(999_999).await.is_err());
    assert!(handle.list_balls_in_bubble(201).await.is_ok());
    assert!(handle.list_balls_in_bubble(999_999).await.is_ok());
    assert!(handle.list_dynamic_balls_in_bubble(201).await.is_ok());
    assert!(handle.list_dynamic_balls_in_bubble(999_999).await.is_ok());
    assert!(handle.apply_damage(999_999, DamageVector::default()).await.unwrap().is_none());
    assert!(handle.apply_true_damage(999_999, 50.0).await.unwrap().is_none());
    assert!(!handle.remove_warp_bubble(999_999).await.unwrap());
    assert!(!handle.emit_packaged_actions(999_999, vec![]).await);
    assert!(handle.emit_packaged_actions(201, vec![DestinyEvent::Stop { ball_id: 201 }]).await);
    assert!(!handle.apply_ecm_jam(999_998, 5.0).await.unwrap());

    // ApplyEcmJam immunity via active module cycle with self_ewar_immune
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id: 201,
            owner_char_id: Some(90_000_010),
            module_id: 7003,
            duration_s: 10.0,
            repeat: -1,
            self_ewar_immune: true,
            ..ActiveModuleCycle::default()
        })
        .await
        .unwrap();
    assert!(!handle.apply_ecm_jam(201, 5.0).await.unwrap());

    // RecordIncomingArmorDamage + FastForwardClock + AddInterdictionSphere + AddMobileWarpDisruptor
    assert!(handle.record_incoming_armor_damage(201, [10.0, 20.0, 30.0, 40.0]).await.is_ok());
    assert!(handle.fast_forward_clock(201, 30).await.is_ok());
    assert!(handle.add_interdiction_sphere(8901, DVec3::ZERO, 20_000.0).await.unwrap());
    assert!(handle.add_mobile_warp_disruptor(8902, DVec3::ZERO, 15_000.0).await.unwrap());

    // SetMovementMode::Stop reconciles active prop cycle on valid ball
    assert!(handle.set_movement_mode(201, MovementMode::Stop).await.is_ok());

    // ApplyTrueDamage partial then lethal triggers deactivation hook
    let partial = handle.apply_true_damage(201, 100.0).await.unwrap().unwrap();
    assert!(!partial.1);
    let lethal = handle.apply_true_damage(201, 10_000.0).await.unwrap().unwrap();
    assert!(lethal.1);
    assert!(deactivated.lock().unwrap().iter().any(|(ch, mid)| *ch == 90_000_010 && *mid == 7003));

    // AcceptDocking both with station ball present & fallback station_pos
    handle
        .spawn_ball(make_actor_ball(60_003_760, 56, DVec3::new(100_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    handle
        .spawn_ball(make_actor_ball(205, 587, DVec3::new(100_500.0, 0.0, 0.0)))
        .await
        .unwrap();
    handle
        .spawn_ball(make_actor_ball(206, 587, DVec3::new(200_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    let docked_in_bp = handle.accept_docking(205, 60_003_760, DVec3::ZERO).await;
    assert!(docked_in_bp.is_some());
    let docked_fallback = handle.accept_docking(206, 60_003_761, DVec3::new(250_000.0, 0.0, 0.0)).await;
    assert!(docked_fallback.is_some());

    // SpawnAsteroid idempotent duplicate + PopulateAsteroids + GetAsteroidState None
    let ast_ball = make_actor_ball(9601, 1230, DVec3::new(10_000.0, 0.0, 0.0));
    let ast = AsteroidState {
        asteroid_id: 9601,
        belt_id: 40_000_001,
        type_id: 1230,
        initial_units: 500,
        remaining_units: 500,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };
    assert_eq!(handle.spawn_asteroid(ast.clone(), ast_ball.clone()).await.unwrap(), 9601);
    assert_eq!(handle.spawn_asteroid(ast, ast_ball).await.unwrap(), 9601);
    assert!(handle.get_asteroid_state(999_999).await.unwrap().is_none());
    let populated = handle.populate_asteroid_belt_entities().await.unwrap();
    assert!(!populated.is_empty());

    // Warp completion persisted on DB character active ship
    let warp_ship = 207i64;
    db.with_mut_snapshot(|snap| {
        snap.characters.insert(
            90_000_099,
            eve_db::CharacterRecord {
                character_id: 90_000_099,
                user_id: 99,
                name: "WarpPilot".into(),
                corporation_id: 1_000_009,
                alliance_id: None,
                war_faction_id: None,
                station_id: None,
                structure_id: None,
                solar_system_id: sys_id,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: warp_ship,
                balance: eve_db::IskCents(0),
                plex_balance: 0,
                loyalty_points: Default::default(),
                standings: Default::default(),
                security_status: 0.0,
                clone_grade: 1,
                home_station_id: 60_003_760,
                race_id: 1,
                bloodline_id: 1,
                ancestry_id: 1,
                school_id: 11,
                neural: Default::default(),
                skill_points: 0.0,
                free_skill_points: 0.0,
                last_clone_jump_unix: 0,
                jump_fatigue_minutes: 0.0,
                jump_cooldown_until_unix: 0,
                jump_fatigue_until_unix: 0,
                description: String::new(),
                title: String::new(),
                activity_status: None,
                gender: 0,
                create_date_unix: 0,
                fleet_id: None,
                autopilot_waypoints: vec![],
                version: 1,
                system_epoch: 0,
            },
        );
    });
    let mut b207 = make_actor_ball(warp_ship, 587, DVec3::ZERO);
    b207.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(150_000.0, 0.0, 0.0),
        warp_speed_au_s: 10_000.0,
        total_distance: 150_000.0,
        phase: eve_destiny::WarpPhase::Decelerating,
        elapsed_warp_time: 10.0,
    };
    handle.spawn_ball(b207).await.unwrap();
    let _ = handle.step_manual(5.0).await.unwrap();
    let _ = handle.step_manual(5.0).await.unwrap();

    // Target lock completion on an Upwell structure ball controlled by an active gunner (`active_pilot_by_structure`)
    let struct_ball = 88_801i64;
    let struct_tgt = 88_802i64;
    db.with_mut_snapshot(|snap| {
        let mut ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
        ext.active_pilot_by_structure.insert(struct_ball, 90_000_010);
        crate::services::station_structure::structure_upwell_ops::save_ext_state(snap, &ext);
    });
    handle.spawn_ball_with_combat(make_actor_ball(struct_ball, 35832, DVec3::new(50_000.0, 0.0, 0.0)), CombatState::default()).await.unwrap();
    handle.spawn_ball_with_combat(make_actor_ball(struct_tgt, 587, DVec3::new(60_000.0, 0.0, 0.0)), CombatState::default()).await.unwrap();
    handle
        .add_target_lock(
            struct_ball,
            TargetLock {
                target_id: struct_tgt,
                lock_time_ms: 500.0,
                remaining_lock_ms: 100.0,
                locked: false,
                max_range_m: Some(250_000.0),
            },
        )
        .await
        .unwrap();
    let _ = handle.step_manual(1.0).await.unwrap();
    assert!(locks_completed.lock().unwrap().iter().any(|&(ch, tgt)| ch == 90_000_010 && tgt == struct_tgt));
}

#[tokio::test]
async fn issue_803_drain_observer_updates_increments_tick_on_set_state() {
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);
    let ship_id = 200_125_001_i64;
    let capsule_id = 200_125_002_i64;
    handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            speed_fraction: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let _ = handle.step_manual(1.0).await.unwrap();
    let (tick_before, _, _) = handle.drain_observer_updates(ship_id).await.unwrap();

    // Pod ejection on the same tick spawns capsule_id and queues SetState(capsule_id).
    handle
        .spawn_ball(Ball {
            id: capsule_id,
            type_id: 670,
            position: DVec3::new(1_010.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            speed_fraction: 0.0,
            inertia: 1.0,
            mass: 1_000.0,
            radius: 15.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    handle.push_set_state(capsule_id).await.unwrap();
    let (tick_set_state, events, _) = handle.drain_observer_updates(capsule_id).await.unwrap();
    assert!(events
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::SetState { observer_ball_id } if *observer_ball_id == capsule_id)));
    assert_eq!(
        tick_set_state,
        tick_before,
        "SetState drain must stamp at current_tick without inflating ballpark tick (#1169)"
    );
}

#[tokio::test]
async fn issue_804_active_module_cycle_weapon_damage_emits_on_damage_message() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let pending = Arc::new(eve_db::lock_rank::RankedRwLock::new(
        std::collections::HashMap::<i64, Vec<crate::router::PendingMachoNotification>>::new(),
    ));
    let (notify_tx, _) = tokio::sync::broadcast::channel(64);
    let reg = SystemRegistry::with_sde_and_db(sde, Arc::clone(&db))
        .with_notifications(Arc::clone(&pending), notify_tx);
    let handle = reg.get_or_spawn(30_000_142);

    let attacker_char = 90_000_001_i64;
    let victim_char = 90_000_002_i64;
    let attacker_ship = 200_200_001_i64;
    let victim_ship = 200_200_002_i64;
    handle
        .spawn_ball_with_meta(
            Ball {
                id: attacker_ship,
                type_id: 587,
                position: DVec3::ZERO,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                speed_fraction: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: attacker_char,
                char_id: Some(attacker_char),
                corp_id: 1_000_009,
                alliance_id: 0,
                security_status: 0.0,
                is_cloaked: false,
                name: Some("Attacker".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    handle
        .spawn_ball_with_meta(
            Ball {
                id: victim_ship,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                speed_fraction: 0.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                mode: MovementMode::Stop,
            },
            CombatState::default(),
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: victim_char,
                char_id: Some(victim_char),
                corp_id: 1_000_009,
                alliance_id: 0,
                security_status: 0.0,
                is_cloaked: false,
                name: Some("Victim".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    handle
        .add_module_cycle(ActiveModuleCycle {
            owner_char_id: Some(attacker_char),
            ship_id: attacker_ship,
            module_id: 200_200_010,
            module_type_id: 3640,
            effect_name: "targetAttack".into(),
            duration_s: 2.0,
            elapsed_s: 1.5,
            repeat: 1,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            shield_boost_amount: 0.0,
            target_id: Some(victim_ship),
            damage_vector: DamageVector::new(10.0, 15.0, 20.0, 0.0),
            remote_shield_amount: 0.0,
            remote_armor_amount: 0.0,
            remote_hull_amount: 0.0,
            remote_cap_amount: 0.0,
            target_velocity_multiplier: None,
            max_effective_range_m: Some(10_000.0),
            ..Default::default()
        })
        .await
        .unwrap();

    handle.step_manual(1.0).await.unwrap();
    let guard = pending.read().unwrap();
    let attacker_notifs = guard.get(&attacker_char).expect("attacker notifications");
    assert!(attacker_notifs.iter().any(|n| n.event == "OnDamageMessage"));
    assert!(attacker_notifs.iter().any(|n| n.event == "OnDamageMessages"));
    let victim_notifs = guard.get(&victim_char).expect("victim notifications");
    assert!(victim_notifs.iter().any(|n| n.event == "OnDamageMessage"));
    assert!(victim_notifs.iter().any(|n| n.event == "OnDamageMessages"));
}

#[tokio::test]
async fn apply_fit_projection_updates_baseline_preserving_active_multipliers_and_rejecting_stale_rev() {
    let mut bp = eve_destiny::Ballpark::new(30_000_142);
    let ship_id: BallId = 700_000_001;
    bp.add_ball(make_actor_ball(ship_id, 587, DVec3::ZERO));
    bp.combat_states.insert(
        ship_id,
        CombatState {
            prop_velocity_multiplier: 1.5,
            web_velocity_multiplier: 0.5,
            ..Default::default()
        },
    );

    let mk_proj = |rev: u64, max_vel: f64, mass: f64| eve_dogma::FitProjection {
        rev: eve_dogma::FitRev(rev),
        ship_id,
        ship_type_id: 587,
        active_stance_type_id: None,
        last_stance_switch_ms: None,
        baseline: eve_dogma::CombatBaseline {
            max_velocity: max_vel,
            mass,
            agility: 3.0,
            signature_radius: 40.0,
            max_shield_hp: 600.0,
            max_armor_hp: 500.0,
            max_structure_hp: 400.0,
            max_capacitor: 300.0,
            shield_recharge_time_ms: 100_000.0,
            capacitor_recharge_time_ms: 120_000.0,
            shield_resonance: [1.0; 4],
            armor_resonance: [1.0; 4],
            structure_resonance: [1.0; 4],
            warp_speed_au_s: 5.0,
            max_locked_targets: 4,
            max_target_range: 25_000.0,
            scan_resolution: 500.0,
        },
        propulsion: None,
        modules: std::collections::HashMap::new(),
    };

    let mut projections = std::collections::HashMap::new();
    let applied = apply_fit_projection_in_actor(
        &mut bp,
        &mut projections,
        ship_id,
        mk_proj(2, 400.0, 1_100_000.0),
    );
    assert!(applied);
    assert_eq!(projections[&ship_id].rev, eve_dogma::FitRev(2));
    // Active prop (1.5) and web (0.5) multipliers remain intact over the new 400.0 baseline.
    let expected_vel = 400.0 * 1.5 * 0.5;
    assert!((bp.balls[&ship_id].max_velocity - expected_vel).abs() < 1e-6);
    assert!(
        bp.pending_events.iter().any(|ev| matches!(
            ev,
            DestinyEvent::SetMaxSpeed { ball_id, max_speed }
                if *ball_id == ship_id && (*max_speed - expected_vel).abs() < 1e-6
        )),
        "ApplyFitProjection must emit SetMaxSpeed on velocity change"
    );

    // Older FitRev(1) must be rejected without clobbering FitRev(2) baselines.
    let stale = apply_fit_projection_in_actor(
        &mut bp,
        &mut projections,
        ship_id,
        mk_proj(1, 200.0, 900_000.0),
    );
    assert!(!stale);
    assert_eq!(projections[&ship_id].rev, eve_dogma::FitRev(2));
    assert!((bp.balls[&ship_id].max_velocity - expected_vel).abs() < 1e-6);

    // End-to-end actor command handle path stores latest_fit_projection and updates ball.
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);
    handle
        .spawn_ball(make_actor_ball(ship_id, 587, DVec3::ZERO))
        .await
        .unwrap();
    handle
        .set_combat_state(ship_id, CombatState::default())
        .await
        .unwrap();
    assert!(handle.apply_fit_projection(ship_id, mk_proj(3, 420.0, 1_050_000.0)).await);
    assert_eq!(handle.latest_fit_projection(ship_id).unwrap().rev, eve_dogma::FitRev(3));
    let (_, snap_ball) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!((snap_ball.max_velocity - 420.0).abs() < 1e-6);
}

#[tokio::test]
async fn system_view_updates_across_commands_steps_sync_mutators_and_lock_free_under_ballpark_lock() {
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);

    let initial_view = handle.view();
    assert_eq!(initial_view.solar_system_id, 30_000_142);
    assert_eq!(initial_view.current_tick, 1);
    assert!((initial_view.tidi_factor - 1.0).abs() < 1e-12);
    assert!(initial_view.balls.is_empty());

    let ship_a: BallId = 810_000_001;
    let ship_b: BallId = 810_000_002;
    let mut ball_a = make_actor_ball(ship_a, 28_659, DVec3::new(1_000.0, 0.0, 0.0));
    ball_a.velocity = DVec3::new(120.0, 0.0, 0.0);
    ball_a.mode = MovementMode::GotoDirection { direction: DVec3::X };
    ball_a.mass = 12_000_000.0;
    ball_a.radius = 130.0;

    handle
        .spawn_ball_with_meta(
            ball_a,
            CombatState {
                shield_hp: 250.0,
                max_shield_hp: 500.0,
                armor_hp: 300.0,
                max_armor_hp: 400.0,
                structure_hp: 150.0,
                max_structure_hp: 300.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 900,
                category_id: 6,
                owner_id: 90_001,
                corp_id: 98_001,
                alliance_id: 99_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    handle
        .spawn_ball_with_meta(
            make_actor_ball(ship_b, 587, DVec3::new(2_500.0, 0.0, 0.0)),
            CombatState {
                is_tethered: true,
                is_cloaked: true,
                warp_scramble_strength: 2,
                warp_core_strength: 1,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_002,
                is_cloaked: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let after_spawn = handle.view();
    assert!(after_spawn.contains_ball(ship_a));
    assert_eq!(after_spawn.owner_id(ship_a), Some(90_001));
    assert_eq!(after_spawn.corp_id(ship_a), Some(98_001));
    assert_eq!(after_spawn.alliance_id(ship_a), Some(99_001));
    assert_eq!(after_spawn.group_id(ship_a), Some(900));
    assert_eq!(after_spawn.ball_velocity(ship_a), Some(DVec3::new(120.0, 0.0, 0.0)));
    assert!(matches!(
        after_spawn.movement_mode(ship_a),
        Some(MovementMode::GotoDirection { .. })
    ));
    let fracs = after_spawn.damage_fractions(ship_a).unwrap();
    assert!((fracs.shield - 0.5).abs() < 1e-9);
    assert!((fracs.armor - 0.75).abs() < 1e-9);
    assert!((fracs.structure - 0.5).abs() < 1e-9);
    assert!(after_spawn.is_ball_cloaked(ship_b));
    assert!(after_spawn.is_ball_tethered(ship_b));
    assert!(after_spawn.is_ball_invulnerable(ship_b));
    assert!(after_spawn.is_ball_scrambled(ship_b));

    let bastion_mod = 95_001_i64;
    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id: ship_a,
            module_id: bastion_mod,
            module_type_id: 33400,
            effect_name: "bastion".to_string(),
            duration_s: 60.0,
            repeat: 1,
            ..ActiveModuleCycle::default()
        })
        .await
        .unwrap();
    let _ = handle.step_manual(1.0).await.unwrap();

    let after_step = handle.view();
    assert_eq!(after_step.current_tick, 2);
    assert!((after_step.sim_elapsed_secs - 1.0).abs() < 1e-9);
    assert!(after_step.has_active_module(ship_a));
    assert!(after_step.has_active_bastion(ship_a));

    // Synchronous mutators keep SystemView current without going through cmd_tx.
    handle.try_set_ball_position(ship_a, DVec3::new(9_000.0, 0.0, 0.0));
    assert_eq!(
        handle.view().ball_position(ship_a),
        Some(DVec3::new(9_000.0, 0.0, 0.0))
    );
    assert!(handle.view().has_active_bastion(ship_a));

    handle.try_step_manual(1.0);
    assert_eq!(handle.view().current_tick, 3);

    handle.try_remove_module_cycle(ship_a, bastion_mod);
    let after_mod_rm = handle.view();
    assert!(!after_mod_rm.has_active_bastion(ship_a));
    assert!(!after_mod_rm.has_active_module(ship_a));

    handle.set_tidi_factor_raw(0.5);
    assert!((handle.view().tidi_factor - 0.5).abs() < 1e-12);

    // Lock-free view reads succeed even while handle.ballpark is exclusively write-locked,
    // keep seeing the pre-write snapshot until the write guard drops cleanly, and immediately
    // reflect direct `.ballpark.write()` mutations once the guard releases.
    {
        let mut bp_write = handle.ballpark.write().unwrap();
        bp_write.set_warp_scramble_strength(ship_a, 2);
        bp_write.combat_states.get_mut(&ship_a).unwrap().is_cloaked = true;
        let lock_free_view = handle.view();
        assert_eq!(lock_free_view.current_tick, 3);
        assert!(lock_free_view.contains_ball(ship_a));
        assert!(!lock_free_view.is_ball_scrambled(ship_a));
        assert!(!lock_free_view.is_ball_cloaked(ship_a));
    }
    let after_direct_write = handle.view();
    assert!(after_direct_write.is_ball_scrambled(ship_a));
    assert!(after_direct_write.is_ball_cloaked(ship_a));
    assert!((after_direct_write.tidi_factor - 0.5).abs() < 1e-12);

    // Panicking while holding a direct write guard does NOT publish half-mutated dirty state,
    // whereas recovering and completing a write guard publishes cleanly.
    let pre_poison_pos = handle.view().ball_position(ship_a);
    assert!(pre_poison_pos.is_some());
    assert_ne!(pre_poison_pos, Some(DVec3::new(999_999.0, 0.0, 0.0)));
    let poison_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut bp_write = handle.ballpark.write().unwrap();
        bp_write.set_ball_position(ship_a, DVec3::new(999_999.0, 0.0, 0.0));
        panic!("simulated ballpark poison for #1342 view test");
    }));
    assert!(poison_res.is_err());
    assert!(handle.ballpark.try_read().is_err());
    let view_while_poisoned = handle.view();
    assert_eq!(view_while_poisoned.current_tick, 3);
    assert!(view_while_poisoned.contains_ball(ship_a));
    assert_eq!(view_while_poisoned.ball_position(ship_a), pre_poison_pos);
    {
        let mut recovered = handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
        recovered.set_ball_position(ship_a, DVec3::new(12_345.0, 0.0, 0.0));
    }
    assert_eq!(
        handle.view().ball_position(ship_a),
        Some(DVec3::new(12_345.0, 0.0, 0.0))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn set_movement_mode_and_warp_defer_reply_until_system_view_published() {
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);
    let ship_id: BallId = 810_000_101;
    handle
        .spawn_ball_with_meta(
            make_actor_ball(ship_id, 587, DVec3::new(5_000.0, 0.0, 0.0)),
            CombatState {
                max_shield_hp: 500.0,
                shield_hp: 500.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    for i in 0..25 {
        let dir = if i % 2 == 0 { DVec3::X } else { DVec3::Y };
        handle
            .set_movement_mode(ship_id, MovementMode::GotoDirection { direction: dir })
            .await
            .unwrap();
        let view = handle.view();
        assert!(
            matches!(view.movement_mode(ship_id), Some(MovementMode::GotoDirection { direction }) if (*direction - dir).length() < 1e-12),
            "iteration {i}: GotoDirection not immediately visible on SystemView after await"
        );

        handle
            .set_movement_mode(ship_id, MovementMode::Stop)
            .await
            .unwrap();
        assert!(
            matches!(handle.view().movement_mode(ship_id), Some(MovementMode::Stop)),
            "iteration {i}: Stop not immediately visible on SystemView after await"
        );
    }

    let warp_dest = DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let warp_mode = MovementMode::new_warp(DVec3::new(5_000.0, 0.0, 0.0), warp_dest, 3.0);
    handle
        .set_warp_movement_mode(ship_id, warp_mode, WarpProvenance::PlayerOrFleetWarp)
        .await
        .unwrap();
    assert!(matches!(
        handle.view().movement_mode(ship_id),
        Some(MovementMode::Warp { .. })
    ));
    assert!(handle.set_movement_mode(999_999_999, MovementMode::Stop).await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn try_step_full_tick_refreshes_system_view_sim_elapsed_and_expired_bastion() {
    let reg = SystemRegistry::new();
    let handle = reg.get_or_spawn(30_000_142);
    let ship_id: BallId = 810_000_201;
    handle
        .spawn_ball_with_meta(
            make_actor_ball(ship_id, 28_659, DVec3::new(10_000.0, 0.0, 0.0)),
            CombatState {
                max_shield_hp: 1000.0,
                shield_hp: 1000.0,
                max_capacitor: 5000.0,
                capacitor: 5000.0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 900,
                category_id: 6,
                owner_id: 90_001,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    handle
        .add_module_cycle(ActiveModuleCycle {
            ship_id,
            module_id: 96_001,
            module_type_id: 33400,
            effect_name: "bastion".to_string(),
            duration_s: 1.0,
            repeat: 0,
            ..ActiveModuleCycle::default()
        })
        .await
        .unwrap();
    let before = handle.view();
    assert!(before.has_active_module(ship_id));
    assert!(before.has_active_bastion(ship_id));
    assert!(before.sim_elapsed_secs.abs() < 1e-9);

    let tick_after = handle.try_step_full_tick(1.5);
    let after = handle.view();
    assert_eq!(after.current_tick, tick_after);
    assert!((after.sim_elapsed_secs - 1.5).abs() < 1e-9);
    assert!(!after.has_active_bastion(ship_id));
    assert!(!after.has_active_module(ship_id));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn special_site_ball_spawn_guard_rollback_confirm_and_notify_wakeup() {
    let reg = Arc::new(SystemRegistry::new());
    let sys_id: SolarSystemId = 30_000_142;
    let ball_id: i64 = 700_099;

    // 1. Acquiring the guard enters `Spawning` without marking `Spawned`.
    let g1 = reg
        .begin_special_site_ball_spawn(sys_id, ball_id)
        .await
        .expect("first caller acquires spawn guard");
    assert!(!reg.is_special_site_ball_spawned(sys_id, ball_id));
    assert!(
        !reg.claim_special_site_ball(sys_id, ball_id),
        "legacy claim must not overwrite an active Spawning guard"
    );

    // 2. A concurrent caller waits on `Notify` while `g1` is held, then takes over when `g1` drops unconfirmed.
    let reg_w1 = Arc::clone(&reg);
    let w1 = tokio::spawn(async move {
        reg_w1.begin_special_site_ball_spawn(sys_id, ball_id).await
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    assert!(!w1.is_finished(), "concurrent waiter must block while Spawning guard is held");

    drop(g1);
    let g2 = tokio::time::timeout(std::time::Duration::from_secs(2), w1)
        .await
        .expect("waiter must wake after unconfirmed guard Drop")
        .expect("waiter task succeeds")
        .expect("rolled-back slot must hand off Some(guard) to retrying caller");
    assert!(!reg.is_special_site_ball_spawned(sys_id, ball_id));

    // 3. A second concurrent waiter blocks on `g2` and observes `None` (AlreadySpawned) once `g2.confirm()` runs.
    let reg_w2 = Arc::clone(&reg);
    let w2 = tokio::spawn(async move {
        reg_w2.begin_special_site_ball_spawn(sys_id, ball_id).await
    });
    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    assert!(!w2.is_finished(), "second waiter must block until confirm()");

    g2.confirm();
    assert!(reg.is_special_site_ball_spawned(sys_id, ball_id));
    let w2_res = tokio::time::timeout(std::time::Duration::from_secs(2), w2)
        .await
        .expect("waiter must wake after confirm()")
        .expect("second waiter task succeeds");
    assert!(w2_res.is_none(), "after confirm(), concurrent waiter must observe AlreadySpawned (None)");
    assert!(reg.begin_special_site_ball_spawn(sys_id, ball_id).await.is_none());
}

#[tokio::test]
async fn unified_break_protection_and_force_decloak_return_unique_events() {
    let reg = SystemRegistry::new();
    let sys_id: SolarSystemId = 30_000_142;
    let h = reg.get_or_spawn(sys_id);
    let char_id: CharacterId = 90_000_001;
    let ship_id: BallId = 8801;

    let mut cs = CombatState {
        shield_hp: 123.0,
        max_shield_hp: 500.0,
        capacitor: 321.0,
        max_capacitor: 500.0,
        is_tethered: true,
        ..Default::default()
    };
    cs.apply_jump_cloak_and_invuln_timers(30);
    cs.warp_exit_invulnerable_remaining_ticks = 15;
    h.spawn_ball_with_meta(
        make_actor_ball(ship_id, 587, DVec3::ZERO),
        cs,
        SlimItemMeta {
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // Grant actor/registry-owned location_change_invuln and break simultaneously with undock + warp-exit + tether + gate cloak.
    reg.grant_location_change_invuln(char_id, 5_000);
    let ev = h
        .break_protection(
            ship_id,
            Some(char_id),
            ProtectionBreakReason::TargetLock,
            1_000,
        )
        .await
        .unwrap();
    assert!(ev.gate_cloak_broken);
    assert!(ev.invuln_cancelled);
    assert!(ev.warp_exit_invuln_cancelled);
    assert!(ev.tether_broken);
    assert!(ev.slim_cloak_cleared);
    assert!(ev.should_emit_jump_cloak_cancelled());

    // Idempotent repeat break emits no duplicate cancellation flags.
    let ev_repeat = h
        .break_protection(
            ship_id,
            Some(char_id),
            ProtectionBreakReason::ModuleActivation,
            1_000,
        )
        .await
        .unwrap();
    assert!(!ev_repeat.invuln_cancelled);
    assert!(!ev_repeat.any_cloak_broken());

    // Covert cloak survives WarpInitiation, then ForceDecloak (EssGateEntry) strips cloak cycle atomically
    // without touching non-cloak cycles, HP, or capacitor.
    h.modify_combat_state(ship_id, |c| {
        c.is_cloaked = true;
        c.is_covert_cloak = true;
        c.gate_cloak_remaining_ticks = 0;
    })
    .await
    .unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 9101,
        module_type_id: 11578,
        owner_char_id: Some(char_id),
        effect_name: "cloakingWarpSafe".into(),
        duration_s: 3600.0,
        repeat: -1,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 9102,
        module_type_id: 438,
        owner_char_id: Some(char_id),
        effect_name: "afterburner".into(),
        duration_s: 10.0,
        repeat: 1,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();

    let warp_ev = h
        .break_protection(
            ship_id,
            Some(char_id),
            ProtectionBreakReason::WarpInitiation,
            1_000,
        )
        .await
        .unwrap();
    assert!(!warp_ev.covert_cloak_broken);
    assert!(!warp_ev.should_emit_jump_cloak_cancelled());
    assert!(h.has_module_cycle(ship_id, 9101).await);
    assert!(h.is_ship_actively_cloaked(ship_id).await);

    let forced_ev = h
        .force_decloak(
            ship_id,
            Some(char_id),
            ProtectionBreakReason::EssGateEntry,
            1_000,
        )
        .await
        .unwrap();
    assert!(forced_ev.covert_cloak_broken);
    assert_eq!(forced_ev.removed_cloak_cycles.len(), 1);
    assert_eq!(forced_ev.removed_cloak_cycles[0].module_id, 9101);
    assert!(forced_ev.removed_cloak_cycles[0].is_covert);
    assert!(!h.has_module_cycle(ship_id, 9101).await);
    assert!(h.has_module_cycle(ship_id, 9102).await);
    assert!(!h.is_ship_actively_cloaked(ship_id).await);

    let snap = h.get_protection_snapshot(ship_id).await.unwrap().unwrap();
    assert!(!snap.has_any_cloak());
    assert!(!snap.is_protected(1_000));

    let cs_after = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(!cs_after.has_any_cloak());
    assert!((cs_after.shield_hp - 123.0).abs() < 1e-6);
    assert!((cs_after.capacitor - 321.0).abs() < 1e-6);
    let slim_after = h
        .ballpark
        .read()
        .unwrap()
        .ball_slim_meta
        .get(&ship_id)
        .cloned()
        .unwrap();
    assert!(!slim_after.is_cloaked);
}

#[tokio::test]
async fn protection_add_module_cycle_then_break_from_gate_cloak_and_non_covert_cloak_mechanics() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let reg = SystemRegistry::with_sde_and_db(sde, db.clone());
    let sys_id: SolarSystemId = 30_000_142;
    let h = reg.get_or_spawn(sys_id);

    // 1. F1486-01: `add_module_cycle` zeroing `undock_invulnerable_remaining_ticks = 0` while
    //    `sync_invuln_with_undock_timer = true` must NOT cause `BreakProtection` (`pre_snap: None`)
    //    to skip `deactivate_cloak(ball_id)`, `CloakBall(0)`, `slim_meta.is_cloaked=false`, or `OnJumpCloakCancelled`.
    let ship_gate: BallId = 91_486_001;
    let char_gate: CharacterId = 9_486_001;
    let mut cs_gate = CombatState {
        capacitor: 500.0,
        max_capacitor: 500.0,
        ..Default::default()
    };
    cs_gate.apply_jump_cloak_and_invuln_timers(60);
    h.spawn_ball_with_meta(
        make_actor_ball(ship_gate, 587, DVec3::ZERO),
        cs_gate,
        SlimItemMeta {
            char_id: Some(char_gate),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h.ballpark.write().unwrap().pending_events.clear();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: ship_gate,
        module_id: 71_001,
        owner_char_id: Some(char_gate),
        duration_s: 5.0,
        repeat: 1,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    let cs_mid = h.get_combat_state(ship_gate).await.unwrap().unwrap();
    assert_eq!(cs_mid.undock_invulnerable_remaining_ticks, 0);
    assert_eq!(cs_mid.gate_cloak_remaining_ticks, 60);
    let ev_gate = h
        .break_protection(
            ship_gate,
            Some(char_gate),
            ProtectionBreakReason::ModuleActivation,
            1_000,
        )
        .await
        .unwrap();
    assert!(ev_gate.gate_cloak_broken);
    assert!(ev_gate.slim_cloak_cleared);
    assert!(ev_gate.should_emit_jump_cloak_cancelled());
    let pending = h.ballpark.read().unwrap().pending_events.clone();
    assert!(pending.iter().any(|ev| matches!(
        ev,
        DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == ship_gate
    )));
    assert!(pending.iter().any(|ev| matches!(
        ev,
        DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
            if *ball_id == ship_gate && guid == "effects.Cloak"
    )));
    let slim_gate = h
        .ballpark
        .read()
        .unwrap()
        .ball_slim_meta
        .get(&ship_gate)
        .cloned()
        .unwrap();
    assert!(!slim_gate.is_cloaked);

    // 2. F1486-02: Non-covert Prototype Cloak survives `SubwarpMovement`, breaks atomically on
    //    `WarpInitiation` (stripping cycle + DB `module_active_states` without `OnJumpCloakCancelled`).
    let ship_nc: BallId = 91_486_002;
    let char_nc: CharacterId = 9_486_002;
    let cloak_mod_id: ItemId = 71_002;
    h.spawn_ball_with_meta(
        make_actor_ball(ship_nc, 587, DVec3::new(10_000_000_000.0, 0.0, 0.0)),
        CombatState {
            is_cloaked: true,
            is_covert_cloak: false,
            cloak_velocity_multiplier: Some(0.25),
            undock_invulnerable_remaining_ticks: 15,
            ..Default::default()
        },
        SlimItemMeta {
            char_id: Some(char_nc),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            eve_db::InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11370,
                owner_id: char_nc,
                location_id: ship_nc,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: ship_nc,
        module_id: cloak_mod_id,
        module_type_id: 11370,
        owner_char_id: Some(char_nc),
        effect_name: "cloaking".into(),
        duration_s: 3600.0,
        repeat: -1,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();

    let ev_subwarp = h
        .break_protection(
            ship_nc,
            Some(char_nc),
            ProtectionBreakReason::SubwarpMovement,
            1_000,
        )
        .await
        .unwrap();
    assert!(!ev_subwarp.non_gate_cloak_broken);
    assert!(!ev_subwarp.should_emit_jump_cloak_cancelled());
    assert!(ev_subwarp.removed_cloak_cycles.is_empty());
    assert!(h.has_module_cycle(ship_nc, cloak_mod_id).await);
    assert!(h.is_ship_actively_cloaked(ship_nc).await);
    assert!(db.with_snapshot(|s| s.module_active_states.contains(&cloak_mod_id)));

    h.ballpark.write().unwrap().pending_events.clear();
    let ev_warp = h
        .break_protection(
            ship_nc,
            Some(char_nc),
            ProtectionBreakReason::WarpInitiation,
            1_000,
        )
        .await
        .unwrap();
    assert!(ev_warp.non_gate_cloak_broken);
    assert!(!ev_warp.covert_cloak_broken);
    assert!(!ev_warp.gate_cloak_broken);
    assert!(!ev_warp.should_emit_jump_cloak_cancelled());
    assert_eq!(ev_warp.removed_cloak_cycles.len(), 1);
    assert_eq!(ev_warp.removed_cloak_cycles[0].module_id, cloak_mod_id);
    assert!(!ev_warp.removed_cloak_cycles[0].is_covert);
    assert!(!h.has_module_cycle(ship_nc, cloak_mod_id).await);
    assert!(!h.is_ship_actively_cloaked(ship_nc).await);
    assert!(!db.with_snapshot(|s| s.module_active_states.contains(&cloak_mod_id)));
    assert!(!h.get_protection_snapshot(ship_nc).await.unwrap().unwrap().has_any_cloak());

    // 3. F1486-04: `force_decloak` preserves `reason.clears_invuln()` for non-`Manual` reasons.
    h.modify_combat_state(ship_nc, |c| {
        c.is_cloaked = true;
        c.is_covert_cloak = true;
        c.undock_invulnerable_remaining_ticks = 35;
    })
    .await
    .unwrap();
    let ev_force = h
        .force_decloak(
            ship_nc,
            Some(char_nc),
            ProtectionBreakReason::ModuleActivation,
            1_000,
        )
        .await
        .unwrap();
    assert!(ev_force.covert_cloak_broken);
    assert!(ev_force.invuln_cancelled);
    let snap_force = h.get_protection_snapshot(ship_nc).await.unwrap().unwrap();
    assert!(!snap_force.has_any_cloak());
    assert_eq!(snap_force.snapshot.undock_invulnerable_remaining_ticks, 0);

    // 4. F1486-05: `try_step_full_tick`, `SetSpeedFraction`, `reconcile_disconnected_warp_sync`,
    //    and `apply_damage` destruction keep `protection_registry` synced/evicted without stale entries.
    h.ballpark.write().unwrap().balls.get_mut(&ship_nc).unwrap().speed_fraction = 0.0;
    h.modify_combat_state(ship_nc, |c| c.apply_jump_cloak_and_invuln_timers(45))
        .await
        .unwrap();
    h.ballpark.write().unwrap().ball_slim_meta.get_mut(&ship_nc).unwrap().is_cloaked = true;
    h.try_step_full_tick(1.0);
    assert!(h.get_protection_snapshot(ship_nc).await.unwrap().unwrap().snapshot.has_gate_cloak());

    h.set_speed_fraction(ship_nc, 0.5).await.unwrap();
    let snap_spd = h.get_protection_snapshot(ship_nc).await.unwrap().unwrap();
    assert!(!snap_spd.snapshot.has_gate_cloak());
    assert_eq!(snap_spd.snapshot.undock_invulnerable_remaining_ticks, 0);

    h.ballpark.write().unwrap().balls.get_mut(&ship_nc).unwrap().speed_fraction = 0.0;
    h.modify_combat_state(ship_nc, |c| c.apply_jump_cloak_and_invuln_timers(45))
        .await
        .unwrap();
    h.ballpark.write().unwrap().ball_slim_meta.get_mut(&ship_nc).unwrap().is_cloaked = true;
    let (_, arrived) =
        h.reconcile_disconnected_warp_sync(ship_nc, DVec3::new(10_000_000_000.0, 0.0, 0.0));
    assert!(arrived);
    assert!(!h.ballpark.read().unwrap().ball_slim_meta[&ship_nc].is_cloaked);
    let snap_reconciled = h.get_protection_snapshot(ship_nc).await.unwrap().unwrap();
    assert!(!snap_reconciled.snapshot.has_gate_cloak());
    assert_eq!(snap_reconciled.snapshot.undock_invulnerable_remaining_ticks, 0);

    h.modify_combat_state(ship_nc, |c| {
        c.shield_hp = 1.0;
        c.armor_hp = 1.0;
        c.structure_hp = 1.0;
        c.is_cloaked = true;
    })
    .await
    .unwrap();
    h.try_step_full_tick(1.0);
    assert!(h.has_active_non_gate_cloak_sync(ship_nc));
    let dmg_res = h
        .apply_damage(ship_nc, DamageVector::new(1_000.0, 0.0, 0.0, 0.0))
        .await
        .unwrap()
        .unwrap();
    assert!(dmg_res.1);
    assert!(h.get_protection_snapshot(ship_nc).await.unwrap().is_none());
    assert!(!h.has_active_non_gate_cloak_sync(ship_nc));
}

#[tokio::test]
async fn r4_try_emit_destiny_remove_cycle_and_db_ball_eviction_preserve_sync_and_lock_ranks() {
    let reg = SystemRegistry::new();
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let (sys_id, other_sys_id): (SolarSystemId, SolarSystemId) = (30_000_142, 30_000_144);
    let (h, other_h) = (reg.get_or_spawn(sys_id), reg.get_or_spawn(other_sys_id));
    celestials::register_db_system_ballpark(&db, sys_id, &h);
    let (s_emit, s_cyc, s_db1, s_db2, s_db3) = (9801, 9802, 9803, 9804, 9805);
    for (bid, gate) in [(s_emit, true), (s_cyc, false), (s_db1, true), (s_db2, true), (s_db3, true)] {
        let mut cs = CombatState::default();
        if gate { cs.apply_jump_cloak_and_invuln_timers(45); }
        h.spawn_ball_with_meta(
            make_actor_ball(bid, 587, DVec3::ZERO), cs,
            SlimItemMeta { char_id: Some(90_000_100 + bid), is_cloaked: gate, ..Default::default() },
        ).await.unwrap();
    }
    // 1. F1486-R4-01: same-thread `try_emit_destiny_event` with `actor_state`, `ballpark`, and `RANK_LEAF` locks held.
    {
        let _st = h.actor_state.write().unwrap();
        assert!(h.try_emit_destiny_event(s_emit, DestinyEvent::SetSpeedFraction { ball_id: s_emit, fraction: 0.5 }));
    }
    let _ = h.query_current_tick().await.unwrap();
    assert!((h.ballpark.read().unwrap().balls[&s_emit].speed_fraction - 0.5).abs() < 1e-6);
    {
        let _bp = h.ballpark.read().unwrap();
        let leaf = eve_db::lock_rank::RankedMutex::<(), { eve_db::lock_rank::RANK_LEAF }>::new(());
        let _leaf = leaf.lock().unwrap();
        assert!(h.try_emit_destiny_event(s_emit, DestinyEvent::Stop { ball_id: s_emit }));
    }
    let _ = h.query_current_tick().await.unwrap();
    assert!(matches!(h.ballpark.read().unwrap().balls[&s_emit].mode, MovementMode::Stop));

    // 2. F1486-R4-02a: `try_remove_module_cycle` updates `protection_registry` + `has_active_non_gate_cloak_sync`.
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: s_cyc, module_id: 99_001, module_type_id: 11578, effect_name: "cloakingWarpSafe".into(),
        owner_char_id: Some(90_009_802), duration_s: 5.0, elapsed_s: 0.0, repeat: 1, ..Default::default()
    }).await.unwrap();
    h.modify_combat_state(s_cyc, |c| { c.is_covert_cloak = true; c.cloak_velocity_multiplier = Some(0.25); }).await.unwrap();
    assert!(h.has_active_non_gate_cloak_sync(s_cyc));
    assert!(h.actor_state.read().unwrap().protection_registry[&s_cyc].has_active_cloak_cycle);
    let _ = h.try_remove_module_cycle(s_cyc, 99_001);
    let snap_cyc = h.actor_state.read().unwrap().protection_registry[&s_cyc];
    assert!(!snap_cyc.has_active_cloak_cycle && !snap_cyc.snapshot.has_cloak_velocity_multiplier && !snap_cyc.snapshot.is_covert_cloak);
    assert!(!h.has_active_non_gate_cloak_sync(s_cyc));

    // 3. F1486-R4-02b: `try_remove_ball_for_db` evicts `protection_registry` outside and inside held locks.
    for bid in [s_db1, s_db2, s_db3] { assert!(h.actor_state.read().unwrap().protection_registry.contains_key(&bid)); }
    try_remove_ball_for_db(&db, sys_id, s_db1);
    {
        let _bp_other = other_h.ballpark.write().unwrap();
        try_remove_ball_for_db(&db, sys_id, s_db2);
        let _st_other = other_h.actor_state.write().unwrap();
        try_remove_ball_for_db(&db, sys_id, s_db3);
    }
    let (bp_r, st_r) = (h.ballpark.read().unwrap(), h.actor_state.read().unwrap());
    for bid in [s_db1, s_db2, s_db3] {
        assert!(!bp_r.balls.contains_key(&bid) && !st_r.protection_registry.contains_key(&bid));
    }
}

#[tokio::test]
async fn protection_registry_syncs_across_remove_step_manual_disconnected_warp_and_destiny_event() {
    let reg = SystemRegistry::new();
    let h = reg.get_or_spawn(30_000_142);

    // 1. `try_remove_ball` and `try_remove_destroyed_ball` evict `protection_registry`.
    let (ship_a, ship_b, ship_c, ship_d, ship_e, ship_f) = (95_001, 95_002, 95_003, 95_004, 95_005, 95_006);
    h.spawn_ball_with_combat(make_actor_ball(ship_a, 587, DVec3::ZERO), CombatState { undock_invulnerable_remaining_ticks: 15, ..Default::default() }).await.unwrap();
    h.spawn_ball_with_combat(make_actor_ball(ship_b, 587, DVec3::new(500.0, 0.0, 0.0)), CombatState { is_cloaked: true, cloak_velocity_multiplier: Some(0.1), ..Default::default() }).await.unwrap();
    assert!(h.get_protection_snapshot(ship_a).await.unwrap().is_some() && h.has_active_non_gate_cloak_sync(ship_b));

    h.try_remove_ball(ship_a);
    assert!(h.actor_state.read().unwrap().protection_registry.get(&ship_a).is_none());
    assert!(h.get_protection_snapshot(ship_a).await.unwrap().is_none());

    h.try_remove_destroyed_ball(ship_b);
    assert!(h.actor_state.read().unwrap().protection_registry.get(&ship_b).is_none());
    assert!(!h.has_active_non_gate_cloak_sync(ship_b) && h.get_protection_snapshot(ship_b).await.unwrap().is_none());

    // 2. `try_step_manual` steps and syncs `protection_registry`.
    h.spawn_ball_with_combat(
        Ball { speed_fraction: 0.0, ..make_actor_ball(ship_c, 587, DVec3::new(1_000.0, 0.0, 0.0)) },
        CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 2, undock_invulnerable_remaining_ticks: 2, ..Default::default() },
    ).await.unwrap();
    h.try_step_manual(1.0);
    let snap_c1 = h.actor_state.read().unwrap().protection_registry[&ship_c];
    assert_eq!((snap_c1.snapshot.gate_cloak_remaining_ticks, snap_c1.snapshot.undock_invulnerable_remaining_ticks), (1, 1));
    h.try_step_manual(1.0);
    let snap_c2 = h.actor_state.read().unwrap().protection_registry[&ship_c];
    assert_eq!((snap_c2.snapshot.gate_cloak_remaining_ticks, snap_c2.snapshot.undock_invulnerable_remaining_ticks, snap_c2.snapshot.is_cloaked), (0, 0, false));

    // 3. `prepare_disconnected_warp_sync` clears gate cloak/invuln via `set_mode` and syncs registry.
    h.spawn_ball_with_combat(make_actor_ball(ship_d, 587, DVec3::ZERO), CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 20, undock_invulnerable_remaining_ticks: 20, ..Default::default() }).await.unwrap();
    assert!(h.actor_state.read().unwrap().protection_registry[&ship_d].snapshot.has_gate_cloak());
    h.prepare_disconnected_warp_sync(ship_d, 587, 40.0, 1_200_000.0, 2.5, 350.0, 3.0, DVec3::ZERO, DVec3::new(1_000_000.0, 0.0, 0.0));
    let snap_d = h.actor_state.read().unwrap().protection_registry[&ship_d];
    assert!(!snap_d.snapshot.has_gate_cloak() && !snap_d.snapshot.is_cloaked && snap_d.snapshot.undock_invulnerable_remaining_ticks == 0);

    // 4. `SystemCommand::EmitDestinyEvent` (`SetSpeedFraction`) breaks protection and syncs `protection_registry`.
    h.spawn_ball_with_combat(
        Ball { speed_fraction: 0.0, ..make_actor_ball(ship_e, 587, DVec3::new(2_000.0, 0.0, 0.0)) },
        CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 30, undock_invulnerable_remaining_ticks: 30, ..Default::default() },
    ).await.unwrap();
    assert!(h.get_protection_snapshot(ship_e).await.unwrap().unwrap().snapshot.has_gate_cloak());
    assert!(h.emit_destiny_event(ship_e, DestinyEvent::SetSpeedFraction { ball_id: ship_e, fraction: 0.5 }).await);
    let snap_e = h.get_protection_snapshot(ship_e).await.unwrap().unwrap().snapshot;
    assert!(!snap_e.has_gate_cloak() && !snap_e.is_cloaked && snap_e.undock_invulnerable_remaining_ticks == 0);

    // 5. `cleaned_stale` in `activate_module_cycle_atomic` (even on err) and `reconcile_module_cycle_before_activate` clear `has_active_cloak_cycle`.
    h.spawn_ball_with_meta(
        make_actor_ball(ship_f, 587, DVec3::new(3_000.0, 0.0, 0.0)),
        CombatState { capacitor: 1.0, max_capacitor: 100.0, ..Default::default() },
        SlimItemMeta { char_id: Some(95_906), owner_id: 95_906, category_id: 6, group_id: 25, ..Default::default() },
    ).await.unwrap();
    let stale_cycle = |module_id, repeat, elapsed_s, cap_need_per_cycle| ActiveModuleCycle {
        ship_id: ship_f, module_id, module_type_id: 11_370, owner_char_id: Some(95_906),
        effect_name: "cloaking".to_string(), repeat, duration_s: 1.0, elapsed_s, cap_need_per_cycle,
        started_at_unix: 1_000, ..ActiveModuleCycle::default()
    };
    h.add_module_cycle(stale_cycle(995_001, 0, 1.5, 0.0)).await.unwrap();
    assert!(h.get_protection_snapshot(ship_f).await.unwrap().unwrap().has_active_cloak_cycle);
    let err = h.activate_module_cycle_atomic(AtomicActivateCycleRequest {
        char_id: 95_906, cycle: stale_cycle(995_001, 1, 0.0, 500.0),
        allow_while_warping: false, allow_while_cloaked: true, replace_existing_cycle: false, break_protection: true,
        break_reason: ProtectionBreakReason::ModuleActivation, real_now_unix: 1_700_000_000,
    }).await.unwrap_err();
    assert!(err.contains("Insufficient capacitor"), "got unexpected err: {err}");
    assert!(!h.get_protection_snapshot(ship_f).await.unwrap().unwrap().has_active_cloak_cycle);

    h.add_module_cycle(stale_cycle(995_002, 0, 1.5, 0.0)).await.unwrap();
    assert!(h.get_protection_snapshot(ship_f).await.unwrap().unwrap().has_active_cloak_cycle);
    let (existing, cleaned) = h.reconcile_module_cycle_before_activate(ship_f, 995_002, 1_700_000_000).await;
    assert!(existing.is_none() && cleaned);
    assert!(!h.get_protection_snapshot(ship_f).await.unwrap().unwrap().has_active_cloak_cycle);

    // 6. All 5 synchronous SystemHandle mutators keep `protection_registry` and `has_active_non_gate_cloak_sync` synced.
    let (ship_g, ship_h) = (95_007, 95_008);
    h.try_spawn_ball_with_meta(
        make_actor_ball(ship_g, 587, DVec3::new(4_000.0, 0.0, 0.0)),
        CombatState { is_cloaked: true, gate_cloak_remaining_ticks: 20, undock_invulnerable_remaining_ticks: 20, ..Default::default() },
        SlimItemMeta { char_id: Some(95_907), owner_id: 95_907, is_cloaked: true, ..Default::default() },
    );
    assert_eq!(h.actor_state.read().unwrap().protection_registry[&ship_g].owner_char_id, Some(95_907));
    assert!(h.actor_state.read().unwrap().protection_registry[&ship_g].snapshot.has_gate_cloak());
    assert!(h.try_emit_destiny_event(ship_g, DestinyEvent::Stop { ball_id: ship_g }));
    let snap_g = h.actor_state.read().unwrap().protection_registry[&ship_g].snapshot;
    assert!(!snap_g.has_gate_cloak() && snap_g.undock_invulnerable_remaining_ticks == 0);

    h.try_spawn_ball_with_combat(
        make_actor_ball(ship_h, 587, DVec3::new(5_000.0, 0.0, 0.0)),
        CombatState { is_cloaked: true, cloak_velocity_multiplier: Some(0.1), ..Default::default() },
    );
    assert!(h.has_active_non_gate_cloak_sync(ship_h));
    assert!(h.try_update_combat_state(ship_h, |cs| { cs.is_cloaked = false; cs.cloak_velocity_multiplier = None; }));
    assert!(!h.has_active_non_gate_cloak_sync(ship_h));
    assert!(h.try_recompute_combat_attributes(ship_h, |cs| { cs.is_cloaked = true; cs.gate_cloak_remaining_ticks = 15; }));
    assert!(h.actor_state.read().unwrap().protection_registry[&ship_h].snapshot.has_gate_cloak());
    let ((tx1, rx1), (tx2, rx2), h2) = (std::sync::mpsc::channel(), std::sync::mpsc::channel(), h.clone());
    let holder = std::thread::spawn(move || { let _g = h2.actor_state.write().unwrap(); let _ = tx1.send(()); let _ = rx2.recv(); });
    rx1.recv().unwrap();
    assert!(h.try_emit_destiny_event(ship_h, DestinyEvent::SetSpeedFraction { ball_id: ship_h, fraction: 0.5 }));
    assert_eq!(h.ballpark.read().unwrap().combat_states[&ship_h].gate_cloak_remaining_ticks, 15);
    let _ = tx2.send(());
    holder.join().unwrap();
    assert!(!h.get_protection_snapshot(ship_h).await.unwrap().unwrap().snapshot.has_gate_cloak());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn protection_ledger_scope_sequence_refresh_and_p2_registry_invariants() {
    let reg = SystemRegistry::new();
    let (sys_a, sys_b): (SolarSystemId, SolarSystemId) = (30_000_142, 30_000_144);
    let (h_a, h_b) = (reg.get_or_spawn(sys_a), reg.get_or_spawn(sys_b));
    let (char_a, char_b, ship_id) = (91_486_201_i64, 91_486_202_i64, 1_091_486_201_i64);
    let now = 1_000_u64;

    // 1. Spawn in System A, jump via TakeBall to System B, grant System B invuln:
    //    stale System A BreakProtection & ForceDecloak (and mismatched char_id) must be no-ops.
    h_a.spawn_ball_with_meta(
        make_actor_ball(ship_id, 587, DVec3::ZERO),
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 20,
            undock_invulnerable_remaining_ticks: 20,
            ..Default::default()
        },
        SlimItemMeta {
            char_id: Some(char_a),
            owner_id: char_a,
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let taken = h_a.take_ball(ship_id).await.unwrap();
    h_b.spawn_ball_with_meta(
        taken.ball,
        CombatState {
            is_cloaked: true,
            gate_cloak_remaining_ticks: 25,
            undock_invulnerable_remaining_ticks: 25,
            ..Default::default()
        },
        SlimItemMeta {
            char_id: Some(char_a),
            owner_id: char_a,
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let t_b = now + 90;
    let seq_b1 = reg.grant_location_change_invuln_in_system(char_a, Some(sys_b), t_b);

    let stale_break_a = h_a
        .break_protection(ship_id, Some(char_a), ProtectionBreakReason::ModuleActivation, now)
        .await
        .unwrap();
    assert!(!stale_break_a.invuln_cancelled && !stale_break_a.any_cloak_broken());
    let stale_decloak_a = h_a
        .force_decloak(ship_id, Some(char_a), ProtectionBreakReason::WarpInitiation, now)
        .await
        .unwrap();
    assert!(!stale_decloak_a.invuln_cancelled && !stale_decloak_a.any_cloak_broken());
    assert_eq!(reg.location_change_invuln_until(char_a, now), Some(t_b));
    assert_eq!(
        h_b.get_protection_snapshot(ship_id)
            .await
            .unwrap()
            .unwrap()
            .location_change_invuln_until_unix,
        Some(t_b)
    );

    // Mismatched explicit char_id (`char_b` vs live owner `char_a`) also leaves both grants untouched.
    reg.grant_location_change_invuln_in_system(char_b, Some(sys_b), now + 70);
    let mismatch_ev = h_b
        .break_protection(ship_id, Some(char_b), ProtectionBreakReason::TargetLock, now)
        .await
        .unwrap();
    assert!(!mismatch_ev.invuln_cancelled && !mismatch_ev.gate_cloak_broken);
    assert_eq!(reg.location_change_invuln_until(char_a, now), Some(t_b));
    assert_eq!(reg.location_change_invuln_until(char_b, now), Some(now + 70));

    // P2: `is_target_location_invulnerable` works even with `RANK_BALLPARK` held on the thread.
    {
        let _bp_read = h_b.ballpark.read().unwrap();
        assert!(reg.is_target_location_invulnerable(Some(sys_b), ship_id, None, now));
    }

    // Clear Section 1's System B gate/undock timers and `t_b` cleanly before testing sequence fencing.
    let ev_clear_b1 = h_b
        .break_protection(ship_id, Some(char_a), ProtectionBreakReason::ModuleActivation, now)
        .await
        .unwrap();
    assert!(ev_clear_b1.gate_cloak_broken && ev_clear_b1.undock_invuln_cancelled && ev_clear_b1.invuln_cancelled);
    assert_eq!(reg.location_change_invuln_until(char_a, now), None);

    // 2. Sequence fence: pre-regrant future & stale explicit sequence cannot cancel a newer grant.
    let seq_b1_fresh = reg.grant_location_change_invuln_in_system(char_a, Some(sys_b), t_b);
    let stale_unpolled = h_b.break_protection(
        ship_id,
        Some(char_a),
        ProtectionBreakReason::TargetLock,
        now,
    );
    let t_b2 = now + 180;
    let seq_b2 = reg.grant_location_change_invuln_in_system(char_a, Some(sys_b), t_b2);
    assert!(seq_b2 > seq_b1_fresh && seq_b1_fresh > seq_b1);
    let ev_stale_future = stale_unpolled.await.unwrap();
    assert!(!ev_stale_future.invuln_cancelled);
    assert_eq!(reg.location_change_invuln_until(char_a, now), Some(t_b2));
    assert_eq!(
        h_b.get_protection_snapshot(ship_id)
            .await
            .unwrap()
            .unwrap()
            .location_change_invuln_until_unix,
        Some(t_b2)
    );

    let ev_stale_decloak = h_b
        .force_decloak_at_seq(
            ship_id,
            Some(char_a),
            ProtectionBreakReason::ModuleActivation,
            now,
            Some(seq_b1_fresh),
        )
        .await
        .unwrap();
    assert!(!ev_stale_decloak.invuln_cancelled);
    assert_eq!(reg.location_change_invuln_until(char_a, now), Some(t_b2));

    // Current-sequence break clears `t_b2` once and updates System B protection_registry.
    let ev_cur = h_b
        .break_protection(ship_id, Some(char_a), ProtectionBreakReason::ModuleActivation, now)
        .await
        .unwrap();
    assert!(ev_cur.invuln_cancelled);
    assert_eq!(reg.location_change_invuln_until(char_a, now), None);
    assert!(
        h_b.get_protection_snapshot(ship_id)
            .await
            .unwrap()
            .unwrap()
            .location_change_invuln_until_unix
            .is_none()
    );

    // 3. F1486-T3A-02: `refresh_loaded_protection_registries` respects lock order and system scope
    //    even if a stale ship entry exists under `h_a`.
    let other_ship_in_a = 1_091_486_209_i64;
    h_a.spawn_ball_with_meta(
        make_actor_ball(other_ship_in_a, 587, DVec3::new(1_000.0, 0.0, 0.0)),
        CombatState::default(),
        SlimItemMeta {
            char_id: Some(char_a),
            owner_id: char_a,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    reg.grant_location_change_invuln_in_system(char_a, Some(sys_b), now + 240);
    assert!(
        h_a.get_protection_snapshot(other_ship_in_a)
            .await
            .unwrap()
            .unwrap()
            .location_change_invuln_until_unix
            .is_none()
    );
    assert_eq!(
        h_b.get_protection_snapshot(ship_id)
            .await
            .unwrap()
            .unwrap()
            .location_change_invuln_until_unix,
        Some(now + 240)
    );
}
