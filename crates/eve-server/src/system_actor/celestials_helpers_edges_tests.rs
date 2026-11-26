//! Edge-case tests for `celestials_helpers`: fast-forward clock, scrambled ball sync,
//! >256 registry pruning, static/missing ball warp persistence, and `force_stop_warp`.

#![forbid(unsafe_code)]

use super::*;
use crate::system_actor::SystemRegistry;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, Ballpark, CombatState, DestinyEvent, MovementMode};
use glam::DVec3;
use std::collections::HashSet;
use std::sync::Arc;

fn make_ship_ball(id: BallId, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode,
    }
}

#[tokio::test]
async fn celestials_helpers_pruning_unregistered_and_scrambled_sync() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let registry = SystemRegistry::new();

    // Unregistered system is a safe no-op
    try_remove_ball_for_db(&db, 30_999_999, 12345);

    // Dropped handle upgrade() == None branch
    let dropped_sys = 30_888_001u32;
    {
        let tmp_reg = SystemRegistry::new();
        let tmp_handle = tmp_reg.get_or_spawn(dropped_sys);
        register_db_system_ballpark(&db, dropped_sys, &tmp_handle);
    }
    try_remove_ball_for_db(&db, dropped_sys, 999);

    // Exceed 256 entries with dead Weak pointers so pruning retain() path runs
    {
        let tmp_reg = SystemRegistry::new();
        for offset in 0..260u32 {
            let h = tmp_reg.get_or_spawn(30_700_000 + offset);
            register_db_system_ballpark(&db, 30_700_000 + offset, &h);
        }
    }
    let live_handle = registry.get_or_spawn(30_000_142);
    register_db_system_ballpark(&db, 30_000_142, &live_handle);

    // Scrambled ball sync via sync_ball_positions
    let mut bp = Ballpark::new(30_000_142);
    let ship_id = 99_001_i64;
    bp.add_ball(make_ship_ball(ship_id, DVec3::new(100.0, 0.0, 0.0), MovementMode::Stop));
    let mut cs = CombatState::new();
    cs.warp_scramble_strength = 2;
    bp.combat_states.insert(ship_id, cs);

    let pos_cache = BallPositionSyncCache::new_standalone();
    let scram_cache: eve_db::lock_rank::RankedRwLock<_, { eve_db::lock_rank::RANK_LEAF }> =
        eve_db::lock_rank::RankedRwLock::new(HashSet::new());
    sync_ball_positions(&bp, &pos_cache, &scram_cache);
    assert_eq!(pos_cache.read().unwrap().get(&ship_id), Some(&DVec3::new(100.0, 0.0, 0.0)));
    assert!(scram_cache.read().unwrap().contains(&ship_id));
}

#[test]
fn persist_landed_warps_skips_static_missing_items_and_missing_balls() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut bp = Ballpark::new(30_000_142);

    let static_id = 40_000_001_i64;
    let unseeded_item_id = 99_002_001_i64;
    let missing_ball_id = 99_002_002_i64;

    bp.add_static_ball(make_ship_ball(static_id, DVec3::new(10.0, 20.0, 30.0), MovementMode::Stop), 6);
    bp.add_ball(make_ship_ball(unseeded_item_id, DVec3::new(50.0, 60.0, 70.0), MovementMode::Stop));

    db.with_mut_snapshot(|snap| {
        // Put static_id and missing_ball_id in snap.items, but NOT unseeded_item_id
        for id in [static_id, missing_ball_id] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 587,
                    owner_id: 90_000_001,
                    location_id: 30_000_142,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let empty_epochs = HashMap::new();
    persist_landed_warps_in_db(
        &bp,
        &[
            DestinyEvent::Stop { ball_id: static_id },
            DestinyEvent::OnDeactivatingWarp { ball_id: static_id },
            DestinyEvent::OnDeactivatingWarp { ball_id: unseeded_item_id },
            DestinyEvent::OnDeactivatingWarp { ball_id: missing_ball_id },
        ],
        &Some(Arc::clone(&db)),
        &empty_epochs,
    );

    db.with_snapshot(|snap| {
        assert!(!snap.item_positions.contains_key(&static_id));
        assert!(!snap.item_positions.contains_key(&unseeded_item_id));
        assert!(!snap.item_positions.contains_key(&missing_ball_id));
    });
}

#[tokio::test]
async fn fast_forward_clock_and_force_stop_warp_edge_branches() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sys_id = 30_000_142u32;
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let pos_cache = Arc::new(BallPositionSyncCache::new_standalone());
    let scram_cache = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashSet::new()));
    let mut bp = Ballpark::new(sys_id);
    let mut sim_elapsed = 0.0;

    // 1. Missing ship_id -> early return, nothing broadcast
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        777_001,
        10,
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert!(rx.try_recv().is_err());

    // 2. Stationary non-invulnerable ship -> early return, nothing broadcast
    let ship_id = 99_003_001_i64;
    bp.add_ball(make_ship_ball(ship_id, DVec3::ZERO, MovementMode::Stop));
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        10,
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert!(rx.try_recv().is_err());

    // 3. Stationary invulnerable ship -> steps until undock invuln expires and broadcasts
    let mut cs = CombatState::new();
    cs.undock_invulnerable_remaining_ticks = 3;
    bp.combat_states.insert(ship_id, cs);
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        100, // clamped to 65
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert!(!bp.combat_states[&ship_id].is_invulnerable());
    assert!(rx.try_recv().is_ok());

    // 4. Moving ship (GotoPoint) -> advances, clamps current_tick, prepends SetBallPosition & SetBallVelocity
    let _ = bp.goto_point(ship_id, DVec3::new(10_000.0, 0.0, 0.0));
    let tick_before = bp.current_tick;
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        5,
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert_eq!(bp.current_tick, tick_before);
    let bcast = rx.try_recv().expect("moving ship broadcast");
    assert!(matches!(
        bcast.events.first(),
        Some(DestinyEvent::SetBallPosition { ball_id, .. }) if *ball_id == ship_id
    ));
    assert!(matches!(
        bcast.events.get(1),
        Some(DestinyEvent::SetBallVelocity { ball_id, .. }) if *ball_id == ship_id
    ));
    assert!(
        bcast.observer_events.contains_key(&ship_id),
        "when had_set_state is false, observer events are drained into TickBroadcast"
    );

    // 4b. Moving ship with pending SetState in observer_events -> re-queues SetState at index 0 and preserves queue
    let _ = bp.goto_point(ship_id, DVec3::new(20_000.0, 0.0, 0.0));
    bp.queue_set_state(ship_id);
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        3,
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_cache,
        &scram_cache,
        &tx,
    );
    let bcast_ss = rx.try_recv().expect("moving ship with SetState broadcast");
    assert!(
        !bcast_ss.observer_events.contains_key(&ship_id),
        "when had_set_state is true, SetState observer queue must not be drained by build_tick_broadcast"
    );
    assert!(matches!(
        bp.observer_events.get(&ship_id).and_then(|evs| evs.first()),
        Some(DestinyEvent::SetState { observer_ball_id }) if *observer_ball_id == ship_id
    ));

    // 5. Warping ship exits warp and persists landing coords in DB
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let dest = DVec3::new(1_000_000.0, 0.0, 0.0);
    let _ = bp.set_mode(ship_id, MovementMode::new_warp(DVec3::ZERO, dest, 3.0));
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        300,
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_cache,
        &scram_cache,
        &tx,
    );
    assert!(rx.try_recv().is_ok());
    assert!(db.with_snapshot(|snap| snap.item_positions.contains_key(&ship_id)));

    // 6. SystemHandle::force_stop_warp error & success paths
    let registry = SystemRegistry::new();
    let handle = registry.get_or_spawn(sys_id);
    assert!(handle.force_stop_warp(999_999_999).is_err());

    let warp_ship = 99_003_002_i64;
    let _ = handle
        .spawn_ball(make_ship_ball(warp_ship, DVec3::ZERO, MovementMode::Stop))
        .await;
    {
        let mut guard = handle.ballpark.write().unwrap();
        let _ = guard.set_mode(
            warp_ship,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(2_000_000.0, 0.0, 0.0), 3.0),
        );
    }
    let tick_after_force = handle.force_stop_warp(warp_ship).expect("force_stop_warp ok");
    assert_eq!(tick_after_force, handle.ballpark.read().unwrap().current_tick);
    let snap = handle.get_ball_snapshot(warp_ship).await.unwrap().unwrap();
    assert!(matches!(snap.1.mode, MovementMode::Stop));
    assert_eq!(snap.1.velocity, DVec3::ZERO);
}

#[test]
fn persist_ball_positions_and_landed_warps_kill_all_mutants() {
    use eve_destiny::{SlimItemMeta, WarpPhase};

    assert_eq!(SUBWARP_FLUSH_DISPLACEMENT_SQ_M2, 10_000.0);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sys_id: i64 = 30_000_142;

    let id_moved: BallId = 900_001;
    let id_tiny: BallId = 900_002;
    let id_cat11: BallId = 900_003;
    let id_nan: BallId = 900_004;
    let id_static: BallId = 900_005;
    let id_warp_cruise: BallId = 900_006;
    let id_warp_align: BallId = 900_007;
    let id_stop_zero: BallId = 900_008;
    let id_subwarp_near: BallId = 900_009;
    let id_subwarp_far: BallId = 900_010;
    let id_stop_imm_ok: BallId = 900_011;
    let id_stop_imm_fast: BallId = 900_012;
    let id_setpos_tiny: BallId = 900_013;
    let id_setpos_ok: BallId = 900_014;
    let all_ids = [
        id_moved,
        id_tiny,
        id_cat11,
        id_nan,
        id_static,
        id_warp_cruise,
        id_warp_align,
        id_stop_zero,
        id_subwarp_near,
        id_subwarp_far,
        id_stop_imm_ok,
        id_stop_imm_fast,
        id_setpos_tiny,
        id_setpos_ok,
    ];

    db.with_mut_snapshot(|snap| {
        for &id in &all_ids {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 587,
                    owner_id: 1001,
                    location_id: sys_id,
                    flag: InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
            snap.item_positions.insert(id, [100.0, 200.0, 300.0]);
        }
    });

    let mut bp = Ballpark::new(sys_id as u32);
    bp.current_tick = 1; // NOT % 5

    let add_b = |bp: &mut Ballpark, id: BallId, pos: DVec3, vel: DVec3, mode: MovementMode| {
        let mut b = make_ship_ball(id, pos, mode);
        b.velocity = vel;
        bp.add_ball(b);
    };

    add_b(&mut bp, id_moved, DVec3::new(100.02, 200.0, 300.0), DVec3::ZERO, MovementMode::Stop);
    add_b(&mut bp, id_tiny, DVec3::new(100.0005, 200.0, 300.0), DVec3::ZERO, MovementMode::Stop);
    add_b(&mut bp, id_cat11, DVec3::new(500.0, 200.0, 300.0), DVec3::ZERO, MovementMode::Stop);
    bp.ball_slim_meta.insert(
        id_cat11,
        SlimItemMeta {
            category_id: 11,
            ..Default::default()
        },
    );
    add_b(&mut bp, id_nan, DVec3::new(f64::NAN, 200.0, 300.0), DVec3::ZERO, MovementMode::Stop);

    let empty_epochs = HashMap::new();
    // 1. Inside write lock on current thread, persist_ball_positions_in_db returns immediately
    db.with_mut_snapshot(|_| {
        persist_ball_positions_in_db(&bp, &[id_moved], &Some(db.clone()), &empty_epochs);
    });
    db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions[&id_moved], [100.0, 200.0, 300.0]);
    });

    // 2. Outside write lock: updates moved (>1e-6), skips tiny (<=1e-6), cat 11, NaN
    persist_ball_positions_in_db(
        &bp,
        &[id_moved, id_tiny, id_cat11, id_nan],
        &Some(db.clone()),
        &empty_epochs,
    );
    db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions[&id_moved], [100.02, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_tiny], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_cat11], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_nan], [100.0, 200.0, 300.0]);
    });

    // Reset id_moved and clear id_stop_zero prior position for persist_landed_warps_in_db setup
    db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(id_moved, [100.0, 200.0, 300.0]);
        snap.item_positions.remove(&id_stop_zero);
    });

    add_b(
        &mut bp,
        id_static,
        DVec3::new(900.0, 200.0, 300.0),
        DVec3::new(10.0, 0.0, 0.0),
        MovementMode::GotoDirection { direction: DVec3::X },
    );
    bp.static_ball_ids.insert(id_static);

    add_b(
        &mut bp,
        id_warp_cruise,
        DVec3::new(900.0, 200.0, 300.0),
        DVec3::new(1000.0, 0.0, 0.0),
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e9, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e9,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 5.0,
        },
    );
    add_b(
        &mut bp,
        id_warp_align,
        DVec3::new(900.0, 200.0, 300.0),
        DVec3::new(50.0, 0.0, 0.0),
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e9, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e9,
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 1.0,
        },
    );
    add_b(
        &mut bp,
        id_stop_zero,
        DVec3::new(900.0, 200.0, 300.0),
        DVec3::ZERO,
        MovementMode::Stop,
    );
    add_b(
        &mut bp,
        id_subwarp_near,
        DVec3::new(199.9, 200.0, 300.0), // disp_sq = 99.9^2 < 10_000.0
        DVec3::new(50.0, 0.0, 0.0),
        MovementMode::GotoDirection { direction: DVec3::X },
    );
    add_b(
        &mut bp,
        id_subwarp_far,
        DVec3::new(200.0, 200.0, 300.0), // disp_sq = 100.0^2 >= 10_000.0
        DVec3::new(50.0, 0.0, 0.0),
        MovementMode::GotoDirection { direction: DVec3::X },
    );
    add_b(
        &mut bp,
        id_stop_imm_ok,
        DVec3::new(101.0, 200.0, 300.0),
        DVec3::new(0.5, 0.0, 0.0), // vel_sq = 0.25 <= 1.0
        MovementMode::Stop,
    );
    add_b(
        &mut bp,
        id_stop_imm_fast,
        DVec3::new(101.0, 200.0, 300.0),
        DVec3::new(2.0, 0.0, 0.0), // vel_sq = 4.0 > 1.0
        MovementMode::Stop,
    );
    add_b(
        &mut bp,
        id_setpos_tiny,
        DVec3::new(100.0005, 200.0, 300.0), // delta_sq = 2.5e-7 <= 1e-6
        DVec3::ZERO,
        MovementMode::Stop,
    );
    add_b(
        &mut bp,
        id_setpos_ok,
        DVec3::new(100.02, 200.0, 300.0), // delta_sq = 4e-4 > 1e-4
        DVec3::new(1000.0, 0.0, 0.0),
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e9, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e9,
            phase: WarpPhase::Cruising,
            elapsed_warp_time: 5.0,
        },
    );

    let evs = vec![
        DestinyEvent::SetBallPosition {
            ball_id: id_setpos_tiny,
            position: [100.0005, 200.0, 300.0],
        },
        DestinyEvent::SetBallPosition {
            ball_id: id_setpos_ok,
            position: [100.02, 200.0, 300.0],
        },
    ];

    // Write-locked skips completely
    db.with_mut_snapshot(|_| {
        persist_landed_warps_in_db(&bp, &evs, &Some(db.clone()), &empty_epochs);
    });
    db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions[&id_stop_imm_ok], [100.0, 200.0, 300.0]);
    });

    // Tick 1: immediate SetBallPosition/Stop + >= SUBWARP_FLUSH_DISPLACEMENT_SQ_M2 flush
    persist_landed_warps_in_db(&bp, &evs, &Some(db.clone()), &empty_epochs);
    db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions[&id_stop_imm_ok], [101.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_stop_imm_fast], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_setpos_tiny], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_setpos_ok], [100.02, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_subwarp_near], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_subwarp_far], [200.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_static], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_cat11], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_nan], [100.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_warp_cruise], [100.0, 200.0, 300.0]);
        assert!(!snap.item_positions.contains_key(&id_stop_zero));
    });

    // Tick 5 (% 5 == 0): id_subwarp_near (<100m disp), id_warp_align flushes; id_warp_cruise stays skipped, id_stop_zero has no DB row
    bp.current_tick = 5;
    persist_landed_warps_in_db(&bp, &[], &Some(db.clone()), &empty_epochs);
    db.with_snapshot(|snap| {
        assert_eq!(snap.item_positions[&id_subwarp_near], [199.9, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_warp_align], [900.0, 200.0, 300.0]);
        assert_eq!(snap.item_positions[&id_warp_cruise], [100.0, 200.0, 300.0]);
        assert!(!snap.item_positions.contains_key(&id_stop_zero));
    });

    // 3. fast_forward_clock_in_actor advances tick, sim_elapsed_secs, ball position, and shared caches
    let ball_pos_map = Arc::new(BallPositionSyncCache::new_standalone());
    let scrambled_map = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashSet::new()));
    let tick_before = bp.current_tick;
    let mut sim_elapsed = 10.0;
    let (ev_tx, _) = tokio::sync::broadcast::channel::<crate::system_actor::TickBroadcast>(16);
    fast_forward_clock_in_actor(
        sys_id as u32,
        &mut bp,
        id_subwarp_far,
        4,
        &mut sim_elapsed,
        &Some(db.clone()),
        &ball_pos_map,
        &scrambled_map,
        &ev_tx,
    );
    assert!((sim_elapsed - 14.0).abs() < 1e-6);
    assert_eq!(bp.current_tick, tick_before);
    assert!(bp.balls[&id_subwarp_far].position.x > 200.0);
    assert!(ball_pos_map.read().unwrap().get(&id_subwarp_far).is_some());
}

#[test]
fn incremental_sync_ball_positions_tracks_dynamic_moves_and_static_changes() {
    let mut bp = Ballpark::new(30_000_142);
    bp.add_static_ball(
        Ball {
            id: 40_000_001,
            type_id: 1230,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        25,
    );
    bp.add_ball(Ball {
        id: 500_001,
        type_id: 587,
        position: DVec3::new(0.0, 100.0, 0.0),
        velocity: DVec3::X * 200.0,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    });

    let scram = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashSet::new()));
    let warps = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashMap::new()));
    let tethered = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashSet::new()));
    let cache = Arc::new(BallPositionSyncCache::new(Arc::clone(&warps), tethered));

    // 1. Initial populate
    sync_ball_positions(&bp, &cache, &scram);
    assert_eq!(cache.read().unwrap().len(), 2);
    assert_eq!(cache.read().unwrap()[&40_000_001], DVec3::new(10_000.0, 0.0, 0.0));

    // 2. Dynamic movement + swap without static change
    let _ = bp.step(1.0);
    sync_ball_positions(&bp, &cache, &scram);
    assert_eq!(
        cache.read().unwrap()[&500_001],
        bp.balls[&500_001].position
    );

    // 3. Remove dynamic 500_001 and add dynamic 500_002 in the same tick (same total len!)
    bp.remove_ball(500_001);
    bp.add_ball(Ball {
        id: 500_002,
        type_id: 587,
        position: DVec3::new(50.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1e11, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1e11,
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 2.0,
        },
    });
    sync_ball_positions(&bp, &cache, &scram);
    assert!(!cache.read().unwrap().contains_key(&500_001));
    assert_eq!(cache.read().unwrap()[&500_002], DVec3::new(50.0, 0.0, 0.0));
    assert_eq!(warps.read().unwrap().get(&500_002), Some(&DVec3::new(1e11, 0.0, 0.0)));

    // 4. Static removal + addition updates cache cleanly via static_epoch
    bp.remove_ball(40_000_001);
    bp.add_static_ball(
        Ball {
            id: 40_000_002,
            type_id: 1230,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        25,
    );
    sync_ball_positions(&bp, &cache, &scram);
    assert!(!cache.read().unwrap().contains_key(&40_000_001));
    assert_eq!(cache.read().unwrap()[&40_000_002], DVec3::new(20_000.0, 0.0, 0.0));
}

#[test]
fn fast_forward_clock_preserves_tick_stamp_across_repeated_calls() {
    let mut bp = Ballpark::new(30_000_142);
    bp.current_tick = 1_000;
    let ship_id = 1_897_140_648;
    bp.add_ball(Ball {
        id: ship_id,
        type_id: 11_567,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 70.0,
        inertia: 0.034,
        mass: 2.275e9,
        radius: 7_500.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });
    let cache = Arc::new(BallPositionSyncCache::new_standalone());
    let scram = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashSet::new()));
    let (tx, mut rx) = tokio::sync::broadcast::channel(16);
    let mut sim_elapsed = 0.0;

    // Repeated AdvanceClock calls must advance simulation state without pushing
    // `ballpark.current_tick` cumulatively ahead of client Destiny `currentTime` (`#1159`, `#1165`).
    for call_idx in 1..=5 {
        fast_forward_clock_in_actor(
            30_000_142,
            &mut bp,
            ship_id,
            10,
            &mut sim_elapsed,
            &None,
            &cache,
            &scram,
            &tx,
        );
        assert_eq!(
            bp.current_tick, 1_000,
            "call {call_idx} must not advance current_tick ahead of client clock"
        );
        let broadcast = rx.try_recv().expect("expected tick broadcast");
        assert_eq!(broadcast.tick, 1_000);
        assert!(
            broadcast
                .events
                .iter()
                .any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == ship_id))
        );
    }
    assert!((sim_elapsed - 50.0).abs() < 1e-9);
    assert!(bp.balls[&ship_id].position.x > 0.0);
}

#[test]
fn fast_forward_clock_passively_recharges_stationary_ship_past_300s_cap_without_tick_broadcast() {
    let sys_id = 30_004_759u32;
    let ship_id = 99_003_901i64;
    let db = Arc::new(WorldRepository::with_demo_seed());
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 587,
                owner_id: 90_000_001,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: std::collections::HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        crate::sp6_lifecycle::enter_space_in_snap(snap, ship_id, sys_id);
    });
    let pos_map = Arc::new(BallPositionSyncCache::new_standalone());
    let scram_cache = Arc::new(eve_db::lock_rank::RankedRwLock::new(HashSet::new()));
    let (tx, mut rx) = tokio::sync::broadcast::channel::<crate::system_actor::TickBroadcast>(16);
    let mut bp = Ballpark::new(sys_id);
    bp.add_ball(make_ship_ball(ship_id, DVec3::ZERO, MovementMode::Stop));
    let mut cs = CombatState::new();
    cs.max_capacitor = 168_750.0;
    cs.capacitor = 100.0;
    cs.capacitor_recharge_time_ms = 5_625_000.0;
    cs.max_shield_hp = 200_000.0;
    cs.shield_hp = 10_000.0;
    cs.shield_recharge_time_ms = 2_500_000.0;
    bp.combat_states.insert(ship_id, cs);

    let tick_before = bp.current_tick;
    let mut sim_elapsed = 0.0;
    fast_forward_clock_in_actor(
        sys_id,
        &mut bp,
        ship_id,
        6_000,
        &mut sim_elapsed,
        &Some(Arc::clone(&db)),
        &pos_map,
        &scram_cache,
        &tx,
    );

    assert_eq!(sim_elapsed, 0.0, "stationary fast-forward must not advance sim_elapsed");
    assert_eq!(bp.current_tick, tick_before, "#1159/#1165: current_tick must stay anchored");
    assert!(rx.try_recv().is_err(), "stationary recharge must not flood TickBroadcast");
    let post = &bp.combat_states[&ship_id];
    assert!(
        post.capacitor > 165_000.0,
        "6000s passive recharge over 5625s recharge time must refill Titan cap, got {}",
        post.capacitor
    );
    assert!(
        post.shield_hp > 195_000.0,
        "6000s passive shield recharge over 2500s recharge time must refill shield, got {}",
        post.shield_hp
    );
    let persisted = db
        .with_snapshot(|s| s.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied())
        .expect("SP6 persisted HP should be updated after passive fast-forward recharge");
    assert!((persisted.shield_hp - post.shield_hp).abs() < 1e-3);
}

#[test]
fn clear_landed_warp_tracking_from_events_emits_warp_finished_for_return_warps_but_not_emergency() {
    use crate::system_actor::types::{ActorWarpState, WarpProvenance};
    use std::collections::HashMap;

    let mut ballpark = Ballpark::new(30_000_142);
    let ship_return_prov = 1_001_i64;
    let ship_return_set_only = 1_002_i64;
    let ship_emergency = 1_003_i64;
    let ship_still_warping = 1_004_i64;
    let ship_player_warp = 1_005_i64;

    ballpark.add_ball(make_ship_ball(ship_return_prov, DVec3::ZERO, MovementMode::Stop));
    ballpark.add_ball(make_ship_ball(
        ship_return_set_only,
        DVec3::ZERO,
        MovementMode::Stop,
    ));
    ballpark.add_ball(make_ship_ball(ship_emergency, DVec3::ZERO, MovementMode::Stop));
    ballpark.add_ball(make_ship_ball(
        ship_still_warping,
        DVec3::new(100_000.0, 0.0, 0.0),
        MovementMode::new_warp(
            DVec3::new(100_000.0, 0.0, 0.0),
            DVec3::new(10_000_000.0, 0.0, 0.0),
            3.0,
        ),
    ));
    ballpark.add_ball(make_ship_ball(ship_player_warp, DVec3::ZERO, MovementMode::Stop));

    let mut warp_state = ActorWarpState {
        provenance: HashMap::from([
            (ship_return_prov, WarpProvenance::ReconnectReturnWarp),
            (ship_emergency, WarpProvenance::DisconnectedEmergencyWarp),
            (ship_still_warping, WarpProvenance::PlayerOrFleetWarp),
            (ship_player_warp, WarpProvenance::PlayerOrFleetWarp),
        ]),
        active_warps: HashMap::from([
            (ship_still_warping, 90_000_001_i64),
            (ship_player_warp, 90_000_002_i64),
        ]),
        return_warps: HashSet::from([ship_return_prov, ship_return_set_only]),
        pending_warp_finished: HashSet::new(),
    };

    let events = vec![
        DestinyEvent::Stop {
            ball_id: ship_return_prov,
        },
        DestinyEvent::OnDeactivatingWarp {
            ball_id: ship_return_prov,
        },
        DestinyEvent::Stop {
            ball_id: ship_return_set_only,
        },
        DestinyEvent::OnDeactivatingWarp {
            ball_id: ship_return_set_only,
        },
        DestinyEvent::Stop {
            ball_id: ship_emergency,
        },
        DestinyEvent::OnDeactivatingWarp {
            ball_id: ship_emergency,
        },
        // Lead-in OnDeactivatingWarp while ship_still_warping is still in Warp mode (no Stop): must not clear.
        DestinyEvent::OnDeactivatingWarp {
            ball_id: ship_still_warping,
        },
        DestinyEvent::Stop {
            ball_id: ship_player_warp,
        },
        DestinyEvent::OnDeactivatingWarp {
            ball_id: ship_player_warp,
        },
    ];

    clear_landed_warp_tracking_from_events(&ballpark, &events, &mut warp_state);

    assert!(!warp_state.provenance.contains_key(&ship_return_prov));
    assert!(!warp_state.provenance.contains_key(&ship_emergency));
    assert!(!warp_state.provenance.contains_key(&ship_player_warp));
    assert_eq!(
        warp_state.provenance.get(&ship_still_warping),
        Some(&WarpProvenance::PlayerOrFleetWarp)
    );

    assert!(warp_state.return_warps.is_empty());
    assert!(warp_state.active_warps.contains_key(&ship_still_warping));
    assert!(!warp_state.active_warps.contains_key(&ship_player_warp));

    let pending = &warp_state.pending_warp_finished;
    assert!(
        pending.contains(&ship_return_prov),
        "ReconnectReturnWarp landing must defer OnWarpFinished via pending_warp_finished (#1454)"
    );
    assert!(
        pending.contains(&ship_return_set_only),
        "return_warps tracking without provenance entry must still defer OnWarpFinished (#1454)"
    );
    assert!(
        pending.contains(&ship_player_warp),
        "PlayerOrFleetWarp landing must defer OnWarpFinished"
    );
    assert!(
        !pending.contains(&ship_emergency),
        "DisconnectedEmergencyWarp must NOT emit OnWarpFinished"
    );
    assert!(
        !pending.contains(&ship_still_warping),
        "in-flight warp without Stop event must NOT queue OnWarpFinished"
    );
}

#[tokio::test]
async fn actor_stop_restart_isolates_ball_position_velocity_tether_and_warp_caches() {
    let registry = Arc::new(SystemRegistry::new());
    let sys_id = 30_000_142_u32;
    let ship_id = 99_901_001_i64;
    let dest = DVec3::new(1_000_000_000.0, 250_000.0, -500_000.0);

    let h1 = registry.get_or_spawn(sys_id);
    let mut ship_ball = make_ship_ball(
        ship_id,
        DVec3::new(1_000.0, 2_000.0, 3_000.0),
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: dest,
            warp_speed_au_s: 3.0,
            total_distance: dest.length(),
            phase: eve_destiny::WarpPhase::Cruising,
            elapsed_warp_time: 4.0,
        },
    );
    ship_ball.velocity = DVec3::new(150.0, -25.0, 40.0);
    let mut cs = CombatState::new();
    cs.is_tethered = true;
    h1.spawn_ball_with_combat(ship_ball, cs).await.unwrap();

    assert_eq!(h1.get_ball_velocity_sync(ship_id), Some([150.0, -25.0, 40.0]));
    assert_eq!(
        h1.ball_positions.get_velocity(ship_id),
        Some([150.0, -25.0, 40.0])
    );
    assert!(h1.is_ball_tethered_sync(ship_id));
    assert!(h1.tethered_balls.read().unwrap().contains(&ship_id));
    assert_eq!(h1.committed_warp_destination_sync(ship_id), Some(dest));
    assert_eq!(
        h1.committed_warp_balls.read().unwrap().get(&ship_id).copied(),
        Some(dest)
    );
    drop(h1);

    assert!(registry.stop_system(sys_id).await);
    let h2 = registry.get_or_spawn(sys_id);
    assert_eq!(h2.get_ball_velocity_sync(ship_id), None);
    assert_eq!(h2.ball_positions.get_velocity(ship_id), None);
    assert!(!h2.is_ball_tethered_sync(ship_id));
    assert!(!h2.tethered_balls.read().unwrap().contains(&ship_id));
    assert_eq!(h2.committed_warp_destination_sync(ship_id), None);
    assert!(!h2.committed_warp_balls.read().unwrap().contains_key(&ship_id));
}

#[tokio::test]
async fn two_world_repositories_isolate_try_remove_ball_for_db() {
    let pair = eve_testkit::process_global_ratchet::TwoRepositoryPair::with_demo_seed_isolated();
    let db_a = pair.left_repo();
    let db_b = pair.right_repo();
    let sys_id = 30_000_142_u32;
    let ball_id = 99_901_002_i64;

    let reg_a = SystemRegistry::new();
    let reg_b = SystemRegistry::new();
    let h_a = reg_a.get_or_spawn(sys_id);
    let h_b = reg_b.get_or_spawn(sys_id);
    register_db_system_ballpark(db_a, sys_id, &h_a);
    register_db_system_ballpark(db_b, sys_id, &h_b);

    h_a.spawn_ball(make_ship_ball(
        ball_id,
        DVec3::new(100.0, 0.0, 0.0),
        MovementMode::Stop,
    ))
    .await
    .unwrap();
    h_b.spawn_ball(make_ship_ball(
        ball_id,
        DVec3::new(200.0, 0.0, 0.0),
        MovementMode::Stop,
    ))
    .await
    .unwrap();
    assert!(h_a.get_ball_snapshot(ball_id).await.unwrap().is_some());
    assert!(h_b.get_ball_snapshot(ball_id).await.unwrap().is_some());

    try_remove_ball_for_db(db_a, sys_id, ball_id);

    assert!(h_a.get_ball_snapshot(ball_id).await.unwrap().is_none());
    assert!(h_b.get_ball_snapshot(ball_id).await.unwrap().is_some());

    let weak_slot = db_a
        .get_transient_runtime_slot::<DbBallparkRegistry>(DB_BALLPARK_REGISTRY_SLOT)
        .flatten()
        .map(|slot| Arc::downgrade(&slot))
        .expect("db_a DbBallparkRegistry slot initialized");
    assert!(weak_slot.upgrade().is_some());
    drop(h_a);
    drop(reg_a);
    drop(h_b);
    drop(reg_b);
    drop(pair);
    assert!(
        weak_slot.upgrade().is_none(),
        "dropping WorldRepository must free DbBallparkRegistry transient runtime slot"
    );
}


