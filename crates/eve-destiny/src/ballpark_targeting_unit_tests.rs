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
