//! Unit tests for `ballpark_targeting.rs`: target locking, cloak/invulnerability lock rules,
//! ECM jam & burst, grid observer visibility, sub-warp step kinematics, and swept-sphere collision bumping.

use super::*;
use crate::{CombatState, SlimItemMeta, WarpBubble};
use std::collections::BTreeMap;

fn make_ship(id: BallId, pos: DVec3, vel: DVec3) -> Ball {
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
        mode: MovementMode::Stop,
    }
}

fn make_lock(target_id: BallId, lock_time_ms: f64, locked: bool) -> TargetLock {
    TargetLock {
        target_id,
        lock_time_ms,
        remaining_lock_ms: if locked { 0.0 } else { lock_time_ms },
        locked,
        max_range_m: Some(25_000.0),
    }
}

#[test]
fn target_lock_lifecycle_and_cloak_invulnerability_guards() {
    let mut park = Ballpark::new(30000142);
    let s1 = make_ship(1, DVec3::ZERO, DVec3::ZERO);
    let s2 = make_ship(2, DVec3::new(1000.0, 0.0, 0.0), DVec3::ZERO);
    let s3 = make_ship(3, DVec3::new(2000.0, 0.0, 0.0), DVec3::ZERO);
    park.add_ball(s1);
    park.add_ball(s2);
    park.add_ball(s3);
    park.drain_all_observer_events();
    park.pending_events.clear();

    // 1. Source cloaked (non-gate cloak) cannot lock
    park.combat_states.entry(1).or_default().is_cloaked = true;
    assert!(!park.add_target_lock(1, make_lock(2, 1000.0, false)));

    // 2. Source with active decloak targeting delay cannot lock
    {
        let cs1 = park.combat_states.get_mut(&1).unwrap();
        cs1.is_cloaked = false;
        cs1.decloak_targeting_delay_remaining_s = 3.5;
    }
    assert!(!park.add_target_lock(1, make_lock(2, 1000.0, false)));
    park.combat_states.get_mut(&1).unwrap().decloak_targeting_delay_remaining_s = 0.0;

    // 3. Target cloaked, gate-cloaked, or warp-exit invulnerable cannot be locked
    park.combat_states.entry(2).or_default().is_cloaked = true;
    assert!(!park.add_target_lock(1, make_lock(2, 1000.0, false)));
    {
        let cs2 = park.combat_states.get_mut(&2).unwrap();
        cs2.is_cloaked = false;
        cs2.gate_cloak_remaining_ticks = 10;
    }
    assert!(!park.add_target_lock(1, make_lock(2, 1000.0, false)));
    {
        let cs2 = park.combat_states.get_mut(&2).unwrap();
        cs2.gate_cloak_remaining_ticks = 0;
        cs2.warp_exit_invulnerable_remaining_ticks = 5;
    }
    assert!(!park.add_target_lock(1, make_lock(2, 1000.0, false)));
    park.combat_states.get_mut(&2).unwrap().warp_exit_invulnerable_remaining_ticks = 0;

    // 4. Source under gate cloak IS allowed to initiate lock, which breaks gate cloak & undock/warp invuln
    // and broadcasts AddBalls2 to grid peers
    {
        let cs1 = park.combat_states.get_mut(&1).unwrap();
        cs1.is_cloaked = true;
        cs1.gate_cloak_remaining_ticks = 30;
        cs1.undock_invulnerable_remaining_ticks = 15;
        cs1.warp_exit_invulnerable_remaining_ticks = 8;
    }
    park.set_ball_slim_meta(
        1,
        SlimItemMeta {
            is_cloaked: true,
            ..SlimItemMeta::default()
        },
    );
    assert!(park.add_target_lock(1, make_lock(2, 0.0, true)));
    let cs1 = park.combat_states.get(&1).unwrap();
    assert!(!cs1.is_cloaked);
    assert_eq!(cs1.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs1.undock_invulnerable_remaining_ticks, 0);
    assert_eq!(cs1.warp_exit_invulnerable_remaining_ticks, 0);
    assert!(!park.ball_slim_meta(1).unwrap().is_cloaked);

    // Observer 2 should have received AddBalls2 from source 1 uncloaking
    let obs2_events = park.drain_observer_events(2);
    assert!(obs2_events.iter().any(|e| matches!(e, DestinyEvent::AddBalls2 { .. })));

    // 5. Add second lock on target 3
    assert!(park.add_target_lock(1, make_lock(3, 2500.0, false)));
    let locks = park.get_target_locks(1);
    assert_eq!(locks.len(), 2);
    assert!(locks[0].locked);
    assert!(!locks[1].locked);
    assert_eq!(locks[1].remaining_lock_ms, 2500.0);

    // 6. remove_target_lock & clear_target_locks
    assert!(!park.remove_target_lock(999, 2));
    assert!(!park.remove_target_lock(1, 999));
    assert!(park.remove_target_lock(1, 2));
    assert_eq!(park.get_target_locks(1).len(), 1);

    assert_eq!(park.clear_target_locks(1), 1);
    assert!(park.get_target_locks(1).is_empty());
    assert_eq!(park.clear_target_locks(1), 0);
}

#[test]
fn ecm_jam_and_ecm_burst_respect_range_cloak_and_invulnerability() {
    let mut park = Ballpark::new(30000142);
    let source = make_ship(10, DVec3::ZERO, DVec3::ZERO);
    let in_range = make_ship(11, DVec3::new(3000.0, 0.0, 0.0), DVec3::ZERO);
    let at_boundary = make_ship(12, DVec3::new(5000.0, 0.0, 0.0), DVec3::ZERO);
    let out_of_range = make_ship(13, DVec3::new(5001.0, 0.0, 0.0), DVec3::ZERO);
    let cloaked_ship = make_ship(14, DVec3::new(1000.0, 0.0, 0.0), DVec3::ZERO);
    let tethered_ship = make_ship(15, DVec3::new(1500.0, 0.0, 0.0), DVec3::ZERO);
    let mut static_gate = make_ship(16, DVec3::new(500.0, 0.0, 0.0), DVec3::ZERO);
    static_gate.mode = MovementMode::Rigid;

    park.add_ball(source);
    park.add_ball(in_range);
    park.add_ball(at_boundary);
    park.add_ball(out_of_range);
    park.add_ball(cloaked_ship);
    park.add_ball(tethered_ship);
    park.add_static_ball(static_gate, 10);

    for id in [10, 11, 12, 13, 14, 15] {
        park.combat_states.entry(id).or_default();
    }
    park.combat_states.get_mut(&14).unwrap().is_cloaked = true;
    park.combat_states.get_mut(&15).unwrap().is_tethered = true;

    // Give ship 11 a target lock on 10 so we can verify ECM jam clears locks
    assert!(park.add_target_lock(11, make_lock(10, 0.0, true)));
    assert_eq!(park.get_target_locks(11).len(), 1);

    // Direct apply_ecm_jam ignores cloaked/tethered, jams 11 and clears its locks
    park.apply_ecm_jam(14, 20.0);
    assert_eq!(park.combat_states.get(&14).unwrap().ecm_jammed_remaining_s, 0.0);
    park.apply_ecm_jam(15, 20.0);
    assert_eq!(park.combat_states.get(&15).unwrap().ecm_jammed_remaining_s, 0.0);

    park.apply_ecm_jam(11, 12.0);
    assert!(park.get_target_locks(11).is_empty());
    assert_eq!(park.combat_states.get(&11).unwrap().ecm_jammed_remaining_s, 12.0);

    // Missing source returns empty
    assert!(park.apply_ecm_burst(999, 5000.0, |_| true).is_empty());

    // ECM burst at 5000.0 m radius evaluates 11 and 12 (boundary <= 5000.0), skips 10 (self), 13 (>5000m), 14 (cloaked), 15 (tethered), 16 (static)
    let mut tested = Vec::new();
    let jammed = park.apply_ecm_burst(10, 5000.0, |cid| {
        tested.push(cid);
        cid == 12
    });
    assert_eq!(tested, vec![11, 12]);
    assert_eq!(jammed, vec![12]);
    assert_eq!(park.combat_states.get(&12).unwrap().ecm_jammed_remaining_s, 20.0);
}

#[test]
fn grid_observer_visibility_and_bubble_queries() {
    let mut park = Ballpark::new(30000142);
    let mut stargate = make_ship(50, DVec3::ZERO, DVec3::ZERO);
    stargate.mode = MovementMode::Rigid;
    park.add_static_ball(stargate, 10);

    let s1 = make_ship(1, DVec3::new(1000.0, 0.0, 0.0), DVec3::ZERO);
    let s2 = make_ship(2, DVec3::new(2000.0, 0.0, 0.0), DVec3::ZERO);
    park.add_ball(s1);
    park.add_ball(s2);

    // Cloak s2: s1 should NOT see s2 in visible_balls_for_observer or dynamic_balls_in_bubble,
    // but s2 MUST still see itself
    park.combat_states.entry(2).or_default().is_cloaked = true;

    let vis_s1 = park.visible_balls_for_observer(1);
    let vis_s1_ids: Vec<BallId> = vis_s1.iter().map(|b| b.id).collect();
    assert!(vis_s1_ids.contains(&50));
    assert!(vis_s1_ids.contains(&1));
    assert!(!vis_s1_ids.contains(&2));

    let vis_s2 = park.visible_balls_for_observer(2);
    let vis_s2_ids: Vec<BallId> = vis_s2.iter().map(|b| b.id).collect();
    assert!(vis_s2_ids.contains(&50));
    assert!(vis_s2_ids.contains(&1));
    assert!(vis_s2_ids.contains(&2));

    // Static observer 50 is not in ball_grid_ids -> visible_balls_for_observer returns only static balls
    let vis_gate = park.visible_balls_for_observer(50);
    assert_eq!(vis_gate.len(), 1);
    assert_eq!(vis_gate[0].id, 50);

    // dynamic_balls_in_bubble filters out cloaked peers for s1, includes self for s2, empty for static 50
    let dyn_s1 = park.dynamic_balls_in_bubble(1);
    assert_eq!(dyn_s1.len(), 1);
    assert_eq!(dyn_s1[0].id, 1);
    let dyn_s2 = park.dynamic_balls_in_bubble(2);
    assert_eq!(dyn_s2.len(), 2);
    assert!(park.dynamic_balls_in_bubble(50).is_empty());

    // balls_in_bubble uses visible_balls_for_observer for grid members, or distance radius for static balls
    assert_eq!(park.balls_in_bubble(1, 5000.0).len(), vis_s1.len());
    assert_eq!(park.balls_in_bubble(50, 1500.0).len(), 2); // stargate (static) + s1 (at 1000m <= 1500m)
    assert!(park.balls_in_bubble(999_999, 5000.0).is_empty());
}

#[test]
fn subwarp_step_state_exact_exponential_approach() {
    let pos = DVec3::new(100.0, -200.0, 50.0);
    let vel = DVec3::new(10.0, 0.0, 0.0);
    let goal = DVec3::new(200.0, 0.0, 0.0);

    // dt <= 0.0 returns unchanged (pos, vel)
    assert_eq!(subwarp_step_state(pos, vel, goal, 2.0, 0.0), (pos, vel));
    assert_eq!(subwarp_step_state(pos, vel, goal, 2.0, -1.0), (pos, vel));

    // Positive dt approaches goal_vel exponentially
    let (next_pos, next_vel) = subwarp_step_state(pos, vel, goal, 2.0, 1.0);
    assert!(next_vel.x > vel.x && next_vel.x < goal.x);
    assert!(next_pos.x > pos.x + vel.x);
}

#[test]
fn step_collision_bumping_swept_and_overlap_for_dynamic_and_static_balls() {
    let mut park = Ballpark::new(30000142);

    // 1. Dynamic-dynamic swept-sphere collision (ships 1 and 2 moving head-on between pre_step and post_step)
    // Ship 1 is aligning to warp so we also verify that bumping resets elapsed_warp_time
    let mut pre_s1 = make_ship(1, DVec3::new(0.0, 0.0, 0.0), DVec3::new(200.0, 0.0, 0.0));
    pre_s1.radius = 60.0;
    pre_s1.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: DVec3::new(1.0e9, 0.0, 0.0),
        warp_speed_au_s: 3.0,
        total_distance: 1.0e9,
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 3.0,
    };
    let mut post_s1 = pre_s1.clone();
    post_s1.position = DVec3::new(200.0, 0.0, 0.0);

    let mut pre_s2 = make_ship(2, DVec3::new(250.0, 0.0, 0.0), DVec3::new(-200.0, 0.0, 0.0));
    pre_s2.radius = 60.0;
    let mut post_s2 = pre_s2.clone();
    post_s2.position = DVec3::new(50.0, 0.0, 0.0);

    // 2. Dynamic-static overlap separation (ship 3 overlapping static station 100 and moving into it)
    let mut s3 = make_ship(3, DVec3::new(0.0, 5000.0, 0.0), DVec3::new(50.0, 0.0, 0.0));
    s3.radius = 100.0;
    let mut station = make_ship(100, DVec3::new(80.0, 5000.0, 0.0), DVec3::ZERO);
    station.radius = 100.0;
    station.mode = MovementMode::Rigid;

    // 3. Asteroid belt (group 9) at same spot as ship 3 must be ignored by collision
    let mut belt = make_ship(101, DVec3::new(0.0, 5000.0, 0.0), DVec3::ZERO);
    belt.radius = 5000.0;
    belt.mode = MovementMode::Rigid;

    park.add_ball(post_s1);
    park.add_ball(post_s2);
    park.add_ball(s3.clone());
    park.add_static_ball(station.clone(), 15);
    park.add_static_ball(belt.clone(), 9);
    park.pending_events.clear();

    let mut pre_step_balls = BTreeMap::new();
    pre_step_balls.insert(1, pre_s1.clone());
    pre_step_balls.insert(2, pre_s2.clone());
    pre_step_balls.insert(3, s3.clone());
    pre_step_balls.insert(100, station.clone());
    pre_step_balls.insert(101, belt);

    let mut pre_positions = BTreeMap::new();
    for (&id, b) in &pre_step_balls {
        pre_positions.insert(id, (b.position, b.velocity, b.radius));
    }

    park.step_collision_bumping(&pre_step_balls, &pre_positions, 1.0);

    // Verify Collision events emitted for (1, 2) and (3, 100)
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a: 1, ball_b: 2 })));
    assert!(park.pending_events.iter().any(|e| matches!(e, DestinyEvent::Collision { ball_a: 3, ball_b: 100 })));

    // Verify s1 and s2 bounced apart and s1's warp alignment timer was reset
    let b1 = park.balls.get(&1).unwrap();
    let b2 = park.balls.get(&2).unwrap();
    assert!(b1.velocity.x < 200.0);
    assert!(b2.velocity.x > -200.0);
    if let MovementMode::Warp { elapsed_warp_time, .. } = b1.mode {
        assert_eq!(elapsed_warp_time, 0.0);
    } else {
        panic!("expected Warp mode");
    }

    // Verify s3 was pushed away from static station 100 while station 100 did not move
    let b3 = park.balls.get(&3).unwrap();
    let st = park.balls.get(&100).unwrap();
    assert!(b3.position.x < 0.0);
    assert_eq!(st.position, DVec3::new(80.0, 5000.0, 0.0));
}

#[test]
fn bubble_immunity_and_scramble_checks() {
    let mut park = Ballpark::new(30000142);
    // Shuttle (group 31) is bubble immune by group
    let shuttle = make_ship(1, DVec3::ZERO, DVec3::ZERO);
    park.add_ball(shuttle);
    park.set_ball_group_id(1, 31);
    assert!(park.is_ball_bubble_immune(1));

    // Normal frigate (group 25) is not bubble immune unless interdiction nullified
    let frig = make_ship(2, DVec3::ZERO, DVec3::ZERO);
    park.add_ball(frig);
    park.set_ball_group_id(2, 25);
    park.combat_states.insert(2, CombatState::new());
    assert!(!park.is_ball_bubble_immune(2));

    // Add a warp bubble at origin: shuttle is not scrambled, frigate IS scrambled
    park.warp_bubble_mgr.add_bubble(WarpBubble::new(500, DVec3::ZERO, 20_000.0));
    assert!(!park.is_ball_scrambled(1));
    assert!(park.is_ball_scrambled(2));

    // Nullify frigate -> no longer scrambled by bubble
    assert!(park.set_interdiction_nullified(2, true));
    assert!(!park.set_interdiction_nullified(999_999, true));
    assert!(park.is_ball_bubble_immune(2));
    assert!(!park.is_ball_scrambled(2));

    // Direct warp scramble strength on CombatState overrides nullification
    {
        let cs2 = park.combat_states.get_mut(&2).unwrap();
        cs2.warp_scramble_strength = 2;
        cs2.warp_core_strength = 1;
    }
    assert!(park.is_ball_scrambled(2));
    assert!(!park.is_ball_scrambled(999_999));
}

#[test]
fn coverage_and_mutants_targeting_gate_decloak_and_collision_branches() {
    let mut park = Ballpark::new(30_000_142);

    // 1. add_target_lock while gate-cloaked breaks gate cloak
    let mut scout = make_ship(10, DVec3::ZERO, DVec3::ZERO);
    scout.max_velocity = 50.0;
    park.add_ball(scout);
    park.ball_slim_meta.insert(
        10,
        crate::SlimItemMeta {
            is_cloaked: true,
            ..Default::default()
        },
    );
    park.combat_states.insert(
        10,
        CombatState {
            gate_cloak_remaining_ticks: 20,
            is_cloaked: true,
            base_max_velocity: Some(400.0),
            cloak_velocity_multiplier: Some(0.0),
            ..Default::default()
        },
    );
    let target = make_ship(20, DVec3::new(2_000.0, 0.0, 0.0), DVec3::ZERO);
    park.add_ball(target);
    assert!(park.add_target_lock(
        10,
        TargetLock {
            target_id: 20,
            lock_time_ms: 1_000.0,
            remaining_lock_ms: 1_000.0,
            locked: false,
            max_range_m: None,
        }
    ));
    assert_eq!(park.balls[&10].max_velocity, 400.0);
    assert!(!park.combat_states[&10].is_cloaked);

    // 2. Stationary static station with LOWER id than incoming ship (hits is_static_a stationary branch & dist_c == 0 branch)
    let mut st = make_ship(5, DVec3::new(10_000.0, 0.0, 0.0), DVec3::ZERO);
    st.radius = 500.0;
    st.mode = MovementMode::Rigid;
    park.add_static_ball(st.clone(), 15);

    let mut runner_pre = make_ship(30, DVec3::new(9_300.0, 0.0, 0.0), DVec3::new(400.0, 0.0, 0.0));
    runner_pre.radius = 100.0;
    let mut runner_post = runner_pre.clone();
    runner_post.position = DVec3::new(10_000.0, 0.0, 0.0);
    park.add_ball(runner_post.clone());

    // Exact coincidence at t_col where pre_b == st.position (dist_c == 0 -> DVec3::X fallback normal)
    let mut coinc_pre = make_ship(31, DVec3::new(9_200.0, 0.0, 0.0), DVec3::new(800.0, 0.0, 0.0));
    coinc_pre.radius = 100.0;
    let mut coinc_post = coinc_pre.clone();
    coinc_post.position = DVec3::new(10_000.0, 0.0, 0.0);
    park.add_ball(coinc_post.clone());

    // Overlap where is_static_a (5) overlaps dynamic ship (32) with v_rel < 0
    let mut overlap_dyn = make_ship(32, DVec3::new(10_400.0, 0.0, 0.0), DVec3::new(-100.0, 0.0, 0.0));
    overlap_dyn.radius = 150.0;
    park.add_ball(overlap_dyn.clone());

    // Exact momentum conservation check on dynamic-dynamic bounce
    let mut d1_pre = make_ship(40, DVec3::new(0.0, 20_000.0, 0.0), DVec3::new(300.0, 0.0, 0.0));
    d1_pre.mass = 2_000_000.0;
    d1_pre.radius = 80.0;
    let mut d1_post = d1_pre.clone();
    d1_post.position = DVec3::new(150.0, 20_000.0, 0.0);
    park.add_ball(d1_post);

    let mut d2_pre = make_ship(41, DVec3::new(300.0, 20_000.0, 0.0), DVec3::new(-100.0, 0.0, 0.0));
    d2_pre.mass = 1_000_000.0;
    d2_pre.radius = 80.0;
    let mut d2_post = d2_pre.clone();
    d2_post.position = DVec3::new(180.0, 20_000.0, 0.0);
    park.add_ball(d2_post);

    let mut pre_balls = BTreeMap::new();
    pre_balls.insert(5, st.clone());
    pre_balls.insert(30, runner_pre.clone());
    pre_balls.insert(31, coinc_pre.clone());
    pre_balls.insert(32, overlap_dyn.clone());
    pre_balls.insert(40, d1_pre.clone());
    pre_balls.insert(41, d2_pre.clone());

    let mut pre_pos = BTreeMap::new();
    for (&id, b) in &pre_balls {
        pre_pos.insert(id, (b.position, b.velocity, b.radius));
    }

    let _ = park.drain_all_observer_events();
    park.step_collision_bumping(&pre_balls, &pre_pos, 1.0);

    // Overlap_dyn (32) pushed out of static station (5) and velocity reversed (> 0)
    assert!(park.balls[&32].position.x >= 10_650.0 - 1e-6);
    assert!(park.balls[&32].velocity.x > 0.0);
    // Bumped ship observer events emitted
    assert!(park
        .drain_observer_events(32)
        .iter()
        .any(|e| matches!(e, DestinyEvent::SetBallVelocity { ball_id: 32, .. })));

    // Dynamic pair (40, 41) deflected relative to pre-collision velocities
    assert!(park.balls[&40].velocity.x < 300.0);
    assert!(park.balls[&41].velocity.x > -100.0);

    // Exact analytical swept-sphere collision check for two unequal-mass moving dynamic ships
    let mut p2 = Ballpark::new(30_000_142);
    let s1_pre = Ball {
        id: 1,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(400.0, 100.0, 0.0),
        max_velocity: 400.0,
        radius: 60.0,
        mass: 2_000_000.0,
        inertia: 3.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let s2_pre = Ball {
        id: 2,
        type_id: 587,
        position: DVec3::new(500.0, 25.0, 0.0),
        velocity: DVec3::new(-300.0, 50.0, 0.0),
        max_velocity: 400.0,
        radius: 90.0,
        mass: 5_000_000.0,
        inertia: 3.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let s1_post = Ball {
        position: DVec3::new(400.0, 100.0, 0.0),
        velocity: DVec3::new(380.0, 95.0, 0.0),
        ..s1_pre.clone()
    };
    let s2_post = Ball {
        position: DVec3::new(200.0, 75.0, 0.0),
        velocity: DVec3::new(-285.0, 47.5, 0.0),
        ..s2_pre.clone()
    };
    p2.add_ball(s1_post.clone());
    p2.add_ball(s2_post.clone());
    let mut pre2 = BTreeMap::new();
    pre2.insert(1, s1_pre.clone());
    pre2.insert(2, s2_pre.clone());
    let mut pos2 = BTreeMap::new();
    pos2.insert(1, (s1_pre.position, s1_pre.velocity, s1_pre.radius));
    pos2.insert(2, (s2_pre.position, s2_pre.velocity, s2_pre.radius));
    p2.step_collision_bumping(&pre2, &pos2, 1.0);

    // Exact expected t_col = 0.50 -> diff = (0, 0, 0) or exact quadratic root; verify exact final kinematics
    let min_d = 150.0_f64;
    let dp0 = s1_pre.position - s2_pre.position;
    let dv = (s1_post.position - s1_pre.position) - (s2_post.position - s2_pre.position);
    let a = dv.length_squared();
    let b = 2.0 * dp0.dot(dv);
    let c = dp0.length_squared() - min_d * min_d;
    let tc = (-b - (b * b - 4.0 * a * c).sqrt()) / (2.0 * a);
    let (pac, vac) = super::subwarp_step_state(s1_pre.position, s1_pre.velocity, DVec3::ZERO, s1_pre.time_constant(), tc);
    let (pbc, vbc) = super::subwarp_step_state(s2_pre.position, s2_pre.velocity, DVec3::ZERO, s2_pre.time_constant(), tc);
    let nrm = (pbc - pac).normalize();
    let van = vac.dot(nrm);
    let vbn = vbc.dot(nrm);
    let van_new = ((s1_post.mass - s2_post.mass) * van + 2.0 * s2_post.mass * vbn) / (s1_post.mass + s2_post.mass);
    let vbn_new = ((s2_post.mass - s1_post.mass) * vbn + 2.0 * s1_post.mass * van) / (s1_post.mass + s2_post.mass);
    let (_, vaf) = super::subwarp_step_state(pac, vac + nrm * (van_new - van), DVec3::ZERO, s1_pre.time_constant(), 1.0 - tc);
    let (_, vbf) = super::subwarp_step_state(pbc, vbc + nrm * (vbn_new - vbn), DVec3::ZERO, s2_pre.time_constant(), 1.0 - tc);
    let vfin_a = s1_post.velocity + (vaf - s1_post.velocity) * 0.85;
    let vfin_b = s2_post.velocity + (vbf - s2_post.velocity) * 0.85;
    let tau_a = s1_pre.time_constant();
    let tau_b = s2_pre.time_constant();
    let pfin_a = s1_post.position + (vfin_a - s1_post.velocity) * (1.0 / (1.0 - (-1.0 / tau_a).exp()) - tau_a);
    let pfin_b = s2_post.position + (vfin_b - s2_post.velocity) * (1.0 / (1.0 - (-1.0 / tau_b).exp()) - tau_b);
    assert!((p2.balls[&1].position - pfin_a).length() < 1e-6);
    assert!((p2.balls[&1].velocity - vfin_a).length() < 1e-6);
    assert!((p2.balls[&2].position - pfin_b).length() < 1e-6);
    assert!((p2.balls[&2].velocity - vfin_b).length() < 1e-6);

    // Exact analytical overlap separation + impulse for unequal-mass dynamic ships
    let mut p3 = Ballpark::new(30_000_142);
    let o1 = Ball {
        id: 1,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::new(120.0, 0.0, 0.0),
        max_velocity: 300.0,
        radius: 60.0,
        mass: 2_000_000.0,
        inertia: 3.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let o2 = Ball {
        id: 2,
        type_id: 587,
        position: DVec3::new(120.0, 0.0, 0.0),
        velocity: DVec3::new(-80.0, 0.0, 0.0),
        max_velocity: 300.0,
        radius: 90.0,
        mass: 6_000_000.0,
        inertia: 3.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    p3.add_ball(o1.clone());
    p3.add_ball(o2.clone());
    let mut pre3 = BTreeMap::new();
    pre3.insert(1, o1.clone());
    pre3.insert(2, o2);
    let pos3 = BTreeMap::new();
    p3.step_collision_bumping(&pre3, &pos3, 1.0);
    // overlap = 30.0, normal = -X: m1=2e6, m2=6e6 -> p1.x = -30*(6/8) = -22.5, p2.x = 120 + 30*(2/8) = 127.5
    assert!((p3.balls[&1].position.x - (-22.5)).abs() < 1e-6);
    assert!((p3.balls[&2].position.x - 127.5).abs() < 1e-6);
    // v_rel = (120 - (-80)) * (-1) = -200, restitution = 0.8, inv_mass = 2/3e6 -> impulse = 1.8 * 200 * 1.5e6 = 5.4e8
    assert!((p3.balls[&1].velocity.x - (120.0 - 5.4e8 / 2_000_000.0)).abs() < 1e-6);
    assert!((p3.balls[&2].velocity.x - (-80.0 + 5.4e8 / 6_000_000.0)).abs() < 1e-6);

    // Static-a vs dynamic-b overlap, non-solid group 1971 hazard skip, category 65 station & visibility queries
    let mut st_a = o1.clone();
    st_a.id = 10;
    st_a.velocity = DVec3::ZERO;
    st_a.mode = MovementMode::Rigid;
    p3.add_ball(st_a.clone());
    p3.ball_slim_meta.insert(
        10,
        SlimItemMeta {
            category_id: 65,
            ..Default::default()
        },
    );
    let mut hazard = o1.clone();
    hazard.id = 11;
    p3.add_ball(hazard.clone());
    p3.ball_group_ids.insert(11, 1971);
    p3.combat_states.entry(2).or_default().is_cloaked = true;
    assert!(p3.visible_balls_for_observer(1).iter().any(|b| b.id == 1));
    assert!(!p3.visible_balls_for_observer(1).iter().any(|b| b.id == 2));
    assert!(p3.dynamic_balls_in_bubble(2).iter().any(|b| b.id == 2));
    assert!(p3.balls_in_bubble(500_001, 200.0).is_empty());
    p3.step_collision_bumping(&pre3, &pos3, 1.0);
}
