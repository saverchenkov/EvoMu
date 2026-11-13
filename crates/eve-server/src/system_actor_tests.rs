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
    assert!(
        tick_set_state > tick_before,
        "SetState drain must advance ballpark tick ({tick_set_state} > {tick_before}) to prevent Park.FlushState stamp collision"
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
