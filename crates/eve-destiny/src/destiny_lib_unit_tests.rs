//! Unit and mutation-killing tests for `eve-destiny` root module (`lib.rs`).

use super::*;

fn sample_ball(id: BallId, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::new(10.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn damage_vector_combat_state_and_ball_align_math_kill_mutations() {
    assert_eq!(AU_METERS, 149_597_870_700.0);
    assert_eq!(BUBBLE_RADIUS_METERS, 8_000_000.0);
    assert_eq!(BUBBLE_TRANSITION_THRESHOLD_METERS, 10_000_000.0);
    assert_eq!(DECLOAK_DISTANCE_METERS, 2_000.0);

    let w = MovementMode::new_warp(DVec3::ZERO, DVec3::new(3000.0, 4000.0, 0.0), 3.0);
    assert_eq!(
        w,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(3000.0, 4000.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 5000.0,
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
        }
    );

    let dv = DamageVector::new(10.0, 20.0, 30.0, 40.0);
    assert_eq!(dv.total(), 100.0);
    assert_eq!(dv.scaled(0.5), DamageVector::new(5.0, 10.0, 15.0, 20.0));

    let s1 = CombatState::seed_rng_from_u64(0);
    let s2 = CombatState::seed_rng_from_u64(1);
    assert_ne!(s1, 0);
    assert_ne!(s2, 0);
    assert_ne!(s1, s2);

    let mut cs = CombatState::new();
    cs.set_interdiction_nullified(true);
    assert!(cs.is_interdiction_nullified);

    // Warp scramble boundaries: strength > 0 && strength >= core_strength
    cs.warp_core_strength = 0;
    cs.warp_scramble_strength = 0;
    assert!(!cs.is_warp_scrambled());
    cs.warp_core_strength = 2;
    cs.warp_scramble_strength = 1;
    assert!(!cs.is_warp_scrambled());
    cs.warp_scramble_strength = 2;
    assert!(cs.is_warp_scrambled());

    // Action breaks synchronization
    cs.gate_cloak_remaining_ticks = 5;
    cs.is_cloaked = true;
    cs.warp_exit_invulnerable_remaining_ticks = 10;
    cs.undock_invulnerable_remaining_ticks = 0;
    cs.sync_invuln_with_undock_timer = true;
    assert!(!cs.is_warp_exit_invulnerable());
    assert!(!cs.is_invulnerable());
    cs.sync_action_breaks();
    assert_eq!(cs.gate_cloak_remaining_ticks, 0);
    assert!(!cs.is_cloaked);
    assert_eq!(cs.warp_exit_invulnerable_remaining_ticks, 0);
    assert!(!cs.sync_invuln_with_undock_timer);

    // Invulnerable states reject both normal and true damage
    cs.is_tethered = true;
    assert!(cs.is_invulnerable());
    assert_eq!(cs.apply_damage(DamageVector::new(500.0, 0.0, 0.0, 0.0)), (0.0, false));
    assert_eq!(cs.apply_true_damage(500.0), (0.0, false));
    cs.is_tethered = false;

    // Multi-layer damage spillover with resonances
    cs.shield_hp = 100.0;
    cs.armor_hp = 100.0;
    cs.structure_hp = 100.0;
    cs.shield_resonance = [0.5, 1.0, 1.0, 1.0]; // 50% EM resist -> 400 raw EM = 200 eff shield -> 200 raw EM spills to armor
    cs.armor_resonance = [0.5, 1.0, 1.0, 1.0]; // 50% EM resist -> 200 raw EM = 100 eff armor -> 0 spills to structure
    let (applied, destroyed) = cs.apply_damage(DamageVector::new(400.0, 0.0, 0.0, 0.0));
    assert!((applied - 200.0).abs() < 1e-6);
    assert!(!destroyed);
    assert_eq!(cs.shield_hp, 0.0);
    assert_eq!(cs.armor_hp, 0.0);
    assert!((cs.structure_hp - 100.0).abs() < 1e-6);

    // True damage ignores resonance and destroys structure
    let (true_applied, killed) = cs.apply_true_damage(150.0);
    assert!((true_applied - 100.0).abs() < 1e-6);
    assert!(killed);
    assert_eq!(cs.structure_hp, 0.0);

    // Ball align math: tau = (3.0 * 1_200_000) / 1_000_000 = 3.6s, align = -ln(0.25)*3.6 = 4.9906597s -> 5 ticks
    let b = sample_ball(1, DVec3::ZERO);
    assert!((b.time_constant() - 3.6).abs() < 1e-9);
    assert!((b.align_time_seconds() - (-0.25_f64.ln() * 3.6)).abs() < 1e-9);
    assert_eq!(b.align_server_ticks(), 5);
}

#[test]
fn grid_bubble_and_warp_bubble_interception_geometry() {
    let g = GridBubble::new(1, DVec3::new(100.0, 200.0, 300.0), 1000.0);
    assert!(g.contains(DVec3::new(-900.0, 200.0, 1300.0)));
    assert!(!g.contains(DVec3::new(-901.0, 200.0, 300.0)));
    assert!(g.overlaps_bounds(DVec3::new(1050.0, 0.0, 0.0), DVec3::new(1200.0, 400.0, 400.0)));
    assert!(!g.overlaps_bounds(DVec3::new(1100.0, 0.0, 0.0), DVec3::new(1200.0, 400.0, 400.0)));

    // WarpBubble radius clamp & contains
    let wb_small = WarpBubble::new(10, DVec3::ZERO, 0.0);
    assert_eq!(wb_small.radius, 1.0);
    let wb = WarpBubble::new(20, DVec3::new(1_000_000.0, 0.0, 0.0), 20_000.0);
    assert!(wb.contains(DVec3::new(1_020_000.0, 0.0, 0.0)));
    assert!(!wb.contains(DVec3::new(1_020_001.0, 0.0, 0.0)));

    // Catch bubble within 500km of destination (1_000_000m) intercepts at perimeter 980_000m
    let intercept = wb
        .intercept_warp(DVec3::ZERO, DVec3::new(1_100_000.0, 0.0, 0.0))
        .unwrap();
    assert!((intercept.x - 980_000.0).abs() < 1e-3);

    // Mid-warp bubble > 500km before destination does NOT intercept
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(2_000_000.0, 0.0, 0.0))
        .is_none());

    let mut mgr = WarpBubbleMgr::new();
    mgr.add_bubble(wb);
    mgr.add_bubble(WarpBubble::new(
        21,
        DVec3::new(900_000.0, 0.0, 0.0),
        20_000.0,
    ));
    assert!(mgr.is_scrambled_at(DVec3::new(900_000.0, 0.0, 0.0)));
    assert!(!mgr.is_scrambled_at(DVec3::ZERO));
    let (first_id, first_pos) = mgr
        .check_interception(DVec3::ZERO, DVec3::new(1_100_000.0, 0.0, 0.0))
        .unwrap();
    assert_eq!(first_id, 21);
    assert!((first_pos.x - 880_000.0).abs() < 1e-3);
    assert!(mgr.remove_bubble(21).is_some());
}

#[test]
fn ballpark_special_modes_docking_and_event_helpers() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(sample_ball(1, DVec3::new(100.0, 200.0, 300.0)));
    park.add_ball(sample_ball(2, DVec3::new(400.0, 500.0, 600.0)));

    // Mushroom ball validation & creation
    assert!(!park.add_mushroom_ball(10, 100, DVec3::ZERO, 500.0, 0.0, 10.0, 9001));
    assert!(!park.add_mushroom_ball(1, 100, DVec3::ZERO, 500.0, 5.0, 10.0, 9001));
    assert!(park.add_mushroom_ball(10, 100, DVec3::ZERO, 500.0, 5.0, 10.0, 9001));
    assert_eq!(park.balls[&10].mode, MovementMode::Mushroom);
    assert_eq!(park.mushroom_states[&10].owner_id, 9001);

    // Troll, Boid, Miniball, Formation
    assert!(park.set_ball_troll(999, 5).is_err());
    assert_eq!(park.set_ball_troll(1, 5).unwrap(), 6);
    assert_eq!(park.balls[&1].mode, MovementMode::Troll);

    assert!(park.set_boid(999, true).is_err());
    park.set_boid(1, true).unwrap();
    assert_eq!(park.balls[&1].mode, MovementMode::Boid);
    park.set_boid(1, false).unwrap();
    assert_eq!(park.balls[&1].mode, MovementMode::Stop);

    assert!(park.add_miniball(20, 1, DVec3::X, 0.0).is_err());
    assert!(park.add_miniball(1, 1, DVec3::X, 10.0).is_err());
    assert!(park.add_miniball(20, 999, DVec3::X, 10.0).is_err());
    park.add_miniball(20, 1, DVec3::new(5.0, 0.0, 0.0), 10.0)
        .unwrap();
    assert_eq!(park.balls[&20].position, DVec3::new(105.0, 200.0, 300.0));

    assert!(park.formation_follow(1, 1, DVec3::ZERO, 100.0).is_err());
    assert!(park.formation_follow(1, 999, DVec3::ZERO, 100.0).is_err());
    assert!(park.formation_follow(999, 1, DVec3::ZERO, 100.0).is_err());
    park.formation_follow(2, 1, DVec3::new(50.0, 0.0, -50.0), 100.0)
        .unwrap();
    assert_eq!(park.balls[&2].mode, MovementMode::Formation);

    // Harmonic, Free, Interactive, Controller, Modules, Dbuffs, Breacher DoT, Sovereignty, Drones
    assert!(!park.set_ball_harmonic(999, 1));
    assert!(park.set_ball_harmonic(1, 3));
    assert_eq!(park.ball_harmonics[&1], 3);

    assert!(!park.set_ball_free(999, false));
    assert!(park.set_ball_free(1, false));
    assert_eq!(park.balls[&1].velocity, DVec3::ZERO);

    assert!(!park.set_ball_interactive(999, true));
    assert!(park.set_ball_interactive(1, true));

    assert!(!park.emit_client_controller_event(999, None, "ev", 1, None));
    assert!(park.emit_client_controller_event(1, Some(2), "ev", 1, Some(0.5)));

    assert!(!park.update_ship_module_states(999, vec![]));
    assert!(park.update_ship_module_states(1, vec![(10, 1, 2048, 27, 1.0, 1.0)]));

    assert!(!park.update_dbuffs(999, vec![]));
    assert!(park.update_dbuffs(1, vec![(100, 1.1, 60)]));

    assert!(!park.apply_breacher_dot(999, 50.0, 0.01, 10));
    assert!(park.apply_breacher_dot(1, 50.0, 0.01, 10));
    assert_eq!(
        park.combat_states[&1].active_breacher_dot,
        Some((50.0, 0.01, 10))
    );
    assert!(park.apply_breacher_dot(1, 50.0, 0.01, 0));
    assert_eq!(park.combat_states[&1].active_breacher_dot, None);

    park.set_sovereignty(99_000_001);
    park.update_drone_state(30, 9001, 1, 1, 2454, 9001, Some(2));
    park.emit_packaged_actions(Some(1), &[DestinyEvent::Stop { ball_id: 1 }]);

    // Docking removes ship ball while preserving its observer_events
    let docked = park
        .accept_docking_at(1, 60_003_760, DVec3::new(1000.0, 2000.0, 3000.0))
        .unwrap();
    assert_eq!(docked.id, 1);
    assert!(!park.balls.contains_key(&1));
    assert!(park.observer_events.contains_key(&1));
}
