//! Unit tests covering `celestials_helpers` (`register_db_system_ballpark`,
//! `try_remove_ball_for_db`, `persist_landed_warps_in_db`).

use super::*;
use crate::system_actor::SystemRegistry;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, DestinyEvent, MovementMode};
use glam::DVec3;
use std::sync::Arc;

#[tokio::test]
async fn celestials_helpers_remove_registered_balls_and_persist_landed_warp_coords() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sys_id = 30_000_142u32;
    let registry = SystemRegistry::new();
    let handle = registry.get_or_spawn(sys_id);
    register_db_system_ballpark(&db, sys_id, &handle);

    let ball_id = 98_900_111i64;
    let _ = handle
        .spawn_ball(Ball {
            id: ball_id,
            type_id: 587,
            position: DVec3::new(1_250.0, -250.0, 750.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    assert!(handle.get_ball_snapshot(ball_id).await.unwrap().is_some());

    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ball_id,
            InventoryItemRecord {
                item_id: ball_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    // No-op when db_opt is None or events lack OnDeactivatingWarp
    let mut bp = Ballpark::new(sys_id);
    bp.add_ball(Ball {
        id: ball_id,
        type_id: 587,
        position: DVec3::new(4_000.0, 5_000.0, 6_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    let empty_epochs = HashMap::new();
    persist_landed_warps_in_db(&bp, &[], &None, &empty_epochs);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);

    // Persist coordinates on OnDeactivatingWarp
    persist_landed_warps_in_db(
        &bp,
        &[DestinyEvent::OnDeactivatingWarp { ball_id }],
        &Some(Arc::clone(&db)),
        &empty_epochs,
    );
    let saved_pos = db.with_snapshot(|snap| snap.item_positions.get(&ball_id).copied());
    assert_eq!(saved_pos, Some([4_000.0, 5_000.0, 6_000.0]));

    // try_remove_ball_for_db synchronously removes ball from ballpark
    try_remove_ball_for_db(&db, sys_id, ball_id);
    assert!(handle.get_ball_snapshot(ball_id).await.unwrap().is_none());

    // try_remove_ball_for_db on unregistered system is a clean no-op
    try_remove_ball_for_db(&db, 30_999_999, ball_id);
}

#[tokio::test]
async fn fast_forward_clock_warp_invuln_and_force_stop_warp_cover_all_branches() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sys_id = 30_000_142u32;
    let ship_id = 98_900_222i64;
    let mut bp = Ballpark::new(sys_id);
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let ball_positions = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new()));
    let scrambled_balls = Arc::new(eve_db::lock_rank::RankedRwLock::new(
        std::collections::HashSet::new(),
    ));
    let (event_tx, mut event_rx) = tokio::sync::broadcast::channel(16);
    let mut sim_elapsed = 0.0;
    let db_opt = Some(Arc::clone(&db));

    // Missing ball and stopped non-invuln ball both return immediately without advancing sim_elapsed
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        999_999,
        10,
        &mut sim_elapsed,
        &db_opt,
        &ball_positions,
        &scrambled_balls,
        &event_tx,
    );
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        10,
        &mut sim_elapsed,
        &db_opt,
        &ball_positions,
        &scrambled_balls,
        &event_tx,
    );
    assert_eq!(sim_elapsed, 0.0);

    // Stationary + invulnerable advances until invulnerability expires and broadcasts tick
    bp.combat_states.insert(
        ship_id,
        eve_destiny::CombatState {
            undock_invulnerable_remaining_ticks: 3,
            ..Default::default()
        },
    );
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        100,
        &mut sim_elapsed,
        &db_opt,
        &ball_positions,
        &scrambled_balls,
        &event_tx,
    );
    assert!((sim_elapsed - 3.0).abs() < 1e-6);
    assert!(event_rx.try_recv().is_ok());

    // Moving in warp lands at destination, persists item_positions, updates observer_events, and breaks on OnDeactivatingWarp
    let dest = DVec3::new(200_000.0, 0.0, 0.0);
    if let Some(b) = bp.balls.get_mut(&ship_id) {
        b.mode = MovementMode::new_warp(DVec3::ZERO, dest, 6.0);
        b.velocity = DVec3::new(350.0, 0.0, 0.0);
    }
    let before_elapsed = sim_elapsed;
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        300,
        &mut sim_elapsed,
        &db_opt,
        &ball_positions,
        &scrambled_balls,
        &event_tx,
    );
    assert!(sim_elapsed > before_elapsed);
    assert!(matches!(bp.balls[&ship_id].mode, MovementMode::Stop));
    let saved = db.with_snapshot(|snap| snap.item_positions.get(&ship_id).copied());
    assert!(saved.is_some());
    let bcast = event_rx.try_recv().unwrap();
    assert!(!bcast.events.is_empty());

    // sync_ball_positions records scrambled balls
    bp.set_warp_scramble_strength(ship_id, 2);
    sync_ball_positions(&bp, &ball_positions, &scrambled_balls);
    assert!(scrambled_balls.read().unwrap().contains(&ship_id));

    // SystemHandle::force_stop_warp stops active warp and errors on missing ball
    let registry = SystemRegistry::new();
    let handle = registry.get_or_spawn(sys_id);
    let _ = handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::new(500.0, 0.0, 0.0),
            max_velocity: 400.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 50.0,
            speed_fraction: 1.0,
            mode: MovementMode::new_warp(DVec3::ZERO, DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
        })
        .await;
    assert!(handle.force_stop_warp(ship_id).is_ok());
    let snap = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(snap.1.mode, MovementMode::Stop));
    assert!(handle.force_stop_warp(999_888_777).is_err());

    // register_db_system_ballpark prunes dead weak entries once map exceeds 256
    for idx in 0..260u32 {
        let temp_reg = SystemRegistry::new();
        let temp_handle = temp_reg.get_or_spawn(31_000_000 + idx);
        register_db_system_ballpark(&db, 31_000_000 + idx, &temp_handle);
    }
    register_db_system_ballpark(&db, sys_id, &handle);
    try_remove_ball_for_db(&db, sys_id, ship_id);
    assert!(handle.get_ball_snapshot(ship_id).await.unwrap().is_none());
}

#[test]
fn subwarp_and_mjd_item_position_persistence_and_stationary_noop() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sys_id = 30_000_142u32;
    let ship_id = 98_900_222i64;
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, [100.0, 0.0, 0.0]);
    });

    let mut bp = Ballpark::new(sys_id);
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    let empty_epochs = HashMap::new();
    // 1. Stationary ball -> zero snapshot writes (wal_seq unchanged).
    let v0 = db.with_snapshot(|s| s.wal_seq);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(db.with_snapshot(|s| s.wal_seq), v0);

    // 2. Small sub-warp move (< 100m) on non-interval tick defers; tick % 5 == 0 flushes.
    bp.current_tick = 1;
    bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::GotoDirection { direction: DVec3::X };
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(120.0, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([100.0, 0.0, 0.0])
    );
    bp.current_tick = 5;
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([120.0, 0.0, 0.0])
    );

    // 3. Large sub-warp displacement (>= 100m) flushes immediately on any tick.
    bp.current_tick = 6;
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(350.0, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([350.0, 0.0, 0.0])
    );

    // 4. Active warp (Cruising) skips item_positions overwrite; explicit persist_ball_positions_in_db updates.
    bp.current_tick = 10;
    bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1_000_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1_000_000.0,
        phase: eve_destiny::WarpPhase::Cruising,
        elapsed_warp_time: 5.0,
    };
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(500_000.0, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([350.0, 0.0, 0.0])
    );

    bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Stop;
    persist_ball_positions_in_db(&bp, &[ship_id], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([500_000.0, 0.0, 0.0])
    );

    // 5. Exact 100.0 m displacement threshold on non-5-tick: 99.9 m defers, 100.0 m persists immediately.
    bp.current_tick = 7;
    bp.balls.get_mut(&ship_id).unwrap().mode =
        MovementMode::GotoDirection { direction: DVec3::X };
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(500_099.9, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([500_000.0, 0.0, 0.0])
    );
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(500_100.0, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)), &empty_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([500_100.0, 0.0, 0.0])
    );
}

#[tokio::test]
async fn persist_ball_positions_and_landed_warps_reject_stale_epoch_and_wrong_system() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sys_id = 30_000_142u32;
    let other_sys_id = 30_002_187u32;
    let ship_id = 100_999_888i64;
    let initial_pos = [1_000.0, 2_000.0, 3_000.0];
    let init_cs = eve_destiny::CombatState {
        shield_hp: 500.0,
        max_shield_hp: 500.0,
        armor_hp: 400.0,
        max_armor_hp: 400.0,
        structure_hp: 350.0,
        max_structure_hp: 350.0,
        capacitor: 250.0,
        max_capacitor: 250.0,
        ..Default::default()
    };

    let (epoch_1, epoch_2) = db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            eve_db::InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: i64::from(sys_id),
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let e1 = crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            Some(&init_cs),
            Some(sys_id),
            Some(initial_pos),
        );
        let e2 = crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            Some(&init_cs),
            Some(sys_id),
            Some(initial_pos),
        );
        (e1, e2)
    });
    assert!(epoch_2 > epoch_1);

    let mut bp = Ballpark::new(sys_id);
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(99_000.0, 88_000.0, 77_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });

    // 1. Stale epoch must be rejected by both persist_ball_positions_in_db and persist_landed_warps_in_db.
    let stale_epochs = HashMap::from([(ship_id, epoch_1)]);
    persist_ball_positions_in_db(&bp, &[ship_id], &Some(Arc::clone(&db)), &stale_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some(initial_pos),
        "W5C3-01: persist_ball_positions_in_db must reject stale space_entry_epoch"
    );
    persist_landed_warps_in_db(
        &bp,
        &[DestinyEvent::OnDeactivatingWarp { ball_id: ship_id }],
        &Some(Arc::clone(&db)),
        &stale_epochs,
    );
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some(initial_pos),
        "W5C3-01: persist_landed_warps_in_db must reject stale space_entry_epoch"
    );

    // 2. Wrong solar_system_id must also be rejected even with matching epoch.
    let mut wrong_sys_bp = Ballpark::new(other_sys_id);
    wrong_sys_bp.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::new(99_000.0, 88_000.0, 77_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    let current_epochs = HashMap::from([(ship_id, epoch_2)]);
    persist_ball_positions_in_db(
        &wrong_sys_bp,
        &[ship_id],
        &Some(Arc::clone(&db)),
        &current_epochs,
    );
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some(initial_pos),
        "W5C3-01: persist_ball_positions_in_db must reject wrong solar_system_id"
    );

    // 3. Matching (sys_id, epoch_2) persists the updated position.
    persist_ball_positions_in_db(&bp, &[ship_id], &Some(Arc::clone(&db)), &current_epochs);
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([99_000.0, 88_000.0, 77_000.0])
    );

    // 4. Through SystemActor: spawning at epoch_2 then advancing DB to epoch_3 causes
    // SetBallPosition in the old actor to be rejected as stale.
    let registry = SystemRegistry::with_sde_and_db(
        Arc::new(eve_sde::SdeRegistry::with_canonical_seed()),
        Arc::clone(&db),
    );
    let handle = registry.get_or_spawn(sys_id);
    handle
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 2_000.0, 3_000.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            init_cs.clone(),
        )
        .await
        .unwrap();
    let epoch_3 = db.with_mut_snapshot(|snap| {
        crate::sp6_lifecycle::leave_space_in_snap(
            snap,
            ship_id,
            Some(&init_cs),
            Some(sys_id),
            Some([11_111.0, 22_222.0, 33_333.0]),
        )
    });
    assert!(epoch_3 > epoch_2);
    assert!(
        handle
            .set_ball_position(ship_id, DVec3::new(44_444.0, 55_555.0, 66_666.0))
            .await
    );
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([11_111.0, 22_222.0, 33_333.0]),
        "W5C3-01: SystemActor SetBallPosition with stale actor epoch must not overwrite DB"
    );
}
