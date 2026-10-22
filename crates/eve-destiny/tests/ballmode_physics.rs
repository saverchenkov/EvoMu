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

#[tokio::test]
async fn test_mushroom_fourth_root_radius_expansion_and_expiry() {
    // parity: ballmode:DSTBALL_MUSHROOM
    eve_testkit::attest(&["ballmode:DSTBALL_MUSHROOM"]);
    // _destiny.so Ballpark::AddMushroom (0x36fd0) & Ballpark::Evolve (0x2d170..0x2d1eb):
    // stationary center, R(t) = R_max * (t / duration)^{0.25} for 0 < t < duration, removed at t >= duration.
    let mut park = Ballpark::new(30_000_142);
    let mush_id: BallId = 9_100_001;
    let center = DVec3::new(1_000.0, -2_000.0, 3_000.0);
    let r_max = 16_000.0;
    let duration_s = 16.0;
    let owner_id = 90_000_001;
    assert!(park.add_mushroom_ball(mush_id, 24_500, center, r_max, duration_s, 1_000.0, owner_id));
    assert_eq!(park.balls[&mush_id].radius, 0.0);
    assert_eq!(park.balls[&mush_id].velocity, DVec3::ZERO);

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&mush_id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode mushroom");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_MUSHROOM);
    assert_eq!(decoded[0].follow_range, Some(r_max as f32));
    assert_eq!(decoded[0].owner_id, Some(owner_id));

    for k in 1..=15 {
        park.step(1.0);
        let b = park.balls.get(&mush_id).expect("mushroom still active before duration");
        assert_eq!(b.position, center, "mushroom center must remain fixed at tick {k}");
        assert_eq!(b.velocity, DVec3::ZERO);
        let expected_r = r_max * ((k as f64) / duration_s).powf(0.25);
        assert!(
            close(b.radius, expected_r, 1e-12),
            "tick {k}: radius {} vs expected {expected_r}",
            b.radius
        );
    }
    // At k = 1, (1/16)^0.25 = 0.5 => R(1) = 8000 m; at k = 16, t/duration == 1.0 => expires and is removed.
    let evs = park.step(1.0);
    assert!(!park.balls.contains_key(&mush_id), "mushroom ball must expire at t == duration");
    assert!(
        evs.iter()
            .any(|e| matches!(e, eve_destiny::DestinyEvent::RemoveBall { ball_id } if *ball_id == mush_id))
    );
}

#[tokio::test]
async fn test_troll_free_coasting_then_freezes_to_rigid() {
    // parity: ballmode:DSTBALL_TROLL
    eve_testkit::attest(&["ballmode:DSTBALL_TROLL"]);
    // _destiny.so Ballpark::SetBallTroll (0x3cea0) & Ballpark::Evolve (0x2eff0, 0x2d103..0x2dbb8):
    // coasts as a free sub-warp ball with zero goal velocity until current_tick >= effect_stamp,
    // then converts to MovementMode::Rigid with zero velocity.
    let mut park = Ballpark::new(30_000_142);
    let id: BallId = 9_100_002;
    let mut b = ship(id, DVec3::ZERO, MovementMode::Stop);
    b.velocity = DVec3::new(365.0, 0.0, 0.0);
    park.add_ball(b);
    let effect_stamp = park.set_ball_troll(id, 6).expect("set_ball_troll");
    assert_eq!(effect_stamp, park.current_tick + 6);

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode troll");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_TROLL);
    assert_eq!(decoded[0].effect_stamp, Some(effect_stamp as i32));

    let t0 = tau(&park.balls[&id]);
    for k in 1..=5 {
        park.step(1.0);
        let t = k as f64;
        let expect_v = 365.0 * (-t / t0).exp();
        let expect_x = 365.0 * t0 * (1.0 - (-t / t0).exp());
        let ball = &park.balls[&id];
        assert_eq!(ball.mode, MovementMode::Troll, "tick {k} still Troll");
        assert!(close(ball.velocity.x, expect_v, 1e-9));
        assert!(close(ball.position.x, expect_x, 1e-9));
    }

    // On tick 6 (current_tick == effect_stamp), integrates final coast step and freezes into Rigid.
    park.step(1.0);
    let frozen_x = 365.0 * t0 * (1.0 - (-6.0 / t0).exp());
    assert_eq!(park.balls[&id].mode, MovementMode::Rigid);
    assert_eq!(park.balls[&id].velocity, DVec3::ZERO);
    assert!(close(park.balls[&id].position.x, frozen_x, 1e-9));

    // Subsequent ticks never move the frozen ball.
    for _ in 0..4 {
        park.step(1.0);
        assert_eq!(park.balls[&id].mode, MovementMode::Rigid);
        assert_eq!(park.balls[&id].velocity, DVec3::ZERO);
        assert!(close(park.balls[&id].position.x, frozen_x, 1e-9));
    }
}

#[tokio::test]
async fn test_boid_free_subwarp_coasting_and_toggle_to_stop() {
    // parity: ballmode:DSTBALL_BOID
    eve_testkit::attest(&["ballmode:DSTBALL_BOID"]);
    // _destiny.so Ballpark::SetBoid (0x71870) & Ballpark::Evolve (0x2eff0):
    // SetBoid(id, true) sets DSTBALL_BOID (free sub-warp ball integrating exponential drag without expiring);
    // SetBoid(id, false) calls Stop (DSTBALL_STOP, speed_fraction = 0).
    let mut park = Ballpark::new(30_000_142);
    let id: BallId = 9_100_003;
    let v0 = DVec3::new(200.0, -150.0, 0.0);
    let mut b = ship(id, DVec3::ZERO, MovementMode::Stop);
    b.velocity = v0;
    park.add_ball(b);
    park.set_boid(id, true).expect("enable boid");
    assert_eq!(park.balls[&id].mode, MovementMode::Boid);

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode boid");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_BOID);
    assert_ne!(decoded[0].flags & eve_destiny::wire::BALL_FLAG_IS_FREE, 0);

    let t0 = tau(&park.balls[&id]);
    for k in 1..=10 {
        park.step(1.0);
        let t = k as f64;
        let expect_v = v0 * (-t / t0).exp();
        let expect_pos = v0 * (t0 * (1.0 - (-t / t0).exp()));
        let ball = &park.balls[&id];
        assert_eq!(ball.mode, MovementMode::Boid);
        assert!((ball.velocity - expect_v).length() < 1e-9);
        assert!((ball.position - expect_pos).length() < 1e-9);
    }

    park.set_boid(id, false).expect("disable boid -> Stop");
    assert_eq!(park.balls[&id].mode, MovementMode::Stop);
    assert_eq!(park.balls[&id].speed_fraction, 0.0);
}

#[tokio::test]
async fn test_miniball_parent_tracking_and_sub_sphere_collision() {
    // parity: ballmode:DSTBALL_MINIBALL
    eve_testkit::attest(&["ballmode:DSTBALL_MINIBALL"]);
    // _destiny.so Ballpark::AddMiniball (0x36130) & collision pass (0x2a3cb):
    // tracks parent.position + relative_offset, ignores self-collision with parent, and acts as a rigid
    // collision sub-sphere against external dynamic ships.
    let mut park = Ballpark::new(30_000_142);
    let parent_id: BallId = 9_100_010;
    let mb_id: BallId = 9_100_011;
    let offset = DVec3::new(0.0, 500.0, 0.0);
    park.add_ball(ship(parent_id, DVec3::ZERO, MovementMode::Stop));
    park.set_mode(parent_id, MovementMode::GotoDirection { direction: DVec3::X })
        .unwrap();
    park.set_speed_fraction(parent_id, 1.0);
    park.add_miniball(mb_id, parent_id, offset, 120.0)
        .expect("add_miniball");

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&mb_id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode miniball");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_MINIBALL);
    assert_eq!(decoded[0].radius, 120.0);

    let t0 = tau(&park.balls[&parent_id]);
    for k in 1..=5 {
        park.step(1.0);
        let t = k as f64;
        let expect_px = 365.0 * (t - t0 * (1.0 - (-t / t0).exp()));
        let parent_pos = park.balls[&parent_id].position;
        assert!(close(parent_pos.x, expect_px, 1e-9));
        assert_eq!(park.balls[&mb_id].position, parent_pos + offset);
    }

    // External ship flying into the miniball bounces off the miniball's 120m surface while miniball stays locked to parent.
    let intruder_id: BallId = 9_100_012;
    let next_parent_x = 365.0 * (6.0 - t0 * (1.0 - (-6.0 / t0).exp()));
    let next_mb_pos = DVec3::new(next_parent_x, 0.0, 0.0) + offset;
    let mut intr = ship(intruder_id, next_mb_pos + DVec3::new(0.0, 260.0, 0.0), MovementMode::Stop);
    intr.velocity = DVec3::new(0.0, -200.0, 0.0);
    park.add_ball(intr);
    park.step(1.0);
    let new_mb_pos = park.balls[&mb_id].position;
    assert_eq!(new_mb_pos, park.balls[&parent_id].position + offset);
    let sep = (park.balls[&intruder_id].position - new_mb_pos).length();
    assert!(sep >= 120.0 + 37.0 - 1e-6, "intruder pushed outside miniball radius: sep={sep}");
    assert!(park.balls[&intruder_id].velocity.y > 0.0, "intruder velocity reflected along +Y");
}

#[tokio::test]
async fn test_formation_follow_leader_basis_slot_and_quadratic_damping() {
    // parity: ballmode:DSTBALL_FORMATION
    eve_testkit::attest(&["ballmode:DSTBALL_FORMATION"]);
    // _destiny.so Ballpark::FormationFollow (0x37a90) & Ballpark::Evolve (0x275e5..0x2782e):
    // transforms slot_offset (right, up, forward) by leader's heading basis; steers at v_max when
    // dist >= v_max * dt and applies quadratic damping v_goal = dir * v_max * (dist / (v_max * dt))^2 when dist < v_max * dt.
    let mut park = Ballpark::new(30_000_142);
    let leader_id: BallId = 9_100_020;
    let follower_id: BallId = 9_100_021;
    // Leader at origin facing +X (speed_fraction = 0 so leader stays at origin with forward = +X, right = -Z, up = +Y).
    park.add_ball(ship(
        leader_id,
        DVec3::ZERO,
        MovementMode::GotoDirection { direction: DVec3::X },
    ));
    let slot_offset = DVec3::new(0.0, 3_000.0, -4_000.0); // slot world pos = (-4000, 3000, 0)
    let follower_start = DVec3::new(-400.0, 300.0, 0.0); // 500m from leader along slot ray (no sphere overlap)
    park.add_ball(ship(follower_id, follower_start, MovementMode::Stop));
    park.formation_follow(follower_id, leader_id, slot_offset, 5_000.0)
        .expect("formation_follow");

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&follower_id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode formation");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_FORMATION);
    assert_eq!(decoded[0].target_id, Some(leader_id));
    assert_eq!(decoded[0].follow_range, Some(5_000.0));

    // 1. Far regime (dist > v_max * dt = 365 m): full goal speed v_max along (-0.8, 0.6, 0).
    let slot_world = DVec3::new(-4_000.0, 3_000.0, 0.0);
    let dir = slot_world.normalize();
    let t0 = tau(&park.balls[&follower_id]);
    for k in 1..=6 {
        park.step(1.0);
        let t = k as f64;
        let expect_v = dir * (365.0 * (1.0 - (-t / t0).exp()));
        let expect_pos =
            follower_start + dir * (365.0 * (t - t0 * (1.0 - (-t / t0).exp())));
        let f = &park.balls[&follower_id];
        assert!((f.velocity - expect_v).length() < 1e-9, "tick {k}: v={:?}", f.velocity);
        assert!((f.position - expect_pos).length() < 1e-7, "tick {k}: pos={:?}", f.position);
    }

    // 2. Near regime (dist = 0.5 * v_max * dt = 182.5 m < 365 m from rest):
    // quadratic damping gives v_goal = dir * 365 * (0.5)^2 = dir * 91.25 m/s.
    park.set_ball_position(follower_id, slot_world - dir * 182.5);
    park.balls.get_mut(&follower_id).unwrap().velocity = DVec3::ZERO;
    park.step(1.0);
    let v_goal_damped = dir * (365.0 * 0.25);
    let expect_v_near = v_goal_damped * (1.0 - (-1.0 / t0).exp());
    let expect_pos_near =
        (slot_world - dir * 182.5) + v_goal_damped * (1.0 - t0 * (1.0 - (-1.0 / t0).exp()));
    let f = &park.balls[&follower_id];
    assert!((f.velocity - expect_v_near).length() < 1e-9);
    assert!((f.position - expect_pos_near).length() < 1e-9);
}

#[tokio::test]
async fn test_orbit_multi_tick_radial_convergence_tangential_speed_and_angular_rotation() {
    // parity: ballmode:DSTBALL_ORBIT
    eve_testkit::attest(&["ballmode:DSTBALL_ORBIT"]);
    let mut park = Ballpark::new(30_000_142);
    let anchor_id: BallId = 9_200_001;
    let orbiter_id: BallId = 9_200_002;
    park.add_ball(ship(anchor_id, DVec3::ZERO, MovementMode::Stop));
    park.add_ball(ship(orbiter_id, DVec3::new(8_000.0, 0.0, 0.0), MovementMode::Stop));

    let orbit_range = 2_500.0;
    let expected_radius = 37.0 + 37.0 + orbit_range;
    park.set_mode(
        orbiter_id,
        MovementMode::Orbit {
            target_id: anchor_id,
            range: orbit_range,
        },
    )
    .expect("set orbit");
    park.set_speed_fraction(orbiter_id, 1.0);

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&orbiter_id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode orbit");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_ORBIT);
    assert_eq!(decoded[0].target_id, Some(anchor_id));
    assert_eq!(decoded[0].follow_range, Some(orbit_range as f32));

    let mut total_angle = 0.0;
    let mut prev_dir = park.balls[&orbiter_id].position.normalize();
    for _ in 0..45 {
        park.step(1.0);
        let cur_dir = park.balls[&orbiter_id].position.normalize();
        let dtheta = prev_dir.dot(cur_dir).clamp(-1.0, 1.0).acos();
        assert!(dtheta > 0.0, "orbiter must advance in angle each tick");
        total_angle += dtheta;
        prev_dir = cur_dir;
    }

    let b = &park.balls[&orbiter_id];
    let dist = b.position.length();
    assert!(
        (dist - expected_radius).abs() < 300.0,
        "orbit radius {dist} must converge near {expected_radius}"
    );
    let r_hat = b.position / dist;
    let v_rad = b.velocity.dot(r_hat).abs();
    let v_tan = (b.velocity - r_hat * b.velocity.dot(r_hat)).length();
    assert!(v_tan > 300.0 && v_rad < 80.0, "v_tan={v_tan}, v_rad={v_rad}");
    assert!(total_angle > 3.0, "orbiter must sweep > 3.0 rad over 45 ticks, got {total_angle}");
}

#[tokio::test]
async fn test_follow_multi_tick_stationary_convergence_quadratic_damping_and_moving_target() {
    // parity: ballmode:DSTBALL_FOLLOW
    eve_testkit::attest(&["ballmode:DSTBALL_FOLLOW"]);
    let mut park = Ballpark::new(30_000_142);
    let target_id: BallId = 9_200_010;
    let follower_id: BallId = 9_200_011;
    park.add_ball(ship(target_id, DVec3::new(6_000.0, 0.0, 0.0), MovementMode::Stop));
    park.add_ball(ship(follower_id, DVec3::ZERO, MovementMode::Stop));

    let follow_range = 1_500.0;
    let desired_dist = 37.0 + 37.0 + follow_range;
    park.set_mode(
        follower_id,
        MovementMode::Follow {
            target_id,
            range: follow_range,
        },
    )
    .expect("set follow");
    park.set_speed_fraction(follower_id, 1.0);

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&follower_id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode follow");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_FOLLOW);
    assert_eq!(decoded[0].target_id, Some(target_id));
    assert_eq!(decoded[0].follow_range, Some(follow_range as f32));

    // 1. Stationary target: approaches from 6 km and damps velocity to zero near desired_dist.
    for _ in 0..50 {
        park.step(1.0);
    }
    let sep = (park.balls[&target_id].position - park.balls[&follower_id].position).length();
    assert!((sep - desired_dist).abs() < 25.0, "stationary follow sep={sep} vs {desired_dist}");
    assert!(park.balls[&follower_id].velocity.length() < 8.0);

    // 2. Moving target: leader moves along +X at 182.5 m/s (50% speed), follower tracks and matches speed.
    park.set_mode(target_id, MovementMode::GotoDirection { direction: DVec3::X })
        .unwrap();
    park.set_speed_fraction(target_id, 0.5);
    for _ in 0..30 {
        park.step(1.0);
    }
    let v_target = park.balls[&target_id].velocity.x;
    let v_follow = park.balls[&follower_id].velocity.x;
    let moving_sep = (park.balls[&target_id].position - park.balls[&follower_id].position).length();
    assert!((v_follow - v_target).abs() < 15.0, "follower speed {v_follow} tracks {v_target}");
    assert!(moving_sep > desired_dist && moving_sep < desired_dist + 800.0, "moving_sep={moving_sep}");
}

#[tokio::test]
async fn test_warp_full_lifecycle_align_accelerate_cruise_decelerate_and_dropout() {
    // parity: ballmode:DSTBALL_WARP
    eve_testkit::attest(&["ballmode:DSTBALL_WARP"]);
    let mut park = Ballpark::new(30_000_142);
    let id: BallId = 9_200_020;
    let dest = DVec3::new(8.0 * eve_destiny::AU_METERS, 0.0, 0.0);
    park.add_ball(ship(id, DVec3::ZERO, MovementMode::Stop));
    park.set_mode(id, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
        .expect("start warp");

    let encoded = eve_destiny::wire::encode_add_balls_with_park(
        0,
        park.current_tick as u32,
        &[&park.balls[&id]],
        Some(&park),
    );
    let (_, _, decoded) = eve_destiny::wire::decode_add_balls(&encoded).expect("decode warp");
    assert_eq!(decoded[0].mode, eve_destiny::wire::DSTBALL_WARP);
    assert_eq!(decoded[0].warp_destination, Some(dest));
    assert_eq!(decoded[0].warp_speed_tenth_au_s, Some(30));

    let (mut saw_align, mut saw_accel, mut saw_cruise, mut saw_decel, mut exited) =
        (false, false, false, false, false);
    let mut peak_speed = 0.0_f64;
    for _ in 0..120 {
        if let MovementMode::Warp { phase, .. } = park.balls[&id].mode {
            match phase {
                eve_destiny::WarpPhase::Aligning => saw_align = true,
                eve_destiny::WarpPhase::Accelerating => saw_accel = true,
                eve_destiny::WarpPhase::Cruising => saw_cruise = true,
                eve_destiny::WarpPhase::Decelerating => saw_decel = true,
            }
        }
        let evs = park.step(1.0);
        peak_speed = peak_speed.max(park.balls[&id].velocity.x);
        if evs.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::OnDeactivatingWarp { ball_id } if *ball_id == id)) {
            assert!(evs.iter().any(|e| matches!(e, eve_destiny::DestinyEvent::Stop { ball_id } if *ball_id == id)));
            exited = true;
            break;
        }
    }
    assert!(saw_align && saw_accel && saw_cruise && saw_decel && exited);
    assert!(close(peak_speed, 3.0 * eve_destiny::AU_METERS, 1.0));
    assert_eq!(park.balls[&id].mode, MovementMode::Stop);
    assert!((dest - park.balls[&id].position).length() <= 2_500.0);
}
