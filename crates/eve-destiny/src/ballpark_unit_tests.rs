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

#[test]
fn test_issue_421_breaking_gate_cloak_restores_velocity_and_emits_ego_and_observer_events() {
    let mut park = Ballpark::new(30_000_142);
    let observer_id = 100_000_010;
    let ship_a = 100_000_011;
    let ship_b = 100_000_012;
    let target_id = 100_000_013;

    park.add_ball(make_ship(observer_id, DVec3::new(0.0, 0.0, 0.0)));
    park.add_ball(make_ship(target_id, DVec3::new(5_000.0, 0.0, 0.0)));

    for ship_id in [ship_a, ship_b] {
        let mut ship = make_ship(ship_id, DVec3::new(1_000.0, 0.0, 0.0));
        ship.max_velocity = 35.0; // slowed while cloaked
        park.add_ball(ship);
        park.ball_slim_meta.insert(
            ship_id,
            SlimItemMeta {
                is_cloaked: true,
                ..Default::default()
            },
        );
        park.combat_states.insert(
            ship_id,
            crate::CombatState {
                gate_cloak_remaining_ticks: 30,
                is_cloaked: true,
                base_max_velocity: Some(350.0),
                cloak_velocity_multiplier: Some(0.0),
                ..Default::default()
            },
        );
    }
    let _ = park.drain_all_observer_events();

    // 1. Breaking gate cloak via set_speed_fraction restores max_velocity, clears slim cloak,
    // emits CloakBall(0) + OnSpecialFX("effects.Cloak", false) to ego, and reveals to observers.
    assert!(park.set_speed_fraction(ship_a, 1.0));
    assert_eq!(park.balls[&ship_a].max_velocity, 350.0);
    assert!(!park.ball_slim_meta[&ship_a].is_cloaked);
    let ego_a = park.drain_observer_events(ship_a);
    assert!(ego_a.iter().any(|e| matches!(
        e,
        DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == ship_a
    )));
    assert!(ego_a.iter().any(|e| matches!(
        e,
        DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
            if *ball_id == ship_a && guid == "effects.Cloak"
    )));
    let obs_events = park.drain_observer_events(observer_id);
    assert!(obs_events.iter().any(|e| matches!(
        e,
        DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == ship_a)
    )));

    // 2. Breaking gate cloak via add_target_lock restores max_velocity, clears slim cloak,
    // and emits ego CloakBall(0) + OnSpecialFX("effects.Cloak", false).
    assert!(park.add_target_lock(
        ship_b,
        TargetLock {
            target_id,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 1_000.0,
            locked: false,
            max_range_m: None,
        }
    ));
    assert_eq!(park.balls[&ship_b].max_velocity, 350.0);
    assert!(!park.ball_slim_meta[&ship_b].is_cloaked);
    let ego_b = park.drain_observer_events(ship_b);
    assert!(ego_b.iter().any(|e| matches!(
        e,
        DestinyEvent::CloakBall { ball_id, cloaked: 0 } if *ball_id == ship_b
    )));
    assert!(ego_b.iter().any(|e| matches!(
        e,
        DestinyEvent::OnSpecialFX { ball_id, guid, is_active: false, .. }
            if *ball_id == ship_b && guid == "effects.Cloak"
    )));
}

#[test]
fn coverage_and_mutants_ballpark_grids_damage_cloak_and_navigation() {
    // 1. Six-face grid clamping, boundary expansion, overlap rejection, and contraction
    let mut park = Ballpark::with_bubble_radius(30_000_142, 100_000.0);
    let g1 = park.find_or_create_grid_for_position(DVec3::ZERO);
    assert_eq!(park.grids[&g1].min_bounds, DVec3::splat(-100_000.0));
    assert_eq!(park.grids[&g1].max_bounds, DVec3::splat(100_000.0));

    let face_probes = [
        (DVec3::new(150_000.0, 0.0, 0.0), DVec3::new(100_000.0, 0.0, 0.0), true),
        (DVec3::new(-150_000.0, 0.0, 0.0), DVec3::new(-100_000.0, 0.0, 0.0), false),
        (DVec3::new(0.0, 150_000.0, 0.0), DVec3::new(0.0, 100_000.0, 0.0), true),
        (DVec3::new(0.0, -150_000.0, 0.0), DVec3::new(0.0, -100_000.0, 0.0), false),
        (DVec3::new(0.0, 0.0, 150_000.0), DVec3::new(0.0, 0.0, 100_000.0), true),
        (DVec3::new(0.0, 0.0, -150_000.0), DVec3::new(0.0, 0.0, -100_000.0), false),
    ];
    for (pos, expected_face, is_min) in face_probes {
        let gid = park.find_or_create_grid_for_position(pos);
        let gb = &park.grids[&gid];
        assert!(!gb.overlaps_bounds(DVec3::splat(-99_999.0), DVec3::splat(99_999.0)));
        if is_min {
            if expected_face.x > 0.0 {
                assert_eq!(gb.min_bounds.x, 100_000.0);
            }
            if expected_face.y > 0.0 {
                assert_eq!(gb.min_bounds.y, 100_000.0);
            }
            if expected_face.z > 0.0 {
                assert_eq!(gb.min_bounds.z, 100_000.0);
            }
        } else {
            if expected_face.x < 0.0 {
                assert_eq!(gb.max_bounds.x, -100_000.0);
            }
            if expected_face.y < 0.0 {
                assert_eq!(gb.max_bounds.y, -100_000.0);
            }
            if expected_face.z < 0.0 {
                assert_eq!(gb.max_bounds.z, -100_000.0);
            }
        }
        park.contract_or_delete_grid(gid);
    }

    // Isolated grid expansion and contraction with live member
    let mut iso = Ballpark::with_bubble_radius(30_000_142, 100_000.0);
    let ship_m = make_ship(50, DVec3::new(90_000.0, 0.0, 0.0));
    iso.add_ball(ship_m);
    let gid_iso = iso.ball_grid_ids[&50];
    assert!(iso.try_expand_grid(gid_iso, DVec3::new(250_000.0, 0.0, 0.0)));
    assert_eq!(
        iso.grids[&gid_iso].max_bounds.x,
        250_000.0 + crate::GRID_WALL_EXPANSION_THRESHOLD_METERS
    );
    let iso_center = iso.grids[&gid_iso].center;
    assert!(!iso.try_expand_grid(
        gid_iso,
        iso_center + DVec3::new(crate::BUBBLE_TRANSITION_THRESHOLD_METERS + 10.0, 0.0, 0.0)
    ));
    assert!(!iso.try_expand_grid(
        gid_iso,
        iso_center + DVec3::new(0.0, crate::BUBBLE_TRANSITION_THRESHOLD_METERS + 10.0, 0.0)
    ));
    assert!(!iso.try_expand_grid(
        gid_iso,
        iso_center + DVec3::new(0.0, 0.0, crate::BUBBLE_TRANSITION_THRESHOLD_METERS + 10.0)
    ));
    iso.balls.get_mut(&50).unwrap().position = DVec3::new(150_000.0, 0.0, 0.0);
    iso.contract_or_delete_grid(gid_iso);
    assert_eq!(
        iso.grids[&gid_iso].max_bounds.x,
        150_000.0 + crate::GRID_WALL_EXPANSION_THRESHOLD_METERS
    );

    // Adjacent grid blocks overlapping expansion
    let neighbor_id = iso.find_or_create_grid_for_position(DVec3::new(
        -(200_000.0 + crate::GRID_WALL_EXPANSION_THRESHOLD_METERS),
        0.0,
        0.0,
    ));
    assert!(neighbor_id != gid_iso);
    assert!(!iso.try_expand_grid(gid_iso, DVec3::new(-180_000.0, 0.0, 0.0)));

    // 2. Non-unit damage & direct hull ratios, static metadata, pre-cloaked add_ball, cross-grid move
    let mut st = make_ship(900_001, DVec3::ZERO);
    st.mode = MovementMode::Rigid;
    assert!(park.add_static_ball(st, 15));
    assert_eq!(park.ball_slim_meta[&900_001].group_id, 15);
    assert!(park.goto_point(900_001, DVec3::new(100.0, 0.0, 0.0)).is_err());
    assert!(park.set_mode(900_001, MovementMode::Stop).is_err());

    let pre_cloaked = make_ship(51, DVec3::new(1_000.0, 0.0, 0.0));
    park.combat_states.insert(
        51,
        crate::CombatState {
            is_cloaked: true,
            ..Default::default()
        },
    );
    let _ = park.drain_all_observer_events();
    assert!(park.add_ball(pre_cloaked.clone()));
    assert!(!park.drain_observer_events(50).iter().any(|e| matches!(e, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == 51))));

    // Partial non-unit damage ratios on apply_damage_to_ball and apply_direct_structure_damage_to_ball
    let mut dmg_park = Ballpark::new(30_000_142);
    let mut v1 = make_ship(101, DVec3::ZERO);
    v1.max_velocity = 40.0;
    dmg_park.add_ball(v1);
    dmg_park.combat_states.insert(101, crate::CombatState { shield_hp: 200.0, max_shield_hp: 200.0, armor_hp: 400.0, max_armor_hp: 400.0, structure_hp: 500.0, max_structure_hp: 500.0, base_max_velocity: Some(320.0), cloak_velocity_multiplier: Some(0.125), is_cloaked: true, ..Default::default() });
    dmg_park.ball_slim_meta.insert(101, SlimItemMeta { is_cloaked: true, ..Default::default() });
    assert!(dmg_park.deactivate_cloak(101));
    assert_eq!(dmg_park.balls[&101].max_velocity, 320.0);
    assert!(!dmg_park.ball_slim_meta[&101].is_cloaked);

    let _ = dmg_park.drain_all_observer_events();
    let (ap1, dest1, _) = dmg_park.apply_damage_to_ball(101, crate::DamageVector::new(150.0, 0.0, 0.0, 0.0)).unwrap();
    assert_eq!(ap1, 150.0);
    assert!(!dest1);
    assert!(dmg_park.drain_observer_events(101).iter().any(|e| matches!(
        e, DestinyEvent::OnDamageStateChange { shield_ratio, armor_ratio, structure_ratio, .. }
        if (*shield_ratio - 0.25).abs() < 1e-6 && (*armor_ratio - 1.0).abs() < 1e-6 && (*structure_ratio - 1.0).abs() < 1e-6
    )));
    dmg_park.apply_damage_to_ball(101, crate::DamageVector::new(200.0, 0.0, 0.0, 0.0)).unwrap();
    assert!(dmg_park.drain_observer_events(101).iter().any(|e| matches!(
        e, DestinyEvent::OnDamageStateChange { shield_ratio, armor_ratio, structure_ratio, .. }
        if *shield_ratio == 0.0 && (*armor_ratio - 0.625).abs() < 1e-6 && (*structure_ratio - 1.0).abs() < 1e-6
    )));

    dmg_park.add_ball(make_ship(102, DVec3::new(500.0, 0.0, 0.0)));
    dmg_park.combat_states.insert(102, crate::CombatState { shield_hp: 100.0, max_shield_hp: 200.0, armor_hp: 200.0, max_armor_hp: 400.0, structure_hp: 500.0, max_structure_hp: 500.0, ..Default::default() });
    let _ = dmg_park.drain_all_observer_events();
    let (ap2, dest2, _) = dmg_park.apply_direct_structure_damage_to_ball(102, 100.0).unwrap();
    assert_eq!(ap2, 100.0);
    assert!(!dest2);
    assert!(dmg_park.drain_observer_events(102).iter().any(|e| matches!(
        e, DestinyEvent::OnDamageStateChange { shield_ratio, armor_ratio, structure_ratio, .. }
        if (*shield_ratio - 0.5).abs() < 1e-6 && (*armor_ratio - 0.5).abs() < 1e-6 && (*structure_ratio - 0.8).abs() < 1e-6
    )));

    // Non-covert cloak blocks warp, Follow mode emits Follow event, Stop breaks gate cloak
    let mut nav = Ballpark::new(30_000_142);
    nav.add_ball(make_ship(201, DVec3::ZERO));
    nav.add_ball(make_ship(202, DVec3::new(1_000.0, 0.0, 0.0)));
    nav.combat_states.get_mut(&201).unwrap().is_cloaked = true;
    assert!(nav.set_mode(201, MovementMode::new_warp(DVec3::ZERO, DVec3::new(2_000_000.0, 0.0, 0.0), 3.0)).is_err());
    nav.combat_states.get_mut(&201).unwrap().is_cloaked = false;
    nav.combat_states.get_mut(&201).unwrap().gate_cloak_remaining_ticks = 10;
    assert!(nav.set_speed_fraction(201, 0.5));
    assert_eq!(nav.combat_states[&201].gate_cloak_remaining_ticks, 0);

    nav.combat_states.get_mut(&202).unwrap().gate_cloak_remaining_ticks = 10;
    nav.combat_states.get_mut(&202).unwrap().is_cloaked = true;
    assert!(nav.set_mode(202, MovementMode::Follow { target_id: 201, range: 1_500.0 }).is_ok());
    assert!(!nav.combat_states[&202].is_cloaked);
    assert!(nav.pending_events.iter().any(|e| matches!(e, DestinyEvent::Follow { ball_id: 202, target_id: 201, .. })));

    let _ = nav.drain_all_observer_events();
    nav.add_ball(make_ship(203, DVec3::new(50_000_000.0, 0.0, 0.0)));
    assert!(nav.set_ball_position(201, DVec3::new(50_001_000.0, 0.0, 0.0)));
    assert!(nav.drain_observer_events(202).iter().any(|e| matches!(e, DestinyEvent::RemoveBalls { ball_ids } if ball_ids == &vec![201])));
    assert!(nav.drain_observer_events(201).iter().any(|e| matches!(e, DestinyEvent::AddBalls { balls } if balls.iter().all(|b| b.id != 201))));

    nav.ball_group_ids.insert(202, 547);
    nav.balls.get_mut(&202).unwrap().velocity = DVec3::ZERO;
    nav.balls.get_mut(&202).unwrap().mode = MovementMode::GotoDirection { direction: DVec3::new(0.0, 2.0, 0.0) };
    let start_202_y = nav.balls[&202].position.y;
    let jumped_202 = nav.micro_jump(202, 100_001.0).unwrap();
    assert!((jumped_202.y - (start_202_y + 100_001.0)).abs() < 1e-6);

    nav.combat_states.get_mut(&203).unwrap().gate_cloak_remaining_ticks = 5;
    nav.combat_states.get_mut(&203).unwrap().is_cloaked = true;
    nav.combat_states.get_mut(&203).unwrap().cloak_configured_targeting_delay_s = 7.0;
    assert!(nav.deactivate_cloak(203));
    assert_eq!(nav.combat_states[&203].decloak_targeting_delay_remaining_s, 0.0);
}

#[test]
fn ball_interactive_overrides_wire_and_recompute_emits_set_max_speed() {
    let mut park = Ballpark::new(30_000_142);
    let mut ship = make_ship(501, DVec3::ZERO);
    ship.max_velocity = 400.0;
    park.add_ball(ship.clone());
    park.add_ball(make_ship(502, DVec3::new(1_000.0, 0.0, 0.0)));
    let mut station = make_ship(60_003_760, DVec3::new(2_000.0, 0.0, 0.0));
    station.mode = MovementMode::Rigid;
    station.max_velocity = 10.0;
    assert!(park.add_static_ball(station, 15));

    // 1. SetBallInteractive records override, updates AddBalls2/SetState BallHeader flags (offset 37),
    //    and remove_ball cleans up the override entry.
    let mut buf_default = Vec::new();
    crate::wire::encode_single_ball_with_park(&mut buf_default, &park.balls[&501], 1, None, Some(&park));
    assert_ne!(buf_default[37] & crate::wire::BALL_FLAG_IS_INTERACTIVE, 0);

    assert!(park.set_ball_interactive(501, false));
    assert_eq!(park.ball_interactive_overrides.get(&501), Some(&false));
    let mut buf_non_inter = Vec::new();
    crate::wire::encode_single_ball_with_park(&mut buf_non_inter, &park.balls[&501], 1, None, Some(&park));
    assert_eq!(buf_non_inter[37] & crate::wire::BALL_FLAG_IS_INTERACTIVE, 0);

    assert!(park.set_ball_interactive(501, true));
    assert_eq!(park.ball_interactive_overrides.get(&501), Some(&true));
    let mut buf_re_inter = Vec::new();
    crate::wire::encode_single_ball_with_park(&mut buf_re_inter, &park.balls[&501], 1, None, Some(&park));
    assert_ne!(buf_re_inter[37] & crate::wire::BALL_FLAG_IS_INTERACTIVE, 0);

    // 2. recompute_ball_attributes emits SetMaxSpeed when dynamic max_velocity changes, not on static balls.
    let _ = park.drain_all_observer_events();
    if let Some(cs) = park.combat_states.get_mut(&501) {
        cs.base_max_velocity = Some(400.0);
        cs.prop_velocity_multiplier = 2.5;
    }
    park.recompute_ball_attributes(501);
    assert!((park.balls[&501].max_velocity - 1_000.0).abs() < 1e-6);
    assert!(park.drain_observer_events(502).iter().any(|e| matches!(
        e,
        DestinyEvent::SetMaxSpeed { ball_id: 501, max_speed } if (*max_speed - 1_000.0).abs() < 1e-6
    )));

    // Static ball attribute recompute never emits SetMaxSpeed.
    if let Some(cs) = park.combat_states.get_mut(&60_003_760) {
        cs.base_max_velocity = Some(10.0);
        cs.prop_velocity_multiplier = 2.0;
    }
    park.recompute_ball_attributes(60_003_760);
    assert!(!park.drain_observer_events(502).iter().any(|e| matches!(
        e,
        DestinyEvent::SetMaxSpeed { ball_id: 60_003_760, .. }
    )));

    // 3. deactivate_cloak emits SetMaxSpeed after AddBalls2 when cloak velocity penalty clears.
    if let Some(cs) = park.combat_states.get_mut(&501) {
        cs.is_cloaked = true;
        cs.cloak_velocity_multiplier = Some(0.25);
    }
    park.recompute_ball_attributes(501);
    assert!((park.balls[&501].max_velocity - 100.0).abs() < 1e-6);
    let _ = park.drain_all_observer_events();
    assert!(park.deactivate_cloak(501));
    assert!((park.balls[&501].max_velocity - 1_000.0).abs() < 1e-6);
    let obs_502 = park.drain_observer_events(502);
    assert!(obs_502.iter().any(|e| matches!(
        e,
        DestinyEvent::SetMaxSpeed { ball_id: 501, max_speed } if (*max_speed - 1_000.0).abs() < 1e-6
    )));

    // 4. remove_ball cleans up ball_interactive_overrides.
    assert!(park.remove_ball(501).is_some());
    assert!(!park.ball_interactive_overrides.contains_key(&501));

    // 5. Category 25 asteroid balls are treated as static during step_bumps (#792).
    let mut bump_park = Ballpark::new(30_000_142);
    let mut ast_ball = make_ship(701, DVec3::ZERO);
    ast_ball.radius = 200.0;
    ast_ball.mode = MovementMode::Stop;
    bump_park.add_ball(ast_ball);
    bump_park.set_ball_slim_meta(
        701,
        SlimItemMeta {
            group_id: 462,
            category_id: 25,
            owner_id: 1,
            ..Default::default()
        },
    );
    let mut rammer = make_ship(702, DVec3::new(100.0, 0.0, 0.0));
    rammer.velocity = DVec3::new(-300.0, 0.0, 0.0);
    bump_park.add_ball(rammer);
    bump_park.step(1.0);
    assert_eq!(
        bump_park.balls[&701].position,
        DVec3::ZERO,
        "Asteroid (category 25) must remain stationary when bumped by a ship"
    );
    assert_eq!(bump_park.balls[&701].velocity, DVec3::ZERO);
}

#[test]
fn prospective_grid_bounds_clamps_against_all_six_faces_and_falls_back_to_pos() {
    let r = BUBBLE_RADIUS_METERS;
    let empty_park = Ballpark::new(30_000_142);
    let p0 = DVec3::new(10_000.0, -20_000.0, 30_000.0);
    let (min0, max0) = empty_park.prospective_grid_bounds(p0);
    assert_eq!(min0, p0 - DVec3::splat(r));
    assert_eq!(max0, p0 + DVec3::splat(r));
    assert!(empty_park.grids.is_empty());

    // Place a single reference grid centered at the origin `[-r, +r]^3` and test all 6 faces.
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ship(1, DVec3::ZERO));
    assert_eq!(park.grids.len(), 1);
    let g_origin = &park.grids[&1];
    assert_eq!(g_origin.min_bounds, DVec3::splat(-r));
    assert_eq!(g_origin.max_bounds, DVec3::splat(r));

    let d = 1.5 * r;
    // +X neighbor: pos.x > existing.max_bounds.x -> min_b.x clamped to +r
    let (min_px, max_px) = park.prospective_grid_bounds(DVec3::new(d, 0.0, 0.0));
    assert!((min_px.x - r).abs() < 1e-6);
    assert!((max_px.x - (d + r)).abs() < 1e-6);
    assert!((min_px.y - (-r)).abs() < 1e-6);
    assert!((min_px.z - (-r)).abs() < 1e-6);

    // -X neighbor: pos.x < existing.min_bounds.x -> max_b.x clamped to -r
    let (min_nx, max_nx) = park.prospective_grid_bounds(DVec3::new(-d, 0.0, 0.0));
    assert!((min_nx.x - (-d - r)).abs() < 1e-6);
    assert!((max_nx.x - (-r)).abs() < 1e-6);

    // +Y neighbor: pos.y > existing.max_bounds.y -> min_b.y clamped to +r
    let (min_py, max_py) = park.prospective_grid_bounds(DVec3::new(0.0, d, 0.0));
    assert!((min_py.y - r).abs() < 1e-6);
    assert!((max_py.y - (d + r)).abs() < 1e-6);
    assert!((min_py.x - (-r)).abs() < 1e-6);

    // -Y neighbor: pos.y < existing.min_bounds.y -> max_b.y clamped to -r
    let (min_ny, max_ny) = park.prospective_grid_bounds(DVec3::new(0.0, -d, 0.0));
    assert!((min_ny.y - (-d - r)).abs() < 1e-6);
    assert!((max_ny.y - (-r)).abs() < 1e-6);

    // +Z neighbor: pos.z > existing.max_bounds.z -> min_b.z clamped to +r
    let (min_pz, max_pz) = park.prospective_grid_bounds(DVec3::new(0.0, 0.0, d));
    assert!((min_pz.z - r).abs() < 1e-6);
    assert!((max_pz.z - (d + r)).abs() < 1e-6);

    // -Z neighbor: pos.z < existing.min_bounds.z -> max_b.z clamped to -r
    let (min_nz, max_nz) = park.prospective_grid_bounds(DVec3::new(0.0, 0.0, -d));
    assert!((min_nz.z - (-d - r)).abs() < 1e-6);
    assert!((max_nz.z - (-r)).abs() < 1e-6);

    // Degenerate enclosed / inverted case: if an existing grid has min_bounds > max_bounds around pos
    // such that clamping pushes min_b > pos or max_b < pos, `min_b.min(pos)` and `max_b.max(pos)`
    // guarantee `min_b <= pos <= max_b`.
    let mut deg_park = Ballpark::new(30_000_142);
    deg_park.grids.insert(
        10,
        GridBubble {
            id: 10,
            center: DVec3::ZERO,
            min_bounds: DVec3::new(-100.0, -100.0, -100.0),
            max_bounds: DVec3::new(500_000.0, 100.0, 100.0),
            base_radius: r,
            members: std::collections::BTreeSet::new(),
        },
    );
    deg_park.grids.insert(
        11,
        GridBubble {
            id: 11,
            center: DVec3::ZERO,
            min_bounds: DVec3::new(-500_000.0, -100.0, -100.0),
            max_bounds: DVec3::new(100.0, 100.0, 100.0),
            base_radius: r,
            members: std::collections::BTreeSet::new(),
        },
    );
    let probe = DVec3::new(0.0, 200.0, 0.0);
    let (min_deg, max_deg) = deg_park.prospective_grid_bounds(probe);
    assert!(min_deg.x <= probe.x && probe.x <= max_deg.x);
    assert!(min_deg.y <= probe.y && probe.y <= max_deg.y);
    assert!(min_deg.z <= probe.z && probe.z <= max_deg.z);
}

#[test]
fn queue_event_for_all_grid_observers_recompute_quiet_and_apply_true_damage() {
    let mut park = Ballpark::new(30_000_142);
    // Grid A: balls 1 and 2 near origin; Grid B: ball 3 far away (`50,000 km`).
    park.add_ball(make_ship(1, DVec3::ZERO));
    park.add_ball(make_ship(2, DVec3::new(1_000.0, 0.0, 0.0)));
    park.add_ball(make_ship(3, DVec3::new(50_000_000.0, 0.0, 0.0)));
    assert_eq!(park.grids.len(), 2);
    let _ = park.drain_all_observer_events();

    // 1. `queue_event_for_all_grid_observers` with `Some(2)` delivers to 1 and 3, skipping 2.
    let sample_ev = DestinyEvent::SetBallMassive {
        ball_id: 99,
        is_massive: false,
    };
    park.queue_event_for_all_grid_observers(Some(2), &sample_ev);
    assert_eq!(park.drain_observer_events(1).len(), 1);
    assert!(park.drain_observer_events(2).is_empty());
    assert_eq!(park.drain_observer_events(3).len(), 1);

    // With `None`, all 3 observers across both grids receive the event.
    park.queue_event_for_all_grid_observers(None, &sample_ev);
    assert_eq!(park.drain_observer_events(1).len(), 1);
    assert_eq!(park.drain_observer_events(2).len(), 1);
    assert_eq!(park.drain_observer_events(3).len(), 1);

    // 2. `recompute_ball_attributes_quiet`:
    // - Static ball returns `None` even if velocity multiplier changes.
    let mut station = make_ship(60_003_760, DVec3::new(5_000.0, 0.0, 0.0));
    station.mode = MovementMode::Rigid;
    station.max_velocity = 10.0;
    park.add_static_ball(station, 15);
    park.combat_states.entry(60_003_760).or_default().prop_velocity_multiplier = 3.0;
    assert_eq!(park.recompute_ball_attributes_quiet(60_003_760), None);

    // - Dynamic ball with unchanged speed returns `None`.
    assert_eq!(park.recompute_ball_attributes_quiet(1), None);

    // - Dynamic ball with changed speed returns `Some(new_v)` and queues ZERO `SetMaxSpeed` events.
    park.combat_states.get_mut(&1).unwrap().prop_velocity_multiplier = 2.0;
    let quiet_v = park.recompute_ball_attributes_quiet(1);
    assert_eq!(quiet_v, Some(600.0));
    assert!((park.balls[&1].max_velocity - 600.0).abs() < 1e-6);
    assert!(park.drain_observer_events(2).is_empty());

    // - Orphan `CombatState` without a `Ball` (`(None, Some(cs))`) updates combat attributes and returns `None`.
    let mut orphan_cs = crate::CombatState::new();
    orphan_cs.adc_active = true;
    park.combat_states.insert(999, orphan_cs);
    assert_eq!(park.recompute_ball_attributes_quiet(999), None);
    assert_eq!(park.combat_states[&999].shield_resonance, [0.25; 4]);
    assert_eq!(park.combat_states[&999].armor_resonance, [0.25; 4]);
    assert_eq!(park.combat_states[&999].structure_resonance, [0.25; 4]);
    park.combat_states.remove(&999);

    // - Unknown ball (`(None, None)`) returns `None`.
    assert_eq!(park.recompute_ball_attributes_quiet(888_888), None);

    // 3. `apply_true_damage_to_ball`:
    // Configure ball 1 with 99% resistances (`resonance = 0.01`), normal cloak (`is_cloaked = true`),
    // and `100 / 100 / 100` HP.
    {
        let cs = park.combat_states.get_mut(&1).unwrap();
        cs.max_shield_hp = 100.0;
        cs.shield_hp = 100.0;
        cs.max_armor_hp = 100.0;
        cs.armor_hp = 100.0;
        cs.max_structure_hp = 100.0;
        cs.structure_hp = 100.0;
        cs.shield_resonance = [0.01; 4];
        cs.armor_resonance = [0.01; 4];
        cs.structure_resonance = [0.01; 4];
        cs.is_cloaked = true;
        cs.is_tethered = true;
    }
    let _ = park.drain_all_observer_events();
    park.pending_events.clear();

    // While tethered (invulnerable), true damage deals `0.0` and does not destroy the ship.
    let (applied_inv, destroyed_inv, snap_inv) = park.apply_true_damage_to_ball(1, 150.0).unwrap();
    assert_eq!(applied_inv, 0.0);
    assert!(!destroyed_inv);
    assert!(snap_inv.is_some());
    assert_eq!(park.combat_states[&1].shield_hp, 100.0);

    // Untethered (still cloaked with 99% resists): `150.0` true damage ignores resists & normal cloak,
    // depleting all `100` shield and `50` armor.
    park.combat_states.get_mut(&1).unwrap().is_tethered = false;
    let (applied_hit, destroyed_hit, _) = park.apply_true_damage_to_ball(1, 150.0).unwrap();
    assert!((applied_hit - 150.0).abs() < 1e-6);
    assert!(!destroyed_hit);
    assert!((park.combat_states[&1].shield_hp - 0.0).abs() < 1e-6);
    assert!((park.combat_states[&1].armor_hp - 50.0).abs() < 1e-6);
    assert!((park.combat_states[&1].structure_hp - 100.0).abs() < 1e-6);

    // Lethal true damage (`200.0` vs `150.0` remaining HP) destroys ball 1, removes it from `balls`,
    // queues `OnDamageStateChange`, `OnFleetDamageStateChange`, and `TerminalExplosion`, and preserves
    // `observer_events[&1]` so the dying victim can still flush its final tick updates.
    let (applied_kill, destroyed_kill, snap_kill) = park.apply_true_damage_to_ball(1, 200.0).unwrap();
    assert!((applied_kill - 150.0).abs() < 1e-6);
    assert!(destroyed_kill);
    assert_eq!(snap_kill.unwrap().id, 1);
    assert!(!park.balls.contains_key(&1));
    let victim_events = park.drain_observer_events(1);
    assert!(victim_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnDamageStateChange { ball_id: 1, structure_ratio, .. } if *structure_ratio == 0.0)));
    assert!(victim_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnFleetDamageStateChange { ball_id: 1, structure_ratio, .. } if *structure_ratio == 0.0)));
    assert!(victim_events
        .iter()
        .any(|e| matches!(e, DestinyEvent::TerminalExplosion { ball_id: 1, .. })));
}

#[test]
fn proximity_decloak_emits_set_max_speed_after_add_balls2_and_warp_cancel_clamps_velocity() {
    // 1. Proximity decloak in `step`: AddBalls2 is queued strictly BEFORE SetMaxSpeed,
    //    `meta.is_cloaked` is cleared, and `decloak_targeting_delay_remaining_s` is armed.
    let mut park = Ballpark::new(30_000_142);
    park.add_ball(make_ship(1, DVec3::ZERO));
    park.add_ball(make_ship(2, DVec3::new(1_500.0, 0.0, 0.0))); // within 2,000 m decloak radius
    park.set_ball_slim_meta(
        1,
        SlimItemMeta {
            group_id: 25,
            category_id: 6,
            is_cloaked: true,
            ..Default::default()
        },
    );
    {
        let b1 = park.balls.get_mut(&1).unwrap();
        b1.max_velocity = 75.0;
        let cs1 = park.combat_states.get_mut(&1).unwrap();
        cs1.is_cloaked = true;
        cs1.base_max_velocity = Some(300.0);
        cs1.cloak_velocity_multiplier = Some(0.25);
        cs1.cloak_configured_targeting_delay_s = 6.0;
    }
    let _ = park.drain_all_observer_events();
    park.pending_events.clear();

    park.step(1.0);
    assert!(!park.combat_states[&1].is_cloaked);
    assert_eq!(park.combat_states[&1].cloak_velocity_multiplier, None);
    assert!((park.combat_states[&1].decloak_targeting_delay_remaining_s - 6.0).abs() < 1e-6);
    assert!(!park.ball_slim_meta[&1].is_cloaked);
    assert!((park.balls[&1].max_velocity - 300.0).abs() < 1e-6);

    let obs2 = park.drain_observer_events(2);
    let add_idx = obs2
        .iter()
        .position(|e| matches!(e, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == 1)))
        .expect("observer 2 must receive AddBalls2 when ball 1 decloaks");
    let speed_idx = obs2
        .iter()
        .position(|e| matches!(e, DestinyEvent::SetMaxSpeed { ball_id: 1, max_speed } if (*max_speed - 300.0).abs() < 1e-6))
        .expect("observer 2 must receive SetMaxSpeed(300.0) when ball 1 decloaks");
    assert!(
        add_idx < speed_idx,
        "AddBalls2 (idx {add_idx}) must precede SetMaxSpeed (idx {speed_idx}) on proximity decloak"
    );

    // 2. Cancelling an Aligning warp via `goto_point` or `set_mode(Stop)` clamps superluminal/excessive
    //    velocity to `max_velocity`, clears `is_emergency_warping`, and emits `SetBallMassive(true)`,
    //    `OnSpecialFX("effects.Warping", false)`, and `OnDeactivatingWarp` (in `pending_events` only).
    let mut wpark = Ballpark::new(30_000_142);
    wpark.add_ball(make_ship(10, DVec3::ZERO));
    wpark.add_ball(make_ship(11, DVec3::new(500.0, 0.0, 0.0)));
    assert!(wpark
        .set_mode(
            10,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(100_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 100_000_000.0,
                elapsed_warp_time: 0.0,
                phase: WarpPhase::Aligning,
            },
        )
        .is_ok());
    wpark.balls.get_mut(&10).unwrap().velocity = DVec3::new(1_200.0, 0.0, 0.0); // > 300.0 max_velocity
    wpark.combat_states.get_mut(&10).unwrap().is_emergency_warping = true;
    let _ = wpark.drain_all_observer_events();
    wpark.pending_events.clear();

    assert!(wpark.goto_point(10, DVec3::new(0.0, 10_000.0, 0.0)).is_ok());
    assert!((wpark.balls[&10].velocity.length() - 300.0).abs() < 1e-6);
    assert!(!wpark.combat_states[&10].is_emergency_warping);
    let pending_cancel = std::mem::take(&mut wpark.pending_events);
    assert!(pending_cancel
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { ball_id: 10 })));
    let obs11 = wpark.drain_observer_events(11);
    assert!(!obs11
        .iter()
        .any(|e| matches!(e, DestinyEvent::OnDeactivatingWarp { .. })));
    assert!(obs11
        .iter()
        .any(|e| matches!(e, DestinyEvent::SetBallMassive { ball_id: 10, is_massive: true })));
    assert!(obs11.iter().any(|e| matches!(
        e,
        DestinyEvent::OnSpecialFX { ball_id: 10, guid, is_active: false, .. } if guid == "effects.Warping"
    )));

    // Also verify `set_mode(Stop)` from `WarpPhase::Aligning` clamps velocity and clears emergency warp.
    assert!(wpark
        .set_mode(
            10,
            MovementMode::Warp {
                origin: DVec3::ZERO,
                destination: DVec3::new(100_000_000.0, 0.0, 0.0),
                warp_speed_au_s: 3.0,
                total_distance: 100_000_000.0,
                elapsed_warp_time: 0.0,
                phase: WarpPhase::Aligning,
            },
        )
        .is_ok());
    wpark.balls.get_mut(&10).unwrap().velocity = DVec3::new(900.0, 0.0, 0.0);
    wpark.combat_states.get_mut(&10).unwrap().is_emergency_warping = true;
    assert!(wpark.set_mode(10, MovementMode::Stop).is_ok());
    assert!((wpark.balls[&10].velocity.length() - 300.0).abs() < 1e-6);
    assert!(!wpark.combat_states[&10].is_emergency_warping);

    // 3. Warp exit in `step`: clamps residual warp velocity to `max_velocity`, emits `SetBallPosition`
    //    before `Stop`, `SetBallMassive(true)`, and `OnSpecialFX("effects.Warping", false)`.
    let dest = DVec3::new(50_000_000.0, 0.0, 0.0);
    wpark.add_ball(make_ship(12, dest));
    {
        let b10 = wpark.balls.get_mut(&10).unwrap();
        b10.position = dest - DVec3::new(50.0, 0.0, 0.0);
        b10.velocity = DVec3::new(5_000.0, 0.0, 0.0);
        b10.mode = MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: dest,
            warp_speed_au_s: 3.0,
            total_distance: 50_000_000.0,
            elapsed_warp_time: 100.0,
            phase: WarpPhase::Decelerating,
        };
    }
    let _ = wpark.drain_all_observer_events();
    wpark.pending_events.clear();
    wpark.step(1.0);
    assert_eq!(wpark.balls[&10].mode, MovementMode::Stop);
    assert!(wpark.balls[&10].velocity.length() <= wpark.balls[&10].max_velocity + 1e-6);
    let obs10 = wpark.drain_observer_events(10);
    let pos_idx = obs10
        .iter()
        .position(|e| matches!(e, DestinyEvent::SetBallPosition { ball_id: 10, .. }))
        .expect("SetBallPosition must be emitted on warp exit");
    let stop_idx = obs10
        .iter()
        .position(|e| matches!(e, DestinyEvent::Stop { ball_id: 10 }))
        .expect("Stop must be emitted on warp exit");
    assert!(pos_idx < stop_idx, "SetBallPosition must precede Stop on warp exit");

    // 4. Sub-warp grid wall crossing (`obs_before != obs_after` vs `obs_before == obs_after`):
    //    Mover crossing from Grid A into adjacent Grid B gets `AddBalls` (for B's stationary ball)
    //    and `RemoveBalls` (for A's stationary ball), while B's stationary ball gets `AddBalls2`
    //    (for the newcomer) and A's stationary ball gets `RemoveBalls`.
    let r = BUBBLE_RADIUS_METERS;
    let mut gpark = Ballpark::new(30_000_142);
    gpark.add_ball(make_ship(21, DVec3::ZERO)); // Stationary in Grid A `[-r, +r]`
    gpark.add_ball(make_ship(22, DVec3::new(1.5 * r, 0.0, 0.0))); // Stationary in Grid B `[+r, 2.5*r]`
    let mut mover = make_ship(23, DVec3::new(r - 50.0, 0.0, 0.0)); // Starts inside Grid A near +r wall
    mover.velocity = DVec3::new(200.0, 0.0, 0.0);
    mover.max_velocity = 300.0;
    mover.mode = MovementMode::GotoDirection { direction: DVec3::X };
    gpark.add_ball(mover);
    assert_eq!(gpark.ball_grid_ids[&21], gpark.ball_grid_ids[&23]);
    assert_ne!(gpark.ball_grid_ids[&21], gpark.ball_grid_ids[&22]);
    let _ = gpark.drain_all_observer_events();

    gpark.step(1.0);
    assert_eq!(gpark.ball_grid_ids[&23], gpark.ball_grid_ids[&22]);
    let ev_mover = gpark.drain_observer_events(23);
    assert!(ev_mover
        .iter()
        .any(|e| matches!(e, DestinyEvent::AddBalls { balls } if balls.iter().any(|b| b.id == 22))));
    assert!(ev_mover
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBalls { ball_ids } if ball_ids.contains(&21))));

    let ev_dest_stationary = gpark.drain_observer_events(22);
    assert!(ev_dest_stationary
        .iter()
        .any(|e| matches!(e, DestinyEvent::AddBalls2 { balls } if balls.iter().any(|b| b.id == 23))));

    let ev_dep_stationary = gpark.drain_observer_events(21);
    assert!(ev_dep_stationary
        .iter()
        .any(|e| matches!(e, DestinyEvent::RemoveBalls { ball_ids } if ball_ids.contains(&23))));

    // 5. Asteroid (`SlimItemMeta.category_id == 25`) with non-Rigid mode and not in `static_ball_ids`
    //    is treated as static during collision resolution (`ballpark_targeting.rs:330`), so a moving
    //    ship bounces off it without displacing the asteroid.
    let mut cpark = Ballpark::new(30_000_142);
    let ast_id = 401_i64;
    let ship_id = 402_i64;
    let ast_pos = DVec3::new(80.0, 0.0, 0.0);
    cpark.add_ball(Ball {
        id: ast_id,
        type_id: 1230,
        position: ast_pos,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 500.0,
        radius: 60.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    cpark.set_ball_slim_meta(
        ast_id,
        SlimItemMeta {
            group_id: 462,
            category_id: 25,
            ..Default::default()
        },
    );
    cpark.add_ball(Ball {
        id: ship_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(200.0, 0.0, 0.0),
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode: MovementMode::GotoDirection { direction: DVec3::X },
    });
    cpark.step(1.0);
    assert_eq!(cpark.balls[&ast_id].position, ast_pos);
    assert_eq!(cpark.balls[&ast_id].velocity, DVec3::ZERO);
}

#[test]
fn mutant_killers_remove_static_recompute_eps_and_true_damage_ratios() {
    // 1. remove_ball (`was_static = static_ball_ids.remove || matches!(mode, Rigid)`) when old_grid_id is None
    let mut rpark = Ballpark::new(30_000_142);
    rpark.add_ball(Ball {
        id: 1,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    let _ = rpark.drain_observer_events(1);

    // Rigid ball NOT in static_ball_ids and NOT in ball_grid_ids
    rpark.balls.insert(
        2,
        Ball {
            id: 2,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
    );
    assert!(rpark.remove_ball(2).is_some());
    assert!(
        rpark
            .drain_observer_events(1)
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id: 2 })),
        "non-grid Rigid ball must emit RemoveBall to all grid observers"
    );

    // Non-Rigid ball IN static_ball_ids and NOT in ball_grid_ids
    rpark.balls.insert(
        3,
        Ball {
            id: 3,
            type_id: 587,
            position: DVec3::new(20_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e9,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
    );
    rpark.static_ball_ids.insert(3);
    assert!(rpark.remove_ball(3).is_some());
    assert!(
        rpark
            .drain_observer_events(1)
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id: 3 })),
        "non-grid static_ball_ids entry must emit RemoveBall even if mode != Rigid"
    );

    // 2. Exact 1e-6 boundary in recompute_ball_attributes_quiet and set_ball_max_velocity_cap (`> 1e-6` vs `>= 1e-6`)
    rpark.balls.get_mut(&1).unwrap().max_velocity = 0.0;
    let mut cs1 = crate::CombatState::default();
    cs1.base_max_velocity = Some(1e-6);
    cs1.base_mass = Some(1_000_000.0);
    cs1.base_inertia = Some(1.0);
    rpark.combat_states.insert(1, cs1);
    assert_eq!(rpark.recompute_ball_attributes_quiet(1), None);

    rpark.balls.get_mut(&1).unwrap().max_velocity = 0.0;
    let _ = rpark.drain_observer_events(1);
    assert!(rpark.set_ball_max_velocity_cap(1, Some(1e-6)));
    assert!(
        !rpark
            .drain_observer_events(1)
            .iter()
            .any(|e| matches!(e, DestinyEvent::SetMaxSpeed { ball_id: 1, .. })),
        "delta == 1e-6 must NOT emit SetMaxSpeed (`> 1e-6`)"
    );

    // 3. apply_true_damage_to_ball exact ratios across positive layers AND zero-max layers (`> 0.0` vs `>= 0.0`, `/` vs `% / *`)
    let mut dpark = Ballpark::new(30_000_142);
    dpark.add_ball(Ball {
        id: 10,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    dpark.combat_states.insert(
        10,
        crate::CombatState {
            shield_hp: 200.0,
            max_shield_hp: 200.0,
            armor_hp: 200.0,
            max_armor_hp: 200.0,
            structure_hp: 400.0,
            max_structure_hp: 400.0,
            ..Default::default()
        },
    );
    let _ = dpark.drain_observer_events(10);
    dpark.apply_true_damage_to_ball(10, 50.0);
    assert!(dpark.drain_observer_events(10).into_iter().any(|e| matches!(
        e,
        DestinyEvent::OnDamageStateChange { ball_id: 10, shield_ratio, armor_ratio, structure_ratio }
            if (shield_ratio - 0.75).abs() < 1e-9 && (armor_ratio - 1.0).abs() < 1e-9 && (structure_ratio - 1.0).abs() < 1e-9
    )));
    dpark.apply_true_damage_to_ball(10, 200.0);
    assert!(dpark.drain_observer_events(10).into_iter().any(|e| matches!(
        e,
        DestinyEvent::OnDamageStateChange { ball_id: 10, shield_ratio, armor_ratio, structure_ratio }
            if shield_ratio.abs() < 1e-9 && (armor_ratio - 0.75).abs() < 1e-9 && (structure_ratio - 1.0).abs() < 1e-9
    )));
    dpark.apply_true_damage_to_ball(10, 200.0);
    assert!(dpark.drain_observer_events(10).into_iter().any(|e| matches!(
        e,
        DestinyEvent::OnDamageStateChange { ball_id: 10, shield_ratio, armor_ratio, structure_ratio }
            if shield_ratio.abs() < 1e-9 && armor_ratio.abs() < 1e-9 && (structure_ratio - 0.875).abs() < 1e-9
    )));

    // Zero max HP layers -> ratios MUST be finite 0.0 (never NaN or 1.0)
    dpark.add_ball(Ball {
        id: 11,
        type_id: 587,
        position: DVec3::new(1000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    });
    dpark.combat_states.insert(
        11,
        crate::CombatState {
            shield_hp: 50.0,
            max_shield_hp: 0.0,
            armor_hp: 50.0,
            max_armor_hp: 0.0,
            structure_hp: 0.0,
            max_structure_hp: 0.0,
            ..Default::default()
        },
    );
    let _ = dpark.drain_observer_events(11);
    dpark.apply_true_damage_to_ball(11, 0.0);
    assert!(dpark.drain_observer_events(11).into_iter().any(|e| matches!(
        e,
        DestinyEvent::OnDamageStateChange { ball_id: 11, shield_ratio, armor_ratio, structure_ratio }
            if shield_ratio == 0.0 && armor_ratio == 0.0 && structure_ratio == 0.0
    )));

    // 4. Absent ball damage returns None without fabricating CombatState or observer_events (#846)
    assert!(dpark.apply_damage_to_ball(999_777, DamageVector::new(100.0, 0.0, 0.0, 0.0)).is_none());
    assert!(dpark.apply_true_damage_to_ball(999_777, 100.0).is_none());
    assert!(dpark.apply_direct_structure_damage_to_ball(999_777, 100.0).is_none());
    assert!(!dpark.combat_states.contains_key(&999_777));
    assert!(!dpark.observer_events.contains_key(&999_777));

    // remove_ball on absent ball cleans up any orphaned combat_states while preserving terminal observer_events (#846)
    dpark.combat_states.insert(999_778, crate::CombatState::default());
    assert!(dpark.remove_ball(999_778).is_none());
    assert!(!dpark.combat_states.contains_key(&999_778));
}

#[test]
fn in_space_ship_handoff_queues_incremental_events_without_set_state() {
    let mut bp = Ballpark::new(100);
    bp.set_observer_filtering(true);
    let old_ship = Ball {
        id: 500,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1e6,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let wreck = Ball {
        id: 501,
        type_id: 26557,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 10_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let capsule = Ball {
        id: 502,
        type_id: 670,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 335.0,
        inertia: 0.25,
        mass: 16_000.0,
        radius: 15.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    assert!(bp.add_ball(old_ship));
    let _ = bp.remove_ball(500);
    assert!(bp.add_ball(wreck));
    assert!(bp.add_ball(capsule));

    // Absent observer rejects handoff cleanly.
    assert!(!bp.queue_in_space_ship_handoff(500, 999, &[501, 502]));

    assert!(bp.queue_in_space_ship_handoff(500, 502, &[501, 502]));
    assert!(
        bp.pending_handoff_observers.contains(&502),
        "queue_in_space_ship_handoff must hold 502 against background tick theft until drained (#1018, H-2)"
    );
    let events = bp.drain_observer_events(502);
    assert!(
        !bp.pending_handoff_observers.contains(&502),
        "drain_observer_events(502) must clear pending_handoff_observers hold (#1018, H-2)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DestinyEvent::SetState { .. })),
        "in-system handoff must not emit SetState (#970)"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { .. })),
        "in-system handoff must defer RemoveBall out of the immediate AddBalls2 batch (#1018)"
    );
    assert!(events.iter().any(|e| match e {
        DestinyEvent::AddBalls2 { balls } => {
            let ids: Vec<i64> = balls.iter().map(|b| b.id).collect();
            ids == vec![501, 502]
        }
        _ => false,
    }));

    // Tick +1: RemoveBall(500) must remain deferred past godma.Prime/OnSessionChanged window (#1018).
    let _ = bp.step(1.0);
    let tick1_events = bp.drain_observer_events(502);
    assert!(
        !tick1_events
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { .. })),
        "RemoveBall(500) must stay deferred on tick +1 (#1018)"
    );

    // Tick +2: RemoveBall(500) is delivered cleanly once client ego has flipped to capsule 502.
    let _ = bp.step(1.0);
    let tick2_events = bp.drain_observer_events(502);
    assert!(
        tick2_events
            .iter()
            .any(|e| matches!(e, DestinyEvent::RemoveBall { ball_id: 500 })),
        "RemoveBall(500) must be delivered on tick +2 (#1018)"
    );
    assert!(
        bp.scheduled_observer_events.is_empty(),
        "scheduled_observer_events must drain cleanly once delivered (#1018)"
    );

    // If the new capsule observer is itself destroyed before +2 ticks, its scheduled RemoveBall is dropped.
    assert!(bp.queue_in_space_ship_handoff(501, 502, &[502]));
    let _ = bp.drain_observer_events(502);
    let _ = bp.remove_ball(502);
    assert!(
        bp.scheduled_observer_events.is_empty(),
        "remove_ball must prune scheduled_observer_events for removed observers (#1018)"
    );
    assert!(
        !bp.pending_handoff_observers.contains(&502),
        "remove_ball must clear pending_handoff_observers for removed observers (#1018, H-2)"
    );
}

