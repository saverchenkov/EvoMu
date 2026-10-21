//! Ball-mode movement physics, checked as numeric trajectories over 1 Hz `Ballpark::step` ticks.
//!
//! Reference semantics come from evemu_Crucible `DestinyManager::ProcessState` /
//! `DestinyManager::MoveObject` and `DestinyStructs.h`; expected values are computed from those
//! closed-form formulas, not from the simulation.

use eve_destiny::{Ball, BallId, Ballpark, MovementMode};
use glam::DVec3;

/// Test fixture ship (frigate-sized values, test-only).
fn ship(id: BallId, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode,
    }
}

/// Agility time constant tau = I * M / 10^6 seconds (Crucible `DestinyManager.cpp:2630`, `:737`).
fn tau(b: &Ball) -> f64 {
    b.inertia * b.mass / 1_000_000.0
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * b.abs().max(1.0)
}

#[tokio::test]
async fn test_missile_constant_speed_homing_trajectory() {
    // parity: ballmode:DSTBALL_MISSILE
    eve_testkit::attest(&["ballmode:DSTBALL_MISSILE"]);
    // Crucible DestinyManager.cpp:165-174: heading = normalize(target - position);
    // velocity = heading * maxSpeed; position += velocity (one tick). No inertial ramp.
    let mut park = Ballpark::new(30_000_142);
    let target: BallId = 2_000;
    let missile: BallId = 9_000_000_000_000_000_001;
    park.add_ball(ship(target, DVec3::new(200_000.0, 0.0, 0.0), MovementMode::Stop));
    let mut m = ship(missile, DVec3::ZERO, MovementMode::Missile { target_id: target });
    m.max_velocity = 3_750.0;
    m.radius = 5.0;
    m.mass = 1_000.0;
    park.add_ball(m);

    for k in 1..=5 {
        park.step(1.0);
        let b = &park.balls[&missile];
        // k * 3750 m along +X at full speed from the first tick. The sub-warp exponential
        // ramp (tau = 3.45 * 1000 / 1e6 = 0.00345 s) would lag by 3750 * tau = 12.9 m per start,
        // so a 1e-6 m tolerance (float rounding of the normalized heading) tells the models apart.
        let expect = DVec3::new(3_750.0 * k as f64, 0.0, 0.0);
        assert!((b.position - expect).length() < 1e-6, "tick {k}: {:?}", b.position);
        assert!((b.velocity - DVec3::new(3_750.0, 0.0, 0.0)).length() < 1e-9);
    }

    // The target moves sideways: the next tick re-aims at the target's position at tick start.
    // Missile is at x = 18,750 m; target now at +22,500 m X / +30,000 m Y => heading (0.6, 0.8, 0).
    let new_target = DVec3::new(41_250.0, 30_000.0, 0.0);
    park.set_ball_position(target, new_target);
    let before = park.balls[&missile].position;
    park.step(1.0);
    let heading = (new_target - before).normalize();
    let b = &park.balls[&missile];
    assert!((b.position - (before + heading * 3_750.0)).length() < 1e-9);
    assert!(close(b.velocity.length(), 3_750.0, 1e-12));
    assert!((b.velocity.normalize() - DVec3::new(0.6, 0.8, 0.0)).length() < 1e-12);

    // Target gone: keep flying along the last velocity (Crucible DestinyManager.cpp:166).
    let last_v = b.velocity;
    let before = b.position;
    park.remove_ball(target);
    park.step(1.0);
    let b = &park.balls[&missile];
    assert!((b.position - (before + last_v)).length() < 1e-9);
    assert!((b.velocity - last_v).length() < 1e-9);
}

#[tokio::test]
async fn test_goto_exponential_acceleration_trajectory() {
    // parity: ballmode:DSTBALL_GOTO
    eve_testkit::attest(&["ballmode:DSTBALL_GOTO"]);
    // Crucible MoveObject: Vt = Vm * (1 - e^(-t * 10^6 / IM)) (DestinyManager.cpp:735-746, :800, :817).
    // Distance is its integral: x(t) = Vm * (t - tau * (1 - e^(-t/tau))).
    let mut park = Ballpark::new(30_000_142);
    let id: BallId = 1_000;
    park.add_ball(ship(id, DVec3::new(0.0, 0.0, 0.0), MovementMode::Stop));
    park.set_mode(id, MovementMode::GotoDirection { direction: DVec3::new(0.0, 3.0, 4.0) }).unwrap();
    assert!(park.set_speed_fraction(id, 1.0));
    let t0 = tau(&park.balls[&id]);
    assert!(close(t0, 3.681_15, 1e-12));
    let dir = DVec3::new(0.0, 0.6, 0.8);
    for t in 1..=12 {
        park.step(1.0);
        let t = t as f64;
        let v = 365.0 * (1.0 - (-t / t0).exp());
        let x = 365.0 * (t - t0 * (1.0 - (-t / t0).exp()));
        let b = &park.balls[&id];
        assert!((b.velocity - dir * v).length() < 1e-9, "v({t}) = {:?} vs {v}", b.velocity);
        assert!((b.position - dir * x).length() < 1e-7, "x({t}) = {:?} vs {x}", b.position);
    }
    // Speed reaches 99% of max after -ln(0.01) * tau = 16.95 s, and not before.
    for t in 13..=16 {
        park.step(1.0);
        assert!(park.balls[&id].velocity.length() < 0.99 * 365.0, "t = {t}");
    }
    park.step(1.0);
    assert!(park.balls[&id].velocity.length() > 0.99 * 365.0, "t = 17");
}

#[tokio::test]
async fn test_stop_exponential_deceleration_trajectory() {
    // parity: ballmode:DSTBALL_STOP
    eve_testkit::attest(&["ballmode:DSTBALL_STOP"]);
    // Crucible MoveObject decel: asf = psf - (psf - usf) * (1 - e^(-t/tau)) with usf = 0
    // (DestinyManager.cpp:819-828) => v(t) = v0 * e^(-t/tau), x(t) = v0 * tau * (1 - e^(-t/tau)).
    let mut park = Ballpark::new(30_000_142);
    let id: BallId = 1_000;
    let mut b = ship(id, DVec3::ZERO, MovementMode::GotoDirection { direction: DVec3::X });
    b.velocity = DVec3::new(365.0, 0.0, 0.0);
    b.speed_fraction = 1.0;
    park.add_ball(b);
    park.set_mode(id, MovementMode::Stop).unwrap();
    assert_eq!(park.balls[&id].speed_fraction, 0.0);
    let t0 = tau(&park.balls[&id]);
    for t in 1..=20 {
        park.step(1.0);
        let t = t as f64;
        let v = 365.0 * (-t / t0).exp();
        let x = 365.0 * t0 * (1.0 - (-t / t0).exp());
        let b = &park.balls[&id];
        assert!(close(b.velocity.x, v, 1e-9), "v({t}) = {} vs {v}", b.velocity.x);
        assert!(close(b.position.x, x, 1e-9), "x({t}) = {} vs {x}", b.position.x);
        assert_eq!((b.velocity.y, b.velocity.z), (0.0, 0.0));
    }
    // Total coast distance converges to v0 * tau = 1343.6 m.
    assert!(park.balls[&id].position.x < 365.0 * t0);
    assert!(close(park.balls[&id].position.x, 365.0 * t0, 1e-2));
}

#[tokio::test]
async fn test_rigid_ball_never_moves_even_when_hit() {
    // parity: ballmode:DSTBALL_RIGID
    eve_testkit::attest(&["ballmode:DSTBALL_RIGID"]);
    // DestinyStructs.h:48 "RIGID: a ball that will never move"; Crucible ProcessState gives it no
    // movement (DestinyManager.cpp:256). A ship flying into it bounces off; the rigid ball stays put.
    let mut park = Ballpark::new(30_000_142);
    let station: BallId = 60_003_760;
    let mut st = ship(station, DVec3::new(10.0, 20.0, 30.0), MovementMode::Rigid);
    st.radius = 5_000.0;
    st.velocity = DVec3::new(50.0, 0.0, 0.0); // ignored: rigid balls are never integrated
    park.add_ball(st);
    let shp: BallId = 1_000;
    let mut s = ship(shp, DVec3::new(5_300.0, 20.0, 30.0), MovementMode::GotoDirection { direction: DVec3::NEG_X });
    s.velocity = DVec3::new(-365.0, 0.0, 0.0);
    s.speed_fraction = 1.0;
    park.add_ball(s);
    for _ in 0..10 {
        park.step(1.0);
        assert_eq!(park.balls[&station].position, DVec3::new(10.0, 20.0, 30.0));
        // Surface contact is resolved on the ship side only.
        let gap = park.balls[&shp].position.x - 10.0;
        assert!(gap >= 5_037.0 - 1e-6, "ship pushed out of the rigid ball: gap {gap}");
    }
    assert_eq!(park.balls[&station].velocity, DVec3::new(50.0, 0.0, 0.0));
}

#[tokio::test]
async fn test_field_ball_is_static_and_does_not_deflect_ships() {
    // parity: ballmode:DSTBALL_FIELD
    eve_testkit::attest(&["ballmode:DSTBALL_FIELD"]);
    // DestinyStructs.h:47 "FIELD: force field ball"; Crucible ProcessState gives it no movement
    // (DestinyManager.cpp:254). Ships move through it on their normal trajectory.
    let mut park = Ballpark::new(30_000_142);
    let field: BallId = 3_000;
    let mut f = ship(field, DVec3::ZERO, MovementMode::Field);
    f.radius = 20_000.0;
    f.velocity = DVec3::new(0.0, 99.0, 0.0);
    park.add_ball(f);
    let shp: BallId = 1_000;
    park.add_ball(ship(shp, DVec3::new(-1_000.0, 0.0, 0.0), MovementMode::Stop));
    park.set_mode(shp, MovementMode::GotoDirection { direction: DVec3::X }).unwrap();
    park.set_speed_fraction(shp, 1.0);
    let t0 = tau(&park.balls[&shp]);
    for t in 1..=8 {
        park.step(1.0);
        let t = t as f64;
        assert_eq!(park.balls[&field].position, DVec3::ZERO);
        let x = -1_000.0 + 365.0 * (t - t0 * (1.0 - (-t / t0).exp()));
        assert!(close(park.balls[&shp].position.x, x, 1e-9), "ship x({t}) undisturbed by field");
    }
}
