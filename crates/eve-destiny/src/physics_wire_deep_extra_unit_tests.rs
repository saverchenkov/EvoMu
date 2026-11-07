//! Deep branch-coverage and mutation-killing unit tests for `physics.rs` and `wire.rs`.

use super::*;
use crate::physics::{
    combat, compute_formation_goal_velocity, compute_subwarp_goal_velocity,
    formation_leader_frame, integrate_subwarp, step_missile_ball, step_warp_ball,
};
use crate::wire::{
    decode_add_balls, encode_add_balls, encode_damage_state_tuple, encode_destiny_event,
    encode_set_state_payload, encode_single_ball_with_park, resolve_slim_item_meta, slim_item_dict,
    BALL_FLAG_IS_GLOBAL,
};
use eve_proto::PyRep;

fn make_ship(id: BallId, pos: DVec3, mode: MovementMode) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 50.0,
        speed_fraction: 1.0,
        mode,
    }
}

#[test]
fn combat_formulas_and_subwarp_orbit_formation_missile_warp_kinematics() {
    // combat turret zero falloff inside vs outside optimal + signature resolution sensitivity
    assert_eq!(
        combat::compute_turret_chance_to_hit_with_res(0.0, 0.1, 125.0, 125.0, 5_000.0, 5_000.0, 0.0),
        1.0
    );
    let out_zero_falloff = combat::compute_turret_chance_to_hit_with_res(
        0.0, 0.1, 125.0, 125.0, 5_001.0, 5_000.0, 0.0,
    );
    assert!((out_zero_falloff - 0.5_f64.powi(100)).abs() < 1e-12);
    let small_res = combat::compute_turret_chance_to_hit_with_res(
        0.05, 0.1, 125.0, 125.0, 1_000.0, 5_000.0, 5_000.0,
    );
    let large_res = combat::compute_turret_chance_to_hit_with_res(
        0.05, 0.1, 40_000.0, 125.0, 1_000.0, 5_000.0, 5_000.0,
    );
    assert!(small_res > large_res);

    // Follow: max_spd <= 0, exact stop point, and quadratic braking inside 1 tick
    let mut positions = BTreeMap::new();
    positions.insert(2, (DVec3::ZERO, DVec3::ZERO, 50.0));
    let mut follower = make_ship(
        1,
        DVec3::new(1_100.0, 0.0, 0.0),
        MovementMode::Follow {
            target_id: 2,
            range: 1_000.0,
        },
    );
    follower.speed_fraction = 0.0;
    assert_eq!(
        compute_subwarp_goal_velocity(&follower, &positions, 1.0),
        DVec3::ZERO
    );
    follower.speed_fraction = 1.0;
    assert_eq!(
        compute_subwarp_goal_velocity(&follower, &positions, 1.0),
        DVec3::ZERO
    );
    // 100m away from 1,100m stop point with step_dist = 300m -> ratio = (100/300)^2 = 1/9
    follower.position.x = 1_200.0;
    let brake_vel = compute_subwarp_goal_velocity(&follower, &positions, 1.0);
    assert!((brake_vel.x - (-300.0 * (1.0 / 81.0))).abs() < 1e-9);

    // Orbit branches: coincident target, exact radius d12==0, tiny x<=1e-12, extreme x<-0.994, huge tau fallback, unclipped raw_len<=v_max
    let mut orb = make_ship(
        3,
        DVec3::ZERO,
        MovementMode::Orbit {
            target_id: 2,
            range: 900.0,
        },
    );
    assert_eq!(
        compute_subwarp_goal_velocity(&orb, &positions, 1.0),
        DVec3::ZERO
    );

    // Exact orbit radius: dist = 1000 == range(900) + rad(50) + target_rad(50) => d12 == 0
    orb.position = DVec3::new(1_000.0, 0.0, 0.0);
    let exact_ring = compute_subwarp_goal_velocity(&orb, &positions, 1.0);
    assert!(exact_ring.length() > 0.0 && exact_ring.length() <= 300.0 + 1e-6);

    // Tiny non-zero x (|d12| >= 1e-12, |x| <= 1e-12)
    let mut tiny_x_ship = make_ship(
        4,
        DVec3::new(1_000.0 - 1e-8, 0.0, 0.0),
        MovementMode::Orbit {
            target_id: 2,
            range: 900.0,
        },
    );
    tiny_x_ship.max_velocity = 10.0;
    let tiny_x_vel = compute_subwarp_goal_velocity(&tiny_x_ship, &positions, 0.05);
    assert!(tiny_x_vel.is_finite() && tiny_x_vel.length() > 0.0);

    // Extreme inward spiral x < -0.994_014_667_510_986_3
    let mut extreme_orb = make_ship(
        5,
        DVec3::new(200.0, 0.0, 0.0),
        MovementMode::Orbit {
            target_id: 2,
            range: 0.0,
        },
    );
    extreme_orb.radius = 25.0;
    extreme_orb.max_velocity = 10.0;
    extreme_orb.inertia = 0.01;
    extreme_orb.mass = 1_000_000.0;
    positions.insert(20, (DVec3::ZERO, DVec3::ZERO, 25.0));
    extreme_orb.mode = MovementMode::Orbit {
        target_id: 20,
        range: 0.0,
    };
    let extreme_vel = compute_subwarp_goal_velocity(&extreme_orb, &positions, 30.0);
    assert!(extreme_vel.is_finite());

    // Huge tau fallback (pos_denom.abs() <= 1e-12) and raw_len <= v_max unclipped
    let mut heavy_orb = make_ship(
        6,
        DVec3::new(1_000.0, 0.0, 0.0),
        MovementMode::Orbit {
            target_id: 2,
            range: 900.0,
        },
    );
    heavy_orb.inertia = 1e9;
    heavy_orb.mass = 1e9;
    let heavy_vel = compute_subwarp_goal_velocity(&heavy_orb, &positions, 1.0);
    assert!(heavy_vel.length() <= 300.0 + 1e-6);

    let mut unclipped_orb = make_ship(
        7,
        DVec3::new(10.0, 0.0, 0.0),
        MovementMode::Orbit {
            target_id: 2,
            range: 0.0,
        },
    );
    unclipped_orb.radius = 5.0;
    positions.insert(2, (DVec3::ZERO, DVec3::ZERO, 5.0));
    unclipped_orb.max_velocity = 1_000.0;
    unclipped_orb.inertia = 0.1;
    let unclipped_vel = compute_subwarp_goal_velocity(&unclipped_orb, &positions, 1.0);
    assert!(unclipped_vel.length() > 0.0 && unclipped_vel.length() < 1_000.0);

    // formation_leader_frame with stationary GotoDirection along +Y and ZERO, plus compute_formation_goal_velocity
    let leader_y = make_ship(
        8,
        DVec3::ZERO,
        MovementMode::GotoDirection {
            direction: DVec3::new(0.0, 4.0, 0.0),
        },
    );
    let (_, _, up_y, fwd_y) = formation_leader_frame(&leader_y);
    assert_eq!(fwd_y, DVec3::Y);
    assert_eq!(up_y, DVec3::Z);

    let leader_zero = make_ship(
        9,
        DVec3::ZERO,
        MovementMode::GotoDirection {
            direction: DVec3::ZERO,
        },
    );
    assert_eq!(formation_leader_frame(&leader_zero).3, DVec3::X);

    let mut form_ship = make_ship(10, DVec3::new(100.0, 0.0, 0.0), MovementMode::Formation);
    assert_eq!(
        compute_formation_goal_velocity(&form_ship, DVec3::new(100.0, 0.0, 0.0), 1.0),
        DVec3::ZERO
    );
    let form_near = compute_formation_goal_velocity(&form_ship, DVec3::new(250.0, 0.0, 0.0), 1.0);
    assert!((form_near.x - 75.0).abs() < 1e-9);

    // step_missile_ball & integrate_subwarp with dt <= 0.0
    step_missile_ball(&mut form_ship, Some(DVec3::ZERO), 0.0);
    assert_eq!(form_ship.position.x, 100.0);
    integrate_subwarp(&mut form_ship, DVec3::new(300.0, 0.0, 0.0), 0.0);
    assert_eq!(form_ship.position.x, 100.0);

    // step_warp_ball: origin == destination fallback, short v_peak <= k_a warp, and full 10 AU Cruising warp
    let mut short_warper = make_ship(11, DVec3::new(9.5, 0.0, 0.0), MovementMode::Stop);
    let mut phase = WarpPhase::Accelerating;
    let mut elapsed = 0.0;
    let mut exited_short = false;
    for _ in 0..3 {
        if step_warp_ball(
            &mut short_warper,
            1.0,
            DVec3::new(10.0, 0.0, 0.0),
            DVec3::new(10.0, 0.0, 0.0),
            3.0,
            1.0,
            &mut phase,
            &mut elapsed,
        ) {
            exited_short = true;
            break;
        }
    }
    assert!(exited_short && matches!(short_warper.mode, MovementMode::Stop));

    let mut cruise_warper = make_ship(12, DVec3::ZERO, MovementMode::Stop);
    let dest_10au = DVec3::new(10.0 * AU_METERS, 0.0, 0.0);
    let mut c_phase = WarpPhase::Accelerating;
    let mut c_elapsed = 0.0;
    let mut saw_cruising = false;
    let mut completed = false;
    for _ in 0..60 {
        let done = step_warp_ball(
            &mut cruise_warper,
            1.0,
            DVec3::ZERO,
            dest_10au,
            3.0,
            10.0 * AU_METERS,
            &mut c_phase,
            &mut c_elapsed,
        );
        if c_phase == WarpPhase::Cruising {
            saw_cruising = true;
            assert!((cruise_warper.velocity.x - 3.0 * AU_METERS).abs() < 1.0);
        }
        if done {
            completed = true;
            break;
        }
    }
    assert!(saw_cruising);
    assert!(completed);
}

#[test]
fn step_mushroom_troll_post_align_intercept_gate_cloak_breacher_and_locks() {
    let mut park = Ballpark::new(30_000_142);

    // Mushroom with duration_s = 0.0 expires; future effect_stamp stays radius 0; 1/16 elapsed -> 0.5 * max_radius
    park.add_ball(make_ship(1, DVec3::ZERO, MovementMode::Mushroom));
    park.mushroom_states.insert(
        1,
        MushroomState {
            max_radius: 1_000.0,
            duration_s: 0.0,
            effect_stamp: 0,
            owner_id: 99,
        },
    );
    park.add_mushroom_ball(2, 99, DVec3::new(5_000.0, 0.0, 0.0), 800.0, 16.0, 1.0, 99);
    park.mushroom_states.get_mut(&2).unwrap().effect_stamp = 10; // future stamp
    park.add_mushroom_ball(3, 99, DVec3::new(10_000.0, 0.0, 0.0), 800.0, 16.0, 1.0, 99);

    // Troll ball without troll_states entry coasts cleanly
    let mut free_troll = make_ship(4, DVec3::new(20_000.0, 0.0, 0.0), MovementMode::Troll);
    free_troll.velocity = DVec3::new(100.0, 0.0, 0.0);
    park.add_ball(free_troll);

    park.step(1.0);
    assert!(!park.balls.contains_key(&1));
    assert_eq!(park.balls[&2].radius, 0.0);
    assert!((park.balls[&3].radius - 400.0).abs() < 1e-6);
    assert_eq!(park.balls[&4].mode, MovementMode::Troll);
    assert!(park.balls[&4].position.x > 20_000.0);

    // Post-alignment warp bubble interception when parallel component < 0.75 * max_vel before step_warp_ball
    let mut warp_park = Ballpark::new(30_000_142);
    warp_park.add_interdiction_sphere(90, DVec3::new(99_800_000.0, 0.0, 0.0), 25_000.0);
    let mut warper = make_ship(
        10,
        DVec3::ZERO,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(100_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 100_000_000.0,
            elapsed_warp_time: 0.0,
            phase: WarpPhase::Aligning,
        },
    );
    warper.max_velocity = 1_000.0;
    warper.velocity = DVec3::new(749.9, 87.5, 0.0); // speed = 754.99 > 750, cos = 0.9933 > 0.99, dot_x = 749.9 < 750
    warp_park.add_ball(warper);
    warp_park.step(1.0);
    if let MovementMode::Warp {
        destination,
        phase,
        ..
    } = warp_park.balls[&10].mode
    {
        assert_ne!(phase, WarpPhase::Aligning);
        assert!((destination.x - 99_775_000.0).abs() < 1.0);
    } else {
        panic!("expected Warp mode after alignment transition");
    }

    // Stationary gate-cloaked ship decrements gate_cloak_remaining_ticks each tick
    let mut gc_ship = make_ship(11, DVec3::new(1_000.0, 0.0, 0.0), MovementMode::Stop);
    gc_ship.speed_fraction = 0.0;
    warp_park.add_ball(gc_ship);
    warp_park.set_ball_slim_meta(
        11,
        SlimItemMeta {
            is_cloaked: true,
            ..Default::default()
        },
    );
    warp_park
        .combat_states
        .get_mut(&11)
        .unwrap()
        .gate_cloak_remaining_ticks = 2;
    warp_park.step(1.0);
    assert_eq!(warp_park.combat_states[&11].gate_cloak_remaining_ticks, 1);
    assert!(warp_park.ball_slim_meta[&11].is_cloaked);
    warp_park.step(1.0);
    assert_eq!(warp_park.combat_states[&11].gate_cloak_remaining_ticks, 0);
    assert!(!warp_park.ball_slim_meta[&11].is_cloaked);

    // Breacher DoT on zero-max-HP victim + pct_dps cap + subwarp grid expansion + missing grid_id + lock bounds
    let mut dot_park = Ballpark::new(30_000_142);
    let mut dot_victim = make_ship(20, DVec3::new(8_000_010.0, 0.0, 0.0), MovementMode::Stop);
    dot_victim.speed_fraction = 0.0;
    dot_park.add_ball(dot_victim);
    // Anchor grid at 0..=8_000_000 then place ball at 8_000_010 so subwarp step expands grid!
    dot_park.grids.get_mut(&1).unwrap().max_bounds = DVec3::splat(8_000_000.0);
    dot_park.grids.get_mut(&1).unwrap().min_bounds = DVec3::splat(-8_000_000.0);
    dot_park.grids.get_mut(&1).unwrap().center = DVec3::ZERO;
    {
        let cs = dot_park.combat_states.get_mut(&20).unwrap();
        cs.max_shield_hp = 0.0;
        cs.max_armor_hp = 0.0;
        cs.max_structure_hp = 0.0;
        cs.shield_hp = 0.0;
        cs.armor_hp = 0.0;
        cs.structure_hp = 100.0;
    }
    dot_park.apply_breacher_dot(20, 50.0, 0.1, 3);

    // Unlimited-range lock (max_range_m = None) stays locked across grids, and missing ball_grid_ids gets recovered
    let mut watcher = make_ship(21, DVec3::ZERO, MovementMode::Stop);
    watcher.speed_fraction = 0.0;
    dot_park.add_ball(watcher);
    assert!(dot_park.add_target_lock(
        21,
        TargetLock {
            target_id: 20,
            lock_time_ms: 500.0,
            remaining_lock_ms: 500.0,
            locked: false,
            max_range_m: None,
        }
    ));
    dot_park.ball_grid_ids.remove(&21);

    // Mushroom without state + source entering warp drops active target lock
    dot_park.add_ball(make_ship(22, DVec3::new(500.0, 0.0, 0.0), MovementMode::Mushroom));
    dot_park.add_ball(make_ship(
        23,
        DVec3::ZERO,
        MovementMode::Warp {
            origin: DVec3::ZERO,
            destination: DVec3::new(1_000_000.0, 0.0, 0.0),
            warp_speed_au_s: 3.0,
            total_distance: 1_000_000.0,
            elapsed_warp_time: 0.0,
            phase: WarpPhase::Aligning,
        },
    ));
    assert!(dot_park.add_target_lock(
        23,
        TargetLock {
            target_id: 20,
            lock_time_ms: 500.0,
            remaining_lock_ms: 500.0,
            locked: true,
            max_range_m: None,
        }
    ));

    // Sub-warp ship crossing beyond 10,000 km grid extent from grid 1 splits into a new grid
    let mut subwarp_splitter = make_ship(
        24,
        DVec3::ZERO,
        MovementMode::GotoDirection {
            direction: DVec3::X,
        },
    );
    subwarp_splitter.max_velocity = 500.0;
    dot_park.add_ball(subwarp_splitter);
    dot_park.balls.get_mut(&24).unwrap().position.x = 9_999_900.0;
    dot_park.balls.get_mut(&24).unwrap().velocity = DVec3::new(500.0, 0.0, 0.0);
    let gid24_before = dot_park.ball_grid_ids[&24];

    dot_park.step(1.0);
    assert!(dot_park.get_target_locks(23).is_empty());
    assert_ne!(dot_park.ball_grid_ids[&24], gid24_before);
    assert!(dot_park.ball_grid_ids.contains_key(&21));
    assert_eq!(dot_park.get_target_locks(21).len(), 1);
    assert!(dot_park.grids[&1].max_bounds.x >= 8_000_010.0);
}

#[test]
fn wire_slim_meta_damage_state_set_state_and_destiny_event_edge_cases() {
    // resolve_slim_item_meta with None on Rigid boundary IDs and Field
    let mut rigid_parked = make_ship(50_000_000, DVec3::ZERO, MovementMode::Rigid);
    assert_eq!(resolve_slim_item_meta(None, &rigid_parked).group_id, 10);
    rigid_parked.id = 49_999_999;
    assert_eq!(resolve_slim_item_meta(None, &rigid_parked).group_id, 6);
    rigid_parked.id = 60_000_000;
    assert_eq!(resolve_slim_item_meta(None, &rigid_parked).group_id, 15);
    rigid_parked.id = 70_000_000;
    assert_eq!(resolve_slim_item_meta(None, &rigid_parked).group_id, 6);
    rigid_parked.type_id = 15;
    assert_eq!(resolve_slim_item_meta(None, &rigid_parked).group_id, 9);

    let field_ball = make_ship(10, DVec3::ZERO, MovementMode::Field);
    let field_meta = resolve_slim_item_meta(None, &field_ball);
    assert_eq!((field_meta.group_id, field_meta.category_id), (361, 22));
    let mut single_buf = vec![0, 0, 0, 0, 1];
    encode_single_ball_with_park(&mut single_buf, &field_ball, 0, None, None);
    let (_, _, single_decoded) = decode_add_balls(&single_buf).unwrap();
    assert_eq!(single_decoded[0].flags, BALL_FLAG_IS_GLOBAL);

    // Park group_id fallback without explicit SlimItemMeta
    let mut park = Ballpark::new(30_000_142);
    let dyn_ball = make_ship(20, DVec3::ZERO, MovementMode::Mushroom);
    park.add_ball(dyn_ball.clone());
    park.set_ball_group_id(20, 15);
    park.ball_slim_meta.remove(&20);
    let resolved_15 = resolve_slim_item_meta(Some(&park), &dyn_ball);
    assert_eq!((resolved_15.group_id, resolved_15.category_id), (15, 3));

    // Empty decode_add_balls roundtrip
    let empty_bytes = encode_add_balls(9, &[]);
    let (pkt_type, stamp, decoded) = decode_add_balls(&empty_bytes).unwrap();
    assert_eq!((pkt_type, stamp, decoded.len()), (0, 9, 0));

    // slim_item_dict None branches & group 10 without jump_target & group 988
    let bare_meta = SlimItemMeta {
        group_id: 10,
        category_id: 2,
        owner_id: 1,
        char_id: None,
        corp_id: 0,
        alliance_id: -1,
        security_status: 0.0,
        is_cloaked: false,
        skin_id: None,
        skin_material_set_id: None,
        jump_target: None,
        ..SlimItemMeta::default()
    };
    if let PyRep::Dict(entries) = slim_item_dict(&dyn_ball, &bare_meta) {
        let map: std::collections::BTreeMap<String, PyRep> = entries
            .into_iter()
            .filter_map(|(k, v)| match k {
                PyRep::String(s) => Some((s, v)),
                _ => None,
            })
            .collect();
        assert_eq!(map["corpID"], PyRep::None);
        assert_eq!(map["allianceID"], PyRep::None);
        assert_eq!(map["jumps"], PyRep::List(vec![]));
    } else {
        panic!("expected PyRep::Dict from slim_item_dict");
    }

    // encode_damage_state_tuple(None, stamp) and zero max HP clamping
    assert_eq!(
        encode_damage_state_tuple(None, 77),
        PyRep::Tuple(vec![
            PyRep::Tuple(vec![
                PyRep::Real(1.0),
                PyRep::Real(120_000.0),
                PyRep::Integer(77),
            ]),
            PyRep::Real(1.0),
            PyRep::Real(1.0),
        ])
    );

    // encode_set_state_payload excludes static balls from damageState dict
    let mut stat = make_ship(60_000_123, DVec3::new(100.0, 0.0, 0.0), MovementMode::Rigid);
    stat.radius = 500.0;
    park.add_static_ball(stat, 15);
    park.set_ball_slim_meta(
        20,
        SlimItemMeta {
            corp_id: 98_000_001,
            alliance_id: 99_000_001,
            ..Default::default()
        },
    );
    let set_state = encode_set_state_payload(&park, 20);
    if let PyRep::Object { args, .. } = set_state {
        if let PyRep::Dict(kv) = *args {
            let dmg_entry = kv
                .into_iter()
                .find(|(k, _)| *k == PyRep::String("damageState".into()))
                .unwrap()
                .1;
            if let PyRep::Dict(dmg_rows) = dmg_entry {
                assert!(dmg_rows.iter().any(|(id, _)| *id == PyRep::Integer(20)));
                assert!(!dmg_rows
                    .iter()
                    .any(|(id, _)| *id == PyRep::Integer(60_000_123)));
            }
        }
    }

    // encode_destiny_event exact field rounding & None/zero branches
    let warp_ev = encode_destiny_event(
        5,
        &DestinyEvent::WarpTo {
            ball_id: 20,
            destination: [1.0, 2.0, 3.0],
            warp_speed_au_s: 2.75,
        },
        Some(&park),
    );
    if let PyRep::Tuple(top) = warp_ev {
        if let PyRep::Tuple(call) = &top[1] {
            if let PyRep::Tuple(args) = &call[1] {
                assert_eq!(args[5], PyRep::Integer(28));
            }
        }
    }

    let dot_ev = encode_destiny_event(
        10,
        &DestinyEvent::OnDotVictimUpdated {
            ball_id: 20,
            flat_dps: 12.5,
            pct_dps: 0.02,
            remaining_ticks: 5,
        },
        None,
    );
    if let PyRep::Tuple(top) = dot_ev {
        if let PyRep::Tuple(call) = &top[1] {
            if let PyRep::Tuple(args) = &call[1] {
                if let PyRep::List(items) = &args[1] {
                    if let PyRep::Object { args: kv, .. } = &items[0] {
                        if let PyRep::Dict(entries) = kv.as_ref() {
                            assert!(entries.iter().any(|(k, v)| {
                                *k == PyRep::String("expiryTime".into())
                                    && *v == PyRep::Integer(150_000_000)
                            }));
                        }
                    }
                }
            }
        }
    }
    let dot_clear_ev = encode_destiny_event(
        10,
        &DestinyEvent::OnDotVictimUpdated {
            ball_id: 20,
            flat_dps: 0.0,
            pct_dps: 0.0,
            remaining_ticks: 0,
        },
        None,
    );
    if let PyRep::Tuple(top) = dot_clear_ev {
        if let PyRep::Tuple(call) = &top[1] {
            if let PyRep::Tuple(args) = &call[1] {
                assert_eq!(args[1], PyRep::List(vec![]));
            }
        }
    }
}
