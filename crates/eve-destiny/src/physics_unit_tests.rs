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
