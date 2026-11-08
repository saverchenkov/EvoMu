//! Deep branch-coverage and mutation-killing unit tests for `lib.rs`, `ballpark.rs`, and `ballpark_targeting.rs`.

use super::*;

fn make_ball(id: BallId, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode,
    }
}

fn lock_ball(park: &mut Ballpark, src: BallId, target_id: BallId, max_range_m: Option<f64>) -> bool {
    park.add_target_lock(
        src,
        TargetLock {
            target_id,
            lock_time_ms: 100.0,
            remaining_lock_ms: 100.0,
            locked: false,
            max_range_m,
        },
    )
}

fn run_bump_step(park: &mut Ballpark, pre: &BTreeMap<BallId, Ball>, dt: f64) {
    let pre_pos: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre
        .iter()
        .map(|(&id, b)| (id, (b.position, b.velocity, b.radius)))
        .collect();
    park.step_collision_bumping(pre, &pre_pos, dt);
}

fn warp_m(
    origin: DVec3,
    destination: DVec3,
    warp_speed_au_s: f64,
    total_distance: f64,
    elapsed_warp_time: f64,
) -> MovementMode {
    MovementMode::Warp {
        origin,
        destination,
        warp_speed_au_s,
        total_distance,
        elapsed_warp_time,
        phase: WarpPhase::Aligning,
    }
}

fn add_ship_with(
    park: &mut Ballpark,
    id: BallId,
    pos: DVec3,
    vel: DVec3,
    radius: f64,
    heavy: bool,
) {
    let mut b = make_ball(id, pos, MovementMode::Stop);
    b.velocity = vel;
    b.radius = radius;
    if heavy {
        b.inertia = 1e9;
        b.mass = 1e9;
    }
    park.add_ball(b);
}

#[test]
fn combat_state_rng_damage_warp_bubbles_grid_bounds_docking_and_mushroom_edges() {
    assert_eq!(
        CombatState::seed_rng_from_u64(0x61C8_8646_80B5_83EB),
        0x9E37_79B9_7F4A_7C15
    );
    assert_ne!(CombatState::seed_rng_from_u64(1), 0);

    let mut cs = CombatState {
        warp_exit_invulnerable_remaining_ticks: 2,
        shield_hp: 0.0,
        armor_hp: 0.0,
        structure_hp: 1.001e-6,
        ..Default::default()
    };
    assert!(cs.is_warp_exit_invulnerable() && cs.is_invulnerable());
    cs.warp_exit_invulnerable_remaining_ticks = 0;
    assert!(!cs.is_invulnerable());
    assert_eq!(cs.apply_damage(DamageVector::default()), (0.0, false));
    cs.structure_hp = 1.0e-6;
    assert_eq!(cs.apply_damage(DamageVector::default()), (0.0, true));

    let mut zero_res = CombatState {
        shield_hp: 100.0,
        armor_hp: 100.0,
        structure_hp: 100.0,
        shield_resonance: [0.0; 4],
        armor_resonance: [0.0; 4],
        structure_resonance: [0.0; 4],
        ..Default::default()
    };
    assert_eq!(
        zero_res.apply_damage(DamageVector::new(-40.0, 25.0, -5.0, 10.0)),
        (0.0, false)
    );
    assert_eq!(
        (zero_res.shield_hp, zero_res.armor_hp, zero_res.structure_hp),
        (100.0, 100.0, 100.0)
    );

    let mut armor_immune = CombatState {
        shield_hp: 0.0,
        armor_hp: 50.0,
        structure_hp: 50.0,
        armor_resonance: [0.0; 4],
        structure_resonance: [1.0; 4],
        ..Default::default()
    };
    assert_eq!(
        armor_immune.apply_damage(DamageVector::new(20.0, 0.0, 0.0, 0.0)),
        (0.0, false)
    );
    assert_eq!(armor_immune.structure_hp, 50.0);

    let mut partial = CombatState {
        shield_hp: 0.0,
        armor_hp: 0.0,
        structure_hp: 200.0,
        structure_resonance: [0.5; 4],
        ..Default::default()
    };
    assert_eq!(
        partial.apply_damage(DamageVector::new(100.0, 0.0, 0.0, 0.0)),
        (50.0, false)
    );
    assert_eq!(partial.apply_true_damage(30.0), (30.0, false));
    assert_eq!(partial.apply_true_damage(-5.0), (0.0, false));
    partial.structure_hp = 1.0e-6;
    assert_eq!(partial.apply_true_damage(0.0), (0.0, true));

    let mut pod = make_ball(1, DVec3::ZERO, MovementMode::Stop);
    pod.inertia = 0.05;
    assert_eq!(pod.align_server_ticks(), 1);
    pod.inertia = 1.0;
    assert_eq!(pod.align_server_ticks(), 2);

    let grid = GridBubble::new(1, DVec3::new(100.0, 200.0, 300.0), 500.0);
    assert!(grid.contains(grid.min_bounds) && grid.contains(grid.max_bounds));
    assert!(!grid.contains(grid.min_bounds - DVec3::new(0.01, 0.0, 0.0)));
    assert!(!grid.contains(grid.max_bounds + DVec3::new(0.0, 0.01, 0.0)));
    assert!(!grid.contains(grid.max_bounds + DVec3::new(0.0, 0.0, 0.01)));
    assert!(!grid.overlaps_bounds(
        DVec3::new(-1000.0, 0.0, 0.0),
        DVec3::new(grid.min_bounds.x, 500.0, 500.0),
    ));
    assert!(!grid.overlaps_bounds(
        DVec3::new(0.0, grid.max_bounds.y, 0.0),
        DVec3::new(500.0, 2000.0, 500.0),
    ));
    assert!(grid.overlaps_bounds(
        grid.max_bounds - DVec3::splat(1.0),
        grid.max_bounds + DVec3::splat(10.0),
    ));

    let clamped_wb = WarpBubble::new(90, DVec3::ZERO, 0.25);
    assert_eq!(clamped_wb.radius, 1.0);
    assert!(clamped_wb.contains(DVec3::new(1.0 + 9e-4, 0.0, 0.0)));
    assert!(!clamped_wb.contains(DVec3::new(1.0 + 2e-3, 0.0, 0.0)));

    let wb = WarpBubble::new(91, DVec3::new(100_000.0, 10_000.0, 0.0), 10_000.0);
    assert!(wb.intercept_warp(DVec3::ZERO, DVec3::new(0.99, 0.0, 0.0)).is_none());
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(f64::NAN, 0.0, 0.0))
        .is_none());
    let hit_tangent = wb
        .intercept_warp(DVec3::ZERO, DVec3::new(200_000.0, 0.0, 0.0))
        .unwrap();
    assert!((hit_tangent.x - 100_000.0).abs() < 1e-6);

    let wb_center = WarpBubble::new(92, DVec3::new(10_000.0, 0.0, 0.0), 10_000.0);
    assert_eq!(
        wb_center.intercept_warp(DVec3::ZERO, DVec3::new(200_000.0, 0.0, 0.0)),
        Some(DVec3::ZERO)
    );
    assert_eq!(
        wb_center.intercept_warp(DVec3::new(5_000.0, 0.0, 0.0), DVec3::new(200_000.0, 0.0, 0.0)),
        Some(DVec3::new(5_000.0, 0.0, 0.0))
    );

    let dest = DVec3::new(600_000.0, 0.0, 0.0);
    assert!(WarpBubble::new(1, DVec3::new(95_000.0, 0.0, 0.0), 4_999.0)
        .intercept_warp(DVec3::ZERO, dest)
        .is_none());
    assert!(
        (WarpBubble::new(2, DVec3::new(95_000.0, 0.0, 0.0), 5_000.0)
            .intercept_warp(DVec3::ZERO, dest)
            .unwrap()
            .x
            - 90_000.0)
            .abs()
            < 1e-6
    );
    assert!(WarpBubble::new(3, DVec3::new(1_105_001.0, 0.0, 0.0), 5_000.0)
        .intercept_warp(DVec3::ZERO, dest)
        .is_none());
    assert!(
        (WarpBubble::new(4, DVec3::new(1_105_000.0, 0.0, 0.0), 5_000.0)
            .intercept_warp(DVec3::ZERO, dest)
            .unwrap()
            .x
            - 1_100_000.0)
            .abs()
            < 1e-6
    );

    let mut mgr = WarpBubbleMgr::new();
    mgr.add_bubble(WarpBubble::new(1, DVec3::new(400_000.0, 0.0, 0.0), 5_000.0));
    mgr.add_bubble(WarpBubble::new(2, DVec3::new(200_000.0, 0.0, 0.0), 5_000.0));
    let first = mgr.check_interception(DVec3::ZERO, dest).unwrap();
    assert!((first.1.x - 195_000.0).abs() < 1e-6);

    let mut park = Ballpark::new(30_000_142);
    assert!(!park.add_mushroom_ball(50, 99, DVec3::ZERO, 1000.0, 0.0, 1.0, 777));
    assert!(!park.add_mushroom_ball(50, 99, DVec3::ZERO, 1000.0, f64::NAN, 1.0, 777));
    park.set_ball_slim_meta(50, SlimItemMeta::default());
    assert!(park.add_mushroom_ball(50, 99, DVec3::ZERO, 1000.0, 5.0, 1.0, 777));
    assert_eq!(park.ball_slim_meta[&50].owner_id, 777);

    let mut ship = make_ball(10, DVec3::new(100.0, 200.0, 300.0), MovementMode::Stop);
    ship.velocity = DVec3::new(50.0, 0.0, 0.0);
    ship.speed_fraction = 0.5;
    park.add_ball(ship);
    assert!(park.set_ball_free(10, true));
    assert_eq!(park.balls[&10].velocity.x, 50.0);
    assert!(park.set_ball_free(10, false));
    assert_eq!(park.balls[&10].velocity, DVec3::ZERO);

    assert!(park.accept_docking_at(999, 60_000_001, DVec3::ZERO).is_none());
    let docked = park.accept_docking(10, 60_999_999).unwrap();
    assert_eq!(docked.id, 10);
    assert!(park.drain_observer_events(10).iter().any(|e| matches!(
        e,
        DestinyEvent::OnDockingAccepted {
            end_pos,
            station_id: 60_999_999,
            ..
        } if *end_pos == [0.0, 0.0, 0.0]
    )));

    park.add_ball(make_ball(11, DVec3::ZERO, MovementMode::Stop));
    park.observer_events.clear();
    assert!(park.accept_docking_at(11, 60_000_001, DVec3::X).is_some());
    assert!(park
        .drain_observer_events(11)
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnDockingAccepted { .. })));
}

#[test]
fn micro_jump_capitals_marauders_heading_and_cloak_edge_branches() {
    let mut park = Ballpark::new(30_000_142);

    let mut jumper = make_ball(
        1,
        DVec3::ZERO,
        MovementMode::GotoDirection {
            direction: DVec3::new(0.0, 3.0, 4.0),
        },
    );
    jumper.velocity = DVec3::new(0.005, 0.0, 0.0);
    park.add_ball(jumper);
    park.set_ball_group_id(1, 547);
    assert!(park.micro_jump(1, 50_000.0).is_ok());
    assert!((park.balls[&1].position - DVec3::new(0.0, 30_000.0, 40_000.0)).length() < 1e-6);

    let mut alt_cap = make_ball(2, DVec3::ZERO, MovementMode::Stop);
    alt_cap.type_id = 19724;
    park.add_ball(alt_cap);
    park.set_ball_group_id(2, 0);
    assert!(park.micro_jump(2, 100_000.5).is_ok());
    assert!((park.balls[&2].position.x - 250_000.0).abs() < 1e-6);

    let mut pod = make_ball(3, DVec3::ZERO, MovementMode::Stop);
    pod.type_id = 670;
    park.add_ball(pod);
    assert!(park.micro_jump(3, 100_000.0).is_ok());
    assert!((park.balls[&3].position.x - 100_000.0).abs() < 1e-6);

    let mut mjfg = Ballpark::new(30_000_142);
    mjfg.add_ball(make_ball(10, DVec3::ZERO, MovementMode::Stop));
    mjfg.add_ball(make_ball(20, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop));
    mjfg.add_ball(make_ball(30, DVec3::new(-1_000.0, 0.0, 0.0), MovementMode::Stop));
    mjfg.set_ball_group_id(20, 485);
    mjfg.set_ball_group_id(30, 485);

    let mut marauder = make_ball(40, DVec3::new(0.0, 2_000.0, 0.0), MovementMode::Stop);
    marauder.type_id = 28661;
    mjfg.add_ball(marauder);
    mjfg.set_ball_group_id(40, 25);
    mjfg.set_warp_scramble_strength(40, 2);
    mjfg.combat_states.get_mut(&40).unwrap().disallow_assistance = true;

    mjfg.add_ball(make_ball(50, DVec3::new(0.0, 500.0, 0.0), MovementMode::Stop));
    mjfg.combat_states.get_mut(&50).unwrap().is_tethered = true;
    mjfg.add_ball(make_ball(60, DVec3::new(0.0, 0.0, 6_000.0), MovementMode::Stop));
    mjfg.add_ball(make_ball(70, DVec3::new(0.0, 0.0, 6_001.0), MovementMode::Stop));

    let jumped = mjfg
        .micro_jump_field_with_capitals(10, 100_000.0, 6_000.0, 10, 1)
        .unwrap();
    assert_eq!(jumped, vec![10, 20, 40, 60]);

    let mut cp = Ballpark::new(30_000_142);
    cp.add_ball(make_ball(1, DVec3::ZERO, MovementMode::Stop));
    cp.set_ball_slim_meta(1, SlimItemMeta::default());
    cp.add_ball(make_ball(2, DVec3::new(100.0, 0.0, 0.0), MovementMode::Field));
    cp.add_ball(make_ball(3, DVec3::new(2_100.0, 0.0, 0.0), MovementMode::Stop));
    assert!(cp.activate_cloak(1, false, 5.0, 900.0).is_err());

    cp.set_ball_position(3, DVec3::new(2_100.1, 0.0, 0.0));
    assert!(cp.activate_cloak(1, false, 5.0, 900.0).is_ok());
    assert!(cp.ball_slim_meta[&1].is_cloaked);

    {
        let cs = cp.combat_states.get_mut(&1).unwrap();
        cs.base_max_velocity = Some(300.0);
        cs.cloak_velocity_multiplier = Some(1.0);
        cs.cloak_configured_targeting_delay_s = 0.0;
    }
    cp.balls.get_mut(&1).unwrap().max_velocity = 300.0;
    assert!(cp.deactivate_cloak(1));
    assert!(!cp.deactivate_cloak(1));
    assert_eq!(cp.balls[&1].max_velocity, 300.0);
    assert_eq!(cp.combat_states[&1].decloak_targeting_delay_remaining_s, 0.0);
    assert!(!cp.ball_slim_meta[&1].is_cloaked);
}

#[test]
fn grid_visibility_damage_set_mode_and_speed_fraction_edge_branches() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ball(1, DVec3::ZERO, MovementMode::Stop));
    park.add_ball(make_ball(2, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop));
    assert!(lock_ball(&mut park, 1, 2, None));

    let saved_gid = park.ball_grid_ids.remove(&2).unwrap();
    assert!(!park.in_same_bubble(1, 2));
    park.ball_grid_ids.insert(2, saved_gid);
    assert!(park.in_same_bubble(1, 2));

    park.queue_event_for_grid_observers(999_999, None, &DestinyEvent::OnAudioActivated);
    park.remove_ball(2);
    assert!(park.get_target_locks(1).is_empty());

    // Zero max HP + missing grid_id: first non-lethal hit queues observer_events[&3], second destroys
    let mut t3 = make_ball(3, DVec3::new(500.0, 0.0, 0.0), MovementMode::Stop);
    t3.inertia = 0.0;
    park.add_ball(t3);
    {
        let cs = park.combat_states.get_mut(&3).unwrap();
        cs.max_shield_hp = 0.0;
        cs.max_armor_hp = 0.0;
        cs.max_structure_hp = 0.0;
        cs.shield_hp = 0.0;
        cs.armor_hp = 0.0;
        cs.structure_hp = 20.0;
    }
    park.ball_grid_ids.remove(&3);
    park.drain_all_observer_events();
    assert_eq!(
        park.apply_damage_to_ball(3, DamageVector::new(5.0, 0.0, 0.0, 0.0))
            .map(|(a, d, _)| (a, d)),
        Some((5.0, false))
    );
    let evs3 = park.drain_observer_events(3);
    assert!(evs3.iter().any(|e| matches!(
        e,
        DestinyEvent::OnDamageStateChange {
            ball_id: 3,
            shield_ratio,
            armor_ratio,
            structure_ratio,
        } if *shield_ratio == 0.0 && *armor_ratio == 0.0 && *structure_ratio == 0.0
    )));
    assert!(evs3.iter().any(|e| matches!(
        e,
        DestinyEvent::OnFleetDamageStateChange { ball_id: 3, .. }
    )));
    assert_eq!(
        park.apply_damage_to_ball(3, DamageVector::new(20.0, 0.0, 0.0, 0.0))
            .map(|(a, d, _)| (a, d)),
        Some((15.0, true))
    );
    assert!(!park.balls.contains_key(&3));

    let mut t4 = make_ball(4, DVec3::new(600.0, 0.0, 0.0), MovementMode::Stop);
    t4.inertia = 0.0;
    park.add_ball(t4);
    {
        let cs = park.combat_states.get_mut(&4).unwrap();
        cs.max_shield_hp = 0.0;
        cs.max_armor_hp = 0.0;
        cs.max_structure_hp = 0.0;
        cs.structure_hp = 5.0;
    }
    park.ball_grid_ids.remove(&4);
    assert_eq!(
        park.apply_direct_structure_damage_to_ball(4, 5.0)
            .map(|(a, d, _)| (a, d)),
        Some((5.0, true))
    );

    assert!(park.goto_point(999_888, DVec3::X).is_err());
    park.add_ball(make_ball(5, DVec3::new(10.0, 20.0, 30.0), MovementMode::Stop));
    park.balls.get_mut(&5).unwrap().speed_fraction = 0.3;
    park.goto_point(5, DVec3::new(10.0, 20.0, 30.0)).unwrap();
    assert_eq!(park.balls[&5].speed_fraction, 0.3);
    assert_eq!(
        park.balls[&5].mode,
        MovementMode::GotoDirection { direction: DVec3::X }
    );

    assert!(!park.set_ball_radius(999_888, 100.0));
    assert!(!park.set_ball_radius(5, 0.0));
    assert!(park.set_ball_radius(5, 0.25));
    assert_eq!(park.balls[&5].radius, 1.0);
    assert!(park.add_interdiction_sphere(30, DVec3::ZERO, 20_000.0));
    assert!(park.set_ball_position(30, DVec3::new(1_000_000.0, 0.0, 0.0)));
    assert_eq!(
        park.warp_bubble_mgr.bubbles[&30].position,
        DVec3::new(1_000_000.0, 0.0, 0.0)
    );
    park.add_ball(make_ball(
        6,
        DVec3::new(50_000_000.0, 0.0, 0.0),
        MovementMode::Stop,
    ));
    park.combat_states.get_mut(&5).unwrap().is_cloaked = true;
    park.drain_all_observer_events();
    park.set_ball_position(5, DVec3::new(50_001_000.0, 0.0, 0.0));
    assert!(!park
        .drain_observer_events(6)
        .iter()
        .any(|e| matches!(e, DestinyEvent::AddBalls2 { .. })));
    assert!(park.set_mode(999_888, MovementMode::Stop).is_err());
    assert!(park
        .set_mode(5, MovementMode::Follow { target_id: 6, range: -1.0 })
        .is_err());
    assert!(park
        .set_mode(5, MovementMode::Orbit { target_id: 6, range: -1.0 })
        .is_err());
    for bad_warp in [
        warp_m(DVec3::NAN, DVec3::X, 3.0, 100.0, 0.0),
        warp_m(DVec3::ZERO, DVec3::X, 0.0, 100.0, 0.0),
        warp_m(DVec3::ZERO, DVec3::X, 3.0, -10.0, 0.0),
        warp_m(DVec3::ZERO, DVec3::X, 3.0, 100.0, -1.0),
    ] {
        assert!(park.set_mode(5, bad_warp).is_err());
    }
    {
        let cs = park.combat_states.get_mut(&5).unwrap();
        cs.is_cloaked = true;
        cs.is_covert_cloak = true;
        cs.is_emergency_warping = true;
        cs.gate_cloak_remaining_ticks = 4;
    }
    park.set_ball_slim_meta(
        5,
        SlimItemMeta {
            is_cloaked: true,
            ..Default::default()
        },
    );
    assert!(park
        .set_mode(
            5,
            warp_m(
                DVec3::new(50_001_000.0, 0.0, 0.0),
                DVec3::new(50_200_000.0, 0.0, 0.0),
                3.0,
                199_000.0,
                0.0,
            )
        )
        .is_ok());
    assert!(park
        .set_mode(5, MovementMode::Orbit { target_id: 6, range: 500.0 })
        .is_ok());
    assert!(!park.combat_states[&5].is_emergency_warping);
    assert_eq!(park.combat_states[&5].gate_cloak_remaining_ticks, 0);
    assert!(!park.ball_slim_meta[&5].is_cloaked);
    assert!(park.set_mode(5, MovementMode::Boid).is_ok());
    assert!(!park.set_speed_fraction(999_888, 0.5));
    let mut structure = make_ball(7, DVec3::ZERO, MovementMode::Stop);
    structure.max_velocity = 0.0;
    park.add_ball(structure);
    assert!(park.is_immobile_structure_or_static(7));
    assert!(!park.set_speed_fraction(7, 0.5));
    assert!(park.set_speed_fraction(7, 0.0));
    park.combat_states.get_mut(&5).unwrap().gate_cloak_remaining_ticks = 5;
    assert!(park.set_speed_fraction(5, -0.5));
    assert_eq!(park.balls[&5].speed_fraction, 0.0);
    assert_eq!(park.combat_states[&5].gate_cloak_remaining_ticks, 5);
    assert!(park.set_speed_fraction(5, 1.5));
    assert_eq!(park.balls[&5].speed_fraction, 1.0);
    assert_eq!(park.combat_states[&5].gate_cloak_remaining_ticks, 0);
}

#[test]
fn targeting_sync_breaks_ecm_without_combat_state_and_swept_overlap_collisions() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ball(1, DVec3::ZERO, MovementMode::Stop));
    park.add_ball(make_ball(2, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop));

    {
        let cs2 = park.combat_states.get_mut(&2).unwrap();
        cs2.sync_invuln_with_undock_timer = true;
        cs2.undock_invulnerable_remaining_ticks = 0;
        cs2.warp_exit_invulnerable_remaining_ticks = 4;
    }
    assert!(lock_ball(&mut park, 1, 2, Some(50_000.0)));
    assert_eq!(park.combat_states[&2].warp_exit_invulnerable_remaining_ticks, 0);

    park.combat_states.remove(&1);
    park.apply_ecm_jam(1, 3.0);
    assert!(park.get_target_locks(1).is_empty());

    let gid1 = park.ball_grid_ids[&1];
    let g1 = park.grids.remove(&gid1).unwrap();
    assert!(park.dynamic_balls_in_bubble(1).is_empty());
    park.grids.insert(gid1, g1);

    let mut col_park = Ballpark::new(30_000_142);
    add_ship_with(&mut col_park, 10, DVec3::new(150.0, 0.0, 0.0), DVec3::new(200.0, 0.0, 0.0), 50.0, true);
    col_park.add_static_ball(make_ball(20, DVec3::new(52.0, 0.0, 0.0), MovementMode::Rigid), 15);
    let mut pre = col_park.balls.clone();
    pre.get_mut(&10).unwrap().position = DVec3::new(-150.0, 0.0, 0.0);
    pre.get_mut(&20).unwrap().position = DVec3::new(50.0, 0.0, 0.0);
    run_bump_step(&mut col_park, &pre, 1.0);
    assert!(col_park.balls[&10].velocity.x < 0.0);

    let mut col_park2 = Ballpark::new(30_000_142);
    col_park2.add_static_ball(make_ball(5, DVec3::new(2.0, 0.0, 0.0), MovementMode::Rigid), 15);
    add_ship_with(&mut col_park2, 25, DVec3::new(-250.0, 0.0, 0.0), DVec3::new(-200.0, 0.0, 0.0), 50.0, true);
    let mut stat40 = make_ball(40, DVec3::new(0.0, 5_000.0, 0.0), MovementMode::Rigid);
    stat40.radius = 500.0;
    col_park2.add_static_ball(stat40, 15);
    add_ship_with(&mut col_park2, 45, DVec3::new(0.0, 5_000.0, 0.0), DVec3::new(-10.0, 0.0, 0.0), 50.0, false);
    let mut pre2 = col_park2.balls.clone();
    pre2.get_mut(&5).unwrap().position = DVec3::ZERO;
    pre2.get_mut(&25).unwrap().position = DVec3::new(250.0, 0.0, 0.0);
    run_bump_step(&mut col_park2, &pre2, 1.0);
    assert!(col_park2.balls[&25].velocity.x > 0.0);
    assert!((col_park2.balls[&45].position.x + 550.0).abs() < 1e-6);

    let mut col_park3 = Ballpark::new(30_000_142);
    let mut stat50 = make_ball(50, DVec3::ZERO, MovementMode::Rigid);
    stat50.radius = 12_000.0;
    col_park3.add_static_ball(stat50, 15);
    add_ship_with(&mut col_park3, 40, DVec3::new(-11_900.0, 0.0, 0.0), DVec3::new(15.0, 0.0, 0.0), 50.0, false);
    add_ship_with(&mut col_park3, 60, DVec3::new(11_500.0, 0.0, 0.0), DVec3::new(25.0, 0.0, 0.0), 50.0, false);
    add_ship_with(&mut col_park3, 70, DVec3::new(1_000.0, 0.0, 0.0), DVec3::new(-10.0, 0.0, 0.0), 50.0, false);
    let pre3 = col_park3.balls.clone();
    run_bump_step(&mut col_park3, &pre3, 1.0);
    assert!(col_park3.balls[&40].velocity.x < 0.0);
    assert!((col_park3.balls[&60].position.x - 12_050.0).abs() < 1e-6);
    assert_eq!(col_park3.balls[&60].velocity.x, 25.0);
    assert_eq!(col_park3.balls[&70].position.x, 1_000.0);

    let mut col_park4 = Ballpark::new(30_000_142);
    add_ship_with(&mut col_park4, 1, DVec3::new(300.0, 0.0, 0.0), DVec3::new(400.0, 0.0, 0.0), 50.0, true);
    col_park4.add_static_ball(make_ball(2, DVec3::new(220.0, 0.0, 0.0), MovementMode::Rigid), 15);
    col_park4.add_static_ball(make_ball(3, DVec3::new(20.0, 0.0, 0.0), MovementMode::Rigid), 15);
    add_ship_with(&mut col_park4, 10, DVec3::new(100.0, 5_000.0, 0.0), DVec3::new(200.0, 0.0, 0.0), 50.0, true);
    add_ship_with(&mut col_park4, 11, DVec3::new(-100.0, 5_000.0, 0.0), DVec3::new(-200.0, 0.0, 0.0), 50.0, true);
    let mut pre4 = col_park4.balls.clone();
    pre4.get_mut(&1).unwrap().position = DVec3::new(-100.0, 0.0, 0.0);
    pre4.get_mut(&10).unwrap().position = DVec3::new(-100.0, 5_000.0, 0.0);
    pre4.get_mut(&11).unwrap().position = DVec3::new(100.0, 5_000.0, 0.0);
    run_bump_step(&mut col_park4, &pre4, 1.0);
    assert!(col_park4.balls[&1].velocity.x < 0.0);
    assert!(col_park4.balls[&10].velocity.x < 0.0);
    assert!(col_park4.balls[&11].velocity.x > 0.0);
}
