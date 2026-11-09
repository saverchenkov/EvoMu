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
    persist_landed_warps_in_db(&bp, &[], &None);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)));

    // Persist coordinates on OnDeactivatingWarp
    persist_landed_warps_in_db(
        &bp,
        &[DestinyEvent::OnDeactivatingWarp { ball_id }],
        &Some(Arc::clone(&db)),
    );
    let saved_pos = db.with_snapshot(|snap| snap.item_positions.get(&ball_id).copied());
    assert_eq!(saved_pos, Some([4_000.0, 5_000.0, 6_000.0]));

    // try_remove_ball_for_db synchronously removes ball from ballpark
    try_remove_ball_for_db(&db, sys_id, ball_id);
    assert!(handle.get_ball_snapshot(ball_id).await.unwrap().is_none());
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

    // 1. Stationary ball -> zero snapshot writes (wal_seq unchanged).
    let v0 = db.with_snapshot(|s| s.wal_seq);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)));
    assert_eq!(db.with_snapshot(|s| s.wal_seq), v0);

    // 2. Small sub-warp move (< 100m) on non-interval tick defers; tick % 5 == 0 flushes.
    bp.current_tick = 1;
    bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::GotoDirection { direction: DVec3::X };
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(120.0, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)));
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([100.0, 0.0, 0.0])
    );
    bp.current_tick = 5;
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)));
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([120.0, 0.0, 0.0])
    );

    // 3. Large sub-warp displacement (>= 100m) flushes immediately on any tick.
    bp.current_tick = 6;
    bp.balls.get_mut(&ship_id).unwrap().position = DVec3::new(350.0, 0.0, 0.0);
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)));
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
    persist_landed_warps_in_db(&bp, &[], &Some(Arc::clone(&db)));
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([350.0, 0.0, 0.0])
    );

    bp.balls.get_mut(&ship_id).unwrap().mode = MovementMode::Stop;
    persist_ball_positions_in_db(&bp, &[ship_id], &Some(Arc::clone(&db)));
    assert_eq!(
        db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()),
        Some([500_000.0, 0.0, 0.0])
    );
}
