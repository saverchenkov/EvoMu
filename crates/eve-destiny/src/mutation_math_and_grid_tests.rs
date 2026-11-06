use super::*;
use crate::wire::resolve_slim_item_meta;

fn make_ship(id: BallId, pos: DVec3, vel: DVec3) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: vel,
        max_velocity: 400.0,
        inertia: 2.5,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

#[test]
fn combat_damage_layers_resonances_true_damage_and_rng() {
    // Serde defaults for CombatState and Ballpark
    let cs_default = CombatState::new();
    let mut cs_val = serde_json::to_value(&cs_default).unwrap();
    if let Some(obj) = cs_val.as_object_mut() {
        obj.remove("shield_recharge_time_ms");
        obj.remove("capacitor_recharge_time_ms");
        obj.remove("tracking_speed_multiplier");
        obj.remove("shield_boost_multiplier");
        obj.remove("rng_state");
    }
    let cs_de: CombatState = serde_json::from_value(cs_val).unwrap();
    assert_eq!(cs_de.shield_recharge_time_ms, 120_000.0);
    assert_eq!(cs_de.capacitor_recharge_time_ms, 120_000.0);
    assert_eq!(cs_de.tracking_speed_multiplier, 1.0);
    assert_eq!(cs_de.shield_boost_multiplier, 1.0);
    assert_eq!(cs_de.rng_state, 0x9E37_79B9_7F4A_7C15);

    let bp_default = Ballpark::new(30_000_142);
    assert_eq!(bp_default.constellation_id, 20_000_020);
    let mut bp_val = serde_json::to_value(&bp_default).unwrap();
    if let Some(obj) = bp_val.as_object_mut() {
        obj.remove("constellation_id");
        obj.remove("next_grid_id");
    }
    let bp_de: Ballpark = serde_json::from_value(bp_val).unwrap();
    assert_eq!(bp_de.constellation_id, 20_000_020);
    assert_eq!(bp_de.next_grid_id, 1);

    // SplitMix64 deterministic values
    assert_eq!(CombatState::seed_rng_from_u64(0), 16_294_208_416_658_607_535);
    assert_eq!(CombatState::seed_rng_from_u64(1), 10_451_216_379_200_822_465);

    // Warp scramble & invulnerability boolean combinations
    let mut flags = CombatState {
        warp_scramble_strength: 0,
        warp_core_strength: 0,
        ..CombatState::new()
    };
    assert!(!flags.is_warp_scrambled());
    flags.warp_scramble_strength = 1;
    assert!(flags.is_warp_scrambled());

    assert!(!CombatState::new().is_invulnerable());
    assert!(CombatState { is_tethered: true, ..CombatState::new() }.is_invulnerable());
    assert!(CombatState { gate_cloak_remaining_ticks: 1, ..CombatState::new() }.is_invulnerable());
    assert!(CombatState { undock_invulnerable_remaining_ticks: 1, ..CombatState::new() }.is_invulnerable());
    assert!(CombatState { warp_exit_invulnerable_remaining_ticks: 1, ..CombatState::new() }.is_invulnerable());

    // 4-channel shield absorption with exact resonance weighting
    let mut cs = CombatState {
        shield_hp: 200.0,
        max_shield_hp: 200.0,
        armor_hp: 450.0,
        max_armor_hp: 450.0,
        structure_hp: 350.0,
        max_structure_hp: 350.0,
        shield_resonance: [0.5, 0.25, 0.75, 1.0],
        armor_resonance: [0.8, 0.6, 0.4, 0.2],
        structure_resonance: [0.9, 0.7, 0.5, 0.3],
        ..CombatState::new()
    };
    let dmg1 = DamageVector {
        em: 20.0,
        thermal: 40.0,
        kinetic: 20.0,
        explosive: 15.0,
    };
    let (app1, dead1) = cs.apply_damage(dmg1);
    assert!(!dead1);
    assert!((app1 - 50.0).abs() < 1e-9);
    assert!((cs.shield_hp - 150.0).abs() < 1e-9);
    assert_eq!(cs.armor_hp, 450.0);

    // Exact boundary shield_hp == eff depletes shield to 0 without touching armor
    let mut exact_shield = CombatState {
        shield_hp: 50.0,
        armor_hp: 100.0,
        shield_resonance: [0.5, 0.25, 0.75, 1.0],
        ..CombatState::new()
    };
    let (app_ex, _) = exact_shield.apply_damage(dmg1);
    assert!((app_ex - 50.0).abs() < 1e-9);
    assert_eq!(exact_shield.shield_hp, 0.0);
    assert_eq!(exact_shield.armor_hp, 100.0);

    // Multi-layer overflow across shield -> armor -> structure with distinct resonances
    let mut overflow = CombatState {
        shield_hp: 25.0,
        armor_hp: 15.0,
        structure_hp: 100.0,
        shield_resonance: [0.5, 0.5, 0.5, 0.5],
        armor_resonance: [0.8, 0.6, 0.4, 0.2],
        structure_resonance: [0.9, 0.7, 0.5, 0.3],
        ..CombatState::new()
    };
    let dmg_ov = DamageVector {
        em: 20.0,
        thermal: 40.0,
        kinetic: 20.0,
        explosive: 20.0,
    };
    let expected_struct_dmg = 31.0 * (11.0 / 26.0);
    let (app_ov, dead_ov) = overflow.apply_damage(dmg_ov);
    assert!(!dead_ov);
    assert_eq!(overflow.shield_hp, 0.0);
    assert_eq!(overflow.armor_hp, 0.0);
    assert!((app_ov - (25.0 + 15.0 + expected_struct_dmg)).abs() < 1e-6);
    assert!((overflow.structure_hp - (100.0 - expected_struct_dmg)).abs() < 1e-6);

    // Zero resonance absorbs completely without leaking
    let mut immune = CombatState {
        shield_hp: 100.0,
        armor_hp: 100.0,
        structure_hp: 100.0,
        shield_resonance: [0.0; 4],
        armor_resonance: [0.0; 4],
        ..CombatState::new()
    };
    assert_eq!(immune.apply_damage(dmg_ov), (0.0, false));
    assert_eq!(immune.armor_hp, 100.0);
    immune.shield_hp = 0.0;
    assert_eq!(immune.apply_damage(dmg_ov), (0.0, false));
    assert_eq!(immune.structure_hp, 100.0);

    // Negative / zero damage checks structure destruction state
    assert_eq!(immune.apply_damage(DamageVector { em: -5.0, ..Default::default() }), (0.0, false));
    immune.structure_hp = 0.0;
    assert_eq!(immune.apply_damage(DamageVector::default()), (0.0, true));

    // True damage invulnerability, zero/negative amount, and layer propagation
    let mut inv = CombatState { is_tethered: true, ..CombatState::new() };
    assert_eq!(inv.apply_true_damage(50.0), (0.0, false));
    assert_eq!(inv.shield_hp, 450.0);
    assert_eq!(CombatState { gate_cloak_remaining_ticks: 2, ..CombatState::new() }.apply_true_damage(50.0), (0.0, false));
    assert_eq!(CombatState { undock_invulnerable_remaining_ticks: 2, ..CombatState::new() }.apply_true_damage(50.0), (0.0, false));
    assert_eq!(CombatState { warp_exit_invulnerable_remaining_ticks: 2, ..CombatState::new() }.apply_true_damage(50.0), (0.0, false));

    let mut td = CombatState {
        shield_hp: 10.0,
        armor_hp: 15.0,
        structure_hp: 20.0,
        ..CombatState::new()
    };
    assert_eq!(td.apply_true_damage(-3.0), (0.0, false));
    let (ta, destroyed) = td.apply_true_damage(50.0);
    assert_eq!(ta, 45.0);
    assert!(destroyed);
    assert_eq!(td.apply_true_damage(0.0), (0.0, true));
}

#[test]
fn grid_six_face_clamping_warp_interception_and_ball_helpers() {
    // Non-zero origin distance in MovementMode::new_warp
    let wm = MovementMode::new_warp(DVec3::new(1_000.0, 0.0, 0.0), DVec3::new(4_000.0, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { total_distance, .. } = wm {
        assert!((total_distance - 3_000.0).abs() < 1e-9);
    } else {
        panic!("expected warp mode");
    }

    // 6-face grid overlap clamping against base grid at origin (no 1m dead-zone gap or extra expansion)
    let r = 100_000.0;
    let face_targets = [
        (DVec3::new(150_000.0, 0.0, 0.0), DVec3::new(100_000.0, -r, -r), DVec3::new(250_000.0, r, r)),
        (DVec3::new(-150_000.0, 0.0, 0.0), DVec3::new(-250_000.0, -r, -r), DVec3::new(-100_000.0, r, r)),
        (DVec3::new(0.0, 150_000.0, 0.0), DVec3::new(-r, 100_000.0, -r), DVec3::new(r, 250_000.0, r)),
        (DVec3::new(0.0, -150_000.0, 0.0), DVec3::new(-r, -250_000.0, -r), DVec3::new(r, -100_000.0, r)),
        (DVec3::new(0.0, 0.0, 150_000.0), DVec3::new(-r, -r, 100_000.0), DVec3::new(r, r, 250_000.0)),
        (DVec3::new(0.0, 0.0, -150_000.0), DVec3::new(-r, -r, -250_000.0), DVec3::new(r, r, -100_000.0)),
    ];
    for (pos, exp_min, exp_max) in face_targets {
        let mut bp = Ballpark::with_bubble_radius(30_000_142, r);
        let g1 = bp.find_or_create_grid_for_position(DVec3::ZERO);
        let g2 = bp.find_or_create_grid_for_position(pos);
        assert_ne!(g1, g2);
        let gb1 = &bp.grids[&g1];
        let gb2 = &bp.grids[&g2];
        assert!(!gb1.overlaps_bounds(gb2.min_bounds, gb2.max_bounds));
        assert!((gb2.min_bounds - exp_min).length() < 1e-6);
        assert!((gb2.max_bounds - exp_max).length() < 1e-6);
    }

    // Touching box boundary is NOT an overlap (< vs <=)
    let touch = GridBubble::new(1, DVec3::ZERO, 100.0);
    assert!(!touch.overlaps_bounds(DVec3::new(100.0, 0.0, 0.0), DVec3::new(200.0, 10.0, 10.0)));
    assert!(!touch.overlaps_bounds(DVec3::new(-200.0, 0.0, 0.0), DVec3::new(-100.0, 10.0, 10.0)));
    assert!(!touch.overlaps_bounds(DVec3::new(0.0, 100.0, 0.0), DVec3::new(10.0, 200.0, 10.0)));
    assert!(!touch.overlaps_bounds(DVec3::new(0.0, -200.0, 0.0), DVec3::new(10.0, -100.0, 10.0)));
    assert!(!touch.overlaps_bounds(DVec3::new(0.0, 0.0, 100.0), DVec3::new(10.0, 10.0, 200.0)));
    assert!(!touch.overlaps_bounds(DVec3::new(0.0, 0.0, -200.0), DVec3::new(10.0, 10.0, -100.0)));

    // WarpBubble off-axis transverse geometry + origin != ZERO + earliest bubble selection
    let origin = DVec3::new(10_000.0, 20_000.0, 30_000.0);
    let dest = origin + DVec3::new(300_000.0, 400_000.0, 0.0); // dist = 500_000.0, dir = (0.6, 0.8, 0.0)
    let b_center = origin + DVec3::new(240_000.0 - 2_400.0, 320_000.0 + 1_800.0, 0.0); // proj = 400_000, d = 3_000
    let wb = WarpBubble::new(501, b_center, 5_000.0); // thc = 4_000 -> t0 = 396_000
    // Exact boundary dist == 1.0: if < mutates to <=, dist == 1.0 returns None!
    let unit_wb = WarpBubble::new(500, DVec3::new(0.5, 0.0, 0.0), 1.0);
    assert!(unit_wb.intercept_warp(DVec3::ZERO, DVec3::new(1.0, 0.0, 0.0)).is_some());
    assert!(wb.intercept_warp(origin, origin + DVec3::X * 0.5).is_none());
    let hit = wb.intercept_warp(origin, dest).unwrap();
    let expected_hit = origin + DVec3::new(0.6, 0.8, 0.0) * 396_000.0;
    assert!((hit - expected_hit).length() < 1e-4);

    // Exact tangent grazing ray d2 == r2: if > mutates to >=, tangent grazing misses!
    let tangent_wb = WarpBubble::new(505, DVec3::new(100_000.0, 5_000.0, 0.0), 5_000.0);
    let tan_hit = tangent_wb.intercept_warp(DVec3::ZERO, DVec3::new(200_000.0, 0.0, 0.0)).unwrap();
    assert!((tan_hit - DVec3::new(100_000.0, 0.0, 0.0)).length() < 1e-6);

    // Exact touching tangent at bubble perimeter (t0 == 0, t1 == 0 -> Some(origin))
    let orig_tan = WarpBubble::new(506, DVec3::new(0.0, 5_000.0, 0.0), 5_000.0);
    assert_eq!(orig_tan.intercept_warp(DVec3::ZERO, DVec3::new(100_000.0, 0.0, 0.0)), Some(DVec3::ZERO));

    // Short warp dist = 2.0: win_end = dist + 500_000.0 = 500_002.0 (if + mutates to *, 2.0 * 500_000.0 = 1_000_000.0!)
    let beyond_sum_within_prod = WarpBubble::new(507, DVec3::new(600_000.0, 0.0, 0.0), 1_000.0);
    assert!(beyond_sum_within_prod.intercept_warp(DVec3::ZERO, DVec3::new(2.0, 0.0, 0.0)).is_none());

    // Exact boundary t1 == win_start (1_500_000.0) and t0 == win_end (2_500_000.0) on 2_000_000.0m warp
    let edge_start = WarpBubble::new(508, DVec3::new(1_495_000.0, 0.0, 0.0), 5_000.0);
    assert!(edge_start.intercept_warp(DVec3::ZERO, DVec3::new(2_000_000.0, 0.0, 0.0)).is_some());
    let edge_end = WarpBubble::new(509, DVec3::new(2_505_000.0, 0.0, 0.0), 5_000.0);
    assert!(edge_end.intercept_warp(DVec3::ZERO, DVec3::new(2_000_000.0, 0.0, 0.0)).is_some());

    // Bubble outside 500km window around destination is ignored; inside drag window is caught
    let long_dest = origin + DVec3::X * 2_000_000.0;
    let mid_bubble = WarpBubble::new(502, origin + DVec3::X * 500_000.0, 5_000.0);
    assert!(mid_bubble.intercept_warp(origin, long_dest).is_none());
    let drag_bubble = WarpBubble::new(503, origin + DVec3::X * 2_400_000.0, 10_000.0);
    assert!(drag_bubble.intercept_warp(origin, long_dest).is_some());
    let inside_bubble = WarpBubble::new(504, origin, 5_000.0);
    assert_eq!(inside_bubble.intercept_warp(origin, dest), Some(origin));

    let mut mgr = WarpBubbleMgr::new();
    mgr.add_bubble(WarpBubble::new(1, origin + DVec3::new(0.6, 0.8, 0.0) * 450_000.0, 5_000.0));
    mgr.add_bubble(WarpBubble::new(2, origin + DVec3::new(0.6, 0.8, 0.0) * 200_000.0, 5_000.0));
    let (earliest_id, _) = mgr.check_interception(origin, dest).unwrap();
    assert_eq!(earliest_id, 2);

    // Static ball metadata, mushroom validation, miniball offset, troll stamp, and set_boid error
    let mut park = Ballpark::new(30_000_142);
    assert!(park.add_static_ball(make_ship(60_000_001, DVec3::ZERO, DVec3::ZERO), 15));
    assert_eq!(park.ball_slim_meta[&60_000_001].category_id, 3);
    assert!(park.add_static_ball(make_ship(40_000_001, DVec3::ZERO, DVec3::ZERO), 6));
    assert_eq!(park.ball_slim_meta[&40_000_001].category_id, 2);

    park.set_ball_group_id(77_777, 16);
    assert_eq!(park.ball_slim_meta[&77_777].group_id, 16);

    assert!(!park.add_mushroom_ball(80_001, 1, DVec3::ZERO, 100.0, f64::NAN, 10.0, 1));
    assert!(!park.add_mushroom_ball(80_002, 1, DVec3::ZERO, 100.0, 0.0, 10.0, 1));
    assert!(park.add_mushroom_ball(80_003, 1, DVec3::ZERO, 100.0, 5.0, 10.0, 1));
    assert!(!park.add_mushroom_ball(80_003, 1, DVec3::ZERO, 100.0, 5.0, 10.0, 1));

    let parent_pos = DVec3::new(100.0, 200.0, 300.0);
    assert!(park.add_ball(make_ship(90_001, parent_pos, DVec3::new(50.0, 0.0, 0.0))));
    assert!(park.add_miniball(90_002, 90_001, DVec3::ZERO, f64::NAN).is_err());
    assert!(park.add_miniball(90_002, 90_001, DVec3::ZERO, 0.0).is_err());
    let rel = DVec3::new(10.0, -20.0, 30.0);
    park.add_miniball(90_002, 90_001, rel, 25.0).unwrap();
    assert_eq!(park.balls[&90_002].position, parent_pos + rel);

    park.current_tick = 10;
    let troll_stamp = park.set_ball_troll(90_001, 3).unwrap();
    assert_eq!(troll_stamp, 13);
    assert!(park.set_boid(999_888, true).is_err());
    park.set_boid(90_001, true).unwrap();

    assert!(park.set_ball_free(90_001, true));
    assert_ne!(park.balls[&90_001].velocity, DVec3::ZERO);
    assert!(park.set_ball_free(90_001, false));
    assert_eq!(park.balls[&90_001].velocity, DVec3::ZERO);
    assert_eq!(park.balls[&90_001].speed_fraction, 0.0);
    assert!(matches!(park.balls[&90_001].mode, MovementMode::Stop));

    // Resolve slim meta fallback for unregistered group_id and capital ship lookup
    let mut group_only = make_ship(99_999, DVec3::ZERO, DVec3::ZERO);
    group_only.mode = MovementMode::Rigid;
    let rmeta = resolve_slim_item_meta(None, &group_only);
    assert_eq!(rmeta.group_id, 6);
}
