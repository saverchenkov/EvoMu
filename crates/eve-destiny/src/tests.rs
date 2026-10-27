//! Unit tests for `eve-destiny` kinematics, spatial grids, collision bounce, and interdiction bubbles.

use super::*;

#[test]
fn test_rifter_align_time_and_exponential_acceleration() {
    let mut park = Ballpark::new(30000142);
    let rifter = Ball {
        id: 1001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    };

    assert!((rifter.align_time_seconds() - 5.103).abs() < 0.01);

    assert!(park.add_ball(rifter));
    for _ in 0..6 {
        park.step(1.0);
    }

    let updated = park.balls.get(&1001).unwrap();
    assert!(updated.velocity.x > 365.0 * 0.75);
    assert!(updated.position.x > 0.0);
}

#[test]
fn test_short_and_long_warp_exit_without_overshoot() {
    let mut park = Ballpark::new(30000142);
    let short_dest = DVec3::new(0.05 * AU_METERS, 0.0, 0.0);

    let ship = Ball {
        id: 1002,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(365.0, 0.0, 0.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 1.0,
        mode: MovementMode::new_warp(DVec3::ZERO, short_dest, 3.0),
    };

    assert!(park.add_ball(ship));

    park.step(1.0);
    let tick1 = park.balls.get(&1002).unwrap();
    assert!(
        tick1.position.x < 0.05 * AU_METERS * 0.1,
        "Short warp jumped too far on tick 1: {}",
        tick1.position.x
    );

    for _ in 0..60 {
        park.step(1.0);
    }

    let arrived = park.balls.get(&1002).unwrap();
    assert_eq!(arrived.mode, MovementMode::Stop);
    let dist_from_dest = (arrived.position - short_dest).length();
    assert!(
        dist_from_dest <= 2_500.0,
        "Ship overshot destination by {dist_from_dest} meters!"
    );
    assert!(arrived.velocity.length() < 1.0);
}

#[test]
fn test_turret_wrecking_shot_missile_and_proximity_decloak() {
    let p_hit = combat::compute_turret_chance_to_hit(0.0, 240.0, 35.0, 1500.0, 1500.0, 8400.0);
    assert!((p_hit - 1.0).abs() < 1e-6);

    let base_dmg = DamageVector {
        em: 0.0,
        thermal: 10.0,
        kinetic: 2.0,
        explosive: 2.0,
    };
    let (wrecking, is_wrecking) = combat::resolve_turret_shot(base_dmg, p_hit, 0.005);
    assert!(is_wrecking);
    assert!((wrecking.total() - 42.0).abs() < 1e-6);

    // Proximity decloak within 2000m
    let mut park = Ballpark::new(30000142);
    park.add_ball(Ball {
        id: 1,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.combat_states.get_mut(&1).unwrap().is_cloaked = true;

    park.add_ball(Ball {
        id: 2,
        type_id: 587,
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.step(1.0);
    assert!(!park.combat_states[&1].is_cloaked);
}

#[test]
// parity: plan:SP-2, feat:navigation.bubbles-grid, destiny:AddBalls2, destiny:RemoveBalls, destiny:RemoveBall
fn test_spatial_bubbles_10000km_isolation_and_warp_transitions() {
    let mut park = Ballpark::new(30_000_142);
    assert_eq!(park.bubble_radius_meters, 8_000_000.0);

    // Static star at origin (system-wide)
    park.add_static_ball(
        Ball {
            id: 40_009_076,
            type_id: 3802,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 0.0,
            mass: 1.0e30,
            radius: 513_100_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        6,
    );

    let ship_a = Ball {
        id: 100_000_001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(365.0, 0.0, 0.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ship_b = Ball {
        id: 100_000_002,
        type_id: 587,
        position: DVec3::new(10_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ship_c = Ball {
        id: 100_000_003,
        type_id: 587,
        position: DVec3::new(5_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ship_d = Ball {
        id: 100_000_004,
        type_id: 587,
        position: DVec3::new(-5_000_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship_a);
    park.add_ball(ship_b);
    park.add_ball(ship_c);
    park.add_ball(ship_d);

    let gid_a = park.bubble_id_for_ball(100_000_001).unwrap();
    let gid_b = park.bubble_id_for_ball(100_000_002).unwrap();
    let gid_c = park.bubble_id_for_ball(100_000_003).unwrap();
    let gid_d = park.bubble_id_for_ball(100_000_004).unwrap();
    assert_eq!(gid_a, gid_c);
    assert_eq!(gid_a, gid_d);
    assert_ne!(gid_a, gid_b);

    assert!(park.in_same_bubble(100_000_003, 100_000_004));
    assert!(!park.in_same_bubble(100_000_003, 100_000_002));
    assert!(!park.in_same_bubble(100_000_001, 100_000_002));

    let vis_a: Vec<BallId> = park
        .visible_balls_for_observer(100_000_001)
        .iter()
        .map(|b| b.id)
        .collect();
    assert!(vis_a.contains(&40_009_076), "Static star must be visible");
    assert!(vis_a.contains(&100_000_001), "Self must be visible");
    assert!(vis_a.contains(&100_000_003), "Ship C (+5,000 km) must be visible");
    assert!(vis_a.contains(&100_000_004), "Ship D (-5,000 km) must be visible");
    assert!(
        !vis_a.contains(&100_000_002),
        "Ship B (+10,000 km, Grid 2) must NOT be visible in Grid 1"
    );

    let _ = park.drain_all_observer_events();

    park.remove_ball(100_000_004);
    let ev_a_rem = park.drain_observer_events(100_000_001);
    let ev_b_rem = park.drain_observer_events(100_000_002);
    assert!(ev_a_rem
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id: 100_000_004 })));
    assert!(ev_b_rem.is_empty());
    let _ = park.drain_all_observer_events();

    park.set_mode(
        100_000_001,
        MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    )
    .expect("set GotoDirection");
    let ev_b = park.drain_observer_events(100_000_002);
    let ev_c = park.drain_observer_events(100_000_003);
    assert!(
        ev_b.is_empty(),
        "Ship B in Grid 2 must not receive Ship A's local movement update"
    );
    assert!(
        ev_c.iter()
            .any(|e| matches!(e, DestinyEvent::GotoDirection { ball_id: 100_000_001, .. })),
        "Ship C in same grid must receive Ship A's movement update"
    );

    let _ = park.drain_all_observer_events();
    let far_dest = DVec3::new(20_000_000.0, 0.0, 0.0);
    park.set_ball_position(100_000_002, far_dest);
    park.set_mode(
        100_000_001,
        MovementMode::new_warp(DVec3::ZERO, far_dest, 3.0),
    )
    .expect("set warp");
    let _ = park.drain_all_observer_events();

    let mut b_saw_add = false;
    let mut c_saw_remove = false;
    for _ in 0..30 {
        park.step(1.0);
        for ev in park.drain_observer_events(100_000_002) {
            if let DestinyEvent::AddBalls2 { balls } = ev {
                if balls.iter().any(|b| b.id == 100_000_001) {
                    b_saw_add = true;
                }
            }
        }
        for ev in park.drain_observer_events(100_000_003) {
            if let DestinyEvent::RemoveBalls { ball_ids } = ev {
                if ball_ids.contains(&100_000_001) {
                    c_saw_remove = true;
                }
            }
        }
    }
    assert!(
        b_saw_add,
        "Ship B must receive AddBalls2 when Ship A warps into Grid 2"
    );
    assert!(
        c_saw_remove,
        "Ship C must receive RemoveBalls when Ship A warps out of Grid 1"
    );
    assert_eq!(
        park.bubble_id_for_ball(100_000_001),
        park.bubble_id_for_ball(100_000_002)
    );

    if let Some(c_ball) = park.balls.get_mut(&100_000_003) {
        c_ball.position = DVec3::new(7_950_000.0, 0.0, 0.0);
    }
    park.step(1.0);
    assert_eq!(
        park.grids[&gid_a].max_bounds.x,
        8_000_000.0,
        "Grid 1 +x wall adjacent to Grid 2 must not overlap Grid 2 (Grid.md:15)"
    );

    if let Some(c_ball) = park.balls.get_mut(&100_000_003) {
        c_ball.position = DVec3::new(0.0, 7_950_000.0, 0.0);
    }
    park.step(1.0);
    assert_eq!(
        park.grids[&gid_a].max_bounds.y,
        8_050_000.0,
        "Approaching within 100 km of the +8,000 km +y grid wall must expand max_bounds.y to 8,050 km"
    );
    if let Some(c_ball) = park.balls.get_mut(&100_000_003) {
        c_ball.position = DVec3::new(0.0, 5_000_000.0, 0.0);
    }
    park.step(1.0);
    assert_eq!(
        park.grids[&gid_a].max_bounds.y,
        8_000_000.0,
        "Retreating from the expanded wall must contract max_bounds.y back to 8,000 km"
    );

    park.remove_ball(100_000_003);
    assert!(!park.grids.contains_key(&gid_a), "Empty grid must be deleted");
}

#[test]
// parity: plan:SP-4
fn test_destiny_collision_bounce_physics_and_gate_cloak() {
    let mut park = Ballpark::new(30_000_142);

    let ship1 = Ball {
        id: 2001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(15.0, 0.0, 0.0),
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.1,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    let asteroid = Ball {
        id: 9001,
        type_id: 1230,
        position: DVec3::new(80.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: 10_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };

    park.add_ball(ship1);
    park.add_static_ball(asteroid, 25);

    let events = park.step(1.0);
    let has_collision = events.iter().any(|e| {
        matches!(
            e,
            DestinyEvent::Collision { ball_a, ball_b }
                if (*ball_a == 2001 && *ball_b == 9001) || (*ball_a == 9001 && *ball_b == 2001)
        )
    });
    assert!(has_collision, "Must emit DestinyEvent::Collision upon collision");

    let b1 = park.balls.get(&2001).unwrap();
    assert!(
        b1.velocity.x < 0.0,
        "Velocity must bounce back after colliding with static asteroid, got {}",
        b1.velocity.x
    );

    let mut cs = CombatState::default();
    cs.gate_cloak_remaining_ticks = 60;
    let (applied, destroyed) = cs.apply_damage(DamageVector {
        em: 100.0,
        thermal: 100.0,
        kinetic: 100.0,
        explosive: 100.0,
    });
    assert_eq!(applied, 0.0);
    assert!(!destroyed);
}

#[test]
fn test_interdiction_bubbles_trapping_nullifier_and_micro_jump() {
    let mut park = Ballpark::new(30_000_142);

    let bubble_pos = DVec3::new(10_000_000.0, 0.0, 0.0);
    let bubble_radius = 20_000.0;
    assert!(park.add_mobile_warp_disruptor(5001, bubble_pos, bubble_radius));
    assert!(park.warp_bubble_mgr.is_scrambled_at(bubble_pos));
    assert!(park
        .warp_bubble_mgr
        .is_scrambled_at(bubble_pos + DVec3::new(15_000.0, 0.0, 0.0)));
    assert!(!park
        .warp_bubble_mgr
        .is_scrambled_at(bubble_pos + DVec3::new(25_000.0, 0.0, 0.0)));

    let sphere_pos = DVec3::new(20_000_000.0, 0.0, 0.0);
    assert!(park.add_interdiction_sphere(5002, sphere_pos, 20_000.0));
    assert!(park.warp_bubble_mgr.is_scrambled_at(sphere_pos));

    let ship = Ball {
        id: 1001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship);

    let warp_mode = MovementMode::new_warp(DVec3::ZERO, bubble_pos, 3.0);
    park.set_mode(1001, warp_mode).expect("Initiate warp");
    if let MovementMode::Warp { destination, .. } = park.balls[&1001].mode {
        let dist_to_center = (destination - bubble_pos).length();
        assert!(
            (dist_to_center - bubble_radius).abs() < 1.0,
            "Normal ship must be trapped at bubble perimeter: dist={dist_to_center}, radius={bubble_radius}"
        );
    } else {
        panic!("Expected Warp mode");
    }

    let ship_null = Ball {
        id: 1002,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    park.add_ball(ship_null);
    park.set_interdiction_nullified(1002, true);

    let warp_mode_null = MovementMode::new_warp(DVec3::ZERO, bubble_pos, 3.0);
    park.set_mode(1002, warp_mode_null)
        .expect("Initiate warp nullified");
    if let MovementMode::Warp { destination, .. } = park.balls[&1002].mode {
        assert_eq!(
            destination, bubble_pos,
            "Interdiction nullified ship must NOT have destination intercepted by bubbles"
        );
    } else {
        panic!("Expected Warp mode for nullified ship");
    }

    park.set_ball_position(1001, bubble_pos);
    park.set_ball_position(1002, bubble_pos);
    assert!(park.is_ball_scrambled(1001), "Normal ship in bubble must be scrambled");
    assert!(
        !park.is_ball_scrambled(1002),
        "Nullified ship in bubble must NOT be scrambled"
    );

    let err = park.set_mode(
        1001,
        MovementMode::new_warp(bubble_pos, DVec3::new(30_000_000.0, 0.0, 0.0), 3.0),
    );
    assert!(err.is_err(), "Normal ship cannot warp while in bubble");

    let ok = park.set_mode(
        1002,
        MovementMode::new_warp(bubble_pos, DVec3::new(30_000_000.0, 0.0, 0.0), 3.0),
    );
    assert!(ok.is_ok(), "Nullified ship CAN warp while in bubble");

    let mjd_ship = Ball {
        id: 1003,
        type_id: 644,
        position: DVec3::new(50_000.0, 0.0, 0.0),
        velocity: DVec3::new(100.0, 0.0, 0.0),
        max_velocity: 130.0,
        inertia: 0.12,
        mass: 99_000_000.0,
        radius: 350.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    park.add_ball(mjd_ship);
    let new_pos = park.micro_jump(1003, 100_000.0).expect("Micro jump");
    assert_eq!(
        new_pos,
        DVec3::new(150_000.0, 0.0, 0.0),
        "MJD must jump exactly 100 km forward along heading"
    );
    assert_eq!(park.balls[&1003].position, DVec3::new(150_000.0, 0.0, 0.0));
}

#[test]
fn test_non_finite_coordinates_and_warp_speed_rejected() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(Ball {
        id: 1001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    });

    assert!(park.goto_point(1001, DVec3::new(f64::NAN, 0.0, 0.0)).is_err());
    assert!(park.goto_point(1001, DVec3::new(f64::INFINITY, 0.0, 0.0)).is_err());
    assert!(!park.set_ball_position(1001, DVec3::new(0.0, f64::NAN, 0.0)));
    assert!(!park.set_ball_radius(1001, f64::NAN));
    assert!(!park.set_ball_radius(1001, -10.0));
    assert!(park
        .set_mode(
            1001,
            MovementMode::GotoDirection {
                direction: DVec3::new(f64::NAN, 0.0, 0.0),
            },
        )
        .is_err());
    assert!(park
        .set_mode(
            1001,
            MovementMode::Orbit {
                target_id: 1001,
                range: f64::NAN,
            },
        )
        .is_err());
    assert!(park
        .set_mode(
            1001,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), f64::NAN),
        )
        .is_err());
    assert!(park
        .set_mode(
            1001,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 0.0),
        )
        .is_err());
}

#[test]
fn test_issue_69_no_aligning_teleport_or_mid_flight_yank_on_late_bubble() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(Ball {
        id: 2001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    // Anchor a bubble 300 km ahead (outside scramble radius, within 500 km of origin)
    // and warp toward +10,000 km.
    park.add_warp_bubble(9001, DVec3::new(300_000.0, 0.0, 0.0), 20_000.0);
    park.set_mode(
        2001,
        MovementMode::new_warp(DVec3::ZERO, DVec3::new(10_000_000.0, 0.0, 0.0), 3.0),
    )
    .unwrap();
    // Step 1 second during sub-warp alignment: ship must NOT be teleported 280 km into the bubble!
    park.step(1.0);
    assert!(
        park.balls[&2001].position.x < 1_000.0,
        "Aligning ship must not be teleported 300 km forward into bubble during alignment (got x={})",
        park.balls[&2001].position.x
    );
    assert!(
        matches!(
            park.balls[&2001].mode,
            MovementMode::Warp {
                phase: WarpPhase::Aligning,
                ..
            }
        ),
        "Ship must remain in Aligning phase on tick 1"
    );

    // Clear bubble and test mid-flight cruising immunity to newly anchored bubbles
    park.remove_warp_bubble(9001);
    let mut park2 = Ballpark::new(30_000_142);
    park2.add_ball(Ball {
        id: 2002,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.45,
        mass: 1_067_000.0,
        radius: 37.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park2
        .set_mode(
            2002,
            MovementMode::new_warp(DVec3::ZERO, DVec3::new(50_000_000_000.0, 0.0, 0.0), 3.0),
        )
        .unwrap();
    // Step until ship is in Accelerating/Cruising phase (past 5.1s alignment)
    for _ in 0..8 {
        park2.step(1.0);
    }
    let pos_in_tunnel = park2.balls[&2002].position;
    assert!(pos_in_tunnel.x > 10_000.0);
    // Anchor a bubble at the ship's current tunnel position and near destination AFTER warp entry
    park2.add_warp_bubble(9002, pos_in_tunnel, 20_000.0);
    park2.add_warp_bubble(9003, DVec3::new(50_000_000_000.0, 0.0, 0.0), 20_000.0);
    park2.step(1.0);
    assert!(
        matches!(park2.balls[&2002].mode, MovementMode::Warp { .. }),
        "Ship already in warp tunnel must not be yanked out of warp by newly anchored bubble"
    );
}

#[test]
fn test_issue_71_gate_cloak_expiry_emits_cloak_ball_special_fx_and_add_balls2() {
    let mut park = Ballpark::new(30_000_142);
    let mut cs_a = CombatState::default();
    cs_a.is_cloaked = true;
    cs_a.gate_cloak_remaining_ticks = 1;
    let mut meta_a = SlimItemMeta::default();
    meta_a.is_cloaked = true;
    park.set_ball_slim_meta(3001, meta_a);
    park.add_ball(Ball {
        id: 3001,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.combat_states.insert(3001, cs_a);
    // Add grid observer 3002
    park.add_ball(Ball {
        id: 3002,
        type_id: 587,
        position: DVec3::new(5_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    let _ = park.drain_observer_events(3002);

    let evs = park.step(1.0);
    assert!(!park.combat_states[&3001].is_cloaked);
    assert!(!park.ball_slim_meta[&3001].is_cloaked);
    assert!(evs.iter().any(|e| matches!(
        e,
        DestinyEvent::CloakBall {
            ball_id: 3001,
            cloaked: 0
        }
    )));
    assert!(evs.iter().any(|e| matches!(
        e,
        DestinyEvent::OnSpecialFX {
            ball_id: 3001,
            is_active: false,
            ..
        }
    )));
    let obs_evs = park.drain_observer_events(3002);
    assert!(
        obs_evs
            .iter()
            .any(|e| matches!(e, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == 3001))),
        "Grid observer must receive AddBalls2 when gate cloak expires"
    );
}

#[test]
fn test_issue_72_proximity_decloak_ignores_warp_bubbles_and_catches_high_speed_swept_pass() {
    let mut park = Ballpark::new(30_000_142);
    let mut cs_cloaked = CombatState::default();
    cs_cloaked.is_cloaked = true;
    cs_cloaked.is_covert_cloak = true;
    park.add_ball(Ball {
        id: 4001,
        type_id: 11172,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.combat_states.insert(4001, cs_cloaked);
    // Add an intangible Field warp bubble (radius 20,000 m) centered 5,000 m away.
    // Even though surface_dist = 5000 - 40 - 20000 < 2000, a warp bubble must NOT decloak the ship!
    park.add_interdiction_sphere(4002, DVec3::new(5_000.0, 0.0, 0.0), 20_000.0);
    park.step(1.0);
    assert!(
        park.combat_states[&4001].is_cloaked,
        "Intangible warp disruption bubble must not proximity-decloak a cloaked ship"
    );

    // Add a second cloaked ship 500 m away: two cloaked ships must not decloak each other.
    let mut cs_cloaked2 = CombatState::default();
    cs_cloaked2.is_cloaked = true;
    park.add_ball(Ball {
        id: 4003,
        type_id: 11172,
        position: DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    park.combat_states.insert(4003, cs_cloaked2);
    park.step(1.0);
    assert!(
        park.combat_states[&4001].is_cloaked && park.combat_states[&4003].is_cloaked,
        "Two cloaked ships must not decloak each other"
    );

    // Now add an uncloaked fast interceptor that starts at x = -3000, y = 500 and moves at +6000 m/s
    // so at tick end it is at x = +3000, y = 500 (> 3000 m away at both endpoints, but 500 m at t=0.5!).
    park.add_ball(Ball {
        id: 4004,
        type_id: 11176,
        position: DVec3::new(-3_000.0, 500.0, 0.0),
        velocity: DVec3::new(6_000.0, 0.0, 0.0),
        max_velocity: 6_000.0,
        inertia: 0.01,
        mass: 1_000_000.0,
        radius: 30.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    });
    park.step(1.0);
    assert!(
        !park.combat_states[&4001].is_cloaked,
        "Swept-sphere closest approach must decloak ship when fast pass crosses within 2,000 m mid-tick"
    );
}
