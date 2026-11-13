//! Performance, scaling, spatial-collision, cloak-visibility, and observer-queue tests for [`Ballpark`].

use eve_destiny::{
    Ball, BallId, Ballpark, CombatState, DestinyEvent, MovementMode, SlimItemMeta, WarpPhase,
    AU_METERS,
};
use glam::DVec3;
use std::time::Instant;

fn make_static_ball(id: BallId, pos: DVec3, radius: f64) -> Ball {
    Ball {
        id,
        type_id: 1230,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1e12,
        radius,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    }
}

fn make_dynamic_ship(id: BallId, pos: DVec3, dir: DVec3, max_vel: f64) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: dir.normalize_or_zero() * max_vel,
        max_velocity: max_vel,
        inertia: 3.2,
        mass: 1_200_000.0,
        radius: 45.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: dir.normalize_or_zero(),
        },
    }
}

fn populate_static_system(park: &mut Ballpark, count: usize) {
    for i in 0..count {
        let angle = (i as f64) * 0.618_033_988_75;
        let ring = ((i / 50) as f64 + 1.0) * 0.25 * AU_METERS;
        let pos = DVec3::new(
            ring * angle.cos(),
            ((i % 7) as f64 - 3.0) * 10_000.0,
            ring * angle.sin(),
        );
        let radius = if i % 100 == 0 { 500_000.0 } else { 250.0 };
        let group_id = if i % 100 == 0 { 7 } else { 25 };
        park.add_static_ball(make_static_ball(1_000_000 + i as i64, pos, radius), group_id);
    }
}

#[test]
fn zero_dynamic_balls_fast_path_with_many_statics() {
    let mut park = Ballpark::new(30_000_142);
    populate_static_system(&mut park, 3_000);
    assert!(!park.has_dynamic_balls());

    let start_tick = park.current_tick;
    let start = Instant::now();
    for _ in 0..50 {
        let evs = park.step(1.0);
        assert!(evs.is_empty());
    }
    let elapsed = start.elapsed();
    assert_eq!(park.current_tick, start_tick + 50);
    assert!(park.observer_events.is_empty());
    eprintln!(
        "[bench] zero-dynamic 3000-static 50 ticks: {:.3} ms ({:.1} us/tick)",
        elapsed.as_secs_f64() * 1e3,
        elapsed.as_secs_f64() * 1e6 / 50.0
    );
}

#[test]
fn step_time_scales_linearly_with_dynamic_balls() {
    fn measure_ns_per_tick(dynamic_count: usize) -> f64 {
        let mut park = Ballpark::new(30_000_142);
        populate_static_system(&mut park, 2_000);

        // Spread ships across 4 grids (spaced by 20,000 km > 16,000 km grid) with ~5 km cell spacing
        // so both dynamic-dynamic spatial cells and cross-grid isolation are exercised.
        let grid_offsets = [
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(30_000_000.0, 0.0, 0.0),
            DVec3::new(0.0, 0.0, 30_000_000.0),
            DVec3::new(30_000_000.0, 0.0, 30_000_000.0),
        ];
        for i in 0..dynamic_count {
            let g = grid_offsets[i % grid_offsets.len()];
            let local_idx = i / grid_offsets.len();
            let col = (local_idx % 20) as f64;
            let row = (local_idx / 20) as f64;
            let pos = g + DVec3::new(col * 5_000.0, (i % 3) as f64 * 1_500.0, row * 5_000.0);
            let dir = DVec3::new(1.0, 0.0, 0.25);
            let ship = make_dynamic_ship(10_000 + i as i64, pos, dir, 250.0);
            park.add_ball(ship);
        }
        park.drain_all_observer_events();

        // Warm up caches/allocators
        for _ in 0..3 {
            let _ = park.step(1.0);
            park.drain_all_observer_events();
        }

        let ticks = 20;
        let t0 = Instant::now();
        for _ in 0..ticks {
            let _ = park.step(1.0);
            park.drain_all_observer_events();
        }
        t0.elapsed().as_nanos() as f64 / ticks as f64
    }

    let ns_50 = measure_ns_per_tick(50);
    let ns_200 = measure_ns_per_tick(200);
    let ns_800 = measure_ns_per_tick(800);
    let ratio_200_50 = ns_200 / ns_50.max(1.0);
    let ratio_800_200 = ns_800 / ns_200.max(1.0);
    let ratio_800_50 = ns_800 / ns_50.max(1.0);

    eprintln!(
        "[bench] step(1.0) with 2000 statics: D=50 {:.3} ms/tick | D=200 {:.3} ms/tick ({:.2}x for 4x D) | D=800 {:.3} ms/tick ({:.2}x for 4x D, {:.2}x for 16x D)",
        ns_50 / 1e6,
        ns_200 / 1e6,
        ratio_200_50,
        ns_800 / 1e6,
        ratio_800_200,
        ratio_800_50
    );

    // Pure quadratic growth would give ~16x for 4x D and ~256x for 16x D.
    // Linear growth gives ~4x for 4x D (allow comfortable CI/debug headroom < 8x / < 36x).
    assert!(
        ratio_800_200 < 8.0,
        "expected roughly linear 4x scaling from D=200 to D=800, got {ratio_800_200:.2}x"
    );
    assert!(
        ratio_800_50 < 36.0,
        "expected sub-quadratic scaling from D=50 to D=800 (16x D), got {ratio_800_50:.2}x"
    );
}

#[test]
fn grid_bucketed_collision_handles_cell_boundaries_and_static_spheres() {
    let mut park = Ballpark::new(30_000_142);

    // 1. Two ships crossing a 4,000 m spatial cell boundary (x = 4,000.0) head-on must collide
    let mut s1 = make_dynamic_ship(1, DVec3::new(3_920.0, 0.0, 0.0), DVec3::X, 200.0);
    s1.radius = 50.0;
    let mut s2 = make_dynamic_ship(2, DVec3::new(4_080.0, 0.0, 0.0), DVec3::NEG_X, 200.0);
    s2.radius = 50.0;
    park.add_ball(s1);
    park.add_ball(s2);

    // 2. Dynamic ship colliding with a large static station (spanning > 27 cells) must bounce
    let station_pos = DVec3::new(500_000.0, 0.0, 0.0);
    park.add_static_ball_with_meta(
        make_static_ball(9001, station_pos, 15_000.0),
        SlimItemMeta {
            group_id: 15,
            category_id: 3,
            ..Default::default()
        },
    );
    let mut bumper = make_dynamic_ship(
        3,
        station_pos + DVec3::new(-15_100.0, 0.0, 0.0),
        DVec3::X,
        300.0,
    );
    bumper.radius = 60.0;
    park.add_ball(bumper);

    // 3. Ship aligning for warp inside a static hull clearance zone must NOT bounce back
    let mut aligner = make_dynamic_ship(
        4,
        station_pos + DVec3::new(0.0, 15_010.0, 0.0),
        DVec3::NEG_Y,
        300.0,
    );
    aligner.radius = 50.0;
    aligner.mode = MovementMode::Warp {
        origin: aligner.position,
        destination: station_pos - DVec3::new(0.0, 1.0 * AU_METERS, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1.0 * AU_METERS,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    park.add_ball(aligner);

    let events = park.step(1.0);
    let has_ship_pair = events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Collision { ball_a: 1, ball_b: 2 }));
    let has_station_bump = events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Collision { ball_a, ball_b } if (*ball_a == 3 && *ball_b == 9001) || (*ball_a == 9001 && *ball_b == 3)));
    let has_aligner_bounce = events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Collision { ball_a, ball_b } if *ball_a == 4 || *ball_b == 4));
    assert!(has_ship_pair, "cross-cell head-on collision must be detected: {events:?}");
    assert!(has_station_bump, "large static station collision must be detected: {events:?}");
    assert!(!has_aligner_bounce, "warp-aligning ship must clear static station sphere");
}

#[test]
fn cloaked_arrival_not_announced_and_zero_per_tick_remove_balls() {
    let mut park = Ballpark::new(30_000_142);

    // Observer in grid 1
    let observer = make_dynamic_ship(100, DVec3::new(0.0, 0.0, 0.0), DVec3::ZERO, 100.0);
    park.add_ball(observer);
    let _ = park.drain_all_observer_events();

    // Cloaked intruder spawns > 2 km away (outside proximity decloak) on the same grid
    let cloaker_id = 200;
    park.combat_states.insert(
        cloaker_id,
        CombatState {
            is_cloaked: true,
            is_covert_cloak: true,
            ..Default::default()
        },
    );
    let cloaker = make_dynamic_ship(cloaker_id, DVec3::new(10_000.0, 0.0, 0.0), DVec3::X, 200.0);
    park.add_ball(cloaker);

    // Observer must NOT receive AddBalls / AddBalls2 for the cloaked arrival, nor global AddBall
    let spawn_obs = park.drain_observer_events(100);
    assert!(
        spawn_obs.is_empty(),
        "cloaked arrival must not announce AddBalls/AddBalls2 to observer: {spawn_obs:?}"
    );
    assert!(
        !park
            .pending_events
            .iter()
            .any(|ev| matches!(ev, DestinyEvent::AddBall { ball } if ball.id == cloaker_id)),
        "cloaked arrival must not push global AddBall"
    );

    // Step 20 ticks (including moving across space while cloaked and even another ship crossing grids)
    let passerby = make_dynamic_ship(300, DVec3::new(50_000_000.0, 0.0, 0.0), DVec3::X, 300.0);
    park.add_ball(passerby);
    let _ = park.drain_all_observer_events();

    for tick in 0..20 {
        let _ = park.step(1.0);
        let obs_evs = park.drain_observer_events(100);
        assert!(
            !obs_evs.iter().any(|ev| match ev {
                DestinyEvent::RemoveBalls { ball_ids } => ball_ids.contains(&cloaker_id),
                DestinyEvent::RemoveBall { ball_id } => *ball_id == cloaker_id,
                DestinyEvent::AddBalls { balls } | DestinyEvent::AddBalls2 { balls } => {
                    balls.iter().any(|b| b.id == cloaker_id)
                }
                _ => false,
            }),
            "tick {tick}: cloaked ship must not trigger RemoveBalls or AddBalls: {obs_evs:?}"
        );
    }

    // Now decloak: observer must receive AddBalls2 exactly once
    assert!(park.deactivate_cloak(cloaker_id));
    let _ = park.step(1.0);
    let decloak_evs = park.drain_observer_events(100);
    assert!(
        decloak_evs.iter().any(|ev| match ev {
            DestinyEvent::AddBalls2 { balls } => balls.iter().any(|b| b.id == cloaker_id),
            _ => false,
        }),
        "decloaking must announce AddBalls2 to observer: {decloak_evs:?}"
    );
}

#[test]
fn registered_observer_filtering_and_dead_ball_queue_eviction() {
    let mut park = Ballpark::new(30_000_142);
    park.set_observer_filtering(true);

    // Register 3 player observers
    for obs_id in [1_i64, 2, 3] {
        park.register_observer(obs_id);
        park.add_ball(make_dynamic_ship(
            obs_id,
            DVec3::new(obs_id as f64 * 5_000.0, 0.0, 0.0),
            DVec3::X,
            200.0,
        ));
    }

    // Add 200 unregistered NPCs/drones in the same grid
    for npc_id in 1000..1200 {
        park.add_ball(make_dynamic_ship(
            npc_id,
            DVec3::new((npc_id - 1000) as f64 * 100.0, 5_000.0, 0.0),
            DVec3::X,
            150.0,
        ));
    }

    // Only the 3 registered observers should have entries in observer_events!
    assert_eq!(park.observer_events.len(), 3);
    assert!(park.observer_events.contains_key(&1));
    assert!(park.observer_events.contains_key(&2));
    assert!(park.observer_events.contains_key(&3));
    assert!(!park.observer_events.contains_key(&1050));

    // Remove registered observer 2 while preserving a final queued event
    park.drain_all_observer_events();
    assert!(park.queue_set_state(2));
    assert!(park.remove_ball(2).is_some());
    assert!(!park.observer_events.contains_key(&2));

    // Re-add and verify drain_observer_events drops dead ball's queue after 1 drain
    park.register_observer(4);
    park.add_ball(make_dynamic_ship(4, DVec3::ZERO, DVec3::ZERO, 100.0));
    park.observer_events
        .insert(9999, vec![DestinyEvent::Stop { ball_id: 9999 }]);
    let drained_dead = park.drain_observer_events(9999);
    assert_eq!(drained_dead.len(), 1);
    assert!(
        !park.observer_events.contains_key(&9999),
        "dead observer queue must be evicted after one drain"
    );
}
