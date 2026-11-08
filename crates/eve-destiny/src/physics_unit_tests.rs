//! Targeted unit and mutation-killing tests for `physics.rs`.

use super::*;
use crate::{DamageVector, FormationState, MiniballState, MushroomState, TargetLock, TrollState};

fn make_ball(id: BallId, pos: DVec3, vel: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: vel,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode,
    }
}

#[test]
fn special_modes_mushroom_troll_boid_formation_miniball_and_missile_step() {
    let mut park = Ballpark::new(30_000_142);
    assert!(park.step(0.0).is_empty());
    assert!(park.step(-1.0).is_empty());
    assert!(park.step(f64::NAN).is_empty());

    // 1. Mushroom expands and then expires
    park.add_ball(make_ball(1, DVec3::ZERO, DVec3::new(10.0, 0.0, 0.0), MovementMode::Mushroom));
    park.mushroom_states.insert(1, MushroomState {
        effect_stamp: park.current_tick,
        duration_s: 2.0,
        max_radius: 1_000.0,
        owner_id: 1,
    });
    // 2. Troll coasts then freezes into Rigid static ball
    park.add_ball(make_ball(2, DVec3::new(0.0, 10_000.0, 0.0), DVec3::new(100.0, 0.0, 0.0), MovementMode::Troll));
    park.troll_states.insert(2, TrollState {
        effect_stamp: park.current_tick + 2,
    });
    // 3. Boid decelerates under exponential drag
    park.add_ball(make_ball(3, DVec3::new(0.0, 20_000.0, 0.0), DVec3::new(200.0, 0.0, 0.0), MovementMode::Boid));
    // 4. Leader + Formation follower + Miniball + Missile
    park.add_ball(make_ball(4, DVec3::new(5_000.0, 0.0, 0.0), DVec3::new(100.0, 0.0, 0.0), MovementMode::GotoDirection { direction: DVec3::X }));
    park.add_ball(make_ball(5, DVec3::new(4_500.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Formation));
    park.formation_states.insert(5, FormationState {
        leader_id: 4,
        slot_offset: DVec3::new(100.0, 50.0, -200.0),
        follow_range: 200.0,
        effect_stamp: park.current_tick,
    });
    park.add_ball(make_ball(6, DVec3::new(5_000.0, 25.0, 0.0), DVec3::ZERO, MovementMode::Miniball));
    park.miniball_states.insert(6, MiniballState {
        parent_id: 4,
        relative_offset: DVec3::new(0.0, 25.0, 0.0),
    });
    park.add_ball(make_ball(7, DVec3::new(5_000.0, 0.0, 2_000.0), DVec3::ZERO, MovementMode::Missile { target_id: 4 }));

    // Tick 1: Mushroom expands, Troll coasts, Formation/Missile/Miniball advance
    park.step(1.0);
    assert!(park.balls[&1].radius > 500.0 && park.balls[&1].radius < 1_000.0);
    assert_eq!(park.balls[&2].mode, MovementMode::Troll);
    assert!(park.balls[&3].velocity.x < 200.0 && park.balls[&3].velocity.x > 0.0);
    assert!(park.balls[&5].velocity.length() > 0.0);
    assert_eq!(park.balls[&6].position, park.balls[&4].position + DVec3::new(0.0, 25.0, 0.0));
    assert!((park.balls[&7].velocity.length() - 300.0).abs() < 1e-6);

    // Tick 2: Mushroom expires & is removed, Troll freezes into Rigid static ball
    park.step(1.0);
    assert!(!park.balls.contains_key(&1));
    assert_eq!(park.balls[&2].mode, MovementMode::Rigid);
    assert!(park.static_ball_ids.contains(&2));

    // Removing leader 4 causes Miniball 6 to be cleaned up and Missile 7 to coast on last heading
    park.remove_ball(4);
    park.step(1.0);
    assert!(!park.balls.contains_key(&6));
    assert!((park.balls[&7].velocity.length() - 300.0).abs() < 1e-6);
}

#[test]
fn subwarp_follow_orbit_and_warp_phases_to_completion() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ball(10, DVec3::ZERO, DVec3::ZERO, MovementMode::Stop));
    park.add_ball(make_ball(11, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Follow { target_id: 10, range: 500.0 }));
    park.add_ball(make_ball(12, DVec3::new(0.0, 1_500.0, 0.0), DVec3::ZERO, MovementMode::Orbit { target_id: 10, range: 1_000.0 }));

    let dest = DVec3::new(50_000_000.0, 0.0, 0.0);
    let mut warper = make_ball(13, DVec3::new(0.0, 0.0, 5_000.0), DVec3::new(280.0, 0.0, 0.0), MovementMode::Warp {
        origin: DVec3::new(0.0, 0.0, 5_000.0),
        destination: dest,
        warp_speed_au_s: 3.0,
        total_distance: (dest - DVec3::new(0.0, 0.0, 5_000.0)).length(),
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    });
    warper.velocity = (dest - warper.position).normalize() * 250.0;
    park.add_ball(warper);

    for _ in 0..40 {
        park.step(1.0);
        if matches!(park.balls[&13].mode, MovementMode::Stop) {
            break;
        }
    }
    assert_eq!(park.balls[&13].mode, MovementMode::Stop);
    assert!((park.balls[&13].position - dest).length() < 5_000.0);
    assert!(park.combat_states[&13].warp_exit_invulnerable_remaining_ticks > 0);
    assert!((park.balls[&11].position - park.balls[&10].position).length() < 2_000.0);
    assert!(park.balls[&12].velocity.length() > 0.0);
}

#[test]
fn tether_repair_breacher_dot_mjfg_spool_proximity_decloak_and_lock_progression() {
    let mut park = Ballpark::new(30_000_142);

    // 1. Tethered ship repairs 10%/s
    park.add_ball(make_ball(1, DVec3::new(-50_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
    {
        let cs1 = park.combat_states.get_mut(&1).unwrap();
        cs1.is_tethered = true;
        cs1.max_shield_hp = 1_000.0;
        cs1.max_armor_hp = 1_000.0;
        cs1.max_structure_hp = 1_000.0;
        cs1.shield_hp = 500.0;
        cs1.armor_hp = 500.0;
        cs1.structure_hp = 500.0;
    }

    // 2. Command Destroyer (type_id 37480 Bifrost) MJFG spool & Breacher DoT victim
    let mut cmd = make_ball(2, DVec3::ZERO, DVec3::ZERO, MovementMode::Stop);
    cmd.type_id = 37480;
    park.add_ball(cmd);
    {
        let cs2 = park.combat_states.get_mut(&2).unwrap();
        cs2.last_step_capacitor = Some(1_000.0);
        cs2.capacitor = 900.0; // -100 GJ drop >= 70 triggers 9s MJFG spool & 2.5x sig bloom
        cs2.signature_radius = 100.0;
        cs2.active_breacher_dot = Some((50.0, 0.10, 2));
    }

    // 3. Cloaked ship 3 decloaked by swept proximity of ship 4, and target lock completion
    park.add_ball(make_ball(3, DVec3::new(10_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
    park.combat_states.get_mut(&3).unwrap().is_cloaked = true;
    park.add_ball(make_ball(4, DVec3::new(11_500.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));

    park.add_target_lock(4, TargetLock {
        target_id: 2,
        lock_time_ms: 1_500.0,
        remaining_lock_ms: 800.0,
        locked: false,
        max_range_m: Some(50_000.0),
    });

    let evs = park.step(1.0);
    assert!(park.combat_states[&1].shield_hp > 600.0);
    assert!((park.combat_states[&1].armor_hp - 600.0).abs() < 1e-6);
    assert!((park.combat_states[&1].structure_hp - 600.0).abs() < 1e-6);
    assert!((park.combat_states[&2].signature_radius - 250.0).abs() < 1e-6);
    assert!((park.combat_states[&2].mjfg_spool_remaining_s - 8.0).abs() < 1e-6);
    assert!(!park.combat_states[&3].is_cloaked);
    assert!(park.get_target_locks(4)[0].locked);
    assert!(evs.iter().any(|e| matches!(e, DestinyEvent::OnDotVictimUpdated { ball_id: 2, remaining_ticks: 1, .. })));
    assert!(evs.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, source_id: 4, target_id: 2 } if mode == "locked")));
}

#[test]
fn combat_equations_lock_tracking_passive_recharge_turret_missile_spool_and_concord() {
    use super::combat::*;

    assert!(compute_lock_time_seconds(400.0, 125.0) >= 0.25);
    assert_eq!(compute_lock_time_seconds(1e9, 1e9), 0.25);

    assert_eq!(compute_angular_velocity(DVec3::ZERO, DVec3::ZERO, DVec3::new(0.5, 0.0, 0.0), DVec3::Y), 0.0);
    let omega = compute_angular_velocity(DVec3::ZERO, DVec3::ZERO, DVec3::new(1_000.0, 0.0, 0.0), DVec3::new(0.0, 200.0, 0.0));
    assert!((omega - 0.2).abs() < 1e-9);

    assert_eq!(compute_passive_recharge(500.0, 0.0, 10_000.0, 1_000.0), 0.0);
    assert_eq!(compute_passive_recharge(1_000.0, 1_000.0, 10_000.0, 1_000.0), 1_000.0);
    let recharged = compute_passive_recharge(250.0, 1_000.0, 100_000.0, 1_000.0);
    assert!(recharged > 250.0 && recharged < 1_000.0);

    // At optimal range with zero angular velocity, hit chance is 1.0; at optimal + falloff, hit chance is 0.5
    assert!((compute_turret_chance_to_hit(0.0, 0.1, 100.0, 5_000.0, 10_000.0, 5_000.0) - 1.0).abs() < 1e-9);
    assert!((compute_turret_chance_to_hit(0.0, 0.1, 100.0, 15_000.0, 10_000.0, 5_000.0) - 0.5).abs() < 1e-9);
    assert!(compute_turret_chance_to_hit(0.0, 0.1, 100.0, 15_000.0, 10_000.0, 0.0) < 1e-20);

    let base_dmg = DamageVector::new(100.0, 100.0, 100.0, 100.0);
    let (miss_dmg, miss_wreck) = resolve_turret_shot(base_dmg, 0.5, 0.5);
    assert_eq!(miss_dmg.total(), 0.0);
    assert!(!miss_wreck);
    let (wreck_dmg, is_wreck) = resolve_turret_shot(base_dmg, 0.5, 0.005);
    assert!((wreck_dmg.total() - 1_200.0).abs() < 1e-9);
    assert!(is_wreck);
    let (norm_dmg, norm_wreck) = resolve_turret_shot(base_dmg, 0.8, 0.51);
    assert!((norm_dmg.total() - 400.0).abs() < 1e-9);
    assert!(!norm_wreck);

    // Missile damage factor: large slow target -> 1.0, small fast target -> < 1.0 (both drf <= 1 and drf > 1)
    assert_eq!(compute_missile_damage_factor(400.0, 50.0, 100.0, 200.0, 0.6), 1.0);
    let f_pre = compute_missile_damage_factor(30.0, 400.0, 150.0, 100.0, 0.682);
    let f_raw = compute_missile_damage_factor(30.0, 400.0, 150.0, 100.0, 3.2);
    assert!(f_pre > 0.0 && f_pre < 0.25);
    assert!(f_raw > 0.0 && f_raw < 0.25);

    assert!((compute_entropic_spool_multiplier(10, 20, 0.05) - 1.5).abs() < 1e-9);
    assert!((compute_entropic_spool_multiplier(30, 20, 0.05) - 2.0).abs() < 1e-9);

    assert_eq!(concord_response_seconds(1.0), 6.0);
    assert_eq!(concord_response_seconds(0.9), 6.0);
    assert_eq!(concord_response_seconds(0.8), 7.0);
    assert_eq!(concord_response_seconds(0.7), 10.0);
    assert_eq!(concord_response_seconds(0.6), 14.0);
    assert_eq!(concord_response_seconds(0.5), 19.0);
    assert_eq!(concord_response_seconds(0.4), f64::INFINITY);
}

#[test]
fn coverage_and_mutants_proximity_decloak_restores_velocity_and_mjfg_lifecycle() {
    let mut park = Ballpark::new(30_000_142);

    // 1. Cloaked ship with slowed max_velocity and slim_meta.is_cloaked approached within 2km
    let mut cloaked = make_ball(1, DVec3::ZERO, DVec3::ZERO, MovementMode::Stop);
    cloaked.max_velocity = 45.0;
    park.add_ball(cloaked);
    park.ball_slim_meta.insert(
        1,
        crate::SlimItemMeta {
            is_cloaked: true,
            ..Default::default()
        },
    );
    park.combat_states.insert(
        1,
        crate::CombatState {
            is_cloaked: true,
            is_covert_cloak: true,
            base_max_velocity: Some(380.0),
            cloak_velocity_multiplier: Some(1.0),
            cloak_configured_targeting_delay_s: 5.0,
            cloak_stabilization_remaining_s: 120.0,
            ..Default::default()
        },
    );

    let scout = make_ball(2, DVec3::new(1_500.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop);
    park.add_ball(scout);

    // 2. Command Destroyer (type 37480) MJFG spool start, signature bloom *2.5, scramble abort, and full spool
    let mut cd = make_ball(3, DVec3::new(50_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop);
    cd.type_id = 37480;
    park.add_ball(cd);
    park.combat_states.insert(
        3,
        crate::CombatState {
            capacitor: 200.0,
            max_capacitor: 300.0,
            signature_radius: 60.0,
            last_step_capacitor: Some(280.0),
            ..Default::default()
        },
    );

    let passenger = make_ball(4, DVec3::new(52_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop);
    park.add_ball(passenger);

    // 3. Breacher DoT where flat_dps < pct_dps * max_total, with non-unit shield/armor/hull ratios
    let victim = make_ball(5, DVec3::new(80_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop);
    park.add_ball(victim);
    park.combat_states.insert(
        5,
        crate::CombatState {
            shield_hp: 200.0,
            max_shield_hp: 200.0,
            shield_recharge_time_ms: 1e12,
            armor_hp: 400.0,
            max_armor_hp: 400.0,
            structure_hp: 400.0,
            max_structure_hp: 400.0,
            ..Default::default()
        },
    );
    park.apply_breacher_dot(5, 50.0, 0.5, 2);

    let evs = park.step(1.0);

    // Decloaked ship restored max_velocity = 380.0 and cleared slim_meta.is_cloaked
    assert!(!park.combat_states[&1].is_cloaked);
    assert_eq!(park.balls[&1].max_velocity, 380.0);
    assert!(!park.ball_slim_meta[&1].is_cloaked);
    assert_eq!(park.combat_states[&1].decloak_targeting_delay_remaining_s, 5.0);

    // Command destroyer started 9s spool (tick 1 -> 8s remaining) and bloomed signature to 150.0
    assert_eq!(park.combat_states[&3].mjfg_spool_remaining_s, 8.0);
    assert!((park.combat_states[&3].signature_radius - 150.0).abs() < 1e-6);

    // Breacher DoT dealt flat 50.0 (since 50 < 0.5 * 1000 = 500) -> shield 150/200 = 0.75
    assert!((park.combat_states[&5].shield_hp - 150.0).abs() < 1e-6);
    assert!(evs.iter().any(|e| matches!(
        e,
        DestinyEvent::OnDamageStateChange {
            ball_id: 5,
            shield_ratio,
            armor_ratio,
            structure_ratio,
        } if (*shield_ratio - 0.75).abs() < 1e-6
            && (*armor_ratio - 1.0).abs() < 1e-6
            && (*structure_ratio - 1.0).abs() < 1e-6
    )));

    // Complete remaining 8 ticks of MJFG spool -> booshes cd (3) and passenger (4) +100km
    for _ in 0..8 {
        park.step(1.0);
    }
    assert!((park.balls[&3].position.x - 150_000.0).abs() < 1e-6);
    assert!((park.balls[&4].position.x - 152_000.0).abs() < 1e-6);
    assert!((park.combat_states[&3].signature_radius - 60.0).abs() < 1e-6);

    // Tether repair at dt=2.0 with partial shield/armor/hull kills step repair & dot ratio mutants
    let mut tp = Ballpark::new(30_000_142);
    tp.add_ball(make_ball(10, DVec3::ZERO, DVec3::ZERO, MovementMode::Stop));
    tp.combat_states.insert(
        10,
        crate::CombatState {
            is_tethered: true,
            shield_hp: 50.0,
            max_shield_hp: 200.0,
            shield_recharge_time_ms: 1e12,
            armor_hp: 100.0,
            max_armor_hp: 400.0,
            structure_hp: 150.0,
            max_structure_hp: 500.0,
            ..Default::default()
        },
    );
    tp.step(2.0);
    assert!((tp.combat_states[&10].shield_hp - 90.0).abs() < 1e-6);
    assert!((tp.combat_states[&10].armor_hp - 180.0).abs() < 1e-6);
    assert!((tp.combat_states[&10].structure_hp - 250.0).abs() < 1e-6);

    // Direct analytical verification of compute_subwarp_goal_velocity across Follow and off-axis Orbit
    let target_b = Ball {
        id: 1,
        type_id: 587,
        position: DVec3::new(1_000.0, 2_000.0, -500.0),
        velocity: DVec3::new(40.0, -30.0, 20.0),
        max_velocity: 400.0,
        radius: 150.0,
        mass: 1_000_000.0,
        inertia: 3.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    };
    let orbiter = Ball {
        id: 2,
        type_id: 587,
        position: DVec3::new(-3_000.0, 5_000.0, 2_500.0),
        velocity: DVec3::new(120.0, -80.0, 60.0),
        max_velocity: 400.0,
        radius: 50.0,
        mass: 1_000_000.0,
        inertia: 3.0,
        speed_fraction: 0.75,
        mode: MovementMode::Orbit {
            target_id: 1,
            range: 2_000.0,
        },
    };
    let mut pos_map = std::collections::BTreeMap::new();
    pos_map.insert(1, (target_b.position, target_b.velocity, target_b.radius));
    let dt = 2.0_f64;
    let actual_orbit_gv = super::compute_subwarp_goal_velocity(&orbiter, &pos_map, dt);

    let v_max = orbiter.max_velocity * orbiter.speed_fraction;
    let to_t = target_b.position - orbiter.position;
    let dist = to_t.length();
    let u = to_t / dist;
    let nrm = u.cross(orbiter.velocity).normalize();
    let tang = nrm.cross(u).normalize();
    let r_des = 2_000.0 + orbiter.radius + target_b.radius;
    let tau = orbiter.time_constant();
    let a_max = v_max / tau;
    let d12 = ((r_des - dist) * 0.1 / v_max).clamp(-1.0, 1.0);
    let d11 = (a_max * dist).sqrt().min(v_max);
    let arc = dt * d11;
    let x = (arc * d12 / dist) / (d12 * d12 + 1.0).sqrt();
    let phi = (1.0 + x).ln() / d12;
    let r_new = dist * (d12 * phi).exp();
    let (sp, cp) = phi.sin_cos();
    let p_orbit = orbiter.position + u * (dist - r_new * cp) + tang * (r_new * sp);
    let v_dir = (u * (sp - d12 * cp) + tang * (cp + d12 * sp)).normalize();
    let v_spiral = v_dir * d11 + target_b.velocity;
    let ek = (-dt / tau).exp();
    let omk = 1.0 - ek;
    let pos_denom = dt - tau * omk;
    let raw_gv = ((p_orbit - orbiter.position - orbiter.velocity * (tau * omk)) / pos_denom
        + (v_spiral - orbiter.velocity * ek) / omk)
        * 0.5;
    let expected_gv = if raw_gv.length() > v_max {
        raw_gv * (v_max / raw_gv.length())
    } else {
        raw_gv
    };
    assert!((actual_orbit_gv - expected_gv).length() < 1e-9);

    // Unclamped d12 branch where |r_des - dist| * 0.1 < v_max
    let mut orbiter_mid = orbiter.clone();
    orbiter_mid.position = DVec3::new(1_000.0 + 3_200.0, 2_000.0, -500.0);
    let gv_mid = super::compute_subwarp_goal_velocity(&orbiter_mid, &pos_map, 2.0);
    let d12_mid = (r_des - 3_200.0) * 0.1 / v_max;
    let phi_mid = (1.0 + (dt * d11 * d12_mid / 3_200.0) / (d12_mid * d12_mid + 1.0).sqrt()).ln() / d12_mid;
    assert!((d12_mid - (-1_000.0 * 0.1 / 300.0)).abs() < 1e-9 && gv_mid.length() > 0.0 && phi_mid.is_finite());

    // Follow damping zone (err_sq < step_dist_sq) vs far zone
    let mut follower = Ball {
        position: DVec3::new(1_000.0, 2_350.0, -500.0),
        mode: MovementMode::Follow {
            target_id: 1,
            range: 500.0,
        },
        ..orbiter.clone()
    };
    let fg_damp = super::compute_subwarp_goal_velocity(&follower, &pos_map, 2.0);
    // r_total = 700, diff.y = 350 -> stop_point.y = 2700, err.y = 350, step_dist = 300 * 2 = 600, ratio = (350/600)^2
    let ratio = (350.0 / 600.0) * (350.0 / 600.0);
    assert!((fg_damp - DVec3::new(0.0, 300.0 * ratio * ratio, 0.0)).length() < 1e-9);
    follower.position = DVec3::new(1_000.0, 5_000.0, -500.0);
    let fg_far = super::compute_subwarp_goal_velocity(&follower, &pos_map, 2.0);
    assert!((fg_far - DVec3::new(0.0, -300.0, 0.0)).length() < 1e-9);

    // Direct analytical verification of step_warp_ball across Accelerating, Cruising, Decelerating
    let origin = DVec3::new(100_000.0, 0.0, 0.0);
    let dest = origin + DVec3::new(10.0 * crate::AU_METERS, 0.0, 0.0);
    let total_d = 10.0 * crate::AU_METERS;
    let warp_au = 3.0_f64;
    let k_a = warp_au;
    let k_d = k_a / 3.0;
    let v_cruise = warp_au * crate::AU_METERS;
    let eff_total = total_d + 1.0;
    let v_peak = v_cruise.min(eff_total * k_a * k_d / (k_a + k_d));
    let t_accel_end = (v_peak / k_a).ln() / k_a;
    let t_cruise_dur = eff_total / v_peak - 1.0 / k_a - 1.0 / k_d;

    let mut wb = make_ball(99, origin, DVec3::ZERO, MovementMode::Stop);
    let mut ph = crate::WarpPhase::Accelerating;
    let mut elapsed = 1.0_f64;
    assert!(!super::step_warp_ball(&mut wb, 1.0, origin, dest, warp_au, total_d, &mut ph, &mut elapsed));
    assert_eq!(ph, crate::WarpPhase::Accelerating);
    let trav_acc = (k_a * 1.0_f64).exp();
    assert!((wb.position.x - (dest.x - (total_d - trav_acc))).abs() < 1e-3);
    assert!((wb.velocity.x - k_a * trav_acc).abs() < 1e-3);
    assert!((elapsed - 2.0).abs() < 1e-9);

    elapsed = t_accel_end + 1.5;
    assert!(!super::step_warp_ball(&mut wb, 1.0, origin, dest, warp_au, total_d, &mut ph, &mut elapsed));
    assert_eq!(ph, crate::WarpPhase::Cruising);
    let trav_cr = v_peak / k_a + v_peak * 1.5;
    assert!((wb.position.x - (dest.x - (total_d - trav_cr))).abs() < 1.0);
    assert!((wb.velocity.x - v_peak).abs() < 1.0);

    elapsed = t_accel_end + t_cruise_dur + 2.0;
    assert!(!super::step_warp_ball(&mut wb, 1.0, origin, dest, warp_au, total_d, &mut ph, &mut elapsed));
    assert_eq!(ph, crate::WarpPhase::Decelerating);
    let rem_p1 = (v_peak / k_d) * (-k_d * 2.0_f64).exp();
    assert!((wb.position.x - (dest.x - (rem_p1 - 1.0))).abs() < 1.0);
    assert!((wb.velocity.x - rem_p1 * k_d).abs() < 1.0);

    // Terminal warp exit tick, missile/formation/integrate_subwarp, off-origin grid, shuttle warp & lock boundaries
    let mut t_exit = t_accel_end + t_cruise_dur + ((v_peak / (k_d * 10.0)).ln() / k_d) + 2.0;
    assert!(super::step_warp_ball(&mut wb, 1.0, origin, dest, warp_au, total_d, &mut ph, &mut t_exit));
    assert_eq!(wb.mode, MovementMode::Stop);

    let y_leader = make_ball(15, DVec3::ZERO, DVec3::new(0.0, 200.0, 0.0), MovementMode::Stop);
    let (_, r_y, u_y, f_y) = super::formation_leader_frame(&y_leader);
    assert!((f_y - DVec3::Y).length() < 1e-9);
    assert!((r_y.dot(f_y)).abs() < 1e-9 && (u_y.dot(f_y)).abs() < 1e-9);
    let fg_y = super::compute_formation_goal_velocity(&y_leader, DVec3::new(0.0, 100.0, 0.0), 2.0);
    assert!((fg_y.y - (300.0 * (100.0_f64 / 600.0).powi(2))).abs() < 1e-6);

    let mut mis = make_ball(16, DVec3::ZERO, DVec3::new(50.0, 0.0, 0.0), MovementMode::Stop);
    super::step_missile_ball(&mut mis, Some(DVec3::new(0.0, 300.0, 0.0)), 2.0);
    assert!((mis.position.y - 600.0).abs() < 1e-6);

    let mut sub_b = make_ball(17, DVec3::ZERO, DVec3::new(100.0, 0.0, 0.0), MovementMode::Stop);
    super::integrate_subwarp(&mut sub_b, DVec3::new(200.0, 0.0, 0.0), 2.0);
    let tau_s = sub_b.time_constant();
    let dec_s = (-2.0 / tau_s).exp();
    let exp_pos_x = 200.0 * 2.0 - 100.0 * tau_s * (1.0 - dec_s);
    assert!((sub_b.position.x - exp_pos_x).abs() < 1e-6);

    // Off-origin grid exit, shuttle bubble immunity while aligning, cloaked target lock break & exact max_range_m
    let mut sp = Ballpark::new(30_000_142);
    sp.add_ball(make_ball(1, DVec3::new(20_000_000.0, 0.0, 0.0), DVec3::new(10.0, 0.0, 0.0), MovementMode::GotoDirection { direction: DVec3::X }));
    sp.add_ball(make_ball(2, DVec3::new(20_005_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
    sp.add_ball(make_ball(3, DVec3::new(20_010_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));
    assert!(sp.add_target_lock(
        1,
        crate::TargetLock {
            target_id: 2,
            lock_time_ms: 1.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(10_000.0),
        }
    ));
    assert!(sp.add_target_lock(
        1,
        crate::TargetLock {
            target_id: 3,
            lock_time_ms: 1.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: Some(10_000.0),
        }
    ));
    sp.step(1.0);
    sp.balls.get_mut(&1).unwrap().position.x = 20_000_000.0;
    sp.combat_states.get_mut(&3).unwrap().is_cloaked = true;
    let lock_evs = sp.step(1.0);
    assert!(lock_evs.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, target_id: 3, .. } if mode == "lost")));
    assert!(!lock_evs.iter().any(|e| matches!(e, DestinyEvent::OnTarget { mode, target_id: 2, .. } if mode == "lost")));
    let old_gid = sp.ball_grid_ids[&1];
    sp.balls.get_mut(&1).unwrap().position.x = 20_600_000.0;
    let stay_evs = sp.step(1.0);
    assert!(!stay_evs.iter().any(|e| matches!(e, DestinyEvent::RemoveBalls { .. })));
    assert!(sp.grids[&old_gid].max_bounds.x >= 20_600_000.0);
    sp.balls.get_mut(&1).unwrap().position.x = 30_200_000.0;
    sp.step(1.0);
    assert_ne!(sp.ball_grid_ids[&1], old_gid);
    assert!(sp.observer_events[&2].iter().any(|e| matches!(e, DestinyEvent::RemoveBalls { .. })));
}

#[test]
fn proximity_decloak_emits_cloak_ball_zero_and_special_fx_cloak_stop() {
    let mut sp = Ballpark::new(30_000_140);
    sp.add_ball(make_ball(1, DVec3::ZERO, DVec3::ZERO, MovementMode::Stop));
    sp.add_ball(make_ball(2, DVec3::new(1_000.0, 0.0, 0.0), DVec3::ZERO, MovementMode::Stop));

    {
        let cs = sp.combat_states.entry(1).or_default();
        cs.is_cloaked = true;
        cs.gate_cloak_remaining_ticks = 0;
    }

    let events = sp.step(1.0);
    assert!(!sp.combat_states[&1].is_cloaked);
    assert!(
        events.iter().any(|e| matches!(e, DestinyEvent::CloakBall { ball_id: 1, cloaked: 0 })),
        "proximity decloak must emit CloakBall(0), got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            DestinyEvent::OnSpecialFX { ball_id: 1, guid, is_active: false, .. } if guid == "effects.Cloak"
        )),
        "proximity decloak must emit OnSpecialFX(effects.Cloak, is_active=false), got {events:?}"
    );
}

#[test]
fn is_warp_finishing_within_1s_matches_deceleration_dropout_equation() {
    let origin = DVec3::ZERO;
    let dest = DVec3::new(50_000_000.0, 0.0, 0.0);

    // Non-warp modes always return false
    let stopped = make_ball(1, dest, DVec3::ZERO, MovementMode::Stop);
    assert!(!is_warp_finishing_within_1s(&stopped));
    let goto = make_ball(
        2,
        dest,
        DVec3::new(100.0, 0.0, 0.0),
        MovementMode::GotoDirection { direction: DVec3::X },
    );
    assert!(!is_warp_finishing_within_1s(&goto));

    // Non-Decelerating warp phases always return false even when positioned right at destination
    for non_decel_phase in [
        WarpPhase::Aligning,
        WarpPhase::Accelerating,
        WarpPhase::Cruising,
    ] {
        let b = make_ball(
            3,
            dest,
            DVec3::new(250.0, 0.0, 0.0),
            MovementMode::Warp {
                origin,
                destination: dest,
                warp_speed_au_s: 15.0,
                total_distance: dest.length(),
                phase: non_decel_phase,
                elapsed_warp_time: 1.0,
            },
        );
        assert!(!is_warp_finishing_within_1s(&b));
    }

    // Decelerating at 15.0 AU/s (k_d = 5.0), max_velocity = 300.0 (stop_thresh = 100.0):
    // 1.0s dropout limit is rem_plus_1 < 100.0 / (5.0 * e^{-5.0}) ~= 2,968.26 m.
    let mut decel_far = make_ball(
        4,
        dest - DVec3::new(5_000.0, 0.0, 0.0),
        DVec3::new(25_000.0, 0.0, 0.0),
        MovementMode::Warp {
            origin,
            destination: dest,
            warp_speed_au_s: 15.0,
            total_distance: dest.length(),
            phase: WarpPhase::Decelerating,
            elapsed_warp_time: 5.0,
        },
    );
    assert!(!is_warp_finishing_within_1s(&decel_far));

    decel_far.position = dest - DVec3::new(2_800.0, 0.0, 0.0);
    assert!(is_warp_finishing_within_1s(&decel_far));

    // Real 1 Hz Ballpark trajectory: is_warp_finishing_within_1s turns true strictly on the tick before Stop
    let mut park = Ballpark::new(30_000_142);
    let warper = make_ball(
        10,
        origin,
        DVec3::new(250.0, 0.0, 0.0),
        MovementMode::Warp {
            origin,
            destination: dest,
            warp_speed_au_s: 15.0,
            total_distance: dest.length(),
            phase: WarpPhase::Aligning,
            elapsed_warp_time: 0.0,
        },
    );
    park.add_ball(warper);

    let mut saw_gt_1s = false;
    let mut saw_le_1s = false;
    for _ in 0..40 {
        if matches!(park.balls[&10].mode, MovementMode::Stop) {
            break;
        }
        let finishing = is_warp_finishing_within_1s(&park.balls[&10]);
        if finishing {
            saw_le_1s = true;
        } else {
            saw_gt_1s = true;
        }
        park.step(1.0);
        assert_eq!(
            finishing,
            matches!(park.balls[&10].mode, MovementMode::Stop),
            "is_warp_finishing_within_1s must match whether the next 1.0s tick exits Warp"
        );
    }
    assert!(saw_gt_1s && saw_le_1s);
}
