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

#[test]
fn coverage_and_mutants_lib_damage_spillover_invuln_and_bubbles() {
    // 1. All 4 damage channels & distinct layer resonances across Shield -> Armor -> Structure
    let mut cs = CombatState {
        shield_hp: 50.0,
        max_shield_hp: 200.0,
        armor_hp: 60.0,
        max_armor_hp: 200.0,
        structure_hp: 100.0,
        max_structure_hp: 200.0,
        shield_resonance: [0.5, 0.25, 0.5, 1.0],
        armor_resonance: [0.5, 0.5, 0.5, 0.5],
        structure_resonance: [1.0, 1.0, 1.0, 1.0],
        ..Default::default()
    };
    // Raw damage across all 4 channels: (40, 80, 40, 20) -> eff on shield = 20 + 20 + 20 + 20 = 80
    // Shield absorbs 50/80 = 0.625, leaving 0.375 * (40, 80, 40, 20) = (15, 30, 15, 7.5), total = 67.5
    // Armor eff = 0.5 * 67.5 = 33.75 -> armor = 60 - 33.75 = 26.25, structure = 100
    let (applied, destroyed) = cs.apply_damage(DamageVector::new(40.0, 80.0, 40.0, 20.0));
    assert!(!destroyed);
    assert!((applied - 83.75).abs() < 1e-6);
    assert_eq!(cs.shield_hp, 0.0);
    assert!((cs.armor_hp - 26.25).abs() < 1e-6);
    assert_eq!(cs.structure_hp, 100.0);

    // 2. apply_true_damage through Shield -> Armor -> Structure and boundary survival
    let mut cs2 = CombatState {
        shield_hp: 30.0,
        max_shield_hp: 100.0,
        armor_hp: 40.0,
        max_armor_hp: 100.0,
        structure_hp: 50.0,
        max_structure_hp: 100.0,
        ..Default::default()
    };
    let (t1, d1) = cs2.apply_true_damage(80.0);
    assert!(!d1);
    assert!((t1 - 80.0).abs() < 1e-6);
    assert_eq!(cs2.shield_hp, 0.0);
    assert_eq!(cs2.armor_hp, 0.0);
    assert!((cs2.structure_hp - 40.0).abs() < 1e-6);

    // 3. Each invulnerability source tested individually
    let mut inv = CombatState::new();
    inv.gate_cloak_remaining_ticks = 1;
    assert!(inv.is_invulnerable());
    inv.gate_cloak_remaining_ticks = 0;
    inv.undock_invulnerable_remaining_ticks = 1;
    assert!(inv.is_invulnerable());
    inv.undock_invulnerable_remaining_ticks = 0;
    inv.warp_exit_invulnerable_remaining_ticks = 1;
    assert!(inv.is_warp_exit_invulnerable());
    assert!(inv.is_invulnerable());

    // 4. WarpBubble exact 500km catch and drag boundaries (t1 = 1_010_000, t0 = 990_000)
    let wb = WarpBubble::new(1, DVec3::new(1_000_000.0, 0.0, 0.0), 10_000.0);
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(1_510_000.0, 0.0, 0.0))
        .is_some());
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(1_510_001.0, 0.0, 0.0))
        .is_none());
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(490_000.0, 0.0, 0.0))
        .is_some());
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(489_999.0, 0.0, 0.0))
        .is_none());

    // 5. formation_follow preserves positive speed_fraction and sets 1.0 when <= 0.0
    let mut fpark = Ballpark::new(30_000_142);
    fpark.add_ball(Ball {
        id: 1,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::X * 100.0,
        max_velocity: 300.0,
        radius: 40.0,
        mass: 1_000_000.0,
        inertia: 3.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });
    fpark.add_ball(Ball {
        id: 2,
        type_id: 587,
        position: DVec3::new(-500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        radius: 40.0,
        mass: 1_000_000.0,
        inertia: 3.0,
        speed_fraction: 0.4,
        mode: MovementMode::Stop,
    });
    assert!(fpark
        .formation_follow(2, 1, DVec3::new(-200.0, 0.0, 0.0), 100.0)
        .is_ok());
    assert_eq!(fpark.balls[&2].speed_fraction, 0.4);
    fpark.balls.get_mut(&2).unwrap().speed_fraction = 0.0;
    assert!(fpark
        .formation_follow(2, 1, DVec3::new(-200.0, 0.0, 0.0), 100.0)
        .is_ok());
    assert_eq!(fpark.balls[&2].speed_fraction, 1.0);
}

#[test]
fn combat_state_recompute_attributes_baselines_clamps_and_resonance_precedence() {
    let mut ball = Ball {
        id: 77,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        radius: 40.0,
        mass: 1_000_000.0,
        inertia: 2.5,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let mut cs = CombatState::new();
    cs.max_shield_hp = 1_000.0;
    cs.shield_hp = 1_000.0;
    cs.max_armor_hp = 2_000.0;
    cs.armor_hp = 2_000.0;
    cs.max_structure_hp = 3_000.0;
    cs.structure_hp = 3_000.0;
    cs.signature_radius = 50.0;
    cs.capacitor_recharge_time_ms = 200_000.0;

    // 1. `capture_missing_baselines` guard: when `is_cloaked && ball.max_velocity == 0.0`
    //    or `module_immobilized`, `base_max_velocity` is NOT captured.
    cs.is_cloaked = true;
    cs.capture_missing_baselines(&ball);
    assert_eq!(cs.base_max_velocity, None);

    cs.is_cloaked = false;
    cs.module_immobilized = true;
    ball.max_velocity = 250.0;
    cs.capture_missing_baselines(&ball);
    assert_eq!(cs.base_max_velocity, None);

    // Once uncloaked and not immobilized, `base_max_velocity` is captured and never overwritten.
    cs.module_immobilized = false;
    cs.recompute_attributes(&mut ball);
    assert_eq!(cs.base_max_velocity, Some(250.0));
    ball.max_velocity = 999.0;
    cs.capture_missing_baselines(&ball);
    assert_eq!(cs.base_max_velocity, Some(250.0));

    // 2. Velocity multiplier chain across all 3 `self_v` branches + `max_velocity_cap`:
    //    `base_v (250) * ext (1.2) * cloud (0.8) * self_v * web (0.5)`
    cs.external_velocity_multiplier = Some(1.2);
    cs.cloud_velocity_multiplier = Some(0.8);
    cs.web_velocity_multiplier = 0.5;
    cs.prop_velocity_multiplier = 2.0;
    cs.hic_velocity_multiplier = 0.75;

    // Branch A: normal propulsion (`self_v = prop * hic = 1.5`) -> `250 * 1.2 * 0.8 * 1.5 * 0.5 = 180.0`
    cs.recompute_attributes(&mut ball);
    assert!((ball.max_velocity - 180.0).abs() < 1e-6);

    // Branch B: cloaked (`self_v = cloak_mul * hic = 0.4 * 0.75 = 0.3`, ignoring `prop_velocity_multiplier`)
    //           -> `250 * 1.2 * 0.8 * 0.3 * 0.5 = 36.0`
    cs.cloak_velocity_multiplier = Some(0.4);
    cs.recompute_attributes(&mut ball);
    assert!((ball.max_velocity - 36.0).abs() < 1e-6);

    // Branch C: `module_immobilized = true` (`self_v = 0.0`) -> `0.0`
    cs.module_immobilized = true;
    cs.recompute_attributes(&mut ball);
    assert_eq!(ball.max_velocity, 0.0);

    // `max_velocity_cap` clamps positive speeds and floors negative caps at `0.0`.
    cs.module_immobilized = false;
    cs.cloak_velocity_multiplier = None;
    cs.max_velocity_cap = Some(120.0);
    cs.recompute_attributes(&mut ball);
    assert!((ball.max_velocity - 120.0).abs() < 1e-6);
    cs.max_velocity_cap = Some(-10.0);
    cs.recompute_attributes(&mut ball);
    assert_eq!(ball.max_velocity, 0.0);
    cs.max_velocity_cap = None;

    // 3. Mass `(base_m * module_mass_multiplier + module_mass_addition).max(1.0)`
    //    and Inertia `(base_i * external_inertia_multiplier * cloud_inertia_multiplier).max(0.001)`
    cs.module_mass_multiplier = 1.5;
    cs.module_mass_addition = 250_000.0;
    cs.external_inertia_multiplier = Some(0.8);
    cs.cloud_inertia_multiplier = Some(1.25);
    cs.recompute_attributes(&mut ball);
    assert!((ball.mass - 1_750_000.0).abs() < 1e-6);
    assert!((ball.inertia - 2.5).abs() < 1e-6);

    cs.module_mass_multiplier = 0.0;
    cs.module_mass_addition = -500.0;
    cs.external_inertia_multiplier = Some(0.0);
    cs.recompute_attributes(&mut ball);
    assert_eq!(ball.mass, 1.0);
    assert_eq!(ball.inertia, 0.001);

    // 4. Signature radius product of all 6 multipliers + `.max(1.0)` floor.
    cs.module_sig_multiplier = 1.5;
    cs.mjfg_sig_multiplier = Some(2.0);
    cs.env_sig_multiplier = Some(1.2);
    cs.external_sig_multiplier = Some(1.1);
    cs.cloud_sig_multiplier = Some(1.25);
    cs.tp_sig_multiplier = Some(1.4);
    cs.recompute_combat_attributes();
    let expected_sig = 50.0 * 2.0 * 1.5 * 1.2 * 1.1 * 1.25 * 1.4;
    assert!((cs.signature_radius - expected_sig).abs() < 1e-6);
    cs.tp_sig_multiplier = Some(0.0);
    cs.recompute_combat_attributes();
    assert_eq!(cs.signature_radius, 1.0);
    cs.tp_sig_multiplier = Some(1.0);

    // 5. Capacitor recharge time: positive scaled with `.max(1_000.0)` floor; `<= 0.0` stays `0.0`.
    cs.cloud_cap_recharge_multiplier = Some(0.5);
    cs.recompute_combat_attributes();
    assert!((cs.capacitor_recharge_time_ms - 100_000.0).abs() < 1e-6);
    cs.cloud_cap_recharge_multiplier = Some(0.0001);
    cs.recompute_combat_attributes();
    assert_eq!(cs.capacitor_recharge_time_ms, 1_000.0);
    cs.base_capacitor_recharge_time_ms = Some(0.0);
    cs.recompute_combat_attributes();
    assert_eq!(cs.capacitor_recharge_time_ms, 0.0);

    // 6. Environmental max shield / armor / cap multipliers clamp current pools down to new max.
    cs.env_max_shield_multiplier = Some(0.5);
    cs.env_max_armor_multiplier = Some(0.25);
    cs.env_max_cap_multiplier = Some(0.4);
    cs.base_max_capacitor = Some(500.0);
    cs.capacitor = 500.0;
    cs.recompute_combat_attributes();
    assert!((cs.max_shield_hp - 500.0).abs() < 1e-6);
    assert!((cs.shield_hp - 500.0).abs() < 1e-6);
    assert!((cs.max_armor_hp - 500.0).abs() < 1e-6);
    assert!((cs.armor_hp - 500.0).abs() < 1e-6);
    assert!((cs.max_capacitor - 200.0).abs() < 1e-6);
    assert!((cs.capacitor - 200.0).abs() < 1e-6);

    // 7. Resonance precedence chain:
    //    `base_*_resonance` -> `rah_armor_resonance` -> `crab_resonance_multiplier` (clamped `[0.01, 1.0]`)
    //    -> `ehe_hull_resists` (overrides CRAB on structure) -> `adc_active` (`[0.25; 4]` overrides all).
    cs.base_shield_resonance = Some([0.8, 0.6, 0.4, 0.2]);
    cs.base_armor_resonance = Some([0.5, 0.5, 0.5, 0.5]);
    cs.base_structure_resonance = Some([0.9, 0.9, 0.9, 0.9]);
    cs.rah_armor_resonance = Some([0.4, 0.3, 0.2, 0.1]);
    cs.crab_resonance_multiplier = Some(2.0); // tests `.clamp(0.01, 1.0)` upper clamp on `0.8 * 2.0 -> 1.0`
    cs.recompute_combat_attributes();
    assert!((cs.shield_resonance[0] - 1.0).abs() < 1e-6);
    assert!((cs.shield_resonance[3] - 0.4).abs() < 1e-6);
    assert!((cs.armor_resonance[0] - 0.8).abs() < 1e-6);
    assert!((cs.armor_resonance[3] - 0.2).abs() < 1e-6);
    assert!((cs.structure_resonance[0] - 1.0).abs() < 1e-6);

    // Lower clamp `[0.01, 1.0]` on CRAB multiplier + EHE override on hull only:
    cs.crab_resonance_multiplier = Some(0.001);
    cs.ehe_hull_resists = Some([0.05; 4]);
    cs.recompute_combat_attributes();
    assert_eq!(cs.shield_resonance, [0.01; 4]);
    assert_eq!(cs.armor_resonance, [0.01; 4]);
    assert_eq!(cs.structure_resonance, [0.05; 4]);

    // ADC overrides RAH, CRAB, and EHE across all 3 layers to `[0.25; 4]`.
    cs.adc_active = true;
    cs.recompute_combat_attributes();
    assert_eq!(cs.shield_resonance, [0.25; 4]);
    assert_eq!(cs.armor_resonance, [0.25; 4]);
    assert_eq!(cs.structure_resonance, [0.25; 4]);
}

#[test]
fn mutant_killers_capture_missing_baselines_and_recompute_combat_boundaries() {
    use crate::{Ball, MovementMode};
    use glam::DVec3;

    let make_ball = |max_v: f64, mass: f64, inertia: f64| Ball {
        id: 500,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: max_v,
        inertia,
        mass,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };

    // 1. capture_missing_baselines: `!self.is_cloaked || ball.max_velocity > 0.0`
    //    Cloaked with max_velocity == 0.0 -> base_max_velocity stays None.
    let mut cs_clk0 = CombatState::default();
    cs_clk0.is_cloaked = true;
    cs_clk0.signature_radius = 0.0;
    cs_clk0.capture_missing_baselines(&make_ball(0.0, 0.0, 0.0));
    assert_eq!(cs_clk0.base_max_velocity, None);
    assert_eq!(cs_clk0.base_mass, None);
    assert_eq!(cs_clk0.base_inertia, None);
    assert_eq!(cs_clk0.base_signature_radius, None);

    //    Cloaked with max_velocity > 0.0 -> MUST still capture base_max_velocity (`||` vs `&&`, `>` vs `<`).
    let mut cs_clkv = CombatState::default();
    cs_clkv.is_cloaked = true;
    cs_clkv.signature_radius = 45.0;
    cs_clkv.capture_missing_baselines(&make_ball(120.0, 1_500.0, 2.5));
    assert_eq!(cs_clkv.base_max_velocity, Some(120.0));
    assert_eq!(cs_clkv.base_mass, Some(1_500.0));
    assert_eq!(cs_clkv.base_inertia, Some(2.5));
    assert_eq!(cs_clkv.base_signature_radius, Some(45.0));

    //    Pre-set baselines must NOT be overwritten when ball/self have different positive values (`&&` vs `||`).
    let mut cs_pre = CombatState::default();
    cs_pre.base_mass = Some(500.0);
    cs_pre.base_inertia = Some(1.5);
    cs_pre.base_signature_radius = Some(25.0);
    cs_pre.signature_radius = 999.0;
    cs_pre.capture_missing_baselines(&make_ball(200.0, 9_999.0, 9.0));
    assert_eq!(cs_pre.base_mass, Some(500.0));
    assert_eq!(cs_pre.base_inertia, Some(1.5));
    assert_eq!(cs_pre.base_signature_radius, Some(25.0));

    // 2. recompute_combat_attributes: base_signature_radius stays None if signature_radius == 0.0 (`>` vs `>=`),
    //    and crab_resonance_multiplier multiplies hull resists (`*` vs `+` when < 1.0 and hull < 1.0).
    let mut cs_rc = CombatState::default();
    cs_rc.signature_radius = 0.0;
    cs_rc.base_structure_resonance = Some([0.5; 4]);
    cs_rc.crab_resonance_multiplier = Some(0.90);
    cs_rc.recompute_combat_attributes();
    assert_eq!(cs_rc.base_signature_radius, None);
    for r in cs_rc.structure_resonance {
        assert!((r - 0.45).abs() < 1e-9, "0.5 * 0.90 = 0.45, got {r}");
    }
}

