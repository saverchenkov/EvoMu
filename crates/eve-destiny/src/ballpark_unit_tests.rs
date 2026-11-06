//! Targeted unit and mutation-killing tests for `ballpark.rs`.

use super::*;
use crate::TargetLock;

fn make_ship(id: BallId, pos: DVec3) -> Ball {
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
        mode: MovementMode::Stop,
    }
}

#[test]
fn static_balls_slim_meta_wormhole_stages_and_grid_clamping() {
    let mut park = Ballpark::with_bubble_radius(30_000_142, 0.0);
    assert_eq!(park.bubble_radius_meters, 1.0);
    park.set_bubble_radius(250_000.0);
    assert_eq!(park.bubble_radius_meters, 250_000.0);
    assert!(!park.has_dynamic_balls());

    // Static station (group 15 -> category 3) and static field bubble
    let mut station = make_ship(60_003_760, DVec3::ZERO);
    station.mode = MovementMode::Rigid;
    assert!(park.add_static_ball(station.clone(), 15));
    assert!(!park.add_static_ball(station, 15));
    assert_eq!(park.ball_slim_meta(60_003_760).unwrap().category_id, 3);
    assert!(park.is_static_ball(60_003_760));
    assert!(park.is_immobile_structure_or_static(60_003_760));

    let mut static_field = make_ship(70_001, DVec3::new(5_000.0, 0.0, 0.0));
    static_field.mode = MovementMode::Field;
    static_field.radius = 10_000.0;
    assert!(park.add_static_ball(static_field, 12));
    assert!(park.warp_bubble_mgr.bubbles.contains_key(&70_001));

    // Dynamic ship & slim item / wormhole stage change events
    assert!(park.add_ball(make_ship(1, DVec3::ZERO)));
    assert!(park.has_dynamic_balls());
    park.set_ball_group_id(1, 25);
    assert_eq!(park.ball_group_id(1), Some(25));
    let meta = park.ball_slim_meta(1).unwrap().clone();
    assert!(!park.change_slim_item(1, meta.clone()));
    assert!(!park.change_slim_item(999, meta.clone()));
    let mut new_meta = meta;
    new_meta.security_status = 2.5;
    assert!(park.change_slim_item(1, new_meta));
    assert!(park.set_ball_wormhole_stages(1, 2, 1.0));
    assert!(!park.set_ball_wormhole_stages(1, 2, 1.0));

    // queue_set_state places SetState at index 0 and deduplicates
    assert!(!park.queue_set_state(999));
    assert!(park.queue_set_state(1));
    assert!(park.queue_set_state(1));
    let evs = park.drain_observer_events(1);
    assert!(matches!(evs.first(), Some(DestinyEvent::SetState { observer_ball_id: 1 })));
    assert_eq!(evs.iter().filter(|e| matches!(e, DestinyEvent::SetState { .. })).count(), 1);

    // Adjacent grid creation clamps boundaries without overlapping
    let g1 = park.bubble_id_for_ball(1).unwrap();
    let g2 = park.find_or_create_grid_for_position(DVec3::new(300_000.0, 0.0, 0.0));
    assert_ne!(g1, g2);
    assert!(!park.grids[&g1].overlaps_bounds(park.grids[&g2].min_bounds, park.grids[&g2].max_bounds));
}

#[test]
fn damage_application_direct_structure_damage_and_interdiction_deployables() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ship(1, DVec3::ZERO));
    park.add_ball(make_ship(2, DVec3::new(1_000.0, 0.0, 0.0)));

    // Invulnerable ball takes 0 direct structure damage
    park.combat_states.get_mut(&1).unwrap().is_tethered = true;
    let (applied, destroyed, _) = park.apply_direct_structure_damage_to_ball(1, 500.0).unwrap();
    assert_eq!(applied, 0.0);
    assert!(!destroyed);
    park.combat_states.get_mut(&1).unwrap().is_tethered = false;

    // Direct structure damage bypasses shield & armor and destroys when hull reaches 0
    let (applied2, destroyed2, _) = park.apply_direct_structure_damage_to_ball(1, 50.0).unwrap();
    assert_eq!(applied2, 50.0);
    assert!(!destroyed2);
    assert_eq!(park.combat_states[&1].shield_hp, park.combat_states[&1].max_shield_hp);
    let (_, destroyed3, _) = park.apply_direct_structure_damage_to_ball(1, 10_000.0).unwrap();
    assert!(destroyed3);
    assert!(!park.balls.contains_key(&1));

    // Standard apply_damage_to_ball on ship 2
    let (_, killed_2, _) = park.apply_damage_to_ball(2, DamageVector::new(5_000.0, 5_000.0, 5_000.0, 5_000.0)).unwrap();
    assert!(killed_2);
    assert!(!park.balls.contains_key(&2));

    // Interdiction sphere & mobile warp disruptor deployment and update
    assert!(park.add_interdiction_sphere(501, DVec3::new(10_000.0, 0.0, 0.0), 20_000.0));
    assert!(park.add_interdiction_sphere(501, DVec3::new(12_000.0, 0.0, 0.0), 20_000.0));
    assert!(park.add_mobile_warp_disruptor(502, DVec3::new(-10_000.0, 0.0, 0.0), 15_000.0));
    assert!(park.add_mobile_warp_disruptor(502, DVec3::new(-12_000.0, 0.0, 0.0), 15_000.0));
    assert!(park.remove_warp_bubble(501).is_some());
}

#[test]
fn micro_jump_capital_scaling_and_micro_jump_field_boosh_rules() {
    let mut park = Ballpark::new(30_000_142);
    let mut jumper = make_ship(1, DVec3::ZERO);
    jumper.velocity = DVec3::new(0.0, 100.0, 0.0);
    park.add_ball(jumper);

    // Scramble blocks MJD and MJFG
    park.combat_states.get_mut(&1).unwrap().is_mwd_mjd_disabled = true;
    assert!(park.micro_jump(1, 100_000.0).is_err());
    assert!(park.micro_jump_field(1, 100_000.0, 6_000.0, 25).is_err());
    park.combat_states.get_mut(&1).unwrap().is_mwd_mjd_disabled = false;

    // Subcap MJD jumps 100 km along +Y velocity
    let pos1 = park.micro_jump(1, 100_000.0).unwrap();
    assert!((pos1 - DVec3::new(0.0, 100_000.0, 0.0)).length() < 1e-6);

    // Capital ship (group 547 Carrier) MJD upgrades 100 km request to 250 km
    park.set_ball_group_id(1, 547);
    let pos2 = park.micro_jump(1, 100_000.0).unwrap();
    assert!((pos2 - DVec3::new(0.0, 350_000.0, 0.0)).length() < 1e-6);
    park.set_ball_group_id(1, 25);
    park.set_ball_position(1, DVec3::ZERO);

    // MJFG booshes nearby subcaps AND bastioned Marauder (group 900 with disallow_assistance),
    // but skips scrambled regular ships, cloaked ships, and capitals when max_jumped_capitals == 0
    park.add_ball(make_ship(2, DVec3::new(2_000.0, 0.0, 0.0))); // valid subcap
    let mut marauder = make_ship(3, DVec3::new(3_000.0, 0.0, 0.0));
    marauder.type_id = 28659; // Paladin
    park.add_ball(marauder);
    park.set_ball_group_id(3, 900);
    {
        let cs3 = park.combat_states.get_mut(&3).unwrap();
        cs3.is_mwd_mjd_disabled = true;
        cs3.disallow_assistance = true; // Bastion active
    }
    park.add_ball(make_ship(4, DVec3::new(4_000.0, 0.0, 0.0))); // scrambled subcap -> skipped
    park.combat_states.get_mut(&4).unwrap().is_mwd_mjd_disabled = true;
    park.add_ball(make_ship(5, DVec3::new(5_000.0, 0.0, 0.0))); // capital -> skipped unless capitals > 0
    park.set_ball_group_id(5, 485); // Dreadnought

    let jumped = park.micro_jump_field_with_capitals(1, 100_000.0, 6_000.0, 25, 0).unwrap();
    assert_eq!(jumped, vec![1, 2, 3]);
    assert!((park.balls[&3].position - DVec3::new(3_000.0, 100_000.0, 0.0)).length() < 1e-6);
    assert_eq!(park.balls[&4].position, DVec3::new(4_000.0, 0.0, 0.0));
    assert_eq!(park.balls[&5].position, DVec3::new(5_000.0, 0.0, 0.0));
}

#[test]
fn cloak_activation_deactivation_navigation_and_cross_grid_teleport() {
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ship(1, DVec3::ZERO));
    park.add_ball(make_ship(2, DVec3::new(1_500.0, 0.0, 0.0)));

    // Within 2,000m surface-to-surface cannot cloak
    assert!(park.activate_cloak(1, false, 10.0, 900.0).is_err());
    park.set_ball_position(2, DVec3::new(10_000.0, 0.0, 0.0));

    // Targeted ship cannot cloak
    park.add_target_lock(2, TargetLock {
        target_id: 1,
        lock_time_ms: 0.0,
        remaining_lock_ms: 0.0,
        locked: true,
        max_range_m: Some(50_000.0),
    });
    assert!(park.activate_cloak(1, false, 10.0, 900.0).is_err());
    park.clear_target_locks(2);

    // Valid cloak activation -> non-covert cloak blocks warp -> deactivate starts targeting delay
    park.activate_cloak(1, false, 12.0, 900.0).unwrap();
    assert!(park.set_mode(1, MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1.0e8, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1.0e8,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    }).is_err());
    assert!(park.deactivate_cloak(1));
    assert_eq!(park.combat_states[&1].decloak_targeting_delay_remaining_s, 12.0);

    // goto_point, set_ball_radius, set_speed_fraction, and cross-grid set_ball_position
    assert!(park.goto_point(1, DVec3::new(f64::NAN, 0.0, 0.0)).is_err());
    park.goto_point(1, DVec3::new(5_000.0, 0.0, 0.0)).unwrap();
    assert!(!park.set_ball_radius(1, -5.0));
    assert!(park.set_ball_radius(1, 75.0));
    assert!(park.set_speed_fraction(1, 0.8));

    park.add_ball(make_ship(3, DVec3::new(50_000_000.0, 0.0, 0.0)));
    park.drain_all_observer_events();
    assert!(park.set_ball_position(1, DVec3::new(50_000_500.0, 0.0, 0.0)));
    assert!(park.in_same_bubble(1, 3));
    assert!(!park.in_same_bubble(1, 2));
}

#[test]
fn test_issue_419_grid_boundary_no_dead_zone_and_no_1hz_thrashing() {
    let r = 100_000.0;
    let mut park = Ballpark::with_bubble_radius(30_000_142, r);
    park.add_ball(make_ship(10, DVec3::ZERO));
    let gid1 = park.bubble_id_for_ball(10).unwrap();

    // Crossing the +x grid wall by < 1.0m (0.3m) must create a contiguous grid containing pos
    let just_outside = DVec3::new(r + 0.3, 0.0, 0.0);
    park.add_ball(make_ship(11, just_outside));
    let gid2 = park.bubble_id_for_ball(11).unwrap();
    assert_ne!(gid1, gid2);
    assert!(
        park.grids[&gid2].contains(just_outside),
        "New grid must strictly contain pos when crossing grid wall by < 1.0m"
    );
    assert!(!park.grids[&gid1].overlaps_bounds(
        park.grids[&gid2].min_bounds,
        park.grids[&gid2].max_bounds
    ));
    let _ = park.drain_all_observer_events();

    // 10 consecutive 1Hz physics steps must not thrash grids or spam AddBalls2/RemoveBalls
    for _ in 0..10 {
        park.step(1.0);
        assert_eq!(park.bubble_id_for_ball(11), Some(gid2));
        let evs = park.drain_observer_events(11);
        assert!(
            !evs.iter().any(|e| matches!(
                e,
                DestinyEvent::AddBalls2 { .. } | DestinyEvent::RemoveBalls { .. }
            )),
            "Sub-meter grid boundary position must not trigger 1Hz AddBalls2/RemoveBalls thrashing: {evs:?}"
        );
    }
}

#[test]
fn test_issue_420_destruction_preserves_victim_damage_and_terminal_explosion() {
    use crate::CombatState;
    let mut park = Ballpark::new(30_000_142);
    let victim_id = 100_000_077;
    park.add_ball(make_ship(victim_id, DVec3::new(1_000.0, 0.0, 0.0)));
    park.combat_states.insert(
        victim_id,
        CombatState {
            shield_hp: 100.0,
            max_shield_hp: 100.0,
            armor_hp: 100.0,
            max_armor_hp: 100.0,
            structure_hp: 100.0,
            max_structure_hp: 100.0,
            ..Default::default()
        },
    );
    let _ = park.drain_all_observer_events();

    let (_, destroyed, _) = park
        .apply_damage_to_ball(
            victim_id,
            DamageVector {
                em: 1_000.0,
                thermal: 0.0,
                kinetic: 0.0,
                explosive: 0.0,
            },
        )
        .unwrap();
    assert!(destroyed);
    assert!(!park.balls.contains_key(&victim_id));

    let victim_events = park.drain_observer_events(victim_id);
    assert!(
        victim_events
            .iter()
            .any(|e| matches!(e, DestinyEvent::OnDamageStateChange { ball_id, .. } if *ball_id == victim_id)),
        "Victim must still receive OnDamageStateChange after remove_ball: {victim_events:?}"
    );
    assert!(
        victim_events
            .iter()
            .any(|e| matches!(e, DestinyEvent::TerminalExplosion { ball_id, .. } if *ball_id == victim_id)),
        "Victim must still receive TerminalExplosion after remove_ball: {victim_events:?}"
    );

    // Also verify Breacher DoT lethal destruction preserves victim observer events
    let dot_victim = 100_000_088;
    park.add_ball(make_ship(dot_victim, DVec3::new(2_000.0, 0.0, 0.0)));
    park.combat_states.insert(
        dot_victim,
        CombatState {
            shield_hp: 0.0,
            max_shield_hp: 100.0,
            armor_hp: 0.0,
            max_armor_hp: 100.0,
            structure_hp: 10.0,
            max_structure_hp: 100.0,
            ..Default::default()
        },
    );
    park.apply_breacher_dot(dot_victim, 50.0, 0.5, 2);
    let _ = park.drain_all_observer_events();
    park.step(1.0);
    let dot_events = park.drain_observer_events(dot_victim);
    assert!(dot_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnDamageStateChange { ball_id, .. } if *ball_id == dot_victim)));
    assert!(dot_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::TerminalExplosion { ball_id, .. } if *ball_id == dot_victim)));
}
