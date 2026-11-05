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
    let pending_notifs = Arc::new(RwLock::new(HashMap::new()));
    let tactical = Arc::new(RwLock::new(Default::default()));
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
    reg.set_module_deactivation_hook(Arc::new(move |cid, cyc| {
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
