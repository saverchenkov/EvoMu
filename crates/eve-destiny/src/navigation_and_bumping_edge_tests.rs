//! Unit and edge coverage tests for ballpark navigation, velocity clamping,
//! grid boundary clamping, and collision bumping physics.

#![cfg(test)]

use crate::{Ball, BallId, Ballpark, DestinyEvent, MovementMode, TargetLock, WarpPhase};
use glam::DVec3;
use std::collections::BTreeMap;

fn make_test_ball(id: BallId, pos: DVec3, vel: DVec3, radius: f64, mass: f64) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: vel,
        max_velocity: 300.0,
        inertia: 2.0,
        mass,
        radius,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn set_aligning_warp(park: &mut Ballpark, id: BallId, vel: DVec3, max_v: f64) {
    let b = park.balls.get_mut(&id).unwrap();
    b.velocity = vel;
    b.max_velocity = max_v;
    b.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e6,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
}

#[test]
fn test_clamp_ball_subwarp_velocity_and_finite_guards() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    assert!(park.add_ball(make_test_ball(1, DVec3::ZERO, DVec3::ZERO, 50.0, 1000.0)));
    park.set_ball_group_id(1, 25);

    set_aligning_warp(&mut park, 1, DVec3::new(f64::NAN, 10.0, 20.0), 300.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::ZERO);

    set_aligning_warp(&mut park, 1, DVec3::new(f64::INFINITY, 0.0, 0.0), 300.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::ZERO);

    set_aligning_warp(&mut park, 1, DVec3::new(50.0, 0.0, 0.0), -10.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::ZERO);

    set_aligning_warp(&mut park, 1, DVec3::new(50.0, 0.0, 0.0), 0.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::ZERO);

    set_aligning_warp(&mut park, 1, DVec3::new(500.0, 0.0, 0.0), 200.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::new(200.0, 0.0, 0.0));

    set_aligning_warp(&mut park, 1, DVec3::new(150.0, 0.0, 0.0), 200.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::new(150.0, 0.0, 0.0));

    set_aligning_warp(&mut park, 1, DVec3::new(200.0, 0.0, 0.0), 200.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::new(200.0, 0.0, 0.0));

    set_aligning_warp(&mut park, 1, DVec3::ZERO, 200.0);
    park.set_mode(1, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&1].velocity, DVec3::ZERO);
}

#[test]
fn test_goto_point_and_align_warp_cancellation() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    let mut ship = make_test_ball(10, DVec3::ZERO, DVec3::ZERO, 50.0, 100_000.0);
    ship.speed_fraction = 0.0;
    assert!(park.add_ball(ship));

    // 1. Non-finite target point fails
    assert!(park
        .goto_point(10, DVec3::new(f64::NAN, 0.0, 0.0))
        .unwrap_err()
        .contains("InvalidPoint"));

    // 2. Unknown ball fails
    assert!(park
        .goto_point(999, DVec3::new(100.0, 0.0, 0.0))
        .unwrap_err()
        .contains("not found"));

    // 3. Static hull cannot navigate
    let mut station = make_test_ball(20, DVec3::new(5000.0, 0.0, 0.0), DVec3::ZERO, 500.0, 1e8);
    station.mode = MovementMode::Rigid;
    assert!(park.add_static_ball(station, 15));
    assert!(park
        .goto_point(20, DVec3::new(6000.0, 0.0, 0.0))
        .unwrap_err()
        .contains("StructureCannotNavigate"));

    // 4. Active warp cannot steer
    park.balls.get_mut(&10).unwrap().mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e9, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e9,
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 5.0,
    };
    assert!(park
        .goto_point(10, DVec3::new(100.0, 0.0, 0.0))
        .unwrap_err()
        .contains("ShipAlreadyInWarp"));

    // 5. Aligning warp cancellation resets emergency warp, clamps velocity, emits events
    park.balls.get_mut(&10).unwrap().mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e9, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e9,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 1.0,
    };
    park.balls.get_mut(&10).unwrap().velocity = DVec3::new(450.0, 0.0, 0.0);
    park.combat_states.entry(10).or_default().is_emergency_warping = true;
    park.pending_events.clear();

    assert!(park.goto_point(10, DVec3::new(1000.0, 0.0, 0.0)).is_ok());

    let b10 = &park.balls[&10];
    assert_eq!(
        b10.mode,
        MovementMode::GotoDirection {
            direction: DVec3::X
        }
    );
    assert_eq!(b10.speed_fraction, 1.0);
    assert_eq!(b10.velocity, DVec3::new(300.0, 0.0, 0.0));
    assert!(!park.combat_states[&10].is_emergency_warping);

    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { ball_id: 10 })));
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::SetBallMassive { ball_id: 10, is_massive: true })));
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::OnSpecialFX { ball_id: 10, is_active: false, .. })));
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::GotoPoint { ball_id: 10, .. })));

    // 6. Target equal to current position uses default DVec3::X direction
    assert!(park.goto_point(10, b10.position).is_ok());
    assert_eq!(park.balls[&10].mode, MovementMode::GotoDirection { direction: DVec3::X });
}

#[test]
fn test_set_mode_orbit_follow_stop_and_warp_guards() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    let ship = make_test_ball(30, DVec3::ZERO, DVec3::ZERO, 50.0, 100_000.0);
    assert!(park.add_ball(ship));

    // Validation errors
    assert!(park.set_mode(999, MovementMode::Stop).is_err());
    assert!(park.set_mode(30, MovementMode::GotoDirection { direction: DVec3::new(f64::NAN, 0.0, 0.0) }).is_err());
    assert!(park.set_mode(30, MovementMode::Follow { target_id: 20, range: -10.0 }).is_err());
    assert!(park.set_mode(30, MovementMode::Follow { target_id: 20, range: f64::NAN }).is_err());
    assert!(park.set_mode(30, MovementMode::Orbit { target_id: 20, range: -50.0 }).is_err());
    assert!(park.set_mode(30, MovementMode::Orbit { target_id: 20, range: f64::INFINITY }).is_err());

    let invalid_warp = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: -1.0,
        total_distance: 1e6,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    assert!(park.set_mode(30, invalid_warp).is_err());

    let valid_warp = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e6,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };

    // Scrambled ship cannot enter warp
    park.set_warp_scramble_strength(30, 2);
    assert!(park
        .set_mode(30, valid_warp.clone())
        .unwrap_err()
        .contains("WarpScrambled"));
    park.set_warp_scramble_strength(30, 0);

    // Non-covert cloaked ship cannot enter warp
    let cs = park.combat_states.entry(30).or_default();
    cs.is_cloaked = true;
    cs.is_covert_cloak = false;
    assert!(park
        .set_mode(30, valid_warp.clone())
        .unwrap_err()
        .contains("CloakedCannotWarp"));
    park.combat_states.get_mut(&30).unwrap().is_cloaked = false;

    // Successful mode transitions
    // Orbit
    park.pending_events.clear();
    assert!(park
        .set_mode(30, MovementMode::Orbit { target_id: 20, range: 1500.0 })
        .is_ok());
    assert_eq!(
        park.balls[&30].mode,
        MovementMode::Orbit { target_id: 20, range: 1500.0 }
    );
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Orbit { ball_id: 30, target_id: 20, range } if (*range - 1500.0).abs() < 1e-4)));

    // Follow
    park.pending_events.clear();
    assert!(park
        .set_mode(30, MovementMode::Follow { target_id: 20, range: 500.0 })
        .is_ok());
    assert_eq!(
        park.balls[&30].mode,
        MovementMode::Follow { target_id: 20, range: 500.0 }
    );
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Follow { ball_id: 30, target_id: 20, range } if (*range - 500.0).abs() < 1e-4)));

    // GotoDirection
    park.pending_events.clear();
    assert!(park
        .set_mode(30, MovementMode::GotoDirection { direction: DVec3::Y })
        .is_ok());
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::GotoDirection { ball_id: 30, .. })));

    // Stop
    park.pending_events.clear();
    assert!(park.set_mode(30, MovementMode::Stop).is_ok());
    assert_eq!(park.balls[&30].speed_fraction, 0.0);
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Stop { ball_id: 30 })));

    // Warp removes target locks and emits lost target events
    park.target_locks.entry(30).or_default().push(TargetLock {
        target_id: 20, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None,
    });
    park.pending_events.clear();
    assert!(park.set_mode(30, valid_warp).is_ok());
    assert!(!park.target_locks.contains_key(&30));
    assert!(park.pending_events.iter().any(|e| matches!(
        e,
        DestinyEvent::OnTarget { mode, source_id: 30, target_id: 20 } if mode == "lost"
    )));
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 30, .. })));
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnActivatingWarp { ball_id: 30 })));

    // Cancelling aligning warp drops gate cloak and emits CloakBall(0)
    park.combat_states.get_mut(&30).unwrap().gate_cloak_remaining_ticks = 3;
    park.pending_events.clear();
    assert!(park.set_mode(30, MovementMode::Stop).is_ok());
    assert_eq!(park.combat_states[&30].gate_cloak_remaining_ticks, 0);
    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::CloakBall { ball_id: 30, cloaked: 0 })));
}

#[test]
fn test_find_or_create_grid_boundary_clamping_all_axes() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 10_000.0);
    let g1 = park.find_or_create_grid_for_position(DVec3::ZERO);
    assert_eq!(park.grids[&g1].min_bounds, DVec3::splat(-10_000.0));
    assert_eq!(park.grids[&g1].max_bounds, DVec3::splat(10_000.0));

    // Querying same point returns existing grid
    assert_eq!(park.find_or_create_grid_for_position(DVec3::ZERO), g1);

    // Adjacent on +X (pos.x > existing.max_bounds.x)
    let g_px = park.find_or_create_grid_for_position(DVec3::new(15_000.0, 0.0, 0.0));
    assert_eq!(park.grids[&g_px].min_bounds.x, 10_000.0);

    // Adjacent on -X (pos.x < existing.min_bounds.x)
    let g_nx = park.find_or_create_grid_for_position(DVec3::new(-15_000.0, 0.0, 0.0));
    assert_eq!(park.grids[&g_nx].max_bounds.x, -10_000.0);

    // Adjacent on +Y (pos.y > existing.max_bounds.y)
    let g_py = park.find_or_create_grid_for_position(DVec3::new(0.0, 15_000.0, 0.0));
    assert_eq!(park.grids[&g_py].min_bounds.y, 10_000.0);

    // Adjacent on -Y (pos.y < existing.min_bounds.y)
    let g_ny = park.find_or_create_grid_for_position(DVec3::new(0.0, -15_000.0, 0.0));
    assert_eq!(park.grids[&g_ny].max_bounds.y, -10_000.0);

    // Adjacent on +Z (pos.z > existing.max_bounds.z)
    let g_pz = park.find_or_create_grid_for_position(DVec3::new(0.0, 0.0, 15_000.0));
    assert_eq!(park.grids[&g_pz].min_bounds.z, 10_000.0);

    // Adjacent on -Z (pos.z < existing.min_bounds.z)
    let g_nz = park.find_or_create_grid_for_position(DVec3::new(0.0, 0.0, -15_000.0));
    assert_eq!(park.grids[&g_nz].max_bounds.z, -10_000.0);
}

#[test]
fn test_step_collision_bumping_dynamics_and_statics() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 250_000.0);

    // 1. Dynamic vs dynamic collision with warp alignment reset
    let mut s1 = make_test_ball(
        101,
        DVec3::new(-20.0, 0.0, 0.0),
        DVec3::new(50.0, 0.0, 0.0),
        30.0,
        1_000.0,
    );
    s1.mode = MovementMode::Warp {
        origin: DVec3::new(-20.0, 0.0, 0.0),
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e6,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 1.8,
    };
    let s2 = make_test_ball(
        102,
        DVec3::new(20.0, 0.0, 0.0),
        DVec3::new(-50.0, 0.0, 0.0),
        30.0,
        2_000.0,
    );
    assert!(park.add_ball(s1));
    assert!(park.add_ball(s2));

    let pre_balls = park.balls.clone();
    let pre_positions: BTreeMap<BallId, (DVec3, DVec3, f64)> = park
        .balls
        .iter()
        .map(|(&id, b)| (id, (b.position, b.velocity, b.radius)))
        .collect();

    park.step_collision_bumping(&pre_balls, &pre_positions, 1.0);

    // Ships pushed apart
    assert!(park.balls[&101].position.x < -20.0);
    assert!(park.balls[&102].position.x > 20.0);
    // Velocities deflected
    assert!(park.balls[&101].velocity.x < 50.0);
    assert!(park.balls[&102].velocity.x > -50.0);

    // Aligning warp elapsed time reset to 0.0
    if let MovementMode::Warp { phase, elapsed_warp_time, .. } = park.balls[&101].mode {
        assert_eq!(phase, WarpPhase::Aligning);
        assert_eq!(elapsed_warp_time, 0.0);
    } else {
        panic!("Ship 101 expected to remain in Aligning warp mode");
    }

    assert!(park
        .pending_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::Collision { ball_a: 101, ball_b: 102 })));

    // 2. Dynamic vs static station collision
    park.pending_events.clear();
    let mut station = make_test_ball(200, DVec3::ZERO, DVec3::ZERO, 500.0, 1e8);
    station.mode = MovementMode::Rigid;
    assert!(park.add_static_ball(station, 15));

    let s3 = make_test_ball(
        201,
        DVec3::new(510.0, 0.0, 0.0),
        DVec3::new(-80.0, 0.0, 0.0),
        20.0,
        1_000.0,
    );
    assert!(park.add_ball(s3));

    let pre_balls_2 = park.balls.clone();
    let pre_positions_2: BTreeMap<BallId, (DVec3, DVec3, f64)> = park
        .balls
        .iter()
        .map(|(&id, b)| (id, (b.position, b.velocity, b.radius)))
        .collect();

    park.step_collision_bumping(&pre_balls_2, &pre_positions_2, 1.0);

    // Station unaffected
    assert_eq!(park.balls[&200].position, DVec3::ZERO);
    assert_eq!(park.balls[&200].velocity, DVec3::ZERO);

    // Ship deflected outwards (+X)
    assert!(park.balls[&201].position.x > 510.0);
    assert!(park.balls[&201].velocity.x > -80.0);

    // 3. Volumetric markers (group 9 Asteroid Belt) do NOT deflect
    let belt = make_test_ball(300, DVec3::new(10_000.0, 0.0, 0.0), DVec3::ZERO, 1000.0, 1e9);
    assert!(park.add_static_ball(belt, 9));
    let s4 = make_test_ball(
        301,
        DVec3::new(10_100.0, 0.0, 0.0),
        DVec3::new(-10.0, 0.0, 0.0),
        50.0,
        1_000.0,
    );
    assert!(park.add_ball(s4));

    let pre_balls_3 = park.balls.clone();
    let pre_positions_3: BTreeMap<BallId, (DVec3, DVec3, f64)> = park
        .balls
        .iter()
        .map(|(&id, b)| (id, (b.position, b.velocity, b.radius)))
        .collect();

    park.pending_events.clear();
    park.step_collision_bumping(&pre_balls_3, &pre_positions_3, 1.0);
    assert_eq!(park.balls[&301].position, DVec3::new(10_100.0, 0.0, 0.0));
    assert_eq!(park.balls[&301].velocity, DVec3::new(-10.0, 0.0, 0.0));
    assert!(!park.pending_events.iter().any(|e| matches!(
        e,
        DestinyEvent::Collision { ball_a: 300 | 301, .. } | DestinyEvent::Collision { ball_b: 300 | 301, .. }
    )));

    // 4. Swept-sphere collision (c_coef > 0, disc >= 0, t_col in 0..=1)
    let s5 = make_test_ball(501, DVec3::new(50.0, 0.0, 0.0), DVec3::new(150.0, 0.0, 0.0), 30.0, 1000.0);
    let s6 = make_test_ball(502, DVec3::new(-50.0, 0.0, 0.0), DVec3::new(-150.0, 0.0, 0.0), 30.0, 2000.0);
    assert!(park.add_ball(s5));
    assert!(park.add_ball(s6));
    let mut pre_swept = park.balls.clone();
    pre_swept.get_mut(&501).unwrap().position = DVec3::new(-100.0, 0.0, 0.0);
    pre_swept.get_mut(&501).unwrap().velocity = DVec3::new(150.0, 0.0, 0.0);
    pre_swept.get_mut(&502).unwrap().position = DVec3::new(100.0, 0.0, 0.0);
    pre_swept.get_mut(&502).unwrap().velocity = DVec3::new(-150.0, 0.0, 0.0);
    let pre_pos_swept: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre_swept
        .iter()
        .map(|(&id, b)| (id, (b.position, b.velocity, b.radius)))
        .collect();
    park.step_collision_bumping(&pre_swept, &pre_pos_swept, 1.0);
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a: 501, ball_b: 502 })));

    // 5. Swept-sphere static hull deflection
    let s7 = make_test_ball(601, DVec3::new(-100.0, 0.0, 0.0), DVec3::new(-800.0, 0.0, 0.0), 50.0, 1000.0);
    assert!(park.add_ball(s7));
    let mut pre_swept_stat = park.balls.clone();
    pre_swept_stat.get_mut(&601).unwrap().position = DVec3::new(700.0, 0.0, 0.0);
    let pre_pos_stat: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre_swept_stat
        .iter()
        .map(|(&id, b)| (id, (b.position, b.velocity, b.radius)))
        .collect();
    park.step_collision_bumping(&pre_swept_stat, &pre_pos_stat, 1.0);
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a: 200, ball_b: 601 } | DestinyEvent::Collision { ball_a: 601, ball_b: 200 })));
}
