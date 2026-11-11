//! Precision kinematics, warp trajectory, collision deflection, and navigation boundary tests
//! targeting mutation survival across `physics.rs`, `ballpark_targeting.rs`, and `ballpark_navigation.rs`.

#![cfg(test)]

use crate::physics::{compute_subwarp_goal_velocity, step_warp_ball};
use crate::wire::{encode_add_balls, encode_destiny_event, resolve_slim_item_meta, slim_item_dict};
use crate::{
    Ball, BallId, Ballpark, CombatState, DestinyEvent, MiniballState, MovementMode, TargetLock,
    WarpPhase, AU_METERS,
};
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::BTreeMap;

fn ball_with(id: BallId, pos: DVec3, vel: DVec3, max_v: f64, inertia: f64, mass: f64, radius: f64) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: vel,
        max_velocity: max_v,
        inertia,
        mass,
        radius,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

fn assert_vec_close(actual: DVec3, expected: DVec3, tol: f64) {
    assert!(
        (actual - expected).length() <= tol,
        "expected {expected:?} within {tol}, got {actual:?} (diff={})",
        (actual - expected).length()
    );
}

#[test]
fn test_compute_subwarp_goal_velocity_follow_and_orbit_exact_math() {
    let mut positions = BTreeMap::new();
    positions.insert(2, (DVec3::new(100.0, 200.0, 300.0), DVec3::new(10.0, -5.0, 2.0), 50.0));

    // 1. Follow: zero distance uses +X offset (range=300 => r_total=380 > step_dist=200 => full speed)
    let mut f_ball = ball_with(1, DVec3::new(100.0, 200.0, 300.0), DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    f_ball.mode = MovementMode::Follow { target_id: 2, range: 300.0 };
    assert_vec_close(compute_subwarp_goal_velocity(&f_ball, &positions, 1.0), DVec3::new(200.0, 0.0, 0.0), 1e-9);
    f_ball.mode = MovementMode::Follow { target_id: 2, range: 100.0 };
    assert_vec_close(compute_subwarp_goal_velocity(&f_ball, &positions, 1.0), DVec3::new(131.22, 0.0, 0.0), 1e-9);

    // Follow: quadratic damping inside one step (err = 50, max_spd = 100 => ratio = 0.25 => speed = 6.25)
    f_ball.max_velocity = 100.0;
    f_ball.position = DVec3::new(230.0, 200.0, 300.0); // stop_point = 100 + (100+30+50) = 280 -> err = +50 X
    assert_vec_close(compute_subwarp_goal_velocity(&f_ball, &positions, 1.0), DVec3::new(6.25, 0.0, 0.0), 1e-9);

    // 2. Orbit: general spiral with moving target
    let mut o_ball = ball_with(1, DVec3::new(5000.0, 0.0, 0.0), DVec3::new(0.0, 200.0, 0.0), 300.0, 2.0, 1_000_000.0, 50.0);
    o_ball.mode = MovementMode::Orbit { target_id: 2, range: 1000.0 };
    let mut pos_origin = BTreeMap::new();
    pos_origin.insert(2, (DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0), 50.0));
    let g_spiral = compute_subwarp_goal_velocity(&o_ball, &pos_origin, 1.0);
    // Exact check for spiral goal velocity: kills +, -, *, / mutations inside orbit math
    let tau = o_ball.time_constant();
    let v_max: f64 = 300.0;
    let d12 = ((1100.0 - 5000.0) * 0.1 / v_max).clamp(-1.0, 1.0);
    let d11 = ((v_max / tau) * 5000.0).sqrt().min(v_max);
    let arc = d11;
    let x = (arc * d12 / 5000.0) / (d12 * d12 + 1.0).sqrt();
    let phi = (1.0 + x).ln() / d12;
    let r_new = 5000.0 * (d12 * phi).exp();
    let (sp, cp) = phi.sin_cos();
    let p_orbit = DVec3::new(5000.0, 0.0, 0.0) + DVec3::NEG_X * (5000.0 - r_new * cp) + DVec3::Y * (r_new * sp);
    let v_dir = (DVec3::NEG_X * (sp - d12 * cp) + DVec3::Y * (cp + d12 * sp)).normalize();
    let v_spiral = v_dir * d11 + DVec3::new(10.0, 0.0, 0.0);
    let ek = (-1.0 / tau).exp();
    let omk = 1.0 - ek;
    let v_gp = (p_orbit - o_ball.position - o_ball.velocity * (tau * omk)) / (1.0 - tau * omk);
    let v_gv = (v_spiral - o_ball.velocity * ek) / omk;
    let raw = (v_gp + v_gv) * 0.5;
    let expected = if raw.length() > v_max { raw * (v_max / raw.length()) } else { raw };
    assert_vec_close(g_spiral, expected, 1e-6);

    // 3. Orbit: exact target radius (d12 == 0) uses phi = arc / d15
    o_ball.position = DVec3::new(1100.0, 0.0, 0.0);
    let g_zero_d12 = compute_subwarp_goal_velocity(&o_ball, &pos_origin, 1.0);
    let phi0 = d11 / 1100.0;
    let (sp0, cp0) = phi0.sin_cos();
    let p0 = DVec3::new(1100.0, 0.0, 0.0) + DVec3::NEG_X * (1100.0 - 1100.0 * cp0) + DVec3::Y * (1100.0 * sp0);
    let vd0 = (DVec3::NEG_X * sp0 + DVec3::Y * cp0).normalize();
    let vs0 = vd0 * d11 + DVec3::new(10.0, 0.0, 0.0);
    let raw0 = (((p0 - o_ball.position - o_ball.velocity * (tau * omk)) / (1.0 - tau * omk))
        + ((vs0 - o_ball.velocity * ek) / omk))
        * 0.5;
    let exp0 = if raw0.length() > v_max { raw0 * (v_max / raw0.length()) } else { raw0 };
    assert_vec_close(g_zero_d12, exp0, 1e-6);

    // 4. Unclamped orbit (d11 < v_max and raw_len < v_max) so a_max and *0.5 cannot cancel out!
    let mut u_ball = ball_with(5, DVec3::new(50.0, 0.0, 0.0), DVec3::new(0.0, 20.0, 0.0), 1000.0, 10.0, 1_000_000.0, 1.0);
    u_ball.mode = MovementMode::Orbit { target_id: 2, range: 50.0 };
    let mut pos_u = BTreeMap::new();
    pos_u.insert(2, (DVec3::ZERO, DVec3::ZERO, 4.0));
    let g_unclamped = compute_subwarp_goal_velocity(&u_ball, &pos_u, 1.0);
    let tau_u = u_ball.time_constant();
    let d12_u = ((55.0 - 50.0) * 0.1 / 1000.0_f64).clamp(-1.0, 1.0);
    let d11_u = ((1000.0 / tau_u) * 50.0).sqrt().min(1000.0);
    let x_u = (d11_u * d12_u / 50.0) / (d12_u * d12_u + 1.0).sqrt();
    let phi_u = (1.0 + x_u).ln() / d12_u;
    let r_u = 50.0 * (d12_u * phi_u).exp();
    let (sp_u, cp_u) = phi_u.sin_cos();
    let po_u = DVec3::new(50.0, 0.0, 0.0) + DVec3::NEG_X * (50.0 - r_u * cp_u) + DVec3::Y * (r_u * sp_u);
    let vs_u = (DVec3::NEG_X * (sp_u - d12_u * cp_u) + DVec3::Y * (cp_u + d12_u * sp_u)).normalize() * d11_u;
    let ek_u = (-1.0 / tau_u).exp();
    let omk_u = 1.0 - ek_u;
    let exp_u = (((po_u - u_ball.position - u_ball.velocity * (tau_u * omk_u)) / (1.0 - tau_u * omk_u))
        + ((vs_u - u_ball.velocity * ek_u) / omk_u)) * 0.5;
    assert_vec_close(g_unclamped, exp_u, 1e-6);

    // 5. Orbit: extreme negative x < -0.9940146675109863 clamps lterm = -6.885381841077475
    let mut extreme = ball_with(3, DVec3::new(200.0, 0.0, 0.0), DVec3::ZERO, 10.0, 0.001, 100.0, 1.0);
    extreme.mode = MovementMode::Orbit { target_id: 2, range: 5.0 };
    let mut pos_small = BTreeMap::new();
    pos_small.insert(2, (DVec3::ZERO, DVec3::ZERO, 4.0));
    let g_ext = compute_subwarp_goal_velocity(&extreme, &pos_small, 30.0);
    let phi_ext = 6.885_381_841_077_475_f64;
    let r_ext = 200.0 * (-phi_ext).exp();
    let (sp_e, cp_e) = phi_ext.sin_cos();
    let p_ext = DVec3::NEG_X * (200.0 - r_ext * cp_e) + DVec3::NEG_Z * (r_ext * sp_e);
    let vs_ext = (DVec3::NEG_X * (sp_e + cp_e) + DVec3::NEG_Z * (cp_e - sp_e)).normalize() * 10.0;
    let exp_ext = (p_ext / (30.0 - 1e-6) + vs_ext) * 0.5;
    assert_vec_close(g_ext, exp_ext, 1e-6);

    // 6. Orbit: low v_spiral (< 0.05 * v_max) uses fallback (p_orbit - pos)/dt - vel
    let mut slow = ball_with(4, DVec3::new(1.0, 0.0, 0.0), DVec3::new(0.5, 0.0, 0.0), 1000.0, 10_000.0, 1_000_000.0, 1.0);
    slow.mode = MovementMode::Orbit { target_id: 2, range: 5.0 };
    let g_slow = compute_subwarp_goal_velocity(&slow, &pos_small, 1.0);
    let d11_s = 0.1_f64.sqrt();
    let d12_s = 0.0009_f64;
    let phi_s = (1.0 + d11_s * d12_s / (d12_s * d12_s + 1.0).sqrt()).ln() / d12_s;
    let r_s = (d12_s * phi_s).exp();
    let (sp_s, cp_s) = phi_s.sin_cos();
    let exp_slow = DVec3::NEG_X * (1.0 - r_s * cp_s) + DVec3::NEG_Z * (r_s * sp_s) - DVec3::new(0.5, 0.0, 0.0);
    assert_vec_close(g_slow, exp_slow, 1e-6);

    // 7. Orbit: Z-pole (+Z vs -Z) uses opposite pole_ref signs (angle=7rad in quadrant I: cos>0, sin>0)
    let mut zp = ball_with(7, DVec3::new(0.0, 0.0, 1000.0), DVec3::ZERO, 300.0, 2.0, 1e6, 50.0);
    zp.mode = MovementMode::Orbit { target_id: 2, range: 500.0 };
    let gp = compute_subwarp_goal_velocity(&zp, &pos_small, 1.0);
    zp.position = DVec3::new(0.0, 0.0, -1000.0);
    let gn = compute_subwarp_goal_velocity(&zp, &pos_small, 1.0);
    assert!(gp.x < -1.0 && gp.y < -1.0 && gn.x > 1.0 && gn.y > 1.0);
    assert!((gp.x + gn.x).abs() < 1e-6 && (gp.y + gn.y).abs() < 1e-6);
}

#[test]
fn test_step_warp_ball_exact_phases_and_boundaries() {
    let dest = DVec3::new(10.0 * AU_METERS, 0.0, 0.0);
    let mut b = ball_with(10, DVec3::new(1000.0, 0.0, 0.0), DVec3::ZERO, 200.0, 2.0, 1e6, 50.0);
    let mut phase = WarpPhase::Aligning;
    let mut elapsed = 0.0;

    // Exact 0.75 * max_velocity boundary does NOT trigger warp acceleration (must be strictly >)
    b.velocity = DVec3::new(200.0 * 0.75, 0.0, 0.0);
    assert!(!step_warp_ball(&mut b, 0.0, DVec3::ZERO, dest, 3.0, 0.0, &mut phase, &mut elapsed));
    assert_eq!(phase, WarpPhase::Aligning);

    // Dot == 0.99 boundary does NOT trigger warp acceleration (must be strictly >)
    b.velocity = DVec3::new(0.99, (1.0_f64 - 0.99 * 0.99).sqrt(), 0.0) * 180.0;
    assert!(!step_warp_ball(&mut b, 0.0, DVec3::ZERO, dest, 3.0, 0.0, &mut phase, &mut elapsed));
    assert_eq!(phase, WarpPhase::Aligning);

    // Exceeding both triggers Accelerating and updates origin/total_distance
    b.velocity = DVec3::new(0.995, (1.0_f64 - 0.995 * 0.995).sqrt(), 0.0) * 180.0;
    assert!(!step_warp_ball(&mut b, 1.0, DVec3::ZERO, dest, 3.0, 0.0, &mut phase, &mut elapsed));
    assert_eq!(phase, WarpPhase::Accelerating);
    assert!((elapsed - 1.0).abs() < 1e-9);
    if let MovementMode::Warp { total_distance, .. } = b.mode {
        assert!((total_distance - (dest.x - 1000.0)).abs() < 1.0);
    } else {
        panic!("expected Warp");
    }

    // Long warp phase transitions in b.mode (Accel -> Cruise -> Decel)
    let k_a = 3.0;
    let k_d = k_a / 3.0;
    let v_cruise = 3.0 * AU_METERS;
    let raw_dist = dest.length();
    let eff_total = raw_dist + 1.0;
    let v_peak = v_cruise.min(eff_total * k_a * k_d / (k_a + k_d));
    let t_accel_end = (v_peak / k_a).ln() / k_a;
    let t_cruise_dur = (eff_total / v_peak - 1.0 / k_a - 1.0 / k_d).max(0.0);

    // Step across t_accel_end transitions b.mode to Cruising
    phase = WarpPhase::Accelerating;
    elapsed = t_accel_end - 0.5;
    assert!(!step_warp_ball(&mut b, 1.0, DVec3::ZERO, dest, 3.0, raw_dist, &mut phase, &mut elapsed));
    assert!(matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Cruising, .. }));

    // Step across t_accel_end + t_cruise_dur transitions b.mode to Decelerating
    phase = WarpPhase::Cruising;
    elapsed = t_accel_end + t_cruise_dur - 0.5;
    assert!(!step_warp_ball(&mut b, 1.0, DVec3::ZERO, dest, 3.0, raw_dist, &mut phase, &mut elapsed));
    assert!(matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Decelerating, .. }));
    assert!((b.velocity.x - v_peak).abs() < 1.0);
    let exp_x = raw_dist - (raw_dist - (v_peak / k_a + v_peak * (t_cruise_dur - 0.5)));
    assert!((b.position.x - exp_x).abs() < 1.0);

    // Short warp (v_peak < v_cruise, t_cruise_dur == 0) + origin == destination fallback
    let s_dest = DVec3::new(10_000.0, 0.0, 0.0);
    b.position = DVec3::ZERO;
    phase = WarpPhase::Accelerating;
    elapsed = 1.0;
    assert!(!step_warp_ball(&mut b, 0.5, s_dest, s_dest, 3.0, 10_000.0, &mut phase, &mut elapsed));
    let vp_short = (10_001.0 * 3.0 * 1.0) / 4.0;
    let t_acc_short = (vp_short / 3.0_f64).ln() / 3.0;
    assert!((b.position.x - (10_000.0 - (10_000.0 - (3.0_f64).exp()))).abs() < 1.0);
    assert!(elapsed < t_acc_short && (elapsed - 1.5).abs() < 1e-9 && matches!(b.mode, MovementMode::Warp { phase: WarpPhase::Accelerating, .. }));

    // Tiny warp (2m) where v_peak <= k_a => t_accel_end == 0.0 (first tick hits t <= t_accel_end, second exits)
    b.position = DVec3::ZERO;
    phase = WarpPhase::Accelerating;
    elapsed = 0.0;
    assert!(!step_warp_ball(&mut b, 1.0, DVec3::ZERO, DVec3::new(2.0, 0.0, 0.0), 3.0, 2.0, &mut phase, &mut elapsed));
    assert_eq!(phase, WarpPhase::Accelerating);
    assert!(step_warp_ball(&mut b, 1.0, DVec3::ZERO, DVec3::new(2.0, 0.0, 0.0), 3.0, 2.0, &mut phase, &mut elapsed));
    assert_eq!(b.mode, MovementMode::Stop);

    // Decelerating mid-step & exit
    phase = WarpPhase::Decelerating;
    elapsed = t_accel_end + t_cruise_dur + 1.0;
    assert!(!step_warp_ball(&mut b, 1.0, DVec3::ZERO, dest, 3.0, raw_dist, &mut phase, &mut elapsed));
    let rem_plus_1 = (v_peak / k_d) * (-k_d * 1.0).exp();
    assert!((b.velocity.x - rem_plus_1 * k_d).abs() < 1.0);
    assert!((b.position.x - (raw_dist - (rem_plus_1 - 1.0))).abs() < 1.0);

    elapsed = t_accel_end + t_cruise_dur + 40.0;
    assert!(step_warp_ball(&mut b, 1.0, DVec3::ZERO, dest, 3.0, raw_dist, &mut phase, &mut elapsed));
    assert_eq!(b.mode, MovementMode::Stop);
    assert_eq!(b.speed_fraction, 0.0);

    // Off-axis warp with total_distance == 0.0 uses (destination - origin) for both length and dir
    let o_off = DVec3::new(1000.0, 5000.0, 0.0);
    let d_off = DVec3::new(11_000.0, 5000.0, 0.0);
    let mut off = ball_with(11, o_off, DVec3::new(200.0, 0.0, 0.0), 200.0, 2.0, 1e6, 50.0);
    let mut p_off = WarpPhase::Accelerating;
    let mut e_off = 0.0;
    assert!(!step_warp_ball(&mut off, 0.1, o_off, d_off, 3.0, 0.0, &mut p_off, &mut e_off));
    assert!((off.position.y - 5000.0).abs() < 1e-6 && (off.position.x - 1001.0).abs() < 1e-6);
    assert!(matches!(off.mode, MovementMode::Warp { total_distance, .. } if (total_distance - 10_000.0).abs() < 1e-6));
}

#[test]
fn test_step_collision_bumping_exact_swept_and_moving_static() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 250_000.0);

    // 1. Moving static miniball (id 101 < dynamic 102) AND reversed (dynamic 105 < miniball 106)
    let parent = ball_with(100, DVec3::new(0.0, 0.0, 0.0), DVec3::new(10.0, 0.0, 0.0), 300.0, 2.0, 1e6, 20.0);
    let mut mb1 = ball_with(101, DVec3::new(50.0, 0.0, 0.0), DVec3::ZERO, 0.0, 2.0, 1e8, 40.0);
    mb1.mode = MovementMode::Miniball;
    let ship_a = ball_with(102, DVec3::new(120.0, 0.0, 0.0), DVec3::new(-200.0, 0.0, 0.0), 300.0, 2.0, 1000.0, 30.0);
    assert!(park.add_ball(parent));
    assert!(park.add_ball(mb1));
    assert!(park.add_ball(ship_a));
    park.miniball_states.insert(101, MiniballState { parent_id: 100, relative_offset: DVec3::new(50.0, 0.0, 0.0) });

    // Separate Y lane for reversed ID order (dynamic 105 < moving static miniball 106)
    let parent2 = ball_with(104, DVec3::new(0.0, 50_000.0, 0.0), DVec3::new(10.0, 0.0, 0.0), 300.0, 2.0, 1e6, 20.0);
    let ship_c = ball_with(105, DVec3::new(120.0, 50_000.0, 0.0), DVec3::new(-200.0, 0.0, 0.0), 300.0, 2.0, 1000.0, 30.0);
    let mut mb2 = ball_with(106, DVec3::new(50.0, 50_000.0, 0.0), DVec3::ZERO, 0.0, 2.0, 1e8, 40.0);
    mb2.mode = MovementMode::Miniball;
    assert!(park.add_ball(parent2));
    assert!(park.add_ball(ship_c));
    assert!(park.add_ball(mb2));
    park.miniball_states.insert(106, MiniballState { parent_id: 104, relative_offset: DVec3::new(50.0, 0.0, 0.0) });

    let mut pre = park.balls.clone();
    pre.get_mut(&101).unwrap().position = DVec3::new(0.0, 0.0, 0.0);
    pre.get_mut(&102).unwrap().position = DVec3::new(250.0, 0.0, 0.0);
    pre.get_mut(&106).unwrap().position = DVec3::new(0.0, 50_000.0, 0.0);
    pre.get_mut(&105).unwrap().position = DVec3::new(250.0, 50_000.0, 0.0);
    let pre_pos: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre.iter().map(|(&id, b)| (id, (b.position, b.velocity, b.radius))).collect();
    park.step_collision_bumping(&pre, &pre_pos, 1.0);

    assert!((park.balls[&102].position.x - park.balls[&105].position.x).abs() < 1e-6);
    assert!((park.balls[&102].velocity.x - park.balls[&105].velocity.x).abs() < 1e-6);
    assert!((park.balls[&102].position.x - 165.417822).abs() < 1e-2 || park.balls[&102].velocity.x > -200.0);
    // Exact verification for moving static swept bounce:
    let tau102 = pre[&102].time_constant();
    let dp0_m = 250.0_f64;
    let dv_m = -180.0_f64; // (-130) - (+50)
    let t_col_m = (-2.0 * dp0_m * dv_m - ((2.0 * dp0_m * dv_m).powi(2) - 4.0 * dv_m * dv_m * (dp0_m * dp0_m - 4900.0)).sqrt()) / (2.0 * dv_m * dv_m);
    let v_ac_m = -200.0 * (-t_col_m / tau102).exp();
    let v_af_m = (-v_ac_m) * (-(1.0 - t_col_m) / tau102).exp();
    let v_exp_m = -200.0 + (v_af_m - (-200.0)) * 0.85;
    let p_exp_m = 120.0 + (v_exp_m - (-200.0)) * (1.0 / (1.0 - (-1.0 / tau102).exp()) - tau102);
    assert!((park.balls[&102].position.x - p_exp_m).abs() < 1e-3);
    assert!((park.balls[&102].velocity.x - v_exp_m).abs() < 1e-3);
    assert!((park.balls[&105].position.x - p_exp_m).abs() < 1e-3);
    assert!((park.balls[&105].velocity.x - v_exp_m).abs() < 1e-3);

    // 2. Dynamic vs dynamic swept with NON-ZERO goal_vel (kills subwarp_step_state line 241!)
    let mut p2 = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    let mut d1 = ball_with(1, DVec3::new(50.0, 0.0, 0.0), DVec3::new(150.0, 0.0, 0.0), 300.0, 2.0, 1000.0, 30.0);
    let mut d2 = ball_with(2, DVec3::new(-50.0, 0.0, 0.0), DVec3::new(-150.0, 0.0, 0.0), 300.0, 2.0, 2000.0, 30.0);
    d1.mode = MovementMode::GotoDirection { direction: DVec3::X };
    d2.mode = MovementMode::GotoDirection { direction: DVec3::NEG_X };
    assert!(p2.add_ball(d1));
    assert!(p2.add_ball(d2));
    let mut pre2 = p2.balls.clone();
    pre2.get_mut(&1).unwrap().position = DVec3::new(-100.0, 0.0, 0.0);
    pre2.get_mut(&2).unwrap().position = DVec3::new(100.0, 0.0, 0.0);
    let pre_pos2: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre2.iter().map(|(&id, b)| (id, (b.position, b.velocity, b.radius))).collect();
    p2.step_collision_bumping(&pre2, &pre_pos2, 1.0);

    let t_col = 84_000.0 / 180_000.0;
    let dt1 = t_col;
    let dt2 = 1.0 - t_col;
    let tau_a = pre2[&1].time_constant();
    let tau_b = pre2[&2].time_constant();
    let goal_a = 300.0;
    let goal_b = -300.0;
    let v_ac_x = goal_a + (150.0 - goal_a) * (-dt1 / tau_a).exp();
    let v_bc_x = goal_b + (-150.0 - goal_b) * (-dt1 / tau_b).exp();
    let v_an_new = ((1000.0 - 2000.0) * v_ac_x + 2.0 * 2000.0 * v_bc_x) / 3000.0;
    let v_bn_new = ((2000.0 - 1000.0) * v_bc_x + 2.0 * 1000.0 * v_ac_x) / 3000.0;
    let v_af = goal_a + (v_an_new - goal_a) * (-dt2 / tau_a).exp();
    let v_bf = goal_b + (v_bn_new - goal_b) * (-dt2 / tau_b).exp();
    let v1_exp = 150.0 + (v_af - 150.0) * 0.85;
    let v2_exp = -150.0 + (v_bf - (-150.0)) * 0.85;
    let p1_exp = 50.0 + (v1_exp - 150.0) * (1.0 / (1.0 - (-1.0 / tau_a).exp()) - tau_a);
    let p2_exp = -50.0 + (v2_exp - (-150.0)) * (1.0 / (1.0 - (-1.0 / tau_b).exp()) - tau_b);
    assert!((p2.balls[&1].position.x - p1_exp).abs() < 1e-4);
    assert!((p2.balls[&1].velocity.x - v1_exp).abs() < 1e-4);
    assert!((p2.balls[&2].position.x - p2_exp).abs() < 1e-4);
    assert!((p2.balls[&2].velocity.x - v2_exp).abs() < 1e-4);

    // 3. Both static overlap orders with v_rel == 0.0 (no Collision event, exact separation on both)
    let mut p3 = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    let mut st1 = ball_with(200, DVec3::ZERO, DVec3::ZERO, 0.0, 2.0, 1e8, 500.0);
    st1.mode = MovementMode::Rigid;
    assert!(p3.add_static_ball(st1, 15));
    let sh1 = ball_with(201, DVec3::new(510.0, 0.0, 0.0), DVec3::ZERO, 300.0, 2.0, 1000.0, 20.0);
    assert!(p3.add_ball(sh1));
    let sh2 = ball_with(210, DVec3::new(50_510.0, 0.0, 0.0), DVec3::ZERO, 300.0, 2.0, 1000.0, 20.0);
    assert!(p3.add_ball(sh2));
    let mut st2 = ball_with(211, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO, 0.0, 2.0, 1e8, 500.0);
    st2.mode = MovementMode::Rigid;
    assert!(p3.add_static_ball(st2, 15));
    let pre3 = p3.balls.clone();
    let pos3: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre3.iter().map(|(&id, b)| (id, (b.position, b.velocity, b.radius))).collect();
    p3.step_collision_bumping(&pre3, &pos3, 1.0);
    assert!((p3.balls[&201].position.x - 520.0).abs() < 1e-6);
    assert!((p3.balls[&210].position.x - 50_520.0).abs() < 1e-6);
    assert!(!p3.pending_events.iter().any(|e| matches!(e, DestinyEvent::Collision { .. })));

    // 4. 2D angled moving static is_static_b (Y motion changes normal)
    let mut p4 = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    let par4 = ball_with(300, DVec3::new(0.0, -10_000.0, 0.0), DVec3::new(0.0, 20.0, 0.0), 300.0, 2.0, 1e6, 10.0);
    let sh4 = ball_with(301, DVec3::new(0.0, 0.0, 0.0), DVec3::new(-100.0, 0.0, 0.0), 300.0, 2.0, 1e6, 30.0);
    let mut mb4 = ball_with(302, DVec3::new(0.0, 40.0, 0.0), DVec3::ZERO, 0.0, 2.0, 1e8, 40.0);
    mb4.mode = MovementMode::Miniball;
    assert!(p4.add_ball(par4));
    assert!(p4.add_ball(sh4));
    assert!(p4.add_ball(mb4));
    p4.miniball_states.insert(302, MiniballState { parent_id: 300, relative_offset: DVec3::new(0.0, 10_040.0, 0.0) });
    let mut pre4 = p4.balls.clone();
    pre4.get_mut(&301).unwrap().position = DVec3::new(100.0, 0.0, 0.0);
    pre4.get_mut(&302).unwrap().position = DVec3::new(0.0, 0.0, 0.0);
    let pos4: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre4.iter().map(|(&id, b)| (id, (b.position, b.velocity, b.radius))).collect();
    p4.step_collision_bumping(&pre4, &pos4, 1.0);
    assert!(p4.balls[&301].velocity.y.abs() > 1.0);

    // Huge tau swept collision with dt=2.0 hits 0.5*dt fallback (lines 462, 487) + t_col tie-break (466, 491)
    let mut p5 = Ballpark::with_bubble_radius(30_000_142, 250_000.0);
    let ht_a = ball_with(400, DVec3::new(100.0, 0.0, 0.0), DVec3::new(100.0, 0.0, 0.0), 300.0, 1e12, 1e12, 30.0);
    let ht_b = ball_with(401, DVec3::new(0.0, 0.0, 0.0), DVec3::new(-100.0, 0.0, 0.0), 300.0, 1e12, 1e12, 30.0);
    let mut st_y = ball_with(402, DVec3::new(0.0, -140.0, 0.0), DVec3::ZERO, 0.0, 2.0, 1e8, 30.0);
    st_y.mode = MovementMode::Rigid;
    assert!(p5.add_ball(ht_a));
    assert!(p5.add_ball(ht_b));
    assert!(p5.add_static_ball(st_y, 15));
    let mut pre5 = p5.balls.clone();
    pre5.get_mut(&400).unwrap().position = DVec3::new(-100.0, 0.0, 0.0);
    pre5.get_mut(&401).unwrap().position = DVec3::new(100.0, 0.0, 0.0);
    let pos5: BTreeMap<BallId, (DVec3, DVec3, f64)> = pre5.iter().map(|(&id, b)| (id, (b.position, b.velocity, b.radius))).collect();
    p5.step_collision_bumping(&pre5, &pos5, 2.0);
    assert!((p5.balls[&400].position.x - (100.0 + (-170.0) * 1.0)).abs() < 1e-4);
    assert!((p5.balls[&401].position.x - (0.0 + 170.0 * 1.0)).abs() < 1e-4);
}

#[test]
fn test_ballpark_step_warp_exit_clamp_and_set_mode_grid_faces() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 10_000.0);

    // 1. Exact grid face equality (pos.axis == existing.min/max while outside on another axis)
    let g1 = park.find_or_create_grid_for_position(DVec3::ZERO); // [-10000, +10000]^3
    assert_eq!(g1, 1);
    let gx_pos = park.find_or_create_grid_for_position(DVec3::new(10_000.0, 15_000.0, 0.0));
    assert!(park.grids[&gx_pos].min_bounds.x < 10_000.0); // > not >=, so x was not clamped to +10000, y was clamped!
    assert_eq!(park.grids[&gx_pos].min_bounds.y, 10_000.0);

    let gx_neg = park.find_or_create_grid_for_position(DVec3::new(-10_000.0, -15_000.0, 0.0));
    assert!(park.grids[&gx_neg].max_bounds.x > -10_000.0);
    assert_eq!(park.grids[&gx_neg].max_bounds.y, -10_000.0);

    let gz_pos = park.find_or_create_grid_for_position(DVec3::new(10_000.0, 10_000.0, 15_000.0));
    assert_eq!(park.grids[&gz_pos].min_bounds.z, 10_000.0);

    let gz_neg = park.find_or_create_grid_for_position(DVec3::new(-10_000.0, -10_000.0, -15_000.0));
    assert_eq!(park.grids[&gz_neg].max_bounds.z, -10_000.0);

    // 2. set_mode boundaries: range == 0.0, warp total_distance == 0.0, Stop-from-Stop, Warp preserving emergency flag
    let mut ship = ball_with(50, DVec3::ZERO, DVec3::new(500.0, 0.0, 0.0), 200.0, 2.0, 1e6, 50.0);
    ship.mode = MovementMode::Stop;
    assert!(park.add_ball(ship));
    park.set_ball_group_id(50, 25);

    assert!(park.set_mode(50, MovementMode::Follow { target_id: 99, range: 0.0 }).is_ok());
    assert!(park.set_mode(50, MovementMode::Orbit { target_id: 99, range: 0.0 }).is_ok());

    // Stop from Orbit does not clamp velocity when not in Aligning warp
    assert!(park.set_mode(50, MovementMode::Stop).is_ok());
    assert_eq!(park.balls[&50].velocity.x, 500.0);

    // Invalid warp params: NaN origin, negative distance, negative elapsed
    assert!(park.set_mode(50, MovementMode::Warp {
        origin: DVec3::new(f64::NAN, 0.0, 0.0),
        destination: DVec3::new(1e5, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e5,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    }).is_err());
    assert!(park.set_mode(50, MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e5, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: -1.0,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    }).is_err());
    assert!(park.set_mode(50, MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e5, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e5,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: -0.5,
    }).is_err());

    // Valid warp with 0 distance preserves emergency warping flag
    park.combat_states.entry(50).or_default().is_emergency_warping = true;
    assert!(park.set_mode(50, MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 0.0,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    }).is_ok());
    assert!(park.combat_states[&50].is_emergency_warping);

    // 3. Ballpark::step clamps overspeed ship on warp exit (lines 659-661)
    let mut exit_ship = ball_with(60, DVec3::new(1e6 - 10.0, 0.0, 0.0), DVec3::new(1500.0, 0.0, 0.0), 100.0, 2.0, 1e6, 50.0);
    exit_ship.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e6,
        phase: WarpPhase::Decelerating,
        elapsed_warp_time: 100.0,
    };
    assert!(park.add_ball(exit_ship));
    park.step(1.0);
    assert_eq!(park.balls[&60].mode, MovementMode::Stop);
    assert!(park.balls[&60].velocity.length() <= 100.0 + 1e-6);
}

#[test]
fn test_ballpark_attribute_caps_damage_mjfg_cloak_and_grid_position_edges() {
    let mut park = Ballpark::new(30_000_142);

    // 1. recompute_ball_attributes branches:
    //    - (None, None) -> no-op
    park.recompute_ball_attributes(9_999);
    //    - (None, Some(cs)) -> recompute_combat_attributes without Ball
    let mut stray_cs = CombatState::default();
    stray_cs.signature_radius = 40.0;
    stray_cs.capacitor_recharge_time_ms = 0.0;
    stray_cs.rah_armor_resonance = Some([0.4, 0.5, 0.6, 0.7]);
    stray_cs.crab_resonance_multiplier = Some(0.5);
    stray_cs.ehe_hull_resists = Some([0.05, 0.05, 0.05, 0.05]);
    park.combat_states.insert(7001, stray_cs);
    park.recompute_ball_attributes(7001);
    let cs_no_ball = &park.combat_states[&7001];
    assert_eq!(cs_no_ball.base_signature_radius, Some(40.0));
    assert_eq!(cs_no_ball.capacitor_recharge_time_ms, 0.0);
    assert_eq!(cs_no_ball.armor_resonance, [0.2, 0.25, 0.3, 0.35]);
    assert_eq!(cs_no_ball.structure_resonance, [0.05, 0.05, 0.05, 0.05]);
    assert!((cs_no_ball.shield_resonance[0] - 0.5).abs() < 1e-9);

    // adc_active overrides all three layers to [0.25; 4]
    park.combat_states.get_mut(&7001).unwrap().adc_active = true;
    park.recompute_ball_attributes(7001);
    let cs_adc = &park.combat_states[&7001];
    assert_eq!(cs_adc.shield_resonance, [0.25; 4]);
    assert_eq!(cs_adc.armor_resonance, [0.25; 4]);
    assert_eq!(cs_adc.structure_resonance, [0.25; 4]);

    // 2. (Some(ball), Some(cs)) with module_immobilized, cloak_velocity_multiplier, and cloud_cap_recharge_multiplier
    let ship = ball_with(101, DVec3::ZERO, DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    assert!(park.add_ball(ship));
    park.combat_states.insert(101, CombatState::default());
    {
        let cs = park.combat_states.get_mut(&101).unwrap();
        cs.cloud_cap_recharge_multiplier = Some(0.005); // 120_000 * 0.005 = 600 -> clamped to 1_000.0
        cs.module_immobilized = true;
    }
    park.recompute_ball_attributes(101);
    assert_eq!(park.balls[&101].max_velocity, 0.0);
    assert_eq!(park.combat_states[&101].capacitor_recharge_time_ms, 1_000.0);

    {
        let cs = park.combat_states.get_mut(&101).unwrap();
        cs.module_immobilized = false;
        cs.base_max_velocity = Some(200.0);
        cs.cloak_velocity_multiplier = Some(0.25);
        cs.hic_velocity_multiplier = 0.5;
    }
    park.recompute_ball_attributes(101);
    assert!((park.balls[&101].max_velocity - 25.0).abs() < 1e-9);

    // 3. set_ball_max_velocity & set_ball_max_velocity_cap
    assert!(!park.set_ball_max_velocity(9_999, 50.0));
    assert!(!park.set_ball_max_velocity_cap(9_999, Some(50.0)));
    park.pending_events.clear();
    assert!(park.set_ball_max_velocity(101, -12.0));
    assert_eq!(park.balls[&101].max_velocity, 0.0);
    assert!(park.pending_events.iter().any(|ev| matches!(
        ev,
        DestinyEvent::SetMaxSpeed { ball_id: 101, max_speed } if *max_speed == 0.0
    )));

    // Restore cloak off so cap math runs against base=200
    {
        let cs = park.combat_states.get_mut(&101).unwrap();
        cs.cloak_velocity_multiplier = None;
        cs.hic_velocity_multiplier = 1.0;
    }
    park.recompute_ball_attributes(101);
    assert!((park.balls[&101].max_velocity - 200.0).abs() < 1e-9);
    park.pending_events.clear();
    assert!(park.set_ball_max_velocity_cap(101, Some(40.0)));
    assert!((park.balls[&101].max_velocity - 40.0).abs() < 1e-9);
    let set_count_after_cap = park
        .pending_events
        .iter()
        .filter(|ev| matches!(ev, DestinyEvent::SetMaxSpeed { ball_id: 101, .. }))
        .count();
    assert_eq!(set_count_after_cap, 1);
    // Repeat identical cap does not emit duplicate SetMaxSpeed
    assert!(park.set_ball_max_velocity_cap(101, Some(40.0)));
    let set_count_repeat = park
        .pending_events
        .iter()
        .filter(|ev| matches!(ev, DestinyEvent::SetMaxSpeed { ball_id: 101, .. }))
        .count();
    assert_eq!(set_count_repeat, 1);
    // Clearing cap restores 200.0 and emits SetMaxSpeed
    assert!(park.set_ball_max_velocity_cap(101, None));
    assert!((park.balls[&101].max_velocity - 200.0).abs() < 1e-9);

    // 4. deactivate_cloak with non-gate-cloak sets decloak_targeting_delay_remaining_s
    {
        let cs = park.combat_states.get_mut(&101).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
        cs.cloak_velocity_multiplier = Some(0.25);
        cs.cloak_configured_targeting_delay_s = 7.5;
    }
    park.pending_events.clear();
    assert!(park.deactivate_cloak(101));
    assert!(!park.combat_states[&101].is_cloaked);
    assert_eq!(park.combat_states[&101].cloak_velocity_multiplier, None);
    assert!((park.combat_states[&101].decloak_targeting_delay_remaining_s - 7.5).abs() < 1e-9);
    assert!(park.pending_events.iter().any(|ev| matches!(
        ev,
        DestinyEvent::CloakBall { ball_id: 101, cloaked: 0 }
    )));
    assert!(!park.pending_events.iter().any(|ev| matches!(
        ev,
        DestinyEvent::OnSpecialFX { ball_id: 101, guid, .. } if guid == "effects.Cloak"
    )));

    // 5. apply_breacher_dot_with_expiry explicit expiry_filetime > 0
    park.pending_events.clear();
    assert!(park.apply_breacher_dot_with_expiry(101, 25.0, 0.0, 5, 134_000_000_000_000_000));
    assert!(park.pending_events.iter().any(|ev| matches!(
        ev,
        DestinyEvent::OnDotVictimUpdated {
            ball_id: 101,
            remaining_ticks: 5,
            expiry_filetime: 134_000_000_000_000_000,
            ..
        }
    )));

    // 6. apply_true_damage_to_ball (partial + lethal)
    let target = ball_with(102, DVec3::new(1000.0, 0.0, 0.0), DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    assert!(park.add_ball(target));
    park.combat_states.insert(102, CombatState::default()); // 450 shield + 450 armor + 350 hull = 1250 EHP
    let (applied_1, destroyed_1, _) = park.apply_true_damage_to_ball(102, 450.0).unwrap();
    assert!((applied_1 - 450.0).abs() < 1e-6);
    assert!(!destroyed_1);
    assert!((park.combat_states[&102].shield_hp - 0.0).abs() < 1e-6);
    let (_, destroyed_2, _) = park.apply_true_damage_to_ball(102, 900.0).unwrap();
    assert!(destroyed_2);
    assert!(!park.balls.contains_key(&102));

    // 7. micro_jump_field_with_capitals skips invulnerable target in radius while jumping normal target
    let caster = ball_with(201, DVec3::new(0.0, 0.0, 0.0), DVec3::ZERO, 250.0, 2.0, 1e6, 40.0);
    let valid_bystander = ball_with(202, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, 250.0, 2.0, 1e6, 40.0);
    let invuln_bystander = ball_with(203, DVec3::new(2_500.0, 0.0, 0.0), DVec3::ZERO, 250.0, 2.0, 1e6, 40.0);
    assert!(park.add_ball(caster));
    assert!(park.add_ball(valid_bystander));
    assert!(park.add_ball(invuln_bystander));
    park.combat_states.insert(202, CombatState::default());
    let mut inv_cs = CombatState::default();
    inv_cs.undock_invulnerable_remaining_ticks = 5;
    park.combat_states.insert(203, inv_cs);
    let jumped = park
        .micro_jump_field_with_capitals(201, 100_000.0, 6_000.0, 25, 0)
        .expect("mjfg should succeed");
    assert!(jumped.contains(&201));
    assert!(jumped.contains(&202));
    assert!(!jumped.contains(&203));

    // 8. set_ball_position across grid boundary collapses old grid and notifies old + new observers
    let mut gpark = Ballpark::new(30_000_142);
    let obs_old = ball_with(301, DVec3::new(100.0, 0.0, 0.0), DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    let mover = ball_with(302, DVec3::new(200.0, 0.0, 0.0), DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    let obs_new = ball_with(
        303,
        DVec3::new(30_000_000.0, 0.0, 0.0),
        DVec3::ZERO,
        200.0,
        2.0,
        1e6,
        30.0,
    );
    assert!(gpark.add_ball(obs_old));
    assert!(gpark.add_ball(mover));
    assert!(gpark.add_ball(obs_new));
    gpark.observer_events.clear();
    assert!(gpark.set_ball_position(302, DVec3::new(30_000_500.0, 0.0, 0.0)));
    assert!(gpark.in_same_bubble(302, 303));
    assert!(!gpark.in_same_bubble(301, 302));
    assert!(gpark.observer_events[&301]
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::RemoveBalls { ball_ids } if ball_ids == &[302])));
    assert!(gpark.observer_events[&303]
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == 302))));
    assert!(gpark.observer_events[&302]
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::AddBalls { balls } if balls.iter().any(|b| b.id == 303))));
}

#[test]
fn test_navigation_targeting_dedup_collision_bumping_and_step_edges() {
    let mut park = Ballpark::new(30_000_142);
    park.sim_unix_secs = 1_700_000_000;

    // 1. Non-covert cloaked set_mode(Warp) returns Err
    let cloaker = ball_with(10, DVec3::ZERO, DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    assert!(park.add_ball(cloaker));
    let mut cloak_cs = CombatState::default();
    cloak_cs.is_cloaked = true;
    cloak_cs.gate_cloak_remaining_ticks = 0;
    cloak_cs.is_covert_cloak = false;
    park.combat_states.insert(10, cloak_cs);
    let warp_err = park
        .set_mode(10, MovementMode::new_warp(DVec3::ZERO, DVec3::new(1e9, 0.0, 0.0), 3.0))
        .unwrap_err();
    assert!(warp_err.contains("CloakedCannotWarp"));

    // 2. Gate cloak cleared by set_mode(Stop)
    {
        let cs = park.combat_states.get_mut(&10).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 15;
    }
    park.pending_events.clear();
    assert!(park.set_mode(10, MovementMode::Stop).is_ok());
    assert!(!park.combat_states[&10].is_cloaked);
    assert_eq!(park.combat_states[&10].gate_cloak_remaining_ticks, 0);
    assert!(park
        .pending_events
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::CloakBall { ball_id: 10, cloaked: 0 })));

    // 3. Gate cloak cleared by set_speed_fraction > 0
    {
        let cs = park.combat_states.get_mut(&10).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 15;
    }
    assert!(park.set_speed_fraction(10, 0.75));
    assert!(!park.combat_states[&10].is_cloaked);
    assert_eq!(park.combat_states[&10].gate_cloak_remaining_ticks, 0);

    // 4. Gate cloak cleared by add_target_lock + target lock deduplication rules (#749)
    let tgt = ball_with(11, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    assert!(park.add_ball(tgt));
    {
        let cs = park.combat_states.get_mut(&10).unwrap();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 15;
    }
    assert!(park.add_target_lock(
        10,
        TargetLock {
            target_id: 11,
            lock_time_ms: 4_000.0,
            remaining_lock_ms: 4_000.0,
            locked: false,
            max_range_m: Some(50_000.0),
        }
    ));
    assert!(!park.combat_states[&10].is_cloaked);
    assert_eq!(park.target_locks[&10].len(), 1);
    // Upgrade unlocked -> locked keeps len == 1
    assert!(park.add_target_lock(
        10,
        TargetLock {
            target_id: 11,
            lock_time_ms: 4_000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(50_000.0),
        }
    ));
    assert_eq!(park.target_locks[&10].len(), 1);
    assert!(park.target_locks[&10][0].locked);
    // Locked -> unlocked does NOT downgrade
    assert!(park.add_target_lock(
        10,
        TargetLock {
            target_id: 11,
            lock_time_ms: 4_000.0,
            remaining_lock_ms: 3_000.0,
            locked: false,
            max_range_m: Some(50_000.0),
        }
    ));
    assert_eq!(park.target_locks[&10].len(), 1);
    assert!(park.target_locks[&10][0].locked);

    // 5. Stray combat_state (missing ball), cap recharge + last_step_capacitor, sim_unix_secs increment
    let mut stray = CombatState::default();
    stray.capacitor = 100.0;
    stray.max_capacitor = 250.0;
    stray.capacitor_recharge_time_ms = 60_000.0;
    park.combat_states.insert(9_001, stray);
    {
        let cs = park.combat_states.get_mut(&10).unwrap();
        cs.capacitor = 120.0;
        cs.max_capacitor = 250.0;
        cs.capacitor_recharge_time_ms = 60_000.0;
    }
    park.step(1.0);
    assert_eq!(park.sim_unix_secs, 1_700_000_001);
    assert!(park.combat_states[&10].capacitor > 120.0);
    assert_eq!(
        park.combat_states[&10].last_step_capacitor,
        Some(park.combat_states[&10].capacitor)
    );

    // 6. Collision bumping:
    //    a) Static ball id < dynamic ball id (`is_static_a && !is_static_b` branch)
    //    b) Aligning warp ship bumped resets to Aligning and emits observer SetBallPosition/Velocity
    //    c) Zero-radius swept crossing at exact origin triggers dist_c == 0 -> DVec3::X fallback
    let mut cpark = Ballpark::new(30_000_142);
    let mut static_gate = ball_with(5, DVec3::new(0.0, 0.0, 0.0), DVec3::ZERO, 0.0, 1.0, 1e9, 100.0);
    static_gate.mode = MovementMode::Rigid;
    assert!(cpark.add_ball(static_gate));
    cpark.static_ball_ids.insert(5);
    let mut bumper = ball_with(6, DVec3::new(90.0, 0.0, 0.0), DVec3::new(-20.0, 0.0, 0.0), 100.0, 2.0, 1e6, 20.0);
    bumper.mode = MovementMode::GotoDirection { direction: DVec3::NEG_X };
    assert!(cpark.add_ball(bumper));

    // Zero-radius symmetric crossing at (5000, 0, 0)
    let mut z1 = ball_with(21, DVec3::new(4_995.0, 0.0, 0.0), DVec3::new(20.0, 0.0, 0.0), 100.0, 2.0, 1e6, 0.0);
    z1.mode = MovementMode::GotoDirection { direction: DVec3::X };
    let mut z2 = ball_with(22, DVec3::new(5_005.0, 0.0, 0.0), DVec3::new(-20.0, 0.0, 0.0), 100.0, 2.0, 1e6, 0.0);
    z2.mode = MovementMode::GotoDirection { direction: DVec3::NEG_X };
    assert!(cpark.add_ball(z1));
    assert!(cpark.add_ball(z2));

    // Aligning warp ship overlapping dynamic ship at (10_000, 0, 0)
    let mut w_ship = ball_with(31, DVec3::new(10_000.0, 0.0, 0.0), DVec3::ZERO, 100.0, 2.0, 1e6, 40.0);
    w_ship.mode = MovementMode::Warp {
        origin: DVec3::new(10_000.0, 0.0, 0.0),
        destination: DVec3::new(1e7, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e7,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 2.0,
    };
    let blocker = ball_with(32, DVec3::new(10_020.0, 0.0, 0.0), DVec3::new(-10.0, 0.0, 0.0), 100.0, 2.0, 1e6, 40.0);
    assert!(cpark.add_ball(w_ship));
    assert!(cpark.add_ball(blocker));

    cpark.observer_events.clear();
    cpark.step(1.0);
    assert!(cpark.balls[&6].position.x >= 119.0);
    assert!(cpark.balls[&6].velocity.x > 0.0);
    assert!(matches!(
        cpark.balls[&31].mode,
        MovementMode::Warp { phase: WarpPhase::Aligning, elapsed_warp_time, .. } if elapsed_warp_time == 0.0
    ));
    assert!(cpark.observer_events[&32]
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::SetBallPosition { ball_id: 31, .. })));

    // 7. warp_exit_stops: NAN velocity and max_velocity == 0.0 clamping + observer broadcasts
    let mut wpark = Ballpark::new(30_000_142);
    let obs = ball_with(40, DVec3::new(1e6, 0.0, 0.0), DVec3::ZERO, 100.0, 2.0, 1e6, 30.0);
    let mut nan_ship = ball_with(41, DVec3::new(1e6 - 10.0, 0.0, 0.0), DVec3::NAN, 100.0, 2.0, 1e6, 30.0);
    nan_ship.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e6,
        phase: WarpPhase::Decelerating,
        elapsed_warp_time: 100.0,
    };
    let mut zero_cap_ship = ball_with(42, DVec3::new(1e6 - 10.0, 0.0, 0.0), DVec3::new(500.0, 0.0, 0.0), 0.0, 2.0, 1e6, 30.0);
    zero_cap_ship.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1e6, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e6,
        phase: WarpPhase::Decelerating,
        elapsed_warp_time: 100.0,
    };
    assert!(wpark.add_ball(obs));
    assert!(wpark.add_ball(nan_ship));
    assert!(wpark.add_ball(zero_cap_ship));
    wpark.observer_events.clear();
    wpark.step(1.0);
    assert_eq!(wpark.balls[&41].mode, MovementMode::Stop);
    assert!(wpark.balls[&41].velocity.is_finite());
    assert!(wpark.balls[&41].velocity.length() <= wpark.balls[&41].max_velocity + 1e-9);
    assert_eq!(wpark.balls[&42].velocity, DVec3::ZERO);
    assert!(wpark.observer_events[&40]
        .iter()
        .any(|ev| matches!(ev, DestinyEvent::Stop { ball_id: 41 })));
}

#[test]
fn test_wire_encoding_slim_meta_and_destiny_event_edges() {
    let mut align_ball = ball_with(801, DVec3::ZERO, DVec3::ZERO, 200.0, 2.0, 1e6, 30.0);
    align_ball.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(50_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 0.0,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    let buf_align = encode_add_balls(10, &[&align_ball]);
    assert!(!buf_align.is_empty());

    let mut cruise_ball = align_ball.clone();
    cruise_ball.id = 802;
    cruise_ball.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(50_000.0, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 0.0, // falls back to (destination - origin).length()
        phase: WarpPhase::Cruising,
        elapsed_warp_time: 3.0,
    };
    let buf_cruise = encode_add_balls(10, &[&cruise_ball]);
    assert_ne!(buf_align, buf_cruise);

    // Rigid Upwell structure group 4079 + wire_type_id mapping (0 -> 6, 32882 -> 55914)
    let mut upwell_ball = ball_with(1_000_000_001, DVec3::ZERO, DVec3::ZERO, 0.0, 1.0, 1e9, 5000.0);
    upwell_ball.type_id = 32882;
    upwell_ball.mode = MovementMode::Rigid;
    let meta = resolve_slim_item_meta(None, &upwell_ball);
    assert_eq!(meta.group_id, 4079);
    assert_eq!(meta.category_id, 2);
    let dict = slim_item_dict(&upwell_ball, &meta);
    let PyRep::Dict(entries) = dict else {
        panic!("expected PyRep::Dict");
    };
    assert!(entries
        .iter()
        .any(|(k, v)| matches!(k, PyRep::String(s) if s == "typeID") && *v == PyRep::Integer(55914)));

    let mut zero_type_ball = upwell_ball.clone();
    zero_type_ball.type_id = 0;
    let zero_dict = slim_item_dict(&zero_type_ball, &meta);
    let PyRep::Dict(z_entries) = zero_dict else {
        panic!("expected PyRep::Dict");
    };
    assert!(z_entries
        .iter()
        .any(|(k, v)| matches!(k, PyRep::String(s) if s == "typeID") && *v == PyRep::Integer(6)));

    // encode_destiny_event SetMaxSpeed and OnDotVictimUpdated with explicit expiry_filetime > 0
    let max_speed_wire = encode_destiny_event(
        5,
        &DestinyEvent::SetMaxSpeed {
            ball_id: 801,
            max_speed: 312.5,
        },
        None,
    );
    let PyRep::Tuple(ms_pair) = max_speed_wire else {
        panic!("expected tuple");
    };
    assert_eq!(ms_pair[0], PyRep::Integer(5));

    let dot_wire = encode_destiny_event(
        5,
        &DestinyEvent::OnDotVictimUpdated {
            ball_id: 801,
            flat_dps: 15.0,
            pct_dps: 0.01,
            remaining_ticks: 4,
            expiry_filetime: 135_000_000_000_000_000,
        },
        None,
    );
    let PyRep::Tuple(dot_pair) = dot_wire else {
        panic!("expected tuple");
    };
    assert_eq!(dot_pair[0], PyRep::Integer(5));
}

#[test]
fn mutant_killers_grid_no_expand_while_warping_and_decelerating_dest_bounds_all_six_faces() {
    use crate::{BUBBLE_RADIUS_METERS, GRID_WALL_EXPANSION_THRESHOLD_METERS};

    // 1. physics::step L604 (`!is_warping && !exited_warp`): while `still_in_old` and Aligning (`is_warping`),
    //    moving near the grid expansion wall must NOT expand the grid.
    let mut gpark = Ballpark::new(30_000_142);
    gpark.add_ball(ball_with(1, DVec3::ZERO, DVec3::ZERO, 300.0, 1.0, 1e6, 40.0));
    let gid = gpark.ball_grid_ids[&1];
    let init_max_x = gpark.grids[&gid].max_bounds.x;
    let near_wall_x = init_max_x - GRID_WALL_EXPANSION_THRESHOLD_METERS * 0.25;
    let mut b_warp = ball_with(2, DVec3::new(near_wall_x, 0.0, 0.0), DVec3::new(100.0, 0.0, 0.0), 300.0, 1.0, 1e6, 40.0);
    b_warp.mode = MovementMode::Warp {
        origin: DVec3::new(near_wall_x, 0.0, 0.0),
        destination: DVec3::new(1e10, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1e10,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    gpark.add_ball(b_warp);
    let max_x_after_add = gpark.grids[&gid].max_bounds.x;
    gpark.step(0.1);
    assert!(
        (gpark.grids[&gid].max_bounds.x - max_x_after_add).abs() < 1e-6,
        "warping ball inside old grid must not expand grid"
    );

    // 2. physics::step L650-656: Decelerating into `destination` with NO existing destination grid
    //    must reject entry when OUTSIDE any single face (-X/+X/-Y/+Y/-Z/+Z) and accept when INSIDE all 6 faces.
    let dest = DVec3::new(100_000_000.0, 100_000_000.0, 100_000_000.0);
    let raw_dist = 1_000_000_000.0_f64;
    let warp_speed = 3.0_f64;
    let k_a = warp_speed;
    let k_d = k_a / 3.0;
    let eff_total = raw_dist + 1.0;
    let v_peak = (warp_speed * AU_METERS).min(eff_total * k_a * k_d / (k_a + k_d));
    let t_accel_end = (v_peak / k_a).ln() / k_a;
    let t_cruise_dur = (eff_total / v_peak - 1.0 / k_a - 1.0 / k_d).max(0.0);
    let s_decel_start = v_peak / k_d;

    let time_for_rem = |rem_dist: f64| -> f64 {
        t_accel_end + t_cruise_dur + (s_decel_start / (rem_dist + 1.0)).ln() / k_d
    };

    for axis in [
        DVec3::X,
        -DVec3::X,
        DVec3::Y,
        -DVec3::Y,
        DVec3::Z,
        -DVec3::Z,
    ] {
        // Outside by 50 km along `axis` -> must NOT create destination grid
        let mut p_out = Ballpark::new(30_000_142);
        p_out.balls.insert(
            77,
            Ball {
                id: 77,
                type_id: 587,
                position: dest - axis * (BUBBLE_RADIUS_METERS + 50_000.0),
                velocity: axis * 1_000.0,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Warp {
                    origin: dest - axis * raw_dist,
                    destination: dest,
                    warp_speed_au_s: warp_speed,
                    total_distance: raw_dist,
                    phase: WarpPhase::Decelerating,
                    elapsed_warp_time: time_for_rem(BUBBLE_RADIUS_METERS + 50_000.0),
                },
            },
        );
        p_out.step(0.001);
        assert!(
            p_out.grids.is_empty() && !p_out.ball_grid_ids.contains_key(&77),
            "decelerating ball outside face {axis:?} must not enter/create dest grid"
        );

        // Inside by 50 km along `axis` -> MUST create destination grid and attach ball
        let mut p_in = Ballpark::new(30_000_142);
        p_in.balls.insert(
            77,
            Ball {
                id: 77,
                type_id: 587,
                position: dest - axis * (BUBBLE_RADIUS_METERS - 50_000.0),
                velocity: axis * 1_000.0,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1e6,
                radius: 40.0,
                speed_fraction: 1.0,
                mode: MovementMode::Warp {
                    origin: dest - axis * raw_dist,
                    destination: dest,
                    warp_speed_au_s: warp_speed,
                    total_distance: raw_dist,
                    phase: WarpPhase::Decelerating,
                    elapsed_warp_time: time_for_rem(BUBBLE_RADIUS_METERS - 50_000.0),
                },
            },
        );
        p_in.step(0.001);
        assert!(
            p_in.ball_grid_ids.contains_key(&77),
            "decelerating ball inside all 6 faces ({axis:?}) must enter/create dest grid"
        );
    }
}
