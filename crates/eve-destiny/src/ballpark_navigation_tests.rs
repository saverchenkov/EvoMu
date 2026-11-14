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

#[test]
fn multi_au_warp_across_populated_intermediate_bubble_avoids_mid_warp_grid_thrashing() {
    let mut park = Ballpark::new(30_000_142);
    let warper = 1_100;
    let dep_obs = 1_101;
    let mid_bystander = 1_102;
    let dest_obs = 1_103;

    let origin = DVec3::ZERO;
    let mid_pos = DVec3::new(5.0 * AU_METERS, 0.0, 0.0);
    let dest_pos = DVec3::new(10.0 * AU_METERS, 0.0, 0.0);

    park.add_ball(make_ship_ball(warper, origin, 350.0));
    park.add_ball(make_ship_ball(dep_obs, DVec3::new(0.0, 2_000.0, 0.0), 350.0));
    park.add_ball(make_ship_ball(mid_bystander, mid_pos, 350.0));
    park.add_ball(make_ship_ball(
        dest_obs,
        dest_pos + DVec3::new(0.0, 2_500.0, 0.0),
        350.0,
    ));

    let dep_gid = park.bubble_id_for_ball(dep_obs).expect("dep grid");
    let mid_gid = park.bubble_id_for_ball(mid_bystander).expect("mid grid");
    let dest_gid = park.bubble_id_for_ball(dest_obs).expect("dest grid");
    assert_eq!(park.bubble_id_for_ball(warper), Some(dep_gid));
    assert_ne!(dep_gid, mid_gid);
    assert_ne!(mid_gid, dest_gid);
    assert_eq!(park.grids.len(), 3);

    park.set_mode(warper, MovementMode::new_warp(origin, dest_pos, 3.0))
        .expect("start 10 AU warp");
    park.drain_all_observer_events();

    let mut dep_saw_remove = false;
    let mut dest_saw_add2 = false;
    let mut warper_saw_add = false;

    for _ in 0..120 {
        park.step(1.0);
        // Zero orphan intermediate GridBubbles may ever be created along the 10 AU tunnel.
        assert_eq!(
            park.grids.len(),
            3,
            "mid-warp step must never allocate intermediate GridBubbles"
        );

        let mode_now = park.balls[&warper].mode.clone();
        if let MovementMode::Warp { phase, .. } = mode_now {
            if matches!(
                phase,
                WarpPhase::Aligning | WarpPhase::Accelerating | WarpPhase::Cruising
            ) {
                assert_eq!(
                    park.bubble_id_for_ball(warper),
                    Some(dep_gid),
                    "warper must stay in departure bubble during {phase:?}"
                );
            }
        }

        let mid_evs = park.drain_observer_events(mid_bystander);
        assert!(
            !mid_evs.iter().any(|e| matches!(
                e,
                DestinyEvent::AddBalls { .. }
                    | DestinyEvent::AddBalls2 { .. }
                    | DestinyEvent::RemoveBalls { .. }
                    | DestinyEvent::RemoveBall { .. }
            )),
            "intermediate bubble bystander must receive zero spurious AddBalls/RemoveBalls: {mid_evs:?}"
        );

        for ev in park.drain_observer_events(warper) {
            if let DestinyEvent::AddBalls { balls } = &ev {
                assert!(
                    !balls.iter().any(|b| b.id == mid_bystander),
                    "warper must never load intermediate bubble bystander"
                );
                if balls.iter().any(|b| b.id == dest_obs) {
                    warper_saw_add = true;
                }
            }
        }
        for ev in park.drain_observer_events(dep_obs) {
            if matches!(&ev, DestinyEvent::RemoveBalls { ball_ids } if ball_ids.contains(&warper)) {
                assert!(
                    !matches!(
                        park.balls[&warper].mode,
                        MovementMode::Warp {
                            phase: WarpPhase::Aligning
                                | WarpPhase::Accelerating
                                | WarpPhase::Cruising,
                            ..
                        }
                    ),
                    "departure RemoveBalls must not fire before destination deceleration"
                );
                dep_saw_remove = true;
            }
        }
        for ev in park.drain_observer_events(dest_obs) {
            if matches!(&ev, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == warper))
            {
                dest_saw_add2 = true;
            }
        }

        if park.balls[&warper].mode == MovementMode::Stop {
            break;
        }
    }

    assert_eq!(park.balls[&warper].mode, MovementMode::Stop);
    assert_eq!(park.bubble_id_for_ball(warper), Some(dest_gid));
    assert!(dep_saw_remove, "departure observer must receive RemoveBalls");
    assert!(dest_saw_add2, "destination observer must receive AddBalls2");
    assert!(warper_saw_add, "warper must receive AddBalls for destination resident");

    // Also warp into completely unpopulated deep space (2 AU further) and verify exactly 1 new grid is created at destination.
    let deep_dest = DVec3::new(12.0 * AU_METERS, 0.0, 0.0);
    park.set_mode(
        warper,
        MovementMode::new_warp(park.balls[&warper].position, deep_dest, 3.0),
    )
    .expect("warp to unpopulated deep space");
    park.drain_all_observer_events();
    for _ in 0..90 {
        park.step(1.0);
        assert!(
            park.grids.len() <= 4,
            "unpopulated warp must create at most 1 destination grid (got {})",
            park.grids.len()
        );
        if park.balls[&warper].mode == MovementMode::Stop {
            break;
        }
    }
    assert_eq!(park.balls[&warper].mode, MovementMode::Stop);
    assert_eq!(park.grids.len(), 4);
    let final_gid = park.bubble_id_for_ball(warper).expect("deep grid");
    assert!(park.grids[&final_gid].contains(deep_dest));
}

#[test]
fn gate_cloak_decloak_events_precede_warp_to_and_goto_point_while_covert_cloak_persists() {
    let mut park = Ballpark::new(30_000_142);
    let ship = 1_201;
    let peer = 1_202;
    park.add_ball(make_ship_ball(ship, DVec3::ZERO, 300.0));
    park.add_ball(make_ship_ball(peer, DVec3::new(1_500.0, 0.0, 0.0), 300.0));
    {
        let cs = park.combat_states.get_mut(&ship).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 60;
        cs.cloak_velocity_multiplier = Some(0.25);
    }
    park.drain_all_observer_events();

    let dest = DVec3::new(1.0 * AU_METERS, 0.0, 0.0);
    park.set_mode(ship, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
        .expect("warp from gate cloak");
    assert_eq!(park.combat_states[&ship].cloak_velocity_multiplier, None);

    let ship_evs = park.drain_observer_events(ship);
    let cloak_idx = ship_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::CloakBall { ball_id: 1_201, cloaked: 0 }))
        .expect("CloakBall(0) emitted");
    let fx_idx = ship_evs
        .iter()
        .position(|e| {
            matches!(
                e,
                DestinyEvent::OnSpecialFX {
                    ball_id: 1_201,
                    guid,
                    is_active: false,
                    ..
                } if guid == "effects.Cloak"
            )
        })
        .expect("OnSpecialFX(effects.Cloak, false) emitted");
    let warp_idx = ship_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 1_201, .. }))
        .expect("WarpTo emitted");
    assert!(cloak_idx < warp_idx && fx_idx < warp_idx);

    let peer_evs = park.drain_observer_events(peer);
    let add_idx = peer_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == ship)))
        .expect("peer receives AddBalls2 on gate-cloak break");
    let peer_warp_idx = peer_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::WarpTo { ball_id: 1_201, .. }))
        .expect("peer receives WarpTo");
    assert!(add_idx < peer_warp_idx);

    // GotoPoint also breaks gate cloak and emits CloakBall(0) strictly before GotoPoint
    let ship2 = 1_203;
    park.add_ball(make_ship_ball(ship2, DVec3::new(0.0, 500.0, 0.0), 300.0));
    {
        let cs2 = park.combat_states.get_mut(&ship2).unwrap();
        cs2.is_cloaked = true;
        cs2.gate_cloak_remaining_ticks = 60;
        cs2.undock_invulnerable_remaining_ticks = 30;
    }
    park.drain_all_observer_events();
    park.goto_point(ship2, DVec3::new(10_000.0, 0.0, 0.0))
        .expect("goto_point breaks gate cloak");
    let s2_evs = park.drain_observer_events(ship2);
    let s2_cloak = s2_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::CloakBall { ball_id: 1_203, cloaked: 0 }))
        .expect("CloakBall(0) before GotoPoint");
    let s2_goto = s2_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::GotoPoint { ball_id: 1_203, .. }))
        .expect("GotoPoint emitted");
    assert!(s2_cloak < s2_goto);
    assert_eq!(park.combat_states[&ship2].undock_invulnerable_remaining_ticks, 0);

    // Covert-ops cloak stays cloaked across WarpTo; non-covert module cloak rejects WarpTo
    let cov_ship = 1_204;
    park.add_ball(make_ship_ball(cov_ship, DVec3::new(0.0, -500.0, 0.0), 300.0));
    {
        let csc = park.combat_states.get_mut(&cov_ship).unwrap();
        csc.is_cloaked = true;
        csc.is_covert_cloak = true;
        csc.gate_cloak_remaining_ticks = 0;
    }
    park.drain_all_observer_events();
    park.set_mode(cov_ship, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
        .expect("covert cloak warps cleanly");
    assert!(park.combat_states[&cov_ship].is_cloaked);
    let cov_evs = park.drain_observer_events(cov_ship);
    assert!(!cov_evs
        .iter()
        .any(|e| matches!(e, DestinyEvent::CloakBall { cloaked: 0, .. })));

    let proto_ship = 1_205;
    park.add_ball(make_ship_ball(proto_ship, DVec3::new(0.0, -900.0, 0.0), 300.0));
    {
        let csp = park.combat_states.get_mut(&proto_ship).unwrap();
        csp.is_cloaked = true;
        csp.is_covert_cloak = false;
        csp.gate_cloak_remaining_ticks = 0;
    }
    assert!(park
        .set_mode(proto_ship, MovementMode::new_warp(DVec3::ZERO, dest, 3.0))
        .is_err());
}

#[test]
fn force_stop_warp_emits_zero_velocity_event_for_warp_and_subwarp_gh1025() {
    let mut park = Ballpark::new(30_000_142);
    let warper = 1_301;
    let subwarper = 1_302;
    let dest = DVec3::new(42.48 * AU_METERS, 0.0, 0.0);

    park.add_ball(make_ship_ball(warper, DVec3::ZERO, 335.0));
    park.add_ball(make_ship_ball(subwarper, DVec3::new(0.0, 500.0, 0.0), 335.0));

    park.set_mode(warper, MovementMode::new_warp(DVec3::ZERO, dest, 5.0))
        .expect("start warp");
    for _ in 0..20 {
        park.step(1.0);
        if park.balls[&warper].velocity.length() > 1.0e10 {
            break;
        }
    }
    assert!(
        park.balls[&warper].velocity.length() > 1.0e10,
        "warper must reach multi-AU/s warp velocity before force_stop_warp"
    );

    park.drain_all_observer_events();
    park.force_stop_warp(warper).expect("force_stop_warp warper");
    assert!(matches!(park.balls[&warper].mode, MovementMode::Stop));
    assert_eq!(park.balls[&warper].velocity, DVec3::ZERO);
    assert_eq!(park.balls[&warper].speed_fraction, 0.0);
    let warp_evs = park.drain_observer_events(warper);
    let stop_idx = warp_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::Stop { ball_id } if *ball_id == warper))
        .expect("expected Stop event");
    let vel_idx = warp_evs
        .iter()
        .position(|e| matches!(
            e,
            DestinyEvent::SetBallVelocity { ball_id, velocity }
                if *ball_id == warper && *velocity == [0.0, 0.0, 0.0]
        ))
        .expect("force_stop_warp must emit SetBallVelocity([0,0,0]) so DSTBALL_STOP does not coast at warp speed");
    let pos_idx = warp_evs
        .iter()
        .position(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id, .. } if *ball_id == warper))
        .expect("force_stop_warp while warping must emit SetBallPosition");
    assert!(
        stop_idx < vel_idx && vel_idx < pos_idx,
        "Stop -> SetBallVelocity([0,0,0]) -> SetBallPosition order required on force_stop_warp: {warp_evs:?}"
    );

    // Subwarp moving ship stopped via force_stop_warp must also emit SetBallVelocity([0,0,0]).
    park.set_speed_fraction(subwarper, 1.0);
    park.set_mode(
        subwarper,
        MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    )
    .expect("subwarp goto");
    for _ in 0..10 {
        park.step(1.0);
    }
    assert!(park.balls[&subwarper].velocity.length() > 10.0);
    park.drain_all_observer_events();
    park.force_stop_warp(subwarper).expect("force_stop_warp subwarper");
    assert_eq!(park.balls[&subwarper].velocity, DVec3::ZERO);
    let sub_evs = park.drain_observer_events(subwarper);
    assert!(
        sub_evs.iter().any(|e| matches!(e, DestinyEvent::Stop { ball_id } if *ball_id == subwarper)),
        "expected Stop event for subwarper: {sub_evs:?}"
    );
    assert!(
        sub_evs.iter().any(|e| matches!(
            e,
            DestinyEvent::SetBallVelocity { ball_id, velocity }
                if *ball_id == subwarper && *velocity == [0.0, 0.0, 0.0]
        )),
        "subwarp force_stop_warp must emit SetBallVelocity([0,0,0]): {sub_evs:?}"
    );
}
