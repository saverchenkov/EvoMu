//! Unit tests for `physics::combat`, swept-sphere collisions, ECM, target lock decay, tether, MJFG spool, and orbit edge cases.

use super::physics::combat;
use super::*;

fn ship_with_vel(id: BallId, type_id: u32, pos: DVec3, vel: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: vel,
        max_velocity: 2_000.0,
        inertia: 1.5,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: if vel.length_squared() > 0.0 {
            MovementMode::GotoDirection {
                direction: vel.normalize(),
            }
        } else {
            MovementMode::Stop
        },
    }
}

#[test]
fn combat_formulas_lock_angular_turret_missile_spool_and_concord() {
    // Target lock time formula & 0.25s clamp.
    let lock_s = combat::compute_lock_time_seconds(250.0, 40.0);
    assert!((lock_s - 40_000.0 / (250.0 * 40.0_f64.asinh().powi(2))).abs() < 1e-9);
    assert_eq!(combat::compute_lock_time_seconds(10_000_000.0, 500.0), 0.25);

    // Angular velocity: zero distance returns 0; perpendicular relative velocity = v / d.
    assert_eq!(
        combat::compute_angular_velocity(DVec3::ZERO, DVec3::ZERO, DVec3::new(0.5, 0.0, 0.0), DVec3::Y),
        0.0
    );
    let omega = combat::compute_angular_velocity(
        DVec3::ZERO,
        DVec3::ZERO,
        DVec3::new(10_000.0, 0.0, 0.0),
        DVec3::new(0.0, 500.0, 0.0),
    );
    assert!((omega - 0.05).abs() < 1e-12);

    // Passive recharge edge guards & curve.
    assert_eq!(combat::compute_passive_recharge(300.0, 250.0, 10_000.0, 1000.0), 250.0);
    assert_eq!(combat::compute_passive_recharge(100.0, 0.0, 10_000.0, 1000.0), 0.0);
    let recharged = combat::compute_passive_recharge(0.0, 250.0, 120_000.0, 120_000.0);
    let expected_term = 1.0 - (-5.0_f64).exp();
    assert!((recharged - 250.0 * expected_term * expected_term).abs() < 1e-9);

    // Turret chance to hit & shot resolution (miss, wrecking <0.01, normal).
    assert!((combat::compute_turret_chance_to_hit(0.0, 0.05, 40.0, 10_000.0, 15_000.0, 5_000.0) - 1.0).abs() < 1e-12);
    assert!((combat::compute_turret_chance_to_hit(0.0, 0.05, 40.0, 20_000.0, 15_000.0, 5_000.0) - 0.5).abs() < 1e-12);
    let zero_falloff_hit = combat::compute_turret_chance_to_hit(0.0, 0.05, 40.0, 16_000.0, 15_000.0, 0.0);
    assert_eq!(zero_falloff_hit, 0.5_f64.powf(100.0));

    let base = DamageVector::new(100.0, 100.0, 100.0, 100.0);
    let (miss_d, miss_w) = combat::resolve_turret_shot(base, 0.4, 0.4);
    assert_eq!(miss_d.total(), 0.0);
    assert!(!miss_w);

    let (wreck_d, wreck_w) = combat::resolve_turret_shot(base, 0.8, 0.005);
    assert_eq!(wreck_d, base.scaled(3.0));
    assert!(wreck_w);

    let (norm_d, norm_w) = combat::resolve_turret_shot(base, 0.8, 0.51);
    assert!((norm_d.em - 100.0).abs() < 1e-9);
    assert!(!norm_w);

    // Missile damage application (both precomputed <= 1.0 and raw > 1.0 DRF).
    let f_pre = combat::compute_missile_damage_factor(35.0, 400.0, 140.0, 150.0, 0.604);
    assert!(f_pre > 0.0 && f_pre < 0.25);
    let f_raw = combat::compute_missile_damage_factor(400.0, 50.0, 100.0, 150.0, 2.8);
    assert!((f_raw - 1.0).abs() < 1e-12);

    // Entropic disintegrator spool multiplier.
    assert!((combat::compute_entropic_spool_multiplier(3, 5, 0.15) - 1.45).abs() < 1e-12);
    assert!((combat::compute_entropic_spool_multiplier(10, 5, 0.15) - 1.75).abs() < 1e-12);

    // CONCORD high-sec response table across all bands.
    assert_eq!(combat::concord_response_seconds(1.0), 6.0);
    assert_eq!(combat::concord_response_seconds(0.9), 6.0);
    assert_eq!(combat::concord_response_seconds(0.8), 7.0);
    assert_eq!(combat::concord_response_seconds(0.7), 10.0);
    assert_eq!(combat::concord_response_seconds(0.6), 14.0);
    assert_eq!(combat::concord_response_seconds(0.5), 19.0);
    assert!(combat::concord_response_seconds(0.4).is_infinite());
}

#[test]
fn swept_sphere_collisions_and_static_bump_both_id_orderings() {
    let mut park = Ballpark::new(30_000_142);

    // Two fast dynamic ships crossing through each other in a single 1s tick (swept CCD).
    let s1 = ship_with_vel(10, 587, DVec3::new(-400.0, 0.0, 0.0), DVec3::new(1500.0, 0.0, 0.0));
    let s2 = ship_with_vel(20, 587, DVec3::new(400.0, 0.0, 0.0), DVec3::new(-1500.0, 0.0, 0.0));
    assert!(park.add_ball(s1));
    assert!(park.add_ball(s2));

    let evs = park.step(1.0);
    assert!(evs.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a: 10, ball_b: 20 })));
    assert!(evs.iter().any(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id: 10, .. })));
    assert!(evs.iter().any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id: 20, .. })));

    // Static collision with static id < dynamic id AND static id > dynamic id.
    let mut park2 = Ballpark::new(30_000_142);
    let mut st_low = ship_with_vel(5, 57, DVec3::ZERO, DVec3::ZERO);
    st_low.mode = MovementMode::Rigid;
    st_low.radius = 500.0;
    assert!(park2.add_static_ball(st_low, 15));

    // Ship 15 overlapping st_low within 10km (stays on +X side of st_low after step) bounces outward.
    let mut dyn_mid = ship_with_vel(15, 587, DVec3::new(520.0, 0.0, 0.0), DVec3::new(-50.0, 0.0, 0.0));
    dyn_mid.max_velocity = 50.0;
    assert!(park2.add_ball(dyn_mid));

    // Ship 12 deep inside st_huge (>10km overlap, located far from ship 15) is skipped without panicking/teleports.
    let dyn_deep = ship_with_vel(12, 587, DVec3::new(-500_000.0, 0.0, 0.0), DVec3::ZERO);
    assert!(park2.add_ball(dyn_deep));
    let mut st_huge = ship_with_vel(25, 57, DVec3::new(-500_000.0, 0.0, 0.0), DVec3::ZERO);
    st_huge.mode = MovementMode::Rigid;
    st_huge.radius = 20_000.0;
    assert!(park2.add_static_ball(st_huge, 15));

    let evs2 = park2.step(1.0);
    assert!(evs2.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a: 5, ball_b: 15 })));
    assert!(park2.balls[&15].position.x >= 500.0);
}

#[test]
fn target_locking_ecm_burst_tether_mjfg_spool_and_orbit_poles() {
    let mut park = Ballpark::new(30_000_142);

    // 1: Source ship; 2: Target ship; 3: Cloaked ship; 4: Out-of-range ship; 5: Command Destroyer; 6: Tethered ship.
    let mut s1 = ship_with_vel(1, 587, DVec3::ZERO, DVec3::ZERO);
    s1.mode = MovementMode::Orbit {
        target_id: 2,
        range: 5_000.0,
    };
    let s2 = ship_with_vel(2, 587, DVec3::new(0.0, 0.0, 10_000.0), DVec3::ZERO); // aligned on Z axis to hit Orbit pole_ref
    let s3 = ship_with_vel(3, 587, DVec3::new(15_000.0, 0.0, 0.0), DVec3::ZERO);
    let s4 = ship_with_vel(4, 587, DVec3::new(200_000.0, 0.0, 0.0), DVec3::ZERO);
    let s5 = ship_with_vel(5, 37481, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO);
    let s6 = ship_with_vel(6, 587, DVec3::new(-500.0, 0.0, 0.0), DVec3::ZERO);

    assert!(park.add_ball(s1));
    assert!(park.add_ball(s2));
    assert!(park.add_ball(s3));
    assert!(park.add_ball(s4));
    assert!(park.add_ball(s5));
    assert!(park.add_ball(s6));

    // Cloaked target lock is rejected; gate-cloaked source locking breaks gate cloak.
    assert!(park.activate_cloak(3, false, 0.0, 15.0).is_ok());
    assert!(!park.add_target_lock(
        1,
        TargetLock {
            target_id: 3,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 1000.0,
            locked: false,
            max_range_m: None,
        }
    ));
    park.combat_states.get_mut(&1).unwrap().gate_cloak_remaining_ticks = 10;
    park.combat_states.get_mut(&1).unwrap().is_cloaked = true;
    assert!(park.add_target_lock(
        1,
        TargetLock {
            target_id: 2,
            lock_time_ms: 1500.0,
            remaining_lock_ms: 1500.0,
            locked: false,
            max_range_m: Some(50_000.0),
        }
    ));
    assert!(park.add_target_lock(
        1,
        TargetLock {
            target_id: 4,
            lock_time_ms: 500.0,
            remaining_lock_ms: 500.0,
            locked: false,
            max_range_m: Some(100_000.0),
        }
    ));
    assert!(!park.combat_states[&1].is_cloaked);

    // Observer visibility queries (with cloaked ship 3 hidden from 1, and out-of-grid observer).
    let vis = park.visible_balls_for_observer(1);
    assert!(vis.iter().all(|b| b.id != 3));
    assert!(!park.dynamic_balls_in_bubble(1).is_empty());
    assert!(park.dynamic_balls_in_bubble(999_999).is_empty());
    assert!(!park.balls_in_bubble(1, 50_000.0).is_empty());
    assert!(park.balls_in_bubble(999_999, 50_000.0).is_empty());

    // Tethered ship 6 repairs 10% HP per tick when unscrambled/unlocked.
    if let Some(cs6) = park.combat_states.get_mut(&6) {
        cs6.is_tethered = true;
        cs6.shield_hp = 200.0;
        cs6.armor_hp = 200.0;
        cs6.structure_hp = 200.0;
    }

    // Command Destroyer 5 initializes last_step_capacitor on tick 1, then triggers 9s MJFG spool on cap drop >= 70.
    let ev_tick1 = park.step(1.0);
    // Lock on out-of-range target 4 dropped, lock on 2 progressed (1500 -> 500ms).
    assert_eq!(park.get_target_locks(1).len(), 1);
    assert_eq!(park.get_target_locks(1)[0].remaining_lock_ms, 500.0);
    assert!(ev_tick1.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, target_id: 4, .. } if mode == "lost")));
    let expected_s6_shield =
        combat::compute_passive_recharge(245.0, 450.0, 120_000.0, 1_000.0);
    assert!((park.combat_states[&6].shield_hp - expected_s6_shield).abs() < 1e-6);

    // Scrambling tethered ship 6 breaks tether on next step.
    park.combat_states.get_mut(&6).unwrap().warp_scramble_strength = 1;
    // Drop cap on Command Destroyer 5 to start 9s spool and inflate sig radius 2.5x.
    let orig_sig = park.combat_states[&5].signature_radius;
    park.combat_states.get_mut(&5).unwrap().capacitor = 100.0;

    let ev_tick2 = park.step(1.0);
    assert!(!park.combat_states[&6].is_tethered);
    assert!(park.get_target_locks(1)[0].locked);
    assert!(ev_tick2.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, target_id: 2, .. } if mode == "locked")));
    assert!((park.combat_states[&5].signature_radius - orig_sig * 2.5).abs() < 1e-6);

    // Scrambling Command Destroyer 5 cancels MJFG spool and restores signature radius.
    park.combat_states.get_mut(&5).unwrap().is_mwd_mjd_disabled = true;
    park.step(1.0);
    assert!((park.combat_states[&5].signature_radius - orig_sig).abs() < 1e-6);
    assert_eq!(park.combat_states[&5].mjfg_spool_remaining_s, 0.0);

    // Re-trigger MJFG spool and let all 9 ticks complete!
    park.combat_states.get_mut(&5).unwrap().is_mwd_mjd_disabled = false;
    park.combat_states.get_mut(&5).unwrap().last_step_capacitor = Some(250.0);
    park.combat_states.get_mut(&5).unwrap().capacitor = 100.0;
    let pos5_before = park.balls[&5].position;
    for _ in 0..9 {
        park.step(1.0);
    }
    assert!((park.balls[&5].position - pos5_before).length() > 50_000.0);

    // Test ECM burst, manual lock removal, miniball cleanup when parent removed, and warp-in-bubble abort.
    let jammed = park.apply_ecm_burst(1, 50_000.0, |tid| tid == 2);
    assert_eq!(jammed, vec![2]);
    assert!(park.remove_target_lock(1, 2));
    assert!(!park.remove_target_lock(1, 2));

    assert!(park.add_miniball(801, 2, DVec3::new(0.0, 10.0, 0.0), 5.0).is_ok());
    park.step(1.0);
    let _ = park.remove_ball(2);
    park.step(1.0);
    assert!(!park.balls.contains_key(&801));

    // Aligning ship inside warp bubble aborts warp on step, and Breacher DoT destroys low-HP ship.
    park.add_warp_bubble(9100, park.balls[&1].position, 20_000.0);
    let orig_pos1 = park.balls[&1].position;
    park.balls.get_mut(&1).unwrap().mode =
        MovementMode::new_warp(orig_pos1, DVec3::new(10_000_000.0, 0.0, 0.0), 3.0);
    assert!(park.apply_breacher_dot(6, 5_000.0, 1.0, 1));
    let ev_end = park.step(1.0);
    assert!(matches!(park.balls[&1].mode, MovementMode::Stop));
    assert!(ev_end.iter().any(|e| matches!(e, DestinyEvent::TerminalExplosion { ball_id: 6, .. })));
    assert!(!park.balls.contains_key(&6));
}

#[test]
fn step_edge_branches_targeting_and_collision_reset() {
    let mut park = Ballpark::new(30_000_142);
    assert!(park.step(0.0).is_empty());
    assert!(park.step(-2.0).is_empty());
    assert!(park.step(f64::NAN).is_empty());
    assert!(!park.set_interdiction_nullified(999_999, true));

    // Dynamic initial overlap (c_coef <= 0 && v_rel < 0), plus bumping an aligning ship (resets origin & elapsed time).
    let a = ship_with_vel(10, 587, DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0));
    let mut b_align = ship_with_vel(20, 587, DVec3::new(60.0, 0.0, 0.0), DVec3::new(-10.0, 0.0, 0.0));
    b_align.mode = MovementMode::new_warp(
        DVec3::new(-1_000.0, 0.0, 0.0),
        DVec3::new(20_000_000.0, 0.0, 0.0),
        3.0,
    );
    assert!(park.add_ball(a));
    assert!(park.add_ball(b_align));
    let ev_col = park.step(1.0);
    assert!(ev_col
        .iter()
        .any(|e| matches!(e, DestinyEvent::Collision { ball_a: 10, ball_b: 20 })));
    if let MovementMode::Warp {
        phase,
        elapsed_warp_time,
        ..
    } = park.balls[&20].mode
    {
        assert_eq!(phase, WarpPhase::Aligning);
        assert_eq!(elapsed_warp_time, 0.0);
    } else {
        panic!("Expected Aligning warp mode after bump");
    }

    // Target lock rejections (decloak delay, warp-exit invuln), clear_target_locks, ECM on invuln & missing burst source, observer queries.
    let mut s3 = ship_with_vel(30, 587, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO);
    s3.mode = MovementMode::GotoDirection { direction: DVec3::X };
    assert!(park.add_ball(s3));
    park.combat_states.get_mut(&30).unwrap().decloak_targeting_delay_remaining_s = 3.0;
    assert!(!park.add_target_lock(
        30,
        TargetLock {
            target_id: 10,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 1000.0,
            locked: false,
            max_range_m: None,
        }
    ));
    park.combat_states.get_mut(&30).unwrap().decloak_targeting_delay_remaining_s = 0.0;
    park.combat_states.get_mut(&10).unwrap().warp_exit_invulnerable_remaining_ticks = 5;
    assert!(!park.add_target_lock(
        30,
        TargetLock {
            target_id: 10,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 1000.0,
            locked: false,
            max_range_m: None,
        }
    ));
    // Active movement on s3 clears warp_exit_invulnerable_remaining_ticks on step & decays decloak_targeting_delay & lance_disrupted.
    park.combat_states.get_mut(&30).unwrap().warp_exit_invulnerable_remaining_ticks = 5;
    park.combat_states.get_mut(&30).unwrap().decloak_targeting_delay_remaining_s = 2.0;
    park.combat_states.get_mut(&30).unwrap().lance_disrupted_remaining_s = 4.0;
    park.step(1.0);
    assert_eq!(park.combat_states[&30].warp_exit_invulnerable_remaining_ticks, 0);
    assert_eq!(park.combat_states[&30].decloak_targeting_delay_remaining_s, 1.0);
    assert_eq!(park.combat_states[&30].lance_disrupted_remaining_s, 3.0);

    // clear_target_locks on active locks vs empty; apply_ecm_jam on invuln target; apply_ecm_burst on missing source & failing predicate.
    park.combat_states.get_mut(&10).unwrap().warp_exit_invulnerable_remaining_ticks = 0;
    park.combat_states.get_mut(&30).unwrap().decloak_targeting_delay_remaining_s = 0.0;
    assert!(park.add_target_lock(
        30,
        TargetLock {
            target_id: 10,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 500.0,
            locked: false,
            max_range_m: None,
        }
    ));
    park.combat_states.get_mut(&10).unwrap().undock_invulnerable_remaining_ticks = 10;
    park.apply_ecm_jam(10, 15.0);
    assert_eq!(park.combat_states[&10].ecm_jammed_remaining_s, 0.0);
    assert_eq!(park.clear_target_locks(30), 1);
    assert_eq!(park.clear_target_locks(30), 0);
    assert!(park.apply_ecm_burst(999_999, 50_000.0, |_| true).is_empty());
    assert!(park.apply_ecm_burst(30, 50_000.0, |_| false).is_empty());

    // Static observer queries: visible_balls_for_observer(unknown_id) and balls_in_bubble(static_id).
    let mut st = ship_with_vel(9500, 15, DVec3::ZERO, DVec3::ZERO);
    st.mode = MovementMode::Rigid;
    assert!(park.add_static_ball(st, 15));
    assert_eq!(park.visible_balls_for_observer(999_999).len(), 1);
    assert!(park.balls_in_bubble(9500, 100_000.0).len() >= 2);
}

#[test]
fn orbit_negative_z_pole_follow_zero_and_post_align_bubble_intercept() {
    let mut park = Ballpark::new(30_000_142);

    // Follow at exact same position, zero max_velocity, and missing target.
    let f1 = ship_with_vel(1, 587, DVec3::ZERO, DVec3::ZERO);
    let mut f2 = ship_with_vel(2, 587, DVec3::ZERO, DVec3::ZERO);
    f2.mode = MovementMode::Follow {
        target_id: 1,
        range: 0.0,
    };
    let mut f3 = ship_with_vel(3, 587, DVec3::new(100.0, 0.0, 0.0), DVec3::ZERO);
    f3.mode = MovementMode::Follow {
        target_id: 999_999,
        range: 100.0,
    };
    // Orbit on negative Z axis (u.z < 0.0 pole_ref branch) and Orbit with missing target.
    let mut o_neg_z = ship_with_vel(4, 587, DVec3::new(0.0, 0.0, 5_000.0), DVec3::ZERO);
    o_neg_z.mode = MovementMode::Orbit {
        target_id: 1,
        range: 1_000.0,
    };
    let mut o_missing = ship_with_vel(5, 587, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO);
    o_missing.mode = MovementMode::Orbit {
        target_id: 999_999,
        range: 1_000.0,
    };
    // Formation with moving leader & Stop leader & zero-velocity follower.
    let mut form_move = ship_with_vel(6, 587, DVec3::ZERO, DVec3::ZERO);
    form_move.speed_fraction = 0.0;
    let form_stop = ship_with_vel(7, 587, DVec3::new(5_000.0, 0.0, 0.0), DVec3::ZERO);
    let mut form_leader_stop = ship_with_vel(8, 587, DVec3::new(5_000.0, 100.0, 0.0), DVec3::ZERO);
    form_leader_stop.mode = MovementMode::Stop;
    // Zero-distance warp exits immediately; post-alignment warp crossing 75% velocity gets intercepted by bubble.
    let mut w_zero = ship_with_vel(9, 587, DVec3::new(10_000.0, 0.0, 0.0), DVec3::ZERO);
    w_zero.mode = MovementMode::new_warp(
        DVec3::new(10_000.0, 0.0, 0.0),
        DVec3::new(10_000.0, 0.0, 0.0),
        3.0,
    );
    let mut w_align = ship_with_vel(10, 587, DVec3::ZERO, DVec3::new(1_490.0, 0.0, 0.0));
    w_align.inertia = 0.2;
    w_align.mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(20_000_000.0, 0.0, 0.0), 3.0);
    park.add_warp_bubble(9001, DVec3::new(20_000_000.0, 0.0, 0.0), 25_000.0);

    // Gate-cloaked ship with original_max_velocity expiring & proximity decloak setting targeting delay + neighbor grid crossing.
    let neighbor_new = ship_with_vel(12, 587, DVec3::new(15_000_000.0, 0.0, 0.0), DVec3::ZERO);

    assert!(park.add_ball(f1));
    assert!(park.add_ball(f2));
    assert!(park.add_ball(f3));
    assert!(park.add_ball(o_neg_z));
    assert!(park.add_ball(o_missing));
    assert!(park.add_ball(form_move));
    assert!(park.add_ball(form_stop));
    assert!(park.add_ball(form_leader_stop));
    assert!(park.add_ball(w_zero));
    assert!(park.add_ball(w_align));
    assert!(park.add_ball(neighbor_new));

    assert!(park.formation_follow(6, 10, DVec3::new(0.0, 50.0, 0.0), 100.0).is_ok());
    assert!(park.formation_follow(7, 8, DVec3::new(0.0, 50.0, 0.0), 100.0).is_ok());

    park.combat_states.get_mut(&1).unwrap().gate_cloak_remaining_ticks = 1;
    park.combat_states.get_mut(&1).unwrap().is_cloaked = true;
    park.combat_states.get_mut(&1).unwrap().base_max_velocity = Some(399.0);
    park.combat_states.get_mut(&1).unwrap().cloak_velocity_multiplier = Some(0.0);

    assert!(park.set_ball_position(7, DVec3::new(-5_000.0, 0.0, 0.0)));
    assert!(park.set_ball_position(8, DVec3::new(5_000.0, 0.0, 0.0)));
    assert!(park.activate_cloak(7, false, 6.0, 10.0).is_ok());

    park.step(1.0);
    assert_eq!(park.balls[&1].max_velocity, 399.0);
    assert!(park.combat_states[&7].is_cloaked);
    assert!(matches!(park.balls[&9].mode, MovementMode::Stop));

    park.step(1.0);
    if let MovementMode::Warp { destination, phase, .. } = park.balls[&10].mode {
        assert_ne!(phase, WarpPhase::Aligning);
        assert!(destination.x < 20_000_000.0);
    } else {
        panic!("Expected post-alignment intercepted warp mode");
    }

    // Step enough ticks for ship 10 to warp into neighbor_new's grid and emit AddBalls/AddBalls2/RemoveBalls.
    for _ in 0..30 {
        park.step(1.0);
    }
    assert_eq!(park.bubble_id_for_ball(10), park.bubble_id_for_ball(12));
}

