//! Unit tests for warp-tunnel navigation immutability, pre-warp align cancellation, and
//! sub-warp velocity clamping (`ballpark_navigation.rs` & `physics.rs`).

use super::*;
use crate::{WarpBubble, AU_METERS};

fn make_ship_ball(id: BallId, pos: DVec3, max_velocity: f64) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity,
        inertia: 2.5,
        mass: 1_067_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn active_warp_phases_reject_all_subwarp_steering_and_preserve_warp_state() {
    for (idx, phase) in [
        WarpPhase::Accelerating,
        WarpPhase::Cruising,
        WarpPhase::Decelerating,
    ]
    .into_iter()
    .enumerate()
    {
        let mut park = Ballpark::new(30_000_142);
        let ship_id = 1_000 + idx as i64;
        let target_id = 2_000 + idx as i64;
        park.add_ball(make_ship_ball(ship_id, DVec3::ZERO, 350.0));
        park.add_ball(make_ship_ball(target_id, DVec3::new(5_000.0, 0.0, 0.0), 350.0));

        let dest = DVec3::new(10.0 * AU_METERS, 0.0, 0.0);
        park.set_mode(ship_id, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
            .expect("initiate warp");

        let warp_vel = DVec3::new(3.0 * AU_METERS, 0.0, 0.0);
        {
            let b = park.balls.get_mut(&ship_id).unwrap();
            b.velocity = warp_vel;
            if let MovementMode::Warp {
                phase: ref mut p,
                ref mut elapsed_warp_time,
                ..
            } = b.mode
            {
                *p = phase;
                *elapsed_warp_time = 4.0;
            }
        }
        park.pending_events.clear();
        park.drain_all_observer_events();

        let err_goto = park
            .goto_point(ship_id, DVec3::new(0.0, 10_000.0, 0.0))
            .expect_err("goto_point must fail in active warp");
        assert!(err_goto.starts_with("ShipAlreadyInWarp"), "got: {err_goto}");

        for candidate in [
            MovementMode::Stop,
            MovementMode::GotoDirection {
                direction: DVec3::Y,
            },
            MovementMode::Boid,
            MovementMode::Follow {
                target_id,
                range: 1_000.0,
            },
            MovementMode::Orbit {
                target_id,
                range: 2_500.0,
            },
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(0.0, 5.0 * AU_METERS, 0.0), 3.0),
        ] {
            let err = park
                .set_mode(ship_id, candidate)
                .expect_err("set_mode must fail in active warp");
            assert!(err.starts_with("ShipAlreadyInWarp"), "got: {err}");
        }

        assert!(!park.set_speed_fraction(ship_id, 0.0));
        assert!(!park.set_speed_fraction(ship_id, 0.5));

        let b = &park.balls[&ship_id];
        assert_eq!(b.velocity, warp_vel);
        assert_eq!(b.speed_fraction, 1.0);
        assert!(matches!(b.mode, MovementMode::Warp { phase: p, .. } if p == phase));
    }
}

#[test]
fn aligning_warp_cancellation_clamps_velocity_and_restores_mass_and_warping_fx() {
    let mut park = Ballpark::new(30_000_142);
    let ship_a = 101;
    let ship_b = 102;
    let observer = 103;
    park.add_ball(make_ship_ball(ship_a, DVec3::ZERO, 320.0));
    park.add_ball(make_ship_ball(ship_b, DVec3::new(100.0, 0.0, 0.0), 280.0));
    park.add_ball(make_ship_ball(observer, DVec3::new(200.0, 0.0, 0.0), 300.0));

    let dest = DVec3::new(5.0 * AU_METERS, 0.0, 0.0);
    park.set_mode(ship_a, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
        .unwrap();
    park.set_mode(ship_b, MovementMode::new_warp(DVec3::new(100.0, 0.0, 0.0), dest, 3.0))
        .unwrap();

    // Inject anomalous super-luminal velocity onto aligning balls to prove clamp_ball_subwarp_velocity.
    park.balls.get_mut(&ship_a).unwrap().velocity = DVec3::new(2.0 * AU_METERS, 0.0, 0.0);
    park.balls.get_mut(&ship_b).unwrap().velocity = DVec3::new(f64::NAN, 10_000.0, 0.0);
    park.pending_events.clear();
    park.drain_all_observer_events();

    // 1. Cancel ship_a via set_mode(Stop) while still Aligning.
    park.set_mode(ship_a, MovementMode::Stop)
        .expect("cancelling Aligning warp via Stop must succeed");
    let ba = &park.balls[&ship_a];
    assert_eq!(ba.mode, MovementMode::Stop);
    assert!(
        ba.velocity.length() <= ba.max_velocity + 1e-9,
        "velocity must be clamped to max_velocity on warp exit: {}",
        ba.velocity.length()
    );
    let obs_events_a = park.drain_observer_events(observer);
    assert!(obs_events_a.iter().any(|e| matches!(
        e,
        DestinyEvent::SetBallMassive {
            ball_id,
            is_massive: true
        } if *ball_id == ship_a
    )));
    assert!(obs_events_a.iter().any(|e| matches!(
        e,
        DestinyEvent::OnSpecialFX {
            ball_id,
            guid,
            is_active: false,
            ..
        } if *ball_id == ship_a && guid == "effects.Warping"
    )));
    assert!(park.pending_events.iter().any(|e| matches!(
        e,
        DestinyEvent::OnDeactivatingWarp { ball_id } if *ball_id == ship_a
    )));

    // 2. Cancel ship_b via goto_point while still Aligning (NaN velocity sanitized to ZERO).
    park.pending_events.clear();
    park.goto_point(ship_b, DVec3::new(0.0, 5_000.0, 0.0))
        .expect("cancelling Aligning warp via goto_point must succeed");
    let bb = &park.balls[&ship_b];
    assert!(matches!(bb.mode, MovementMode::GotoDirection { .. }));
    assert_eq!(bb.velocity, DVec3::ZERO);
    let obs_events_b = park.drain_observer_events(observer);
    assert!(obs_events_b.iter().any(|e| matches!(
        e,
        DestinyEvent::SetBallMassive {
            ball_id,
            is_massive: true
        } if *ball_id == ship_b
    )));
}

#[test]
fn force_stop_warp_and_ultra_slow_landing_clamp_velocity_to_subwarp_bounds() {
    let mut park = Ballpark::new(30_000_142);
    let cruiser = 501;
    let observer = 502;
    park.add_ball(make_ship_ball(cruiser, DVec3::ZERO, 240.0));
    park.add_ball(make_ship_ball(observer, DVec3::new(500.0, 0.0, 0.0), 300.0));

    let dest = DVec3::new(8.0 * AU_METERS, 0.0, 0.0);
    park.set_mode(cruiser, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
        .unwrap();
    {
        let b = park.balls.get_mut(&cruiser).unwrap();
        b.velocity = DVec3::new(3.0 * AU_METERS, 0.0, 0.0);
        if let MovementMode::Warp { ref mut phase, .. } = b.mode {
            *phase = WarpPhase::Cruising;
        }
    }
    park.combat_states.get_mut(&cruiser).unwrap().is_emergency_warping = true;
    park.pending_events.clear();
    park.drain_all_observer_events();

    assert!(park.force_stop_warp(999_999).is_err());
    park.force_stop_warp(cruiser).expect("force_stop_warp");
    let bc = &park.balls[&cruiser];
    assert_eq!(bc.mode, MovementMode::Stop);
    assert_eq!(bc.velocity, DVec3::ZERO);
    assert_eq!(bc.speed_fraction, 0.0);
    assert!(!park.combat_states[&cruiser].is_emergency_warping);
    let obs = park.drain_observer_events(observer);
    assert!(obs.iter().any(|e| matches!(
        e,
        DestinyEvent::SetBallMassive {
            ball_id: 501,
            is_massive: true
        }
    )));
    assert!(obs.iter().any(|e| matches!(e, DestinyEvent::Stop { ball_id: 501 })));

    // Ultra-slow hull (max_velocity = 0.25 m/s < 1.0 m/s) caught by a warp bubble and landing naturally.
    let slow_ship = 503;
    let bubble_dest = DVec3::new(50_000_000.0, 0.0, 0.0);
    park.warp_bubble_mgr
        .add_bubble(WarpBubble::new(9_001, bubble_dest, 20_000.0));
    park.add_ball(make_ship_ball(slow_ship, DVec3::new(0.0, 1_000.0, 0.0), 0.25));
    park.balls.get_mut(&slow_ship).unwrap().velocity = DVec3::new(0.25, 0.0, 0.0);
    park.set_mode(
        slow_ship,
        MovementMode::new_warp(DVec3::new(0.0, 1_000.0, 0.0), bubble_dest, 3.0),
    )
    .unwrap();
    for _ in 0..60 {
        park.step(1.0);
        if park.balls[&slow_ship].mode == MovementMode::Stop {
            break;
        }
    }
    let bs = &park.balls[&slow_ship];
    assert_eq!(bs.mode, MovementMode::Stop);
    assert!(
        bs.velocity.length() <= bs.max_velocity + 1e-9,
        "landing velocity {} exceeded ultra-slow max_velocity {}",
        bs.velocity.length(),
        bs.max_velocity
    );
}
