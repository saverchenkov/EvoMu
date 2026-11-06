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
                cloak_original_max_velocity: Some(350.0),
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
    dmg_park.combat_states.insert(101, crate::CombatState { shield_hp: 200.0, max_shield_hp: 200.0, armor_hp: 400.0, max_armor_hp: 400.0, structure_hp: 500.0, max_structure_hp: 500.0, cloak_original_max_velocity: Some(320.0), is_cloaked: true, ..Default::default() });
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
