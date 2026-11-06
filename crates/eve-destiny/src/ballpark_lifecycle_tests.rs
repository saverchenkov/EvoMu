//! Unit tests for `CombatState`, `Ball`, `WarpBubble`, `Ballpark` lifecycle commands, and wire codecs.

use super::*;
use crate::wire::{
    encode_add_balls, encode_add_balls_with_park, encode_add_balls_with_type, encode_destiny_event,
    encode_single_ball, encode_single_ball_with_meta, encode_slim_item, encode_slim_item_with_meta,
    resolve_slim_item_meta, slim_item_dict, slim_item_dict_with_park,
};
use eve_proto::PyRep;

fn make_ship(id: BallId, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 400.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn dict_lookup<'a>(dict: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    let PyRep::Dict(entries) = dict else {
        return None;
    };
    entries
        .iter()
        .find(|(k, _)| matches!(k, PyRep::String(s) if s == key))
        .map(|(_, v)| v)
}

#[test]
fn combat_state_rng_invuln_and_layered_damage_math() {
    // Verify SplitMix64 seed non-zero invariant and deterministic mixing.
    assert_ne!(CombatState::seed_rng_from_u64(0), 0);
    assert_ne!(CombatState::seed_rng_from_u64(42), CombatState::seed_rng_from_u64(43));

    let mut cs = CombatState::new();
    cs.set_interdiction_nullified(true);
    assert!(cs.is_interdiction_nullified);

    assert!(!cs.is_warp_scrambled());
    cs.warp_scramble_strength = 2;
    cs.warp_core_strength = 3;
    assert!(!cs.is_warp_scrambled());
    cs.warp_core_strength = 2;
    assert!(cs.is_warp_scrambled());

    // Invulnerability and action-break synchronization.
    cs.warp_exit_invulnerable_remaining_ticks = 5;
    assert!(cs.is_warp_exit_invulnerable());
    assert!(cs.is_invulnerable());
    let zero_applied = cs.apply_damage(DamageVector::new(500.0, 500.0, 500.0, 500.0));
    assert_eq!(zero_applied.0, 0.0);
    assert!(!zero_applied.1);

    let (true_invuln, true_destr) = cs.apply_true_damage(500.0);
    assert_eq!(true_invuln, 0.0);
    assert!(!true_destr);

    cs.gate_cloak_remaining_ticks = 3;
    cs.is_cloaked = true;
    cs.sync_invuln_with_undock_timer = true;
    cs.undock_invulnerable_remaining_ticks = 0;
    assert!(!cs.is_warp_exit_invulnerable());
    assert!(!cs.is_invulnerable());
    cs.sync_action_breaks();
    assert!(!cs.is_cloaked);
    assert_eq!(cs.gate_cloak_remaining_ticks, 0);

    // Zero / negative damage does not harm intact ship.
    let (z_dmg, z_dead) = cs.apply_damage(DamageVector::new(-10.0, -10.0, -10.0, -10.0));
    assert_eq!(z_dmg, 0.0);
    assert!(!z_dead);

    // Zero resonance layer absorbs incoming vector cleanly without NaN.
    cs.shield_resonance = [0.0; 4];
    let (immune_dmg, immune_dead) = cs.apply_damage(DamageVector::new(100.0, 100.0, 100.0, 100.0));
    assert_eq!(immune_dmg, 0.0);
    assert!(!immune_dead);

    // Layered overflow: shield (450, 0.5 EM) + armor (450, 1.0) + structure (350, 1.0).
    cs.shield_resonance = [0.5, 1.0, 1.0, 1.0];
    // 1300 raw EM: 900 raw needed to strip 450 effective shield, leaving 400 EM for armor.
    let (applied1, dead1) = cs.apply_damage(DamageVector {
        em: 1300.0,
        thermal: 0.0,
        kinetic: 0.0,
        explosive: 0.0,
    });
    assert!((applied1 - 850.0).abs() < 1e-6);
    assert!((cs.shield_hp - 0.0).abs() < 1e-6);
    assert!((cs.armor_hp - 50.0).abs() < 1e-6);
    assert!(!dead1);

    // Finish armor (50) and structure (350) with overflow.
    let (applied2, dead2) = cs.apply_damage(DamageVector::new(150.0, 150.0, 150.0, 150.0));
    assert!((applied2 - 400.0).abs() < 1e-6);
    assert!(dead2);
    assert_eq!(cs.structure_hp, 0.0);

    // True damage overflow path across all three layers.
    let mut cs_true = CombatState {
        shield_hp: 100.0,
        max_shield_hp: 100.0,
        armor_hp: 120.0,
        max_armor_hp: 120.0,
        structure_hp: 80.0,
        max_structure_hp: 80.0,
        ..CombatState::default()
    };
    assert_eq!(cs_true.apply_true_damage(-5.0), (0.0, false));
    assert_eq!(cs_true.apply_true_damage(250.0), (250.0, false));
    assert!((cs_true.structure_hp - 50.0).abs() < 1e-6);
    assert_eq!(cs_true.apply_true_damage(80.0), (50.0, true));

    // Ball kinematics helpers and GridBubble bounds.
    let ship = make_ship(10, 587, DVec3::ZERO);
    assert!((ship.time_constant() - 2.4).abs() < 1e-9);
    assert!((ship.align_time_seconds() - (-0.25_f64.ln() * 2.4)).abs() < 1e-9);
    assert_eq!(ship.align_server_ticks(), 4);

    let g1 = GridBubble::new(1, DVec3::ZERO, 1000.0);
    let g2 = GridBubble::new(2, DVec3::new(1500.0, 0.0, 0.0), 1000.0);
    let g3 = GridBubble::new(3, DVec3::new(3000.0, 0.0, 0.0), 1000.0);
    assert!(g1.contains(DVec3::new(1000.0, 0.0, 0.0)));
    assert!(!g1.contains(DVec3::new(1000.1, 0.0, 0.0)));
    assert!(g1.overlaps_bounds(g2.min_bounds, g2.max_bounds));
    assert!(!g1.overlaps_bounds(g3.min_bounds, g3.max_bounds));
}

#[test]
fn warp_bubble_interception_and_ballpark_lifecycle_actions() {
    let wb = WarpBubble::new(9001, DVec3::new(1_000_000.0, 0.0, 0.0), 20_000.0);
    assert!(wb.contains(DVec3::new(1_020_000.0, 0.0, 0.0)));
    assert!(!wb.contains(DVec3::new(1_025_000.0, 0.0, 0.0)));
    // Zero-distance warp returns None.
    assert!(wb.intercept_warp(DVec3::ZERO, DVec3::new(0.5, 0.0, 0.0)).is_none());
    // Off-axis ray misses sphere.
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(1_000_000.0, 50_000.0, 0.0))
        .is_none());
    // Far beyond 500km window returns None.
    assert!(wb
        .intercept_warp(DVec3::ZERO, DVec3::new(2_000_000.0, 0.0, 0.0))
        .is_none());
    // Within 500km window intercepts at bubble entry perimeter.
    let hit = wb
        .intercept_warp(DVec3::ZERO, DVec3::new(1_200_000.0, 0.0, 0.0))
        .expect("intercept expected");
    assert!((hit.x - 980_000.0).abs() < 1e-3);

    let mut park = Ballpark::with_bubble_radius(30_000_142, 300_000.0);
    park.set_bubble_radius(250_000.0);

    // Missing ball direct structure damage & controller helpers return false/None cleanly.
    assert!(park.apply_direct_structure_damage_to_ball(999_999, 50.0).is_none());
    assert!(!park.set_ball_free(999_999, true));
    assert!(!park.set_ball_interactive(999_999, true));
    assert!(!park.set_ball_harmonic(999_999, 1));
    assert!(!park.emit_client_controller_event(999_999, None, "DoDestinyUpdate", 1, Some(0.0)));
    assert!(!park.update_ship_module_states(999_999, vec![]));
    assert!(!park.update_dbuffs(999_999, vec![]));
    assert!(!park.apply_breacher_dot(999_999, 10.0, 0.01, 5));

    // Add observer, target ship, static station, and Upwell structure.
    assert!(park.add_ball(make_ship(101, 587, DVec3::ZERO)));
    assert!(park.add_ball(make_ship(102, 587, DVec3::new(1000.0, 0.0, 0.0))));
    let station = Ball {
        id: 60_003_760,
        type_id: 57,
        position: DVec3::new(50_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1e12,
        radius: 5000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Rigid,
    };
    assert!(park.add_static_ball(station, 15));
    assert!(park.is_immobile_structure_or_static(60_003_760));
    assert!(!park.is_immobile_structure_or_static(101));

    // Upwell structure by category ID (category 65 = Structure, group 1657 = Citadel).
    assert!(park.add_ball(make_ship(200, 35832, DVec3::new(20_000.0, 0.0, 0.0))));
    park.set_ball_slim_meta(
        200,
        SlimItemMeta {
            group_id: 1657,
            category_id: 65,
            ..SlimItemMeta::default()
        },
    );
    assert!(park.is_immobile_structure_or_static(200));

    // Direct structure damage leaves shield & armor untouched and destroys on lethal blow.
    park.combat_states.get_mut(&102).unwrap().warp_exit_invulnerable_remaining_ticks = 2;
    let invuln_res = park.apply_direct_structure_damage_to_ball(102, 100.0).unwrap();
    assert_eq!(invuln_res.0, 0.0);
    assert!(!invuln_res.1);

    park.combat_states.get_mut(&102).unwrap().warp_exit_invulnerable_remaining_ticks = 0;
    let partial = park.apply_direct_structure_damage_to_ball(102, 100.0).unwrap();
    assert!((partial.0 - 100.0).abs() < 1e-6);
    assert!(!partial.1);
    assert_eq!(park.combat_states[&102].shield_hp, 450.0);
    assert_eq!(park.combat_states[&102].armor_hp, 450.0);
    assert_eq!(park.combat_states[&102].structure_hp, 250.0);

    let lethal = park.apply_direct_structure_damage_to_ball(102, 300.0).unwrap();
    assert!((lethal.0 - 250.0).abs() < 1e-6);
    assert!(lethal.1);
    assert!(!park.balls.contains_key(&102));
    assert!(!park.combat_states.contains_key(&102));

    // Test slim meta changes, docking, controller actions, dbuffs, breacher DoT, drone states.
    let new_meta = SlimItemMeta {
        group_id: 988,
        category_id: 2,
        owner_id: 90_000_001,
        char_id: Some(90_000_001),
        corp_id: 1_000_001,
        alliance_id: 99_000_001,
        security_status: -1.5,
        skin_id: Some(1234),
        skin_material_set_id: Some(5678),
        is_cloaked: false,
        jump_target: None,
    };
    assert!(park.change_slim_item(101, new_meta));
    assert!(park.set_ball_wormhole_stages(101, 2, 0.75));
    assert!(park.set_ball_free(101, false));
    assert!(park.set_ball_interactive(101, true));
    assert!(park.set_ball_harmonic(101, 3));
    park.emit_packaged_actions(Some(101), &[DestinyEvent::OnAudioActivated]);
    park.set_sovereignty(99_000_001);
    assert!(park.emit_client_controller_event(101, Some(102), "DoDestinyUpdate", 2, Some(0.1)));
    assert!(park.update_ship_module_states(101, vec![(1, 2, 3, 4, 5.0, 6.0)]));
    assert!(park.update_dbuffs(101, vec![(99, -0.25, 1_000_000)]));
    assert!(park.apply_breacher_dot(101, 50.0, 0.02, 10));
    assert!(park.apply_breacher_dot(101, 0.0, 0.0, 0));
    park.update_drone_state(501, 90_000_001, 101, 1, 2454, 1_000_001, Some(102));

    assert!(park.add_ball(make_ship(103, 587, DVec3::new(25.0, 0.0, 0.0))));
    assert!(park.accept_docking(103, 60_003_760).is_some());
    assert!(park.accept_docking_at(101, 60_003_760, DVec3::new(10.0, 20.0, 30.0)).is_some());

    let events = park.step(1.0);
    assert!(events.len() >= 10);
}

#[test]
fn micro_jump_and_mjfg_capitals_and_marauders() {
    let mut park = Ballpark::new(30_000_142);
    // Missing caster error.
    assert!(park.micro_jump(9999, 100_000.0).is_err());

    // Static / structure error.
    assert!(park.add_ball(make_ship(10, 35832, DVec3::ZERO)));
    park.set_ball_slim_meta(
        10,
        SlimItemMeta {
            group_id: 1404,
            category_id: 65,
            ..SlimItemMeta::default()
        },
    );
    assert!(park.micro_jump(10, 100_000.0).is_err());
    assert!(park.micro_jump_field(10, 100_000.0, 6_000.0, 25).is_err());

    // Command Destroyer caster (Bifrost = 37481) with heading along +X.
    let mut caster = make_ship(1, 37481, DVec3::ZERO);
    caster.velocity = DVec3::new(200.0, 0.0, 0.0);
    assert!(park.add_ball(caster));

    // Nearby subcap cruiser (3000m away).
    assert!(park.add_ball(make_ship(2, 622, DVec3::new(3_000.0, 0.0, 0.0))));

    // Nearby capital Dreadnought (Revelation = 19720, group 485) 2000m away.
    assert!(park.add_ball(make_ship(3, 19720, DVec3::new(2_000.0, 0.0, 0.0))));
    park.set_ball_group_id(3, 485);

    // Nearby Bastioned Marauder (Paladin = 28659, group 900, disallow_assistance = true, is_mwd_mjd_disabled = true) 4000m away.
    assert!(park.add_ball(make_ship(4, 28659, DVec3::new(4_000.0, 0.0, 0.0))));
    park.set_ball_group_id(4, 900);
    if let Some(cs) = park.combat_states.get_mut(&4) {
        cs.disallow_assistance = true;
        cs.is_mwd_mjd_disabled = true;
    }

    // Scrambled caster cannot jump.
    park.combat_states.get_mut(&1).unwrap().is_mwd_mjd_disabled = true;
    assert!(park.micro_jump(1, 100_000.0).is_err());
    assert!(park.micro_jump_field(1, 100_000.0, 6_000.0, 25).is_err());
    park.combat_states.get_mut(&1).unwrap().is_mwd_mjd_disabled = false;

    // MJFG with max_jumped_capitals = 0 skips the Dreadnought (3), jumps Caster (1), Cruiser (2), Bastioned Marauder (4).
    let jumped_no_cap = park
        .micro_jump_field_with_capitals(1, 100_000.0, 6_000.0, 25, 0)
        .expect("mjfg should succeed");
    assert_eq!(jumped_no_cap, vec![1, 3, 2, 4].into_iter().filter(|id| *id != 3).collect::<Vec<_>>());
    assert!((park.balls[&3].position.x - 2_000.0).abs() < 1e-6);
    assert!((park.balls[&4].position.x - 104_000.0).abs() < 1e-6);

    // Now MJFG with max_jumped_capitals = 1 includes the Dreadnought.
    let jumped_with_cap = park
        .micro_jump_field_with_capitals(1, 50_000.0, 200_000.0, 25, 1)
        .expect("mjfg with cap should succeed");
    assert!(jumped_with_cap.contains(&3));
    assert!((park.balls[&3].position.x - 52_000.0).abs() < 1e-6);

    // Personal MJD succeeds when unscrambled & out of warp (3_000 + 100_000 + 50_000 + 100_000 = 253_000).
    let pos_after_mjd = park.micro_jump(2, 100_000.0).expect("mjd should succeed");
    assert!((pos_after_mjd.x - 253_000.0).abs() < 1e-6);
}

#[test]
fn wire_slim_item_and_event_encoding_covers_special_entity_types() {
    let mut park = Ballpark::new(30_000_142);

    // park = None resolution across Rigid station (60M..70M), gate (50M..60M), asteroid (type 15), generic celestial, Field, and subwarp ship.
    let rigid_station = Ball {
        mode: MovementMode::Rigid,
        ..make_ship(60_000_123, 57, DVec3::ZERO)
    };
    let meta_st = resolve_slim_item_meta(None, &rigid_station);
    assert_eq!((meta_st.group_id, meta_st.category_id), (15, 3));

    let rigid_gate = Ball {
        mode: MovementMode::Rigid,
        ..make_ship(50_000_123, 29624, DVec3::ZERO)
    };
    let meta_gate = resolve_slim_item_meta(None, &rigid_gate);
    assert_eq!((meta_gate.group_id, meta_gate.category_id), (10, 2));

    let rigid_belt = Ball {
        mode: MovementMode::Rigid,
        ..make_ship(40_000_010, 15, DVec3::ZERO)
    };
    let meta_belt = resolve_slim_item_meta(None, &rigid_belt);
    assert_eq!((meta_belt.group_id, meta_belt.category_id), (9, 2));

    let rigid_other = Ball {
        mode: MovementMode::Rigid,
        ..make_ship(40_000_099, 6, DVec3::ZERO)
    };
    let meta_other = resolve_slim_item_meta(None, &rigid_other);
    assert_eq!((meta_other.group_id, meta_other.category_id), (6, 2));

    let field_ball = Ball {
        mode: MovementMode::Field,
        ..make_ship(80_000_001, 22634, DVec3::ZERO)
    };
    let meta_field = resolve_slim_item_meta(None, &field_ball);
    assert_eq!((meta_field.group_id, meta_field.category_id), (361, 22));

    // Stargate slim item dict (group_id == 10) with jump_target populated.
    let gate_meta = SlimItemMeta {
        group_id: 10,
        category_id: 2,
        owner_id: 1_000_035,
        char_id: Some(90_000_001),
        corp_id: 1_000_035,
        alliance_id: 99_000_010,
        skin_id: Some(11),
        skin_material_set_id: Some(22),
        jump_target: Some((50_000_999, 30_000_144)),
        ..SlimItemMeta::default()
    };
    let gate_dict = slim_item_dict(&rigid_gate, &gate_meta);
    assert!(dict_lookup(&gate_dict, "jumps").is_some());
    assert!(dict_lookup(&gate_dict, "localCorruptionStageAndMaximum").is_some());

    // Wormhole slim item dict (group_id == 988) with wormholeAge & wormholeSize from ballpark.
    let wh_ball = make_ship(210_000_001, 30574, DVec3::new(500.0, 0.0, 0.0));
    assert!(park.add_ball(wh_ball.clone()));
    park.set_ball_wormhole_stages(wh_ball.id, 1, 1.5);
    let wh_meta = SlimItemMeta {
        group_id: 988,
        category_id: 2,
        ..SlimItemMeta::default()
    };
    let wh_dict = slim_item_dict_with_park(Some(&park), &wh_ball, &wh_meta);
    assert_eq!(dict_lookup(&wh_dict, "nebulaType"), Some(&PyRep::Integer(11806)));
    assert_eq!(dict_lookup(&wh_dict, "wormholeAge"), Some(&PyRep::Integer(1)));
    assert_eq!(dict_lookup(&wh_dict, "wormholeSize"), Some(&PyRep::Real(1.5)));

    // NPC slim item dict (category_id == 11) includes standing thresholds.
    let npc_meta = SlimItemMeta {
        group_id: 550,
        category_id: 11,
        ..SlimItemMeta::default()
    };
    let npc_dict = slim_item_dict(&wh_ball, &npc_meta);
    assert_eq!(
        dict_lookup(&npc_dict, "hostile_response_threshold"),
        Some(&PyRep::Real(11.0))
    );

    // Helper wrappers and remaining DestinyEvent variants.
    let _ = encode_add_balls(5, &[&wh_ball]);
    let _ = encode_add_balls_with_type(1, 5, &[&wh_ball]);
    let _ = encode_add_balls_with_park(1, 5, &[&wh_ball], Some(&park));
    let mut raw_buf = Vec::new();
    encode_single_ball(&mut raw_buf, &wh_ball);
    encode_single_ball_with_meta(&mut raw_buf, &wh_ball, 5, Some(&npc_meta));
    let _ = encode_slim_item(&wh_ball, 25, 6);
    let _ = encode_slim_item_with_meta(&wh_ball, &wh_meta);

    for ev in [
        DestinyEvent::SetState { observer_ball_id: wh_ball.id },
        DestinyEvent::Collision { ball_a: 1, ball_b: 2 },
        DestinyEvent::OnTarget {
            mode: "try".into(),
            source_id: 1,
            target_id: 2,
        },
        DestinyEvent::OnActivatingWarp { ball_id: 1 },
        DestinyEvent::OnDeactivatingWarp { ball_id: 1 },
        DestinyEvent::SetBallRadius { ball_id: 1, radius: 75.0 },
        DestinyEvent::SetBallPosition {
            ball_id: 1,
            position: [1.0, 2.0, 3.0],
        },
        DestinyEvent::SetBallVelocity {
            ball_id: 1,
            velocity: [4.0, 5.0, 6.0],
        },
        DestinyEvent::OnAudioActivated,
        DestinyEvent::OnDotVictimUpdated {
            ball_id: 1,
            flat_dps: 0.0,
            pct_dps: 0.0,
            remaining_ticks: 0,
        },
    ] {
        let rep = encode_destiny_event(10, &ev, None);
        assert!(matches!(rep, PyRep::Tuple(ref items) if items.len() == 2));
    }

    // Group-only category fallback in resolve_slim_item_meta (lines 520-530).
    let mut group_park = Ballpark::new(30_000_142);
    let b_cel = make_ship(701, 16, DVec3::ZERO);
    let b_sta = make_ship(702, 57, DVec3::new(100.0, 0.0, 0.0));
    let b_ship = make_ship(703, 587, DVec3::new(200.0, 0.0, 0.0));
    group_park.ball_group_ids.insert(701, 7);
    group_park.ball_group_ids.insert(702, 15);
    group_park.ball_group_ids.insert(703, 25);
    assert_eq!(resolve_slim_item_meta(Some(&group_park), &b_cel).category_id, 2);
    assert_eq!(resolve_slim_item_meta(Some(&group_park), &b_sta).category_id, 3);
    assert_eq!(resolve_slim_item_meta(Some(&group_park), &b_ship).category_id, 6);
}

#[test]
fn grid_axes_cloak_errors_warp_locks_and_mjd_filters() {
    let mut park = Ballpark::new(30_000_142);
    assert!(!park.has_dynamic_balls());

    // Static Field ball adds warp bubble; duplicate static ball rejected.
    let mut st_field = make_ship(9001, 22634, DVec3::ZERO);
    st_field.mode = MovementMode::Field;
    st_field.radius = 5_000.0;
    assert!(park.add_static_ball(st_field.clone(), 373));
    assert!(!park.add_static_ball(st_field, 373));
    assert!(!park.has_dynamic_balls());
    assert!(park.bubble_id_for_ball(9001).is_none());
    assert!(!park.try_expand_grid(999, DVec3::ZERO));
    park.contract_or_delete_grid(999);
    assert!(!park.in_same_bubble(9001, 888_888));
    assert!(park.in_same_bubble(1, 9001) || !park.in_same_bubble(9001, 9002));

    // Create grids overlapping on X, Y, Z to cover all 6 axis clamp branches.
    assert!(park.add_ball(make_ship(1, 587, DVec3::ZERO)));
    assert!(park.has_dynamic_balls());
    assert!(!park.add_ball(make_ship(1, 587, DVec3::ZERO)));
    assert!(park.set_ball_wormhole_stages(1, 3, 1.5));
    assert!(!park.set_ball_wormhole_stages(1, 3, 1.5));

    assert!(park.add_ball(make_ship(2, 587, DVec3::new(10_000_000.0, 0.0, 0.0))));
    assert!(park.add_ball(make_ship(3, 587, DVec3::new(-10_000_000.0, 0.0, 0.0))));
    assert!(park.add_ball(make_ship(4, 587, DVec3::new(0.0, 10_000_000.0, 0.0))));
    assert!(park.add_ball(make_ship(5, 587, DVec3::new(0.0, -10_000_000.0, 0.0))));
    assert!(park.add_ball(make_ship(6, 587, DVec3::new(0.0, 0.0, 10_000_000.0))));
    assert!(park.add_ball(make_ship(7, 587, DVec3::new(0.0, 0.0, -10_000_000.0))));
    assert!(park.in_same_bubble(1, 9001));
    assert!(park.bubble_id_for_ball(9001).is_some());

    let gid1 = park.bubble_id_for_ball(1).unwrap();
    assert!(!park.try_expand_grid(gid1, DVec3::new(20_000_000.0, 0.0, 0.0)));

    // Move dynamic ball from Grid 1 to Grid 2 with neighbors in both grids to emit RemoveBalls/AddBalls/AddBalls2.
    assert!(park.add_ball(make_ship(8, 587, DVec3::new(100.0, 0.0, 0.0))));
    assert!(!park.set_ball_position(1, DVec3::new(f64::NAN, 0.0, 0.0)));
    assert!(!park.set_ball_position(999_999, DVec3::ZERO));
    assert!(park.set_ball_position(8, DVec3::new(10_000_100.0, 0.0, 0.0)));
    assert_eq!(park.bubble_id_for_ball(8), park.bubble_id_for_ball(2));

    // Update existing interdiction sphere & mobile warp disruptor.
    assert!(park.add_interdiction_sphere(9010, DVec3::new(50_000.0, 0.0, 0.0), 10_000.0));
    assert!(park.add_interdiction_sphere(9010, DVec3::new(60_000.0, 0.0, 0.0), 15_000.0));
    assert!(park.add_mobile_warp_disruptor(9011, DVec3::new(-50_000.0, 0.0, 0.0), 10_000.0));
    assert!(park.add_mobile_warp_disruptor(9011, DVec3::new(-60_000.0, 0.0, 0.0), 15_000.0));
    assert!(!park.set_warp_scramble_strength(999_999, 2));
    assert!(!park.set_warp_core_strength(999_999, 2));

    // Cloak errors (missing ball, proximity <= 2000m, targeted) and decloak restoration.
    assert!(park.activate_cloak(999_999, false, 0.0, 10.0).is_err());
    assert!(park.add_ball(make_ship(9, 587, DVec3::new(500.0, 0.0, 0.0))));
    assert!(park.activate_cloak(1, false, 0.0, 10.0).is_err());

    assert!(park.add_ball(make_ship(10, 587, DVec3::new(300_000.0, 0.0, 0.0))));
    assert!(park.add_ball(make_ship(11, 587, DVec3::new(310_000.0, 0.0, 0.0))));
    assert!(park.add_target_lock(
        11,
        TargetLock {
            target_id: 10,
            lock_time_ms: 1000.0,
            remaining_lock_ms: 0.0,
            locked: true,
            max_range_m: None,
        }
    ));
    assert!(park.activate_cloak(10, false, 4.0, 10.0).is_err());
    assert!(park.remove_target_lock(11, 10));

    // Non-covert cloak blocks warp; deactivate_cloak restores speed & sets targeting delay; gate cloak deactivates cleanly.
    assert!(park.activate_cloak(10, false, 4.0, 10.0).is_ok());
    park.combat_states.get_mut(&10).unwrap().cloak_original_max_velocity = Some(410.0);
    assert!(park
        .set_mode(
            10,
            MovementMode::new_warp(DVec3::new(300_000.0, 0.0, 0.0), DVec3::new(50_000_000.0, 0.0, 0.0), 3.0),
        )
        .is_err());
    assert!(park.deactivate_cloak(10));
    assert!(!park.deactivate_cloak(10));
    assert_eq!(park.balls[&10].max_velocity, 410.0);
    assert_eq!(park.combat_states[&10].decloak_targeting_delay_remaining_s, 4.0);

    // Gate cloak deactivate_cloak, set_mode clearing locks & gate cloak, set_speed_fraction clearing gate cloak.
    park.combat_states.get_mut(&10).unwrap().gate_cloak_remaining_ticks = 30;
    park.combat_states.get_mut(&10).unwrap().is_cloaked = true;
    assert!(park.deactivate_cloak(10));

    park.combat_states.get_mut(&11).unwrap().gate_cloak_remaining_ticks = 30;
    park.combat_states.get_mut(&11).unwrap().is_cloaked = true;
    assert!(park.set_speed_fraction(11, 0.5));
    assert!(!park.combat_states[&11].is_cloaked);

    // Emergency warp & clearing locks on warp initiation + clearing gate cloak on set_mode.
    assert!(park.add_target_lock(11, TargetLock { target_id: 9, lock_time_ms: 1000.0, remaining_lock_ms: 0.0, locked: true, max_range_m: None }));
    park.combat_states.get_mut(&11).unwrap().gate_cloak_remaining_ticks = 15;
    park.combat_states.get_mut(&11).unwrap().is_cloaked = true;
    park.combat_states.get_mut(&11).unwrap().is_emergency_warping = true;
    assert!(park.set_warp_scramble_strength(11, 2));
    let warp_mode = MovementMode::new_warp(DVec3::new(310_000.0, 0.0, 0.0), DVec3::new(50_000_000.0, 0.0, 0.0), 3.0);
    assert!(park.set_mode(11, warp_mode.clone()).is_err());
    assert!(park.set_warp_core_strength(11, 3));
    assert!(park.set_mode(11, warp_mode).is_ok());
    assert!(park.get_target_locks(11).is_empty());
    assert!(!park.combat_states[&11].is_cloaked);

    // MJD / MJFG unknown, in-warp, heading_for_ball Stop/zero GotoDirection, and MJFG candidate exclusions.
    assert!(park.micro_jump(999_999, 100_000.0).is_err());
    assert!(park.micro_jump_field(999_999, 100_000.0, 6_000.0, 25).is_err());
    if let MovementMode::Warp { ref mut phase, .. } = park.balls.get_mut(&11).unwrap().mode { *phase = WarpPhase::Cruising; }
    assert!(park.micro_jump(11, 100_000.0).is_err());
    assert!(park.micro_jump_field(11, 100_000.0, 6_000.0, 25).is_err());

    // Zero GotoDirection heading fallback + SDE capital lookup with group_id = 0.
    assert!(park.add_ball(make_ship(12, 19724, DVec3::new(500_000.0, 0.0, 0.0))));
    park.balls.get_mut(&12).unwrap().mode = MovementMode::GotoDirection { direction: DVec3::ZERO };
    let cap_pos = park.micro_jump(12, 100_000.0).expect("capital 250km jump");
    assert!((cap_pos.x - 750_000.0).abs() < 1e-6);

    // MJFG candidate filtering: excluded group (6), cloaked, max_jumped_ships truncation.
    assert!(park.add_ball(make_ship(13, 587, DVec3::new(750_100.0, 0.0, 0.0))));
    park.set_ball_group_id(13, 6);
    assert!(park.add_ball(make_ship(14, 587, DVec3::new(750_200.0, 0.0, 0.0))));
    park.combat_states.get_mut(&14).unwrap().is_cloaked = true;
    assert!(park.add_ball(make_ship(15, 587, DVec3::new(750_300.0, 0.0, 0.0))));
    assert!(park.add_ball(make_ship(16, 587, DVec3::new(750_400.0, 0.0, 0.0))));
    let limited = park.micro_jump_field(12, 10_000.0, 5_000.0, 2).expect("mjfg limit 2");
    assert_eq!(limited, vec![12, 15]);
}

