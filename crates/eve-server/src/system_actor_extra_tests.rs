//! Additional unit and mutation-killing tests for `system_actor`.

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{
    Ball, CombatState, DestinyEvent, MovementMode, SlimItemMeta, TargetLock,
};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::sync::Arc;

fn make_ball(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 45.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn test_next_downtime_unix_secs_and_registry_configuration_boundaries() {
    // 1. Exact phase equality (rem == phase) must advance by full period (> now_unix)
    assert_eq!(
        next_downtime_unix_secs(39_600, 86_400, 39_600),
        39_600 + 86_400
    );
    // Just 1s before downtime
    assert_eq!(next_downtime_unix_secs(39_599, 86_400, 39_600), 39_600);
    // Just 1s after downtime
    assert_eq!(
        next_downtime_unix_secs(39_601, 86_400, 39_600),
        39_600 + 86_400
    );
    // Zero interval_secs clamps period to 1
    assert_eq!(next_downtime_unix_secs(100, 0, 50), 101);

    // 2. SystemRegistry builders, interval clamping, downtime modulo, and claim sets
    let reg = SystemRegistry::default();
    assert_eq!(
        reg.belt_respawn_interval_secs(),
        DEFAULT_BELT_RESPAWN_INTERVAL_SECS
    );
    assert_eq!(
        reg.downtime_utc_secs_of_day(),
        DEFAULT_DOWNTIME_UTC_SECS_OF_DAY
    );

    reg.set_belt_respawn_interval_secs(0);
    assert_eq!(reg.belt_respawn_interval_secs(), 1);
    reg.set_belt_respawn_interval_secs(7_200);
    assert_eq!(reg.belt_respawn_interval_secs(), 7_200);
    reg.set_belt_respawn_interval_secs(999_999);
    assert_eq!(reg.belt_respawn_interval_secs(), 604_800);

    reg.set_downtime_utc_secs_of_day(86_400);
    assert_eq!(reg.downtime_utc_secs_of_day(), 0);
    reg.set_downtime_utc_secs_of_day(86_415);
    assert_eq!(reg.downtime_utc_secs_of_day(), 15);

    // Claim special-site ball & wormhole ore anomaly hydration idempotency
    assert!(reg.claim_special_site_ball(30_000_142, 990_001));
    assert!(!reg.claim_special_site_ball(30_000_142, 990_001));
    assert!(reg.claim_special_site_ball(30_000_142, 990_002));
    assert!(reg.claim_special_site_ball(30_000_144, 990_001));

    assert!(reg.claim_wh_ore_anomaly_hydration(31_000_001, "WH-ORE-1"));
    assert!(!reg.claim_wh_ore_anomaly_hydration(31_000_001, "WH-ORE-1"));
    assert!(reg.claim_wh_ore_anomaly_hydration(31_000_001, "WH-ORE-2"));

    // Unspawned system sync queries return safe defaults
    assert_eq!(reg.get_ball_position_sync(30_000_142, 1), None);
    assert!(!reg.is_ball_scrambled_sync(30_000_142, 1));
    assert!(!reg.has_active_modules_or_cloak_sync(30_000_142, 1));
    assert_eq!(reg.find_ball_position_sync(1), None);
    assert_eq!(reg.find_ball_system_sync(1), None);
    assert_eq!(reg.get_system_sim_elapsed(30_000_142), 0.0);
    assert!(reg.take_pending_cycle_actions(30_000_142).is_empty());
    assert_eq!(reg.active_systems_count(), 0);
}

#[tokio::test]
async fn test_system_registry_multi_system_sync_queries_tidi_and_hooks() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let (notif_tx, _) = tokio::sync::broadcast::channel(16);
    let pending_notifs = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new()));
    let tactical = Arc::new(eve_db::lock_rank::RankedRwLock::new(Default::default()));
    let rng = Arc::new(crate::rng::MechanicRng::seeded(42));

    let reg = SystemRegistry::with_sde_and_db(sde, db)
        .with_notifications(pending_notifs, notif_tx)
        .with_tactical_state(tactical)
        .with_mechanic_rng(rng);

    // Configure manual TiDi override = 0.5 for Jita before spawning
    reg.cluster_nodes
        .write()
        .unwrap()
        .manual_tidi_overrides
        .insert(30_000_142, 0.5);

    let step_records = Arc::new(std::sync::Mutex::new(Vec::<(u32, f64)>::new()));
    let step_clone = Arc::clone(&step_records);
    reg.set_system_step_hook(Arc::new(move |sys_id, dt_eff, _destroyed| {
        let sc = Arc::clone(&step_clone);
        Box::pin(async move {
            sc.lock().unwrap().push((sys_id, dt_eff));
        })
    }));

    let lock_records = Arc::new(std::sync::Mutex::new(Vec::<(i64, i64)>::new()));
    let lock_clone = Arc::clone(&lock_records);
    reg.set_target_lock_completed_hook(Arc::new(move |cid, tid| {
        lock_clone.lock().unwrap().push((cid, tid));
    }));

    let deact_records = Arc::new(std::sync::Mutex::new(Vec::<(i64, i64)>::new()));
    let deact_clone = Arc::clone(&deact_records);
    reg.set_module_deactivation_hook(Arc::new(move |_sys_id, cid, cyc| {
        deact_clone.lock().unwrap().push((cid, cyc.module_id));
    }));

    let h_perim = reg.get_or_spawn(30_000_144);
    let h_jita = reg.get_or_spawn(30_000_142);
    assert_eq!(reg.active_systems_count(), 2);
    let handles = reg.active_system_handles();
    assert_eq!(handles[0].0, 30_000_142);
    assert_eq!(handles[1].0, 30_000_144);

    // Spawn ball 501 in Jita with owner char_id = 90000001 and ball 502 in Perimeter
    h_jita
        .spawn_ball_with_meta(
            make_ball(501, 587, DVec3::new(1_000.0, 2_000.0, 3_000.0)),
            CombatState {
                shield_hp: 500.0,
                max_shield_hp: 500.0,
                capacitor: 500.0,
                max_capacitor: 500.0,
                warp_scramble_strength: 2,
                warp_core_strength: 0,
                ..CombatState::default()
            },
            SlimItemMeta {
                group_id: 25,
                category_id: 6,
                owner_id: 90_000_001,
                char_id: Some(90_000_001),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    h_jita
        .spawn_ball_with_combat(
            make_ball(502, 587, DVec3::new(1_500.0, 2_000.0, 3_000.0)),
            CombatState::default(),
        )
        .await
        .unwrap();
    h_perim
        .spawn_ball(make_ball(601, 587, DVec3::new(-5_000.0, 0.0, 0.0)))
        .await
        .unwrap();

    // Verify cross-system lookup helpers
    assert_eq!(
        reg.get_ball_position_sync(30_000_142, 501),
        Some(DVec3::new(1_000.0, 2_000.0, 3_000.0))
    );
    assert!(reg.is_ball_scrambled_sync(30_000_142, 501));
    assert!(!reg.is_ball_scrambled_sync(30_000_142, 502));
    assert_eq!(reg.find_ball_system_sync(501), Some(30_000_142));
    assert_eq!(reg.find_ball_system_sync(601), Some(30_000_144));
    assert_eq!(
        reg.find_ball_position_sync(601),
        Some(DVec3::new(-5_000.0, 0.0, 0.0))
    );

    // Queue a pending target lock (remaining_lock_ms = 500ms) and step 2.0s at TiDi 0.5 -> 1.0s sim time
    h_jita
        .add_target_lock(
            501,
            TargetLock {
                target_id: 502,
                lock_time_ms: 500.0,
                remaining_lock_ms: 500.0,
                locked: false,
                max_range_m: None,
            },
        )
        .await
        .unwrap();
    let _ = h_jita.step_manual(2.0).await.unwrap();
    assert!((reg.get_system_sim_elapsed(30_000_142) - 1.0).abs() < 1e-6);
    assert!(step_records
        .lock()
        .unwrap()
        .iter()
        .any(|&(sys, dt)| sys == 30_000_142 && (dt - 1.0).abs() < 1e-6));
    assert!(lock_records.lock().unwrap().contains(&(90_000_001, 502)));
}

#[tokio::test]
async fn test_actor_commands_docking_cloak_tether_ewar_immune_nanite_and_errors() {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let reg = SystemRegistry::with_sde(sde);
    let h = reg.get_or_spawn(30_000_144);

    // Error branches when ball is not in system
    assert!(h.set_movement_mode(999_999, MovementMode::Stop).await.is_err());
    assert!(h.set_speed_fraction(999_999, 0.5).await.is_err());
    assert!(h.push_set_state(999_999).await.is_err());
    assert!(h.get_set_state(999_999).await.is_err());
    assert!(h.set_warp_scramble_strength(999_999, 1).await.is_err());
    assert!(h.set_warp_core_strength(999_999, 1).await.is_err());
    assert!(!h.emit_packaged_actions(999_999, Vec::new()).await);

    // Spawn ship 701 (cloaked + undock invuln + tethered) and ship 702 (tethered)
    h.spawn_ball_with_meta(
        make_ball(701, 587, DVec3::ZERO),
        CombatState {
            is_cloaked: true,
            is_tethered: true,
            undock_invulnerable_remaining_ticks: 10,
            ..CombatState::default()
        },
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            owner_id: 90_000_001,
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h.spawn_ball_with_combat(
        make_ball(702, 587, DVec3::new(2_000.0, 0.0, 0.0)),
        CombatState {
            is_tethered: true,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();

    // BreakGateCloak clears cloak in ballpark and slim_meta
    assert!(h.break_gate_cloak(701).await.unwrap());
    assert!(!h.get_combat_state(701).await.unwrap().unwrap().is_cloaked);

    // Locking a tethered target (702) is rejected
    let tether_err = h
        .add_target_lock(
            701,
            TargetLock {
                target_id: 702,
                lock_time_ms: 1_000.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .unwrap_err();
    assert!(tether_err.contains("tethered"));

    // Untether 702 via set_combat_state; locking 702 from 701 clears 701's tether and undock invulnerability
    h.set_combat_state(702, CombatState::default()).await.unwrap();
    h.add_target_lock(
        701,
        TargetLock {
            target_id: 702,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        },
    )
    .await
    .unwrap();
    let cs_701 = h.get_combat_state(701).await.unwrap().unwrap();
    assert!(!cs_701.is_tethered);
    assert_eq!(cs_701.undock_invulnerable_remaining_ticks, 0);
    assert_eq!(h.clear_target_locks(701).await.unwrap(), 1);

    // EWAR immunity blocks ApplyEcmJam
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: 702,
        module_id: 8001,
        duration_s: 10.0,
        repeat: 1,
        self_ewar_immune: true,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    assert!(reg.has_active_modules_or_cloak_sync(30_000_144, 702));
    assert!(!h.apply_ecm_jam(702, 5.0).await.unwrap());

    // Nanite repair: elapsed_s == 0 refunds all paste; partial elapsed_s refunds unused paste
    h.start_nanite_repair(ActiveNaniteRepair {
        char_id: 90_000_001,
        ship_id: 701,
        module_id: 9001,
        initial_damage_hp: 20.0,
        paste_total_needed: 4,
        paste_consumed_so_far: 4,
        duration_s: 10.0,
        elapsed_s: 0.0,
    })
    .await
    .unwrap();
    let (rem_dmg, refund) = h.stop_nanite_repair(701, 9001).await.unwrap().unwrap();
    assert!((rem_dmg - 20.0).abs() < 1e-6);
    assert_eq!(refund, 4);
    assert!(h.stop_nanite_repair(701, 9001).await.unwrap().is_none());

    h.start_nanite_repair(ActiveNaniteRepair {
        char_id: 90_000_001,
        ship_id: 701,
        module_id: 9002,
        initial_damage_hp: 20.0,
        paste_total_needed: 4,
        paste_consumed_so_far: 4,
        duration_s: 10.0,
        elapsed_s: 2.0,
    })
    .await
    .unwrap();
    assert_eq!(reg.cancel_nanite_repairs_for_ship_sync(701).len(), 1);

    // RecordIncomingArmorDamage, InterdictionSphere, MobileWarpDisruptor, EmitPackagedActions, AcceptDocking
    h.record_incoming_armor_damage(701, [10.0, 20.0, 30.0, 40.0])
        .await
        .unwrap();
    assert!(h.add_interdiction_sphere(8501, DVec3::ZERO, 20_000.0).await.unwrap());
    assert!(h.add_mobile_warp_disruptor(8502, DVec3::ZERO, 20_000.0).await.unwrap());
    assert!(
        h.emit_packaged_actions(
            701,
            vec![DestinyEvent::Stop { ball_id: 701 }]
        )
        .await
    );

    // PopulateAsteroids in Perimeter (30000144)
    let populated = h.populate_asteroid_belt_entities().await.unwrap();
    assert!(!populated.is_empty());

    // AcceptDocking at an unspawned structure position and at a known station
    let docked = h
        .accept_docking(701, 60_999_999, DVec3::new(100.0, 200.0, 300.0))
        .await;
    assert!(docked.is_some());
}

#[tokio::test]
async fn flush_peer_observer_updates_broadcasts_immediately_and_preserves_pending_set_state() {
    let reg = SystemRegistry::new();
    let h = reg.get_or_spawn(30_000_142);

    let acting_ship = 9_101;
    let ready_peer = 9_102;
    let joining_peer = 9_103;

    h.spawn_ball(make_ball(acting_ship, 587, DVec3::ZERO))
        .await
        .unwrap();
    h.spawn_ball(make_ball(ready_peer, 587, DVec3::new(1_000.0, 0.0, 0.0)))
        .await
        .unwrap();
    h.spawn_ball(make_ball(joining_peer, 587, DVec3::new(2_000.0, 0.0, 0.0)))
        .await
        .unwrap();

    let _ = h.drain_observer_updates(acting_ship).await.unwrap();
    let _ = h.drain_observer_updates(ready_peer).await.unwrap();
    let _ = h.drain_observer_updates(joining_peer).await.unwrap();
    h.push_set_state(joining_peer).await.unwrap();

    // Negative case: with 0 live broadcast subscribers, flush_peer_observer_updates is a no-op
    // so direct drain_observer_updates callers keep their events.
    h.set_movement_mode(
        acting_ship,
        MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        h.flush_peer_observer_updates(Some(acting_ship))
            .await
            .unwrap(),
        0
    );
    let (_, direct_evs, _) = h.drain_observer_updates(ready_peer).await.unwrap();
    assert!(direct_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::GotoDirection { ball_id: 9_101, .. })));
    let _ = h.drain_observer_updates(acting_ship).await.unwrap();

    // Positive case: with an active subscriber (`macho_listener`), peer updates broadcast immediately
    // without advancing the 1 Hz tick, excluding `acting_ship` and preserving `joining_peer`'s SetState queue.
    let mut rx = h.subscribe();
    h.set_movement_mode(
        acting_ship,
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(1.5e11, 0.0, 0.0), 3.0),
    )
    .await
    .unwrap();

    let flushed = h
        .flush_peer_observer_updates(Some(acting_ship))
        .await
        .unwrap();
    assert_eq!(flushed, 1);

    let msg = rx.try_recv().expect("immediate sub-tick TickBroadcast");
    assert_eq!(msg.solar_system_id, 30_000_142);
    assert!(!msg.observer_events.contains_key(&acting_ship));
    assert!(!msg.observer_events.contains_key(&joining_peer));
    let peer_evs = msg
        .observer_events
        .get(&ready_peer)
        .expect("ready_peer updates included");
    assert!(peer_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 9_101, .. })));
    assert!(msg.observer_updates_pyrep.contains_key(&ready_peer));

    // Acting pilot still drains its own authoritative WarpTo on its RPC reply path
    let (_, caller_evs, _) = h.drain_observer_updates(acting_ship).await.unwrap();
    assert!(caller_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 9_101, .. })));

    // Joining peer's SetState at index 0 is intact along with the subsequent WarpTo
    let (_, join_evs, _) = h.drain_observer_updates(joining_peer).await.unwrap();
    assert!(matches!(
        join_evs.first(),
        Some(DestinyEvent::SetState {
            observer_ball_id: 9_103
        })
    ));
    assert!(join_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 9_101, .. })));

    // flush_peer_observer_updates(None) flushes all ready observers without exclusions
    h.set_movement_mode(acting_ship, MovementMode::Stop)
        .await
        .unwrap();
    let flushed_all = h.flush_peer_observer_updates(None).await.unwrap();
    assert!(flushed_all >= 2);
}

#[tokio::test]
async fn test_system_actor_persisted_asteroids_damage_nanite_partial_ecm_and_commands() {
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    // Seed first SDE belt asteroid ID for Perimeter (30_000_144 -> 750_000_000) so get_or_spawn hydrates belts
    db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            750_000_000,
            eve_db::AsteroidRecord {
                asteroid_id: 750_000_000,
                solar_system_id: 30_000_144,
                belt_id: 40_009_077,
                type_id: 1230,
                quantity: 500,
                initial_quantity: 1_000,
                position: (0.0, 0.0, 0.0),
                radius: 100.0,
            },
        );
    });
    let reg = SystemRegistry::with_sde_and_db(sde, Arc::clone(&db));
    let h = reg.get_or_spawn(30_000_144);
    // Second get_or_spawn hits cached handle
    let h2 = reg.get_or_spawn(30_000_144);
    assert_eq!(h2.solar_system_id, 30_000_144);

    // Spawn & re-spawn ball via SpawnBall preserving existing slim_meta
    h.spawn_ball(make_ball(750_001, 587, DVec3::new(100.0, 0.0, 0.0)))
        .await
        .unwrap();
    h.spawn_ball(make_ball(750_001, 587, DVec3::new(200.0, 0.0, 0.0)))
        .await
        .unwrap();
    assert!(h.set_ball_max_velocity(750_001, 420.0).await.unwrap());
    assert_eq!(reg.find_ball_position_sync(999_888_777), None);

    // Spawn combat ship 750_002 with active module cycle, apply non-lethal then lethal damage
    h.spawn_ball_with_combat(
        make_ball(750_002, 587, DVec3::new(1_000.0, 0.0, 0.0)),
        CombatState {
            shield_hp: 10.0,
            max_shield_hp: 10.0,
            armor_hp: 10.0,
            max_armor_hp: 10.0,
            structure_hp: 10.0,
            max_structure_hp: 10.0,
            ..CombatState::default()
        },
    )
    .await
    .unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: 750_002,
        module_id: 8801,
        duration_s: 10.0,
        ..Default::default()
    })
    .await
    .unwrap();

    // StopNonWarpModuleCycles keeps active_module_ships accurate
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: 750_002,
        module_id: 8802,
        grants_interdiction_nullification: true,
        duration_s: 10.0,
        ..Default::default()
    })
    .await
    .unwrap();
    h.stop_non_warp_module_cycles(750_002).await.unwrap();
    assert!(!h.has_module_cycle(750_002, 8801).await);
    assert!(h.has_module_cycle(750_002, 8802).await);

    // Partial nanite repair stop (elapsed_s > 0 -> partial paste refund branch)
    h.start_nanite_repair(ActiveNaniteRepair {
        char_id: 90_000_001,
        ship_id: 750_002,
        module_id: 8803,
        initial_damage_hp: 20.0,
        paste_total_needed: 4,
        paste_consumed_so_far: 4,
        duration_s: 10.0,
        elapsed_s: 5.0,
    })
    .await
    .unwrap();
    let (rem_dmg, refund) = h.stop_nanite_repair(750_002, 8803).await.unwrap().unwrap();
    assert!((rem_dmg - 10.0).abs() < 1e-6);
    assert_eq!(refund, 2);

    // ApplyEcmJam on non-immune ball + ApplyEcmBurst + FastForwardClock
    assert!(h.apply_ecm_jam(750_002, 5.0).await.unwrap());
    let _ = h.apply_ecm_burst(750_002, 20_000.0, [20.0; 4], 5.0, 0.1).await.unwrap();
    h.fast_forward_clock(750_002, 2).await.unwrap();

    // ApplyDamage: missing ball returns None; lethal damage destroys 750_002 and persists HP
    assert!(h.apply_damage(999_888_777, eve_destiny::DamageVector { em: 10.0, ..Default::default() }).await.unwrap().is_none());
    let dmg_res = h
        .apply_damage(
            750_002,
            eve_destiny::DamageVector {
                em: 100.0,
                thermal: 100.0,
                kinetic: 100.0,
                explosive: 100.0,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert!(dmg_res.1, "lethal damage should destroy 750_002");

    // Duplicate spawn_asteroid early-return path
    let ast_id = 750_777_i64;
    let ast_state = crate::system_actor::AsteroidState {
        asteroid_id: ast_id,
        belt_id: 1,
        type_id: 1230,
        initial_units: 100,
        remaining_units: 100,
        unit_volume: 0.1,
        depleted: false,
        respawn_at_tick: None,
    };
    assert_eq!(h.spawn_asteroid(ast_state.clone(), make_ball(ast_id, 1230, DVec3::new(100.0, 0.0, 0.0))).await.unwrap(), ast_id);
    assert_eq!(h.spawn_asteroid(ast_state, make_ball(ast_id, 1230, DVec3::new(100.0, 0.0, 0.0))).await.unwrap(), ast_id);

    // Stop while ship has active module cycle
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: 750_001,
        module_id: 8890,
        duration_s: 10.0,
        ..Default::default()
    }).await.unwrap();
    assert!(h.set_movement_mode(750_001, MovementMode::Stop).await.is_ok());

    // StepManual with target_lock_completed_hook resolving character via db + lethal module cycle
    let actor_lock_hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let actor_lock_hits_c = Arc::clone(&actor_lock_hits);
    reg.set_target_lock_completed_hook(Arc::new(move |_cid, _tid| {
        actor_lock_hits_c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&90_000_001) {
            c.active_ship_id = 750_001;
        }
    });
    h.spawn_ball_with_combat(make_ball(750_003, 587, DVec3::new(150.0, 0.0, 0.0)), eve_destiny::CombatState {
        shield_hp: 0.0, max_shield_hp: 10.0, armor_hp: 0.0, max_armor_hp: 10.0, structure_hp: 1.0, max_structure_hp: 10.0, ..Default::default()
    }).await.unwrap();
    h.add_target_lock(750_001, eve_destiny::TargetLock {
        target_id: 750_003,
        lock_time_ms: 500.0,
        remaining_lock_ms: 500.0,
        locked: false,
        max_range_m: None,
    }).await.unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: 750_001,
        owner_char_id: Some(90_000_001),
        module_id: 8891,
        target_id: Some(750_003),
        damage_vector: eve_destiny::DamageVector { em: 50.0, ..Default::default() },
        duration_s: 0.5,
        ..Default::default()
    }).await.unwrap();
    let _ = h.step_manual(1.0).await.unwrap();
    assert!(actor_lock_hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
}

#[tokio::test]
async fn heal_flush_peer_observer_updates_preserves_fx_floor_and_single_stamp() {
    let reg = SystemRegistry::new();
    let h = reg.get_or_spawn(30_000_142);

    let ego_ship = 9_201;
    let peer_ship = 9_202;
    let injured = CombatState {
        shield_hp: 500.0,
        max_shield_hp: 1_000.0,
        armor_hp: 800.0,
        max_armor_hp: 1_000.0,
        structure_hp: 900.0,
        max_structure_hp: 1_000.0,
        capacitor: 0.0,
        max_capacitor: 1_000.0,
        ..Default::default()
    };
    h.spawn_ball_with_combat(make_ball(ego_ship, 11567, DVec3::ZERO), injured)
        .await
        .unwrap();
    h.spawn_ball_with_combat(
        make_ball(peer_ship, 587, DVec3::new(1_000.0, 0.0, 0.0)),
        CombatState::default(),
    )
    .await
    .unwrap();

    // Prime SetState drains so both observers are ready for incremental TickBroadcast updates.
    let _ = h.drain_observer_updates(ego_ship).await.unwrap();
    let _ = h.drain_observer_updates(peer_ship).await.unwrap();
    let mut rx = h.subscribe();

    // Advance one tick (`tick = 1`) then simulate an active FX floor (`last_fx_stamp = 10`) ahead
    // of `current_tick`, as happens under TiDi/DoS FX before the next 1Hz Step.
    let tick_bcast = h.step_manual(1.0).await.unwrap();
    let base_tick = tick_bcast.tick;
    while rx.try_recv().is_ok() {}
    let fx_floor = base_tick + 5;
    h.last_fx_stamp
        .fetch_max(fx_floor, std::sync::atomic::Ordering::AcqRel);

    let healed = h.heal_combat_state(ego_ship).await.unwrap();
    assert!((healed.shield_hp - healed.max_shield_hp).abs() < 1e-6);
    let flushed = h.flush_peer_observer_updates(None).await.unwrap();
    assert!(flushed >= 1);

    let msg = rx
        .try_recv()
        .expect("FlushPeerObserverUpdates must emit immediate TickBroadcast");
    assert_eq!(msg.tick, base_tick);
    for &obs_id in &[ego_ship, peer_ship] {
        let rep = msg
            .observer_updates_pyrep
            .get(&obs_id)
            .unwrap_or_else(|| panic!("missing observer update for {obs_id}"));
        let PyRep::List(items) = rep else {
            panic!("expected PyRep::List updates for {obs_id}, got {rep:?}");
        };
        assert!(!items.is_empty());
        let mut saw_dmg = false;
        for entry in items {
            let parsed = crate::sequencer_invariants::parse_destiny_entry(entry)
                .expect("valid destiny entry");
            assert_eq!(
                parsed.0,
                Some(fx_floor as u32),
                "FlushPeerObserverUpdates for {obs_id} must clamp {} to fx_floor={fx_floor}",
                parsed.1
            );
            if parsed.1 == "OnDamageStateChange" {
                saw_dmg = true;
            }
        }
        assert!(saw_dmg, "missing OnDamageStateChange for observer {obs_id}");
    }
}

#[tokio::test]
async fn activate_module_cycle_atomic_validates_preconditions_without_rollback_or_protection_loss() {
    let reg = SystemRegistry::new();
    let sys_id: SolarSystemId = 30_000_142;
    let h = reg.get_or_spawn(sys_id);
    let char_id: CharacterId = 90_000_001;
    let ship_id: BallId = 7701;

    let mut cs = CombatState {
        capacitor: 100.0,
        max_capacitor: 500.0,
        ..Default::default()
    };
    cs.apply_jump_cloak_and_invuln_timers(40);
    h.spawn_ball_with_meta(
        make_ball(ship_id, 587, DVec3::ZERO),
        cs,
        SlimItemMeta {
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let make_req = |module_id: i64, cap_need: f64, is_mwd_or_mjd: bool| AtomicActivateCycleRequest {
        char_id,
        cycle: ActiveModuleCycle {
            ship_id,
            module_id,
            owner_char_id: Some(char_id),
            duration_s: 5.0,
            repeat: 1,
            cap_need_per_cycle: cap_need,
            is_mwd_or_mjd,
            ..ActiveModuleCycle::default()
        },
        allow_while_warping: false,
        allow_while_cloaked: false,
        replace_existing_cycle: false,
        break_protection: true,
        break_reason: ProtectionBreakReason::ModuleActivation,
        require_ball_in_space: true,
        real_now_unix: 1_999_999_990,
    };

    // 1. Mid-warp rejection preserves capacitor, gate cloak, and undock invulnerability.
    h.set_movement_mode(
        ship_id,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1_000_000.0,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 5.0,
        },
    )
    .await
    .unwrap();
    let warp_err = h
        .activate_module_cycle_atomic(make_req(5001, 25.0, false))
        .await
        .unwrap_err();
    assert!(warp_err.contains("CantActivateInWarp"));
    assert!(!h.has_module_cycle(ship_id, 5001).await);

    // 2. Non-gate active cloak rejection preserves capacitor and cloak state.
    h.force_stop_warp(ship_id).unwrap();
    h.set_movement_mode(ship_id, MovementMode::Stop)
        .await
        .unwrap();
    h.modify_combat_state(ship_id, |c| {
        c.is_cloaked = true;
        c.gate_cloak_remaining_ticks = 0;
        c.undock_invulnerable_remaining_ticks = 40;
        c.capacitor = 100.0;
    })
    .await
    .unwrap();
    let cloak_err = h
        .activate_module_cycle_atomic(make_req(5001, 25.0, false))
        .await
        .unwrap_err();
    assert!(cloak_err.contains("CannotActivateCloaked"));
    let cs_cloaked = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!(cs_cloaked.is_cloaked);
    assert!((cs_cloaked.capacitor - 100.0).abs() < 1e-6);

    // 3. Scrammed propulsion and insufficient capacitor reject with zero undock/gate-cloak loss.
    let scram_ship: BallId = 8702;
    h.spawn_ball_with_meta(
        make_ball(scram_ship, 587, DVec3::ZERO),
        CombatState { capacitor: 500.0, max_capacitor: 500.0, ..Default::default() },
        SlimItemMeta::default(),
    )
    .await
    .unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: scram_ship,
        module_id: 9501,
        target_id: Some(ship_id),
        duration_s: 10.0,
        repeat: 1,
        disables_target_mwd_mjd: true,
        warp_scramble_points: 2,
        ..ActiveModuleCycle::default()
    })
    .await
    .unwrap();
    h.modify_combat_state(ship_id, |c| {
        c.apply_jump_cloak_and_invuln_timers(40);
        c.capacitor = 100.0;
    })
    .await
    .unwrap();
    let scram_err = h
        .activate_module_cycle_atomic(make_req(5001, 25.0, true))
        .await
        .unwrap_err();
    assert!(scram_err.contains("WarpScrambled"));

    let _ = h.remove_module_cycle(scram_ship, 9501).await;
    let cap_err = h
        .activate_module_cycle_atomic(make_req(5001, 250.0, false))
        .await
        .unwrap_err();
    assert!(cap_err.contains("Insufficient capacitor"));
    let cs_after_errs = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_after_errs.gate_cloak_remaining_ticks, 40);
    assert_eq!(cs_after_errs.undock_invulnerable_remaining_ticks, 40);
    assert!((cs_after_errs.capacitor - 100.0).abs() < 1e-6);

    // 4. Atomic success debits cap, adds cycle, breaks gate cloak + invuln in one turn, and prevents duplicates.
    let outcome = h
        .activate_module_cycle_atomic(make_req(5001, 40.0, false))
        .await
        .unwrap();
    assert!((outcome.capacitor_after - 60.0).abs() < 1e-6);
    assert!(outcome.protection_events.gate_cloak_broken);
    assert!(outcome.protection_events.invuln_cancelled);
    assert!(outcome.protection_events.slim_cloak_cleared);
    assert!(outcome.protection_events.should_emit_jump_cloak_cancelled());
    assert!(h.has_module_cycle(ship_id, 5001).await);
    assert!(reg.is_module_active_sync(sys_id, ship_id, 5001));

    let dup_err = h
        .activate_module_cycle_atomic(make_req(5001, 10.0, false))
        .await
        .unwrap_err();
    assert!(dup_err.contains("ModuleAlreadyActive"));
    let cs_final = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_final.capacitor - 60.0).abs() < 1e-6);

    // 5. Aligning warp phase remains permitted to activate non-warp modules atomically.
    let ship_align: BallId = 7702;
    let mut align_ball = make_ball(ship_align, 587, DVec3::new(1_000.0, 0.0, 0.0));
    align_ball.mode = MovementMode::new_warp(
        align_ball.position,
        DVec3::new(1_000_000.0, 0.0, 0.0),
        3.0,
    );
    h.spawn_ball_with_meta(
        align_ball,
        CombatState {
            capacitor: 200.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
        SlimItemMeta::default(),
    )
    .await
    .unwrap();
    let align_ok = h
        .activate_module_cycle_atomic(AtomicActivateCycleRequest {
            char_id,
            cycle: ActiveModuleCycle {
                ship_id: ship_align,
                module_id: 5002,
                owner_char_id: Some(char_id),
                duration_s: 5.0,
                repeat: 1,
                cap_need_per_cycle: 20.0,
                ..ActiveModuleCycle::default()
            },
            allow_while_warping: false,
            allow_while_cloaked: false,
            replace_existing_cycle: false,
            break_protection: true,
            break_reason: ProtectionBreakReason::ModuleActivation,
            require_ball_in_space: true,
            real_now_unix: 1_999_999_990,
        })
        .await
        .unwrap();
    assert!((align_ok.capacitor_after - 180.0).abs() < 1e-6);
    assert!(reg.is_module_active_sync(sys_id, ship_align, 5002));
}

#[tokio::test]
async fn activate_module_cycle_atomic_preserves_protection_when_break_false_and_avoids_prevalidation_mutation() {
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let reg = SystemRegistry::with_sde_and_db(sde, db.clone());
    let sys_id: SolarSystemId = 30_000_142;
    let h = reg.get_or_spawn(sys_id);
    let char_id: CharacterId = 90_000_773;
    let ship_id: BallId = 7703;
    let mod_id: ItemId = 50_773;

    // 1. `break_protection: false` keeps undock, gate-cloak, and warp-exit timers completely intact.
    let mut cs = CombatState {
        capacitor: 50.0,
        max_capacitor: 100.0,
        undock_invulnerable_remaining_ticks: 60,
        gate_cloak_remaining_ticks: 60,
        warp_exit_invulnerable_remaining_ticks: 10,
        sync_invuln_with_undock_timer: true,
        is_cloaked: true,
        ..Default::default()
    };
    cs.is_covert_cloak = false;
    h.spawn_ball_with_meta(
        make_ball(ship_id, 587, DVec3::ZERO),
        cs,
        SlimItemMeta {
            char_id: Some(char_id),
            is_cloaked: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let mutadaptive_cycle = ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: 47259,
        owner_char_id: Some(char_id),
        effect_name: "moduleBonusArmorMutadaptive".into(),
        duration_s: 5.0,
        repeat: 1,
        cap_need_per_cycle: 10.0,
        mutadaptive_spool_step: 0.1,
        mutadaptive_spool_max: 1.0,
        current_spool_cycles: 3,
        ..ActiveModuleCycle::default()
    };
    db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(mod_id);
    });
    let no_break_ok = h
        .activate_module_cycle_atomic(AtomicActivateCycleRequest {
            char_id,
            cycle: mutadaptive_cycle.clone(),
            allow_while_warping: true,
            allow_while_cloaked: true,
            replace_existing_cycle: false,
            break_protection: false,
            break_reason: ProtectionBreakReason::ModuleActivation,
            require_ball_in_space: true,
            real_now_unix: 1_999_999_990,
        })
        .await
        .unwrap();
    assert!(!no_break_ok.protection_events.gate_cloak_broken);
    assert!(!no_break_ok.protection_events.invuln_cancelled);
    assert!(!no_break_ok.protection_events.warp_exit_invuln_cancelled);
    let cs_intact = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!(cs_intact.undock_invulnerable_remaining_ticks, 60);
    assert_eq!(cs_intact.gate_cloak_remaining_ticks, 60);
    assert_eq!(cs_intact.warp_exit_invulnerable_remaining_ticks, 10);
    assert!(cs_intact.is_cloaked);
    let snap_intact = h.get_protection_snapshot(ship_id).await.unwrap().unwrap();
    assert_eq!(snap_intact.snapshot.undock_invulnerable_remaining_ticks, 60);
    assert!(snap_intact.snapshot.has_gate_cloak());

    // 2. Capacitor rejection with `effective_max_capacitor` + existing mutadaptive cycle leaves
    //    `cs.max_capacitor`, `cs.capacitor`, existing cycle (`current_spool_cycles == 3`),
    //    and DB `module_active_states` completely unmutated.
    let mut cap_fail_cycle = mutadaptive_cycle;
    cap_fail_cycle.effective_max_capacitor = Some(500.0);
    cap_fail_cycle.cap_need_per_cycle = 80.0;
    cap_fail_cycle.current_spool_cycles = 0;
    let cap_err = h
        .activate_module_cycle_atomic(AtomicActivateCycleRequest {
            char_id,
            cycle: cap_fail_cycle,
            allow_while_warping: true,
            allow_while_cloaked: true,
            replace_existing_cycle: false,
            break_protection: true,
            break_reason: ProtectionBreakReason::ModuleActivation,
            require_ball_in_space: true,
            real_now_unix: 1_999_999_990,
        })
        .await
        .unwrap_err();
    assert!(cap_err.contains("Insufficient capacitor"));

    let cs_after = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert!((cs_after.max_capacitor - 100.0).abs() < 1e-6);
    assert!((cs_after.capacitor - 40.0).abs() < 1e-6);
    assert_eq!(cs_after.undock_invulnerable_remaining_ticks, 60);
    assert_eq!(cs_after.gate_cloak_remaining_ticks, 60);
    assert!(db.with_snapshot(|s| s.module_active_states.contains(&mod_id)));
    let spool_still = h
        .test_with_actor_state_async(move |st| {
            st.active_module_cycles
                .iter()
                .find(|c| c.ship_id == ship_id && c.module_id == mod_id)
                .map(|c| c.current_spool_cycles)
        })
        .await;
    assert_eq!(spool_still, Some(3));
}

#[tokio::test]
async fn atomic_cloak_activation_from_gate_cloak_non_11578_covert_sde_and_deact_hook() {
    let mut sde_val = SdeRegistry::with_canonical_seed();
    let base_type = sde_val.types.values().next().unwrap().clone();
    let (custom_cov_tid, custom_cov_eid, proto_tid) = (99_578_u32, 55_980_u16, 11_370_u32);
    sde_val.effects.insert(
        custom_cov_eid,
        eve_sde::DogmaEffectType {
            effect_id: custom_cov_eid,
            name: "cloakingWarpSafe".to_string(),
            is_warp_safe: true,
            ..Default::default()
        },
    );
    let mut custom_cov_type = base_type.clone();
    custom_cov_type.type_id = custom_cov_tid;
    custom_cov_type.group_id = 330;
    // Deliberately omit "Covert Ops" and 11578 so SDE effect lookup (`F1486-R2-03`) is exercised.
    custom_cov_type.name = "Syndicate Stealth Unit".to_string();
    custom_cov_type.effect_ids = vec![custom_cov_eid];
    sde_val.types.insert(custom_cov_tid, custom_cov_type);
    let mut proto_type = base_type;
    proto_type.type_id = proto_tid;
    proto_type.group_id = 330;
    proto_type.name = "Prototype Cloaking Device I".to_string();
    proto_type.effect_ids = Vec::new();
    sde_val.types.insert(proto_tid, proto_type);

    let db = Arc::new(eve_db::WorldRepository::new_empty());
    let reg = SystemRegistry::with_sde_and_db(Arc::new(sde_val), db.clone());
    let deact_records = Arc::new(std::sync::Mutex::new(Vec::<(i64, i64)>::new()));
    let deact_clone = Arc::clone(&deact_records);
    reg.set_module_deactivation_hook(Arc::new(move |_sys_id, cid, cyc| deact_clone.lock().unwrap().push((cid, cyc.module_id))));

    let (h, char_id, ship_proto, ship_cov, proto_mod_id, cov_mod_id, now) = (
        reg.get_or_spawn(30_000_142), 91_486_701_i64, 96_001_i64, 96_002_i64, 996_001_i64, 996_002_i64, 1_700_000_000_u64,
    );

    // 1. Cloak activation from gate cloak breaks prior protection BEFORE installing cloak cycle.
    h.spawn_ball_with_meta(
        make_ball(ship_proto, 587, DVec3::ZERO),
        CombatState {
            is_cloaked: true, gate_cloak_remaining_ticks: 60, undock_invulnerable_remaining_ticks: 60,
            capacitor: 500.0, max_capacitor: 500.0, ..Default::default()
        },
        SlimItemMeta { char_id: Some(char_id), owner_id: char_id, category_id: 6, group_id: 25, is_cloaked: true, ..Default::default() },
    ).await.unwrap();
    db.with_mut_snapshot(|s| { s.module_active_states.insert(proto_mod_id); });

    let outcome = h.activate_module_cycle_atomic(AtomicActivateCycleRequest {
        char_id,
        cycle: ActiveModuleCycle {
            ship_id: ship_proto, module_id: proto_mod_id, module_type_id: proto_tid, owner_char_id: Some(char_id),
            effect_name: "cloaking".to_string(), repeat: 1, duration_s: 5.0, speed_boost_factor: -0.90,
            ..ActiveModuleCycle::default()
        },
        allow_while_warping: false, allow_while_cloaked: true, replace_existing_cycle: false, break_protection: true,
        break_reason: ProtectionBreakReason::ModuleActivation, require_ball_in_space: true, real_now_unix: now,
    }).await.unwrap();
    assert!(outcome.protection_events.gate_cloak_broken && outcome.protection_events.invuln_cancelled);
    let cs_proto = h.get_combat_state(ship_proto).await.unwrap().unwrap();
    assert_eq!((cs_proto.gate_cloak_remaining_ticks, cs_proto.undock_invulnerable_remaining_ticks), (0, 0));
    assert!((cs_proto.cloak_velocity_multiplier.unwrap_or(0.0) - 0.1).abs() < 1e-6);
    let prot_proto = h.get_protection_snapshot(ship_proto).await.unwrap().unwrap();
    assert!(prot_proto.has_active_cloak_cycle && prot_proto.has_active_non_gate_cloak());
    assert!(reg.has_active_modules_or_cloak_sync(30_000_142, ship_proto));

    // 2. Non-11578 SDE Covert Ops cloak survives `WarpInitiation`; `force_decloak` strips & fires hook.
    h.spawn_ball_with_meta(
        make_ball(ship_cov, 12032, DVec3::new(2_000.0, 0.0, 0.0)),
        CombatState {
            is_cloaked: true, is_covert_cloak: true, cloak_velocity_multiplier: Some(1.0),
            capacitor: 500.0, max_capacitor: 500.0, ..Default::default()
        },
        SlimItemMeta { char_id: Some(char_id), owner_id: char_id, category_id: 6, group_id: 830, is_cloaked: true, ..Default::default() },
    ).await.unwrap();
    db.with_mut_snapshot(|s| { s.module_active_states.insert(cov_mod_id); });
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: ship_cov, module_id: cov_mod_id, module_type_id: custom_cov_tid, owner_char_id: Some(char_id),
        effect_name: "cloaking".to_string(), repeat: 1, duration_s: 5.0, ..ActiveModuleCycle::default()
    }).await.unwrap();

    deact_records.lock().unwrap().clear();
    let warp_ev = h.break_protection(ship_cov, Some(char_id), ProtectionBreakReason::WarpInitiation, now).await.unwrap();
    assert!(!warp_ev.covert_cloak_broken && warp_ev.removed_cloak_cycles.is_empty());
    assert!(h.get_protection_snapshot(ship_cov).await.unwrap().unwrap().has_active_cloak_cycle);
    assert!(deact_records.lock().unwrap().is_empty());

    let decloak_ev = h.force_decloak(ship_cov, Some(char_id), ProtectionBreakReason::Manual, now).await.unwrap();
    assert!(decloak_ev.covert_cloak_broken && decloak_ev.removed_cloak_cycles.len() == 1);
    assert_eq!(decloak_ev.removed_cloak_cycles[0].module_id, cov_mod_id);
    assert!(!h.get_protection_snapshot(ship_cov).await.unwrap().unwrap().has_active_cloak_cycle);
    assert!(deact_records.lock().unwrap().contains(&(char_id, cov_mod_id)));
}

#[tokio::test]
async fn lancer_atomic_commit_and_clamped_cap_refund_f05() {
    let reg = SystemRegistry::new();
    let h = reg.get_or_spawn(30_000_142);
    let (char_id, ship_id, mod_id, now) = (95_951_i64, 880_951_i64, 880_952_i64, 1_700_000_000_u64);
    let req = |cap_cost| AtomicLancerActivationRequest {
        char_id, ship_id, module_id: mod_id, cap_cost, real_now_unix: now,
    };
    assert!(h.commit_lancer_activation_atomic(req(500.0)).await.unwrap_err().contains("not in space"));
    assert!(h.add_capacitor_clamped(ship_id, 250.0).await.unwrap_err().contains("not in space"));

    h.spawn_ball_with_meta(
        make_ball(ship_id, 77_281, DVec3::ZERO),
        CombatState {
            is_cloaked: true, gate_cloak_remaining_ticks: 30, undock_invulnerable_remaining_ticks: 15,
            capacitor: 400.0, max_capacitor: 1_000.0, ..Default::default()
        },
        SlimItemMeta { char_id: Some(char_id), owner_id: char_id, category_id: 6, group_id: 4594, is_cloaked: true, ..Default::default() },
    ).await.unwrap();

    assert!(h.commit_lancer_activation_atomic(req(600.0)).await.unwrap_err().contains("NotEnoughCapacitor"));
    let cs0 = h.get_combat_state(ship_id).await.unwrap().unwrap();
    assert_eq!((cs0.gate_cloak_remaining_ticks, cs0.undock_invulnerable_remaining_ticks, cs0.capacitor), (30, 15, 400.0));

    let out = h.commit_lancer_activation_atomic(req(250.0)).await.unwrap();
    assert!(out.protection_events.gate_cloak_broken && out.protection_events.invuln_cancelled);
    assert!((out.capacitor_after - 150.0).abs() < 1e-6);

    h.add_module_cycle(ActiveModuleCycle {
        ship_id, module_id: mod_id, module_type_id: 77_398, owner_char_id: Some(char_id),
        effect_name: "superWeapon".into(), duration_s: 30.0, started_at_unix: now, ..Default::default()
    }).await.unwrap();
    assert!(h.commit_lancer_activation_atomic(req(50.0)).await.unwrap_err().contains("ModuleAlreadyActive"));
    let _ = h.remove_module_cycle(ship_id, mod_id).await;

    h.set_capacitor(ship_id, 100.0).await.unwrap();
    assert!((h.add_capacitor_clamped(ship_id, 250.0).await.unwrap() - 350.0).abs() < 1e-6);
    assert!((h.add_capacitor_clamped(ship_id, 5_000.0).await.unwrap() - 1_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn active_module_cycle_records_origin_scene_epoch_for_pilots_none_for_npcs_and_preserves_across_take_restore() {
    assert_eq!(ActiveModuleCycle::default().origin_scene_epoch, None);

    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let reg = SystemRegistry::with_sde_and_db(sde, Arc::clone(&db));
    let h = reg.get_or_spawn(30_000_142);

    let char_id: CharacterId = 90_000_001;
    let uid = db.with_snapshot(|s| s.characters[&char_id].user_id);
    let sess_id = 88_042_u64;
    let now = 1_700_000_000_u64;
    db.acquire_online_lock(uid, sess_id, Some(char_id), true, now)
        .expect("pilot online lock");
    db.set_session_scene_epoch(sess_id, 42);

    let (ship_id, npc_ball_id, struct_ball_id) = (881_001_i64, 881_002_i64, 881_003_i64);
    h.spawn_ball_with_meta(
        make_ball(ship_id, 587, DVec3::ZERO),
        CombatState {
            capacitor: 500.0,
            max_capacitor: 500.0,
            ..Default::default()
        },
        SlimItemMeta {
            char_id: Some(char_id),
            owner_id: char_id,
            category_id: 6,
            group_id: 25,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h.spawn_ball_with_meta(
        make_ball(npc_ball_id, 587, DVec3::new(1_000.0, 0.0, 0.0)),
        CombatState::default(),
        SlimItemMeta {
            char_id: None,
            category_id: 11,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    h.spawn_ball_with_meta(
        make_ball(struct_ball_id, 35_832, DVec3::new(2_000.0, 0.0, 0.0)),
        CombatState::default(),
        SlimItemMeta {
            char_id: None,
            category_id: 65,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    // NPC and structure cycles do not carry a pilot session epoch.
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: npc_ball_id,
        module_id: 91_001,
        effect_name: "targetAttack".into(),
        duration_s: 5.0,
        ..Default::default()
    })
    .await
    .unwrap();
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: struct_ball_id,
        module_id: 91_002,
        effect_name: "targetAttack".into(),
        duration_s: 5.0,
        ..Default::default()
    })
    .await
    .unwrap();

    // Pilot cycle accepted atomically at session scene epoch 42 records Some(42).
    h.activate_module_cycle_atomic(AtomicActivateCycleRequest {
        char_id,
        cycle: ActiveModuleCycle {
            ship_id,
            module_id: 91_003,
            module_type_id: 439,
            owner_char_id: Some(char_id),
            effect_name: "afterburner".into(),
            repeat: 5,
            duration_s: 10.0,
            cap_need_per_cycle: 10.0,
            speed_boost_factor: 1.5,
            ..Default::default()
        },
        allow_while_warping: false,
        allow_while_cloaked: true,
        replace_existing_cycle: false,
        break_protection: false,
        break_reason: ProtectionBreakReason::ModuleActivation,
        require_ball_in_space: true,
        real_now_unix: now,
    })
    .await
    .unwrap();

    // Advancing the session scene epoch before accepting a second pilot cycle stamps the new epoch.
    db.set_session_scene_epoch(sess_id, 99);
    h.add_module_cycle(ActiveModuleCycle {
        ship_id,
        module_id: 91_004,
        module_type_id: 483,
        owner_char_id: Some(char_id),
        effect_name: "shieldBoosting".into(),
        repeat: 5,
        duration_s: 10.0,
        cap_need_per_cycle: 10.0,
        ..Default::default()
    })
    .await
    .unwrap();

    h.test_with_actor_state_async(|st| {
        let epoch_of = |mid: i64| {
            st.active_module_cycles
                .iter()
                .find(|c| c.module_id == mid)
                .expect("cycle present")
                .origin_scene_epoch
        };
        assert_eq!(epoch_of(91_001), None);
        assert_eq!(epoch_of(91_002), None);
        assert_eq!(epoch_of(91_003), Some(42));
        assert_eq!(epoch_of(91_004), Some(99));
    })
    .await;

    // TakeBall + RestoreBallIfAbsent preserves origin_scene_epoch verbatim even after DB epoch changes.
    let taken = h
        .take_ball_preserving_db(ship_id)
        .await
        .expect("taken ship snapshot");
    assert_eq!(
        taken
            .removed_own_cycles
            .iter()
            .find(|c| c.module_id == 91_003)
            .unwrap()
            .origin_scene_epoch,
        Some(42)
    );
    assert_eq!(
        taken
            .removed_own_cycles
            .iter()
            .find(|c| c.module_id == 91_004)
            .unwrap()
            .origin_scene_epoch,
        Some(99)
    );

    db.set_session_scene_epoch(sess_id, 123);
    h.restore_ball_if_absent(taken).await.unwrap();

    let (epoch_91003, epoch_91004) = h
        .test_with_actor_state_async(|st_restored| {
            (
                st_restored
                    .active_module_cycles
                    .iter()
                    .find(|c| c.module_id == 91_003)
                    .unwrap()
                    .origin_scene_epoch,
                st_restored
                    .active_module_cycles
                    .iter()
                    .find(|c| c.module_id == 91_004)
                    .unwrap()
                    .origin_scene_epoch,
            )
        })
        .await;
    assert_eq!(epoch_91003, Some(42));
    assert_eq!(epoch_91004, Some(99));
}

#[tokio::test]
async fn external_active_modules_and_optional_ball_atomic_commit_sync_and_cleanup() {
    let sys_id = 30_000_142_u32;
    let reg = Arc::new(SystemRegistry::new());
    let h = reg.get_or_spawn(sys_id);

    let (char_id, ship_id, target_id, ext_mod_id, cycle_mod_id) =
        (96_100_001_i64, 96_200_001_i64, 96_200_002_i64, 96_300_001_i64, 96_300_002_i64);

    // 1. Atomic commit with `require_ball_in_space: true` rejects missing ball, while `false`
    //    succeeds cleanly and returns exact `target_cap_drained` / `target_shield_boosted` deltas.
    h.spawn_ball_with_combat(
        make_ball(target_id, 587, DVec3::new(1_000.0, 0.0, 0.0)),
        CombatState {
            capacitor: 40.0,
            max_capacitor: 100.0,
            shield_hp: 90.0,
            max_shield_hp: 100.0,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let req_base = AtomicActivateCycleRequest {
        char_id,
        cycle: ActiveModuleCycle {
            ship_id,
            module_id: cycle_mod_id,
            owner_char_id: Some(char_id),
            target_id: Some(target_id),
            duration_s: 5.0,
            repeat: 1,
            cap_drain_target: 65.0,
            remote_shield_amount: 25.0,
            ..Default::default()
        },
        allow_while_warping: false,
        allow_while_cloaked: false,
        replace_existing_cycle: false,
        break_protection: true,
        break_reason: ProtectionBreakReason::ModuleActivation,
        require_ball_in_space: true,
        real_now_unix: 1_900_000_000,
    };
    assert!(h.activate_module_cycle_atomic(req_base.clone()).await.is_err());

    let mut req_no_ball = req_base;
    req_no_ball.require_ball_in_space = false;
    let out = h.activate_module_cycle_atomic(req_no_ball).await.unwrap();
    assert!((out.target_cap_drained - 40.0).abs() < 1e-6);
    assert!((out.target_shield_boosted - 10.0).abs() < 1e-6);
    assert!(reg.is_module_active_sync(sys_id, ship_id, cycle_mod_id));

    // 2. External active modules sync into `active_module_ships`, `is_module_active_sync`, and
    //    `has_active_modules_or_cloak_sync`, and clear on `RemoveBall` / `AcceptDocking`.
    reg.register_external_active_module(sys_id, ship_id, ext_mod_id);
    assert!(reg.is_module_active_sync(sys_id, ship_id, ext_mod_id));
    assert!(h.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(reg.has_active_modules_or_cloak_sync(sys_id, ship_id));

    reg.unregister_external_active_module_in_loaded_systems(ship_id, ext_mod_id);
    assert!(!reg.is_module_active_sync(sys_id, ship_id, ext_mod_id));

    h.spawn_ball(make_ball(ship_id, 587, DVec3::ZERO)).await.unwrap();
    reg.register_external_active_module(sys_id, ship_id, ext_mod_id);
    assert!(h.active_module_ships.read().unwrap().contains(&ship_id));
    h.remove_ball(ship_id).await.unwrap();
    assert!(!reg.is_module_active_sync(sys_id, ship_id, ext_mod_id));
    assert!(!h.active_module_ships.read().unwrap().contains(&ship_id));

    h.spawn_ball(make_ball(ship_id, 587, DVec3::ZERO)).await.unwrap();
    reg.register_external_active_module(sys_id, ship_id, ext_mod_id);
    h.accept_docking(ship_id, 60_003_760, DVec3::new(10_000.0, 0.0, 0.0))
        .await
        .unwrap();
    assert!(!reg.is_module_active_sync(sys_id, ship_id, ext_mod_id));
    assert!(!h.active_module_ships.read().unwrap().contains(&ship_id));
}

#[tokio::test]
async fn external_active_modules_sync_view_try_remove_cycle_and_db_ball_cleanup() {
    let sys_id = 30_000_142_u32;
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(
        Arc::new(SdeRegistry::with_canonical_seed()),
        Arc::clone(&db),
    ));
    let h = reg.get_or_spawn(sys_id);
    super::celestials::register_db_system_ballpark(&db, sys_id, &h);

    let (ship_both, ship_ext_only) = (96_400_001_i64, 96_400_002_i64);
    let (cycle_mod, ext_mod_both, ext_mod_only) = (96_500_001_i64, 96_500_002_i64, 96_500_003_i64);

    h.spawn_ball(make_ball(ship_both, 587, DVec3::ZERO)).await.unwrap();
    h.spawn_ball(make_ball(ship_ext_only, 587, DVec3::new(500.0, 0.0, 0.0)))
        .await
        .unwrap();

    // 1. Sync register/unregister/clear immediately publish `SystemView::has_active_module`.
    assert!(!h.view().has_active_module(ship_ext_only));
    h.register_external_active_module_sync(ship_ext_only, ext_mod_only);
    assert!(h.view().has_active_module(ship_ext_only));
    assert!(h.unregister_external_active_module_sync(ship_ext_only, ext_mod_only));
    assert!(!h.view().has_active_module(ship_ext_only));

    h.register_external_active_module_sync(ship_ext_only, ext_mod_only);
    assert!(h.view().has_active_module(ship_ext_only));
    h.clear_external_active_modules_sync();
    assert!(!h.view().has_active_module(ship_ext_only));

    // 2. `try_remove_module_cycle` preserves external active modules on both same and other ships.
    h.add_module_cycle(ActiveModuleCycle {
        ship_id: ship_both,
        module_id: cycle_mod,
        duration_s: 10.0,
        repeat: 1,
        ..Default::default()
    })
    .await
    .unwrap();
    h.register_external_active_module_sync(ship_both, ext_mod_both);
    h.register_external_active_module_sync(ship_ext_only, ext_mod_only);

    h.try_remove_module_cycle(ship_both, cycle_mod);
    assert!(!h.is_module_active_sync(ship_both, cycle_mod));
    assert!(h.is_module_active_sync(ship_both, ext_mod_both));
    assert!(h.is_module_active_sync(ship_ext_only, ext_mod_only));
    assert!(h.active_module_ships.read().unwrap().contains(&ship_both));
    assert!(h.active_module_ships.read().unwrap().contains(&ship_ext_only));
    assert!(h.has_active_modules_or_cloak_sync(ship_both));
    assert!(h.has_active_modules_or_cloak_sync(ship_ext_only));
    assert!(h.view().has_active_module(ship_both));
    assert!(h.view().has_active_module(ship_ext_only));

    // 3. `try_remove_ball_for_db` cleans up `external_active_modules`, `active_module_ships`, and `SystemView`.
    super::celestials::try_remove_ball_for_db(&db, sys_id, ship_ext_only);
    assert!(h.get_ball_snapshot(ship_ext_only).await.unwrap().is_none());
    assert!(!h.is_module_active_sync(ship_ext_only, ext_mod_only));
    assert!(!h.active_module_ships.read().unwrap().contains(&ship_ext_only));
    assert!(!h.view().has_active_module(ship_ext_only));
    assert!(h.is_module_active_sync(ship_both, ext_mod_both));
    assert!(h.view().has_active_module(ship_both));
}

#[tokio::test]
async fn external_active_modules_try_remove_ball_under_leaf_lock_no_rank_inversion() {
    let sys_id = 30_000_142_u32;
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let reg = Arc::new(SystemRegistry::with_sde_and_db(
        Arc::new(SdeRegistry::with_canonical_seed()),
        Arc::clone(&db),
    ));
    let h = reg.get_or_spawn(sys_id);
    let (ship_id, ext_mod_id) = (96_600_001_i64, 96_600_002_i64);
    h.spawn_ball(make_ball(ship_id, 587, DVec3::ZERO))
        .await
        .unwrap();
    h.register_external_active_module_sync(ship_id, ext_mod_id);
    assert!(h.is_module_active_sync(ship_id, ext_mod_id));
    assert!(h.active_module_ships.read().unwrap().contains(&ship_id));
    assert!(h.view().has_active_module(ship_id));

    let leaf_lock: eve_db::lock_rank::RankedRwLock<(), { eve_db::lock_rank::RANK_LEAF }> =
        eve_db::lock_rank::RankedRwLock::new(());
    let leaf_guard = leaf_lock.write().unwrap();
    h.try_remove_ball(ship_id);
    drop(leaf_guard);

    assert!(h.get_ball_snapshot(ship_id).await.unwrap().is_none());
    assert!(!h.is_module_active_sync(ship_id, ext_mod_id));
    assert!(!h.active_module_ships.read().unwrap().contains(&ship_id));
}
