#![forbid(unsafe_code)]
//! Integration & policy acceptance tests for V24 Destiny delivery policy (`#1078` & `#1077` §E 5–6).

use std::collections::HashMap;

use crate::common;
use eve_destiny::{Ball, Ballpark, DVec3, DestinyEvent, MovementMode, SlimItemMeta, WarpPhase};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::destiny_delivery_policy::{
    clamp_owner_critical_stamp, validate_observer_movement_window,
    validate_warp_handoff_single_stamp, PolicyDestinyFrame, SessionStampPolicyState,
    StampRejectionReason, HELD_FUTURE_HOLD_THRESHOLD_TICKS, OWNER_CRITICAL_MOVEMENT_MAX_LEAD_TICKS,
    OWNER_DIRECT_ECHO_LEAD_TICKS, POST_HELD_LEAD_TICKS,
};
use eve_server::router::ServerContext;

const DEMO_PILOT: i64 = 90_000_001;
const JITA_SYS: u32 = 30_000_142;

async fn setup_undocked_pilot(ctx: &ServerContext, sess_id: u64) -> (SessionState, i64, u64) {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(DEMO_PILOT)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    let ship_id = sess.ship_id.expect("active ship");
    let undock_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "stationSvc",
            None,
            "Undock",
            &[],
            &HashMap::new(),
            None,
        )
        .await
        .expect("Undock");
    let PyRep::KeyVal(ukv) = undock_rep else {
        panic!("Expected KeyVal from Undock");
    };
    let bid = ukv.get("beyonceBoundID").unwrap().as_i64().unwrap() as u64;
    (sess, ship_id, bid)
}

fn mk_observer_ball(id: i64, pos: DVec3) -> Ball {
    Ball {
        id,
        type_id: 587,
        position: pos,
        velocity: DVec3::ZERO,
        max_velocity: 365.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    }
}

fn extract_action_names_and_stamps(py: &PyRep) -> Vec<(i64, String, Vec<PyRep>)> {
    let PyRep::List(items) = py else {
        panic!("expected List, got {py:?}");
    };
    items
        .iter()
        .map(|item| {
            let PyRep::Tuple(outer) = item else {
                panic!("expected (stamp, action) tuple, got {item:?}");
            };
            let stamp = outer[0].as_i64().unwrap();
            let PyRep::Tuple(inner) = &outer[1] else {
                panic!("expected (name, args) tuple");
            };
            let name = inner[0].as_str().unwrap().to_string();
            let args = match &inner[1] {
                PyRep::Tuple(v) => v.clone(),
                other => vec![other.clone()],
            };
            (stamp, name, args)
        })
        .collect()
}

fn mk_wire_entry(stamp: u32, action: &str, ball_id: i64) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(stamp as i64),
        PyRep::Tuple(vec![
            PyRep::String(action.to_string()),
            PyRep::Tuple(vec![PyRep::Integer(ball_id)]),
        ]),
    ])
}

#[tokio::test]
async fn same_tick_goto_keeps_first_owner_echo_observers_get_latest() {
    // parity: destiny:GotoDirection, feat:navigation.destiny-updates
    eve_testkit::attest(&["destiny:GotoDirection", "feat:navigation.destiny-updates"]);
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let (mut sess, ship_id, bid) = setup_undocked_pilot(&ctx, 2291).await;
    let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let observer_id = 100_978_101i64;
    sys_h
        .spawn_ball(mk_observer_ball(
            observer_id,
            ship_ball.position + DVec3::new(800.0, 0.0, 0.0),
        ))
        .await
        .unwrap();
    let _ = sys_h.drain_observer_updates(ship_id).await;
    let _ = sys_h.drain_observer_updates(observer_id).await;

    let kw = HashMap::new();
    // Two distinct headings on the SAME tick: +Y then +Z.
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bid),
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(1.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .expect("first heading +Y");
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bid),
        "CmdGotoDirection",
        &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
        &kw,
        None,
    )
    .await
    .expect("second heading +Z");

    let (_, owner_evs, owner_py) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    let (_, obs_evs, obs_py) = sys_h.drain_observer_updates(observer_id).await.unwrap();

    assert_eq!(owner_evs.len(), 1, "owner receives ONE GotoDirection on same tick");
    let owner_actions = extract_action_names_and_stamps(&owner_py);
    assert_eq!(owner_actions[0].1, "GotoDirection");
    assert_eq!(
        owner_actions[0].2,
        vec![
            PyRep::Integer(ship_id),
            PyRep::Real(0.0),
            PyRep::Real(1.0),
            PyRep::Real(0.0),
        ],
        "owner must receive the FIRST heading"
    );

    assert_eq!(obs_evs.len(), 1, "observer coalesces same-tick GotoDirection");
    let obs_actions = extract_action_names_and_stamps(&obs_py);
    assert_eq!(obs_actions[0].1, "GotoDirection");
    assert_eq!(
        obs_actions[0].2,
        vec![
            PyRep::Integer(ship_id),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(1.0),
        ],
        "observer must receive the LATEST heading"
    );

    // Subsequent ticks must not queue a second deferred owner echo.
    for _ in 0..3 {
        sys_h.step_manual(1.0).await.unwrap();
        let (_, later_owner, _) = sys_h.drain_observer_updates(ship_id).await.unwrap();
        assert!(
            !later_owner
                .iter()
                .any(|e| matches!(e, DestinyEvent::GotoDirection { .. })),
            "no second owner GotoDirection echo may be queued for a later tick: {later_owner:?}"
        );
    }
}

#[tokio::test]
async fn owner_destiny_stamps_are_monotonic_across_fire_move_and_death() {
    // parity: feat:navigation.destiny-updates, destiny:AddBalls2, destiny:GotoDirection, destiny:Stop
    eve_testkit::attest(&[
        "feat:navigation.destiny-updates",
        "destiny:AddBalls2",
        "destiny:GotoDirection",
        "destiny:Stop",
    ]);
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let (mut sess, ship_id, bid) = setup_undocked_pilot(&ctx, 2292).await;
    let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let enemy_id = 100_978_102i64;
    sys_h
        .spawn_ball(mk_observer_ball(
            enemy_id,
            ship_ball.position + DVec3::new(1_200.0, 0.0, 0.0),
        ))
        .await
        .unwrap();

    let mut policy = SessionStampPolicyState::new(sess.session_id as i64, Some(ship_id));
    let mut emitted_stamps: Vec<u32> = Vec::new();
    let kw = HashMap::new();

    // Bootstrap / acquire
    let (t0, _, init_py) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    policy.advance_owner_visible_stamp(t0 as u32);
    if let PyRep::List(entries) = init_py {
        if !entries.is_empty() {
            let out = policy.process_dispatch_batches(
                vec![PolicyDestinyFrame {
                    stamp: t0 as u32,
                    updates: entries,
                    has_set_state: false,
                    wait_for_bubble: false,
                }],
                false,
            );
            for f in out {
                emitted_stamps.push(f.stamp);
            }
        }
    }

    // Move + fire (damage) + stop + death across consecutive ticks
    let tb1 = sys_h.step_manual(1.0).await.unwrap();
    policy.advance_owner_visible_stamp(tb1.tick as u32);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(bid),
        "CmdGotoDirection",
        &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
        None,
    )
    .await
    .unwrap();
    let _ = sys_h
        .apply_damage_with_attacker(
            enemy_id,
            eve_destiny::DamageVector {
                em: 100.0,
                thermal: 100.0,
                kinetic: 100.0,
                explosive: 100.0,
            },
            Some(DEMO_PILOT),
        )
        .await
        .unwrap();
    let (t1, _, move_fire_py) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    if let PyRep::List(entries) = move_fire_py {
        let out = policy.process_dispatch_batches(
            vec![
                PolicyDestinyFrame {
                    stamp: t1 as u32,
                    updates: entries[..1].to_vec(),
                    has_set_state: false,
                    wait_for_bubble: false,
                },
                PolicyDestinyFrame {
                    stamp: t1 as u32,
                    updates: entries[1..].to_vec(),
                    has_set_state: false,
                    wait_for_bubble: false,
                },
            ],
            false,
        );
        // Equal-stamp updates in one dispatch must merge into ONE frame.
        assert_eq!(out.len(), 1);
        emitted_stamps.push(out[0].stamp);
    }

    // Step and kill enemy + stop ship
    let tb2 = sys_h.step_manual(1.0).await.unwrap();
    policy.advance_owner_visible_stamp(tb2.tick as u32);
    let _ = sys_h
        .apply_damage_with_attacker(
            enemy_id,
            eve_destiny::DamageVector {
                em: 50_000.0,
                thermal: 50_000.0,
                kinetic: 50_000.0,
                explosive: 50_000.0,
            },
            Some(DEMO_PILOT),
        )
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bid), "CmdStop", &[], &kw, None)
        .await
        .unwrap();
    let (t2, _, death_stop_py) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    if let PyRep::List(entries) = death_stop_py {
        let out = policy.process_dispatch_batches(
            vec![PolicyDestinyFrame {
                stamp: t2 as u32,
                updates: entries,
                has_set_state: false,
                wait_for_bubble: false,
            }],
            false,
        );
        for f in out {
            emitted_stamps.push(f.stamp);
        }
    }

    assert!(emitted_stamps.len() >= 2);
    assert!(
        emitted_stamps.windows(2).all(|w| w[0] <= w[1]),
        "owner stamps must be non-decreasing across fire, move, stop, and death: {emitted_stamps:?}"
    );

    // Regressing stamp below last_sent_stamp must be rejected with reason recorded.
    let regressed_stamp = (*emitted_stamps.last().unwrap()).saturating_sub(1);
    let rej_out = policy.process_dispatch_batches(
        vec![PolicyDestinyFrame {
            stamp: regressed_stamp,
            updates: vec![mk_wire_entry(regressed_stamp, "GotoDirection", ship_id)],
            has_set_state: false,
            wait_for_bubble: false,
        }],
        true,
    );
    assert!(rej_out.is_empty());
    assert!(matches!(
        policy.rejections.last().map(|r| &r.reason),
        Some(StampRejectionReason::BelowLastSentStamp { .. })
    ));
}

#[tokio::test]
async fn goto_reaches_observer_within_one_tick() {
    // parity: destiny:GotoDirection, feat:navigation.destiny-updates
    eve_testkit::attest(&["destiny:GotoDirection", "feat:navigation.destiny-updates"]);
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let (mut sess, ship_id, bid) = setup_undocked_pilot(&ctx, 2293).await;
    let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let observer_id = 100_978_103i64;
    sys_h
        .spawn_ball(mk_observer_ball(
            observer_id,
            ship_ball.position + DVec3::new(750.0, 0.0, 0.0),
        ))
        .await
        .unwrap();
    let _ = sys_h.drain_observer_updates(ship_id).await;
    let _ = sys_h.drain_observer_updates(observer_id).await;

    let cmd_stamp = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            Some(bid),
            "CmdGotoDirection",
            &[PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdGotoDirection")
        .as_i64()
        .unwrap() as u32;

    let (obs_tick, obs_evs, obs_py) = sys_h.drain_observer_updates(observer_id).await.unwrap();
    assert_eq!(obs_evs.len(), 1, "observer must receive pilot GotoDirection");
    let obs_actions = extract_action_names_and_stamps(&obs_py);
    assert_eq!(obs_actions.len(), 1);
    assert_eq!(obs_actions[0].1, "GotoDirection");
    assert_eq!(
        obs_actions[0].2,
        vec![
            PyRep::Integer(ship_id),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(1.0),
        ]
    );
    assert_eq!(obs_actions[0].0, obs_tick as i64);
    assert!(
        validate_observer_movement_window(cmd_stamp, obs_tick as u32),
        "observer stamp {obs_tick} must be within [{cmd_stamp}, {}]",
        cmd_stamp + 1
    );
}

#[tokio::test]
async fn fresh_acquire_addballs2_on_live_tick_without_movement_replay() {
    // parity: destiny:AddBalls2, feat:navigation.destiny-updates
    eve_testkit::attest(&["destiny:AddBalls2", "feat:navigation.destiny-updates"]);
    let ctx = crate::crimewatch_timers_and_drones::crimewatch_drones_lifecycle::fast_raw_ctx();
    common::pin_clock(&ctx);
    let (_sess, ship_id, _bid) = setup_undocked_pilot(&ctx, 2294).await;
    let sys_h = ctx.systems.get_or_spawn(JITA_SYS);

    for _ in 0..4 {
        sys_h.step_manual(1.0).await.unwrap();
    }
    let _ = sys_h.drain_observer_updates(ship_id).await;
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let newcomer_id = 100_978_104i64;
    sys_h
        .spawn_ball(mk_observer_ball(
            newcomer_id,
            ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0),
        ))
        .await
        .unwrap();

    let (acq_tick, owner_evs, owner_py) = sys_h.drain_observer_updates(ship_id).await.unwrap();
    assert!(acq_tick > 1, "acquisition tick must reflect advanced live scene");
    assert_eq!(owner_evs.len(), 1, "only AddBalls2 should be emitted for newcomer");
    let owner_actions = extract_action_names_and_stamps(&owner_py);
    assert_eq!(owner_actions.len(), 1);
    assert_eq!(owner_actions[0].0, acq_tick as i64);
    assert_eq!(owner_actions[0].1, "AddBalls2");

    let PyRep::List(acq_updates) = owner_py else {
        panic!("expected wire List for owner_py, got {owner_py:?}");
    };
    let mut policy = SessionStampPolicyState::new(1, Some(ship_id));
    policy.advance_owner_visible_stamp(acq_tick as u32);
    let acq_frames = policy.process_dispatch_batches(
        vec![PolicyDestinyFrame {
            stamp: acq_tick as u32,
            updates: acq_updates,
            has_set_state: false,
            wait_for_bubble: false,
        }],
        false,
    );
    assert_eq!(acq_frames.len(), 1);
    assert_eq!(
        policy.fresh_acquire_stamp_by_ball.get(&newcomer_id),
        Some(&(acq_tick as u32))
    );
    let older_stamp = (acq_tick as u32).saturating_sub(2);
    let replay = policy.process_dispatch_batches(
        vec![PolicyDestinyFrame {
            stamp: acq_tick as u32,
            updates: vec![mk_wire_entry(older_stamp, "GotoDirection", newcomer_id)],
            has_set_state: false,
            wait_for_bubble: false,
        }],
        false,
    );
    assert_eq!(replay.len(), 0);
    assert_eq!(
        policy.rejections.last().map(|r| &r.reason),
        Some(&StampRejectionReason::SubwarpReplayAfterFreshAcquire {
            ball_id: newcomer_id,
            action_stamp: older_stamp,
            acquired_stamp: acq_tick as u32,
        })
    );
}

#[test]
fn owner_stamp_never_exceeds_visible_plus_lead() {
    // parity: feat:navigation.destiny-updates
    eve_testkit::attest(&["feat:navigation.destiny-updates"]);
    assert!(OWNER_DIRECT_ECHO_LEAD_TICKS < HELD_FUTURE_HOLD_THRESHOLD_TICKS);
    assert!(OWNER_CRITICAL_MOVEMENT_MAX_LEAD_TICKS < POST_HELD_LEAD_TICKS);

    let visible = 500u32;
    let clamped_cmd = clamp_owner_critical_stamp(visible + 5, visible, OWNER_DIRECT_ECHO_LEAD_TICKS);
    assert_eq!(clamped_cmd, visible + OWNER_DIRECT_ECHO_LEAD_TICKS);

    let mut policy = SessionStampPolicyState::new(77, Some(1001));
    policy.advance_owner_visible_stamp(visible);
    let out = policy.process_dispatch_batches(
        vec![PolicyDestinyFrame {
            stamp: visible + POST_HELD_LEAD_TICKS,
            updates: vec![mk_wire_entry(visible + POST_HELD_LEAD_TICKS, "SetBallPosition", 1001)],
            has_set_state: false,
            wait_for_bubble: false,
        }],
        false,
    );
    assert_eq!(out.len(), 1);
    assert!(out[0].stamp <= visible + OWNER_CRITICAL_MOVEMENT_MAX_LEAD_TICKS);
}

#[test]
fn warp_grid_handoff_moves_scenery_and_balls_on_one_stamp() {
    // parity: feat:navigation.bubbles-grid, destiny:AddBalls2
    eve_testkit::attest(&["feat:navigation.bubbles-grid", "destiny:AddBalls2"]);
    let mut park = Ballpark::new(JITA_SYS);
    let static_gate_a = 50_001_001i64;
    let static_gate_b = 50_001_002i64;
    let dest_pos = DVec3::new(100_000_000.0, 0.0, 0.0);

    park.add_static_ball(
        Ball {
            id: static_gate_a,
            type_id: 16,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e12,
            radius: 5_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        10,
    );
    park.add_static_ball(
        Ball {
            id: static_gate_b,
            type_id: 16,
            position: dest_pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1e12,
            radius: 5_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        10,
    );

    let mover_id = 1_000_000_010i64;
    let dep_obs = 1_000_000_011i64;
    let dst_obs = 1_000_000_012i64;

    let mut mover = mk_observer_ball(mover_id, DVec3::new(0.0, 0.0, 0.0));
    mover.mode = MovementMode::Warp {
        origin: DVec3::ZERO,
        destination: dest_pos,
        warp_speed_au_s: 5.0,
        total_distance: dest_pos.length(),
        phase: WarpPhase::Aligning,
        elapsed_warp_time: 0.0,
    };
    mover.speed_fraction = 1.0;

    park.add_ball(mover);
    park.add_ball(mk_observer_ball(dep_obs, DVec3::new(500.0, 0.0, 0.0)));
    park.add_ball(mk_observer_ball(dst_obs, dest_pos + DVec3::new(500.0, 0.0, 0.0)));
    park.ball_slim_meta.insert(mover_id, SlimItemMeta::default());
    park.ball_slim_meta.insert(dst_obs, SlimItemMeta::default());
    let _ = park.drain_observer_events(mover_id);
    let _ = park.drain_observer_events(dep_obs);
    let _ = park.drain_observer_events(dst_obs);

    let mut mover_grid_timeline: Vec<(u64, DestinyEvent)> = Vec::new();
    let mut removed_during_handoff: Vec<i64> = Vec::new();
    let mut dep_removed: Vec<(u64, Vec<i64>)> = Vec::new();
    let mut dst_added: Vec<(u64, Vec<i64>)> = Vec::new();
    let mut arrival_stop_tick: Option<u64> = None;
    for _ in 0..45 {
        park.step(1.0);
        let tick = park.current_tick;
        for ev in park.drain_observer_events(mover_id) {
            if let DestinyEvent::RemoveBalls { ref ball_ids } = ev {
                removed_during_handoff.extend(ball_ids.iter().copied());
            }
            if matches!(
                ev,
                DestinyEvent::AddBalls { .. }
                    | DestinyEvent::AddBalls2 { .. }
                    | DestinyEvent::RemoveBalls { .. }
            ) {
                mover_grid_timeline.push((tick, ev));
            }
        }
        for ev in park.drain_observer_events(dep_obs) {
            if let DestinyEvent::RemoveBalls { ball_ids } = ev {
                dep_removed.push((tick, ball_ids));
            }
        }
        for ev in park.drain_observer_events(dst_obs) {
            match ev {
                DestinyEvent::AddBalls2 { balls } => {
                    dst_added.push((tick, balls.into_iter().map(|b| b.id).collect()));
                }
                DestinyEvent::Stop { ball_id } if ball_id == mover_id => {
                    arrival_stop_tick = Some(tick);
                }
                _ => {}
            }
        }
        if !mover_grid_timeline.is_empty() {
            break;
        }
    }

    let mut add_stamps: Vec<u64> = Vec::new();
    let mut remove_stamps: Vec<u64> = Vec::new();
    let mut entered_ids: Vec<i64> = Vec::new();
    for (tick, ev) in &mover_grid_timeline {
        match ev {
            DestinyEvent::AddBalls { balls } | DestinyEvent::AddBalls2 { balls } => {
                add_stamps.push(*tick);
                entered_ids.extend(balls.iter().map(|b| b.id));
            }
            DestinyEvent::RemoveBalls { .. } => {
                remove_stamps.push(*tick);
            }
            _ => {}
        }
    }
    assert_eq!(add_stamps.len(), 1);
    assert_eq!(remove_stamps.len(), 1);
    let handoff_tick = add_stamps[0];
    assert_eq!(handoff_tick, remove_stamps[0]);
    assert_eq!(entered_ids, vec![dst_obs]);
    assert_eq!(removed_during_handoff, vec![dep_obs]);
    assert_eq!(dep_removed, vec![(handoff_tick, vec![mover_id])]);
    assert_eq!(dst_added, vec![(handoff_tick, vec![mover_id])]);
    assert_eq!(park.bubble_id_for_ball(mover_id), park.bubble_id_for_ball(dst_obs));
    assert!(park.balls.contains_key(&static_gate_a));
    assert!(park.balls.contains_key(&static_gate_b));

    let borrowed: Vec<(u64, &DestinyEvent)> = mover_grid_timeline
        .iter()
        .map(|(t, ev)| (*t, ev))
        .collect();
    assert!(
        validate_warp_handoff_single_stamp(
            mover_id,
            &[static_gate_a, static_gate_b],
            &borrowed,
            &removed_during_handoff,
        ),
        "warp grid handoff must keep static scenery and move dynamic balls on one stamp: {mover_grid_timeline:?}"
    );
    let bad_scenery_rm = DestinyEvent::RemoveBalls {
        ball_ids: vec![static_gate_a],
    };
    let bad_mover_rm = DestinyEvent::RemoveBalls {
        ball_ids: vec![mover_id],
    };
    let good_add = DestinyEvent::AddBalls2 { balls: Vec::new() };
    assert!(!validate_warp_handoff_single_stamp(
        mover_id,
        &[static_gate_a, static_gate_b],
        &[(handoff_tick, &good_add), (handoff_tick, &bad_scenery_rm)],
        &[],
    ));
    assert!(!validate_warp_handoff_single_stamp(
        mover_id,
        &[static_gate_a, static_gate_b],
        &[(handoff_tick, &good_add), (handoff_tick, &bad_mover_rm)],
        &[],
    ));

    // Arrival deceleration drops out of warp into MovementMode::Stop and emits Stop to destination grid.
    if arrival_stop_tick.is_none() {
        for _ in 0..40 {
            park.step(1.0);
            let tick = park.current_tick;
            for ev in park.drain_observer_events(dst_obs) {
                if matches!(ev, DestinyEvent::Stop { ball_id } if ball_id == mover_id) {
                    arrival_stop_tick = Some(tick);
                }
            }
            if arrival_stop_tick.is_some() {
                break;
            }
        }
    }
    assert!(
        arrival_stop_tick.is_some_and(|t| t >= handoff_tick),
        "mover must complete warp deceleration and emit Stop after handoff tick {handoff_tick}"
    );
    assert!(matches!(park.balls[&mover_id].mode, MovementMode::Stop));
}
