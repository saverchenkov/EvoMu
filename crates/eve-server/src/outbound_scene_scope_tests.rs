use super::*;
use crate::outbound_sequencer::tests::{dummy_updates, frame_actions, named_entry, named_update};
use crate::outbound_sequencer::{
    make_remove_balls_entry, sequence_server_rpc_in_scene_with_events, sequence_tick,
    sequence_tick_with_events, sequence_with_events, AcceptDestinyOutcome, DestinyBatch,
    DestinyDrain, DestinyStream, OutFrame, RpcCompletion, SequencerLedgerEventKind,
    SessionWireState, TypedNotification,
};
use eve_proto::PyRep;

#[test]
fn old_epoch_frames_are_dropped_and_new_epoch_frames_wait_for_scene_entry() {
    let sys_old = 30_000_140_u32;
    let sys_new = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_03, 1190_03).with_scene_epoch(2);
    let ep_old = wire.enter_space(Some(sys_old));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep_old, 10, true, dummy_updates("SetState", 10))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_old), Some(2))))
    )
    .is_some());
    assert!(wire.scene_entered);

    // 1. Frames from an older scene epoch (epoch 1 < active epoch 2) or wrong solar system are dropped immediately.
    let (stale_tick_frame, stale_tick_events) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(ep_old, 11, false, dummy_updates("Stop", 11))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_old), Some(1)))),
    );
    assert!(stale_tick_frame.is_none());
    assert!(stale_tick_events
        .iter()
        .any(|e| e.kind == SequencerLedgerEventKind::Dropped && e.reason.starts_with("stale_scene_epoch:1<2")));

    let wrong_sys_notif = TypedNotification::new("OnDamageMessage", vec![])
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(2))));
    match wire.gate_typed_notification(wrong_sys_notif) {
        GateNotificationOutcome::Dropped(ev) => {
            assert!(ev.reason.starts_with("scene_mismatch:"));
        }
        other => panic!("expected wrong-system notification to drop, got {other:?}"),
    }

    // 2. Frames tagged for a newer scene epoch (epoch 3 > active epoch 2) wait instead of dropping or emitting early.
    let future_destiny_epoch = ep_old + 1;
    let future_tick = DestinyBatch::new(
        future_destiny_epoch,
        22,
        false,
        named_update(22, "SetSpeedFraction", vec![PyRep::Integer(5001), PyRep::Real(0.5)]),
    )
    .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3))));
    let (future_tick_emitted, future_tick_events) = sequence_tick_with_events(&mut wire, future_tick);
    assert!(future_tick_emitted.is_none());
    assert!(future_tick_events.is_empty());

    let future_notif = TypedNotification::new(
        "OnDamageStateChange",
        vec![PyRep::Integer(5001), PyRep::Tuple(vec![])],
    )
    .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3))));
    assert!(matches!(
        wire.gate_typed_notification(future_notif),
        GateNotificationOutcome::Held(None)
    ));
    assert_eq!(wire.held_scene_frame_count(), 2);

    // 3. Advancing to epoch 3 (`AwaitingSetState`, not yet entered) must keep held frames waiting for SetState,
    //    and hold same-epoch pre-entry incremental notifications until scene entry, while dropping epoch-2 stragglers.
    let ep_new = wire.enter_space(Some(sys_new));
    assert_eq!(ep_new, future_destiny_epoch);
    let prune_evs = wire.sync_scene(3, Some(sys_new), true);
    assert!(prune_evs.is_empty());
    assert!(!wire.scene_entered);
    assert_eq!(wire.held_scene_frame_count(), 2);

    let same_epoch_pre_entry_notif =
        TypedNotification::new("OnCombatMessage", vec![PyRep::String("Shot".into())])
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3))));
    assert!(matches!(
        wire.gate_typed_notification(same_epoch_pre_entry_notif),
        GateNotificationOutcome::Held(None)
    ));
    assert_eq!(wire.held_scene_frame_count(), 3);

    let straggler_epoch_2 =
        TypedNotification::new("OnDamageMessage", vec![])
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_old), Some(2))));
    match wire.gate_typed_notification(straggler_epoch_2) {
        GateNotificationOutcome::Dropped(ev) => {
            assert!(ev.reason.starts_with("stale_scene_epoch:2<3"));
        }
        other => panic!("expected epoch-2 straggler to drop at epoch 3, got {other:?}"),
    }

    // Still held if we haven't delivered SetState yet.
    let early_drain = drain_ready_held_frames(&mut wire);
    assert!(early_drain.frames.is_empty());
    assert_eq!(wire.held_scene_frame_count(), 3);

    // 4. Delivering SetState enters scene 3 and flushes the held epoch-3 frames in order behind SetState,
    //    gating entity-dependent frames against the newly materialized entity set.
    let mut bp_new = eve_destiny::Ballpark::new(sys_new);
    bp_new.current_tick = 20;
    bp_new.add_ball(eve_destiny::Ball {
        id: 5001,
        type_id: 587,
        position: eve_destiny::DVec3::ZERO,
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    });
    let set_state_5001 = eve_destiny::wire::encode_set_state_payload(&bp_new, 5001);
    let outcome = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 1,
            response_frame: PyRep::None,
            session_change_frame: Some(PyRep::String("jump_scene_3".into())),
            item_change_before_session_change: false,
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Attach {
                epoch: ep_new,
                set_state: DestinyBatch::new(
                    ep_new,
                    20,
                    true,
                    named_update(20, "SetState", vec![set_state_5001]),
                )
                .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3)))),
            },
            notifications: Vec::new(),
        },
    );
    assert!(wire.scene_entered);
    assert_eq!(wire.held_scene_frame_count(), 0);
    let events_seq: Vec<&str> = outcome
        .frames
        .iter()
        .map(|f| match f {
            OutFrame::SessionChange { .. } => "SessionChange",
            OutFrame::DoDestinyUpdate { has_set_state: true, .. } => "SetState",
            OutFrame::CallRsp { .. } => "CallRsp",
            OutFrame::DoDestinyUpdate { has_set_state: false, .. } => "HeldDestiny",
            OutFrame::Notification { event, .. } => event.as_str(),
            OutFrame::ServiceCall { .. } => "ServiceCall",
        })
        .collect();
    assert_eq!(
        events_seq,
        vec![
            "SessionChange",
            "SetState",
            "CallRsp",
            "HeldDestiny",
            "OnDamageStateChange",
            "OnCombatMessage",
        ]
    );
}

#[test]
fn docked_wire_drops_space_scoped_combat_notifications_1565() {
    let combat_sys = 30_000_140_u32;
    let docked_sys = 30_000_142_u32;
    let mut wire = SessionWireState::new(1565_01, 1565_01).with_scene_epoch(5);
    wire.leave_space();
    let sync_evs = wire.sync_scene(5, Some(docked_sys), false);
    assert!(sync_evs.is_empty());

    for scope in [
        OutboundSceneScope::space(Some(combat_sys), None),
        OutboundSceneScope::space(Some(docked_sys), None),
        OutboundSceneScope::space(Some(docked_sys), Some(4)),
        OutboundSceneScope::space(Some(docked_sys), Some(5)),
    ] {
        let dmg_notif = TypedNotification::new("OnDamageMessage", vec![])
            .with_scene_scope(Some(scope));
        match wire.gate_typed_notification(dmg_notif) {
            GateNotificationOutcome::Dropped(_) => {}
            other => panic!("docked wire must drop space-scoped combat notif {scope:?}, got {other:?}"),
        }
    }

    // Unscoped character notifications (e.g. wallet/account/mail) still deliver while docked.
    let acct_notif = TypedNotification::new("OnAccountChange", vec![]);
    assert!(matches!(
        wire.gate_typed_notification(acct_notif),
        GateNotificationOutcome::Emit(_)
    ));
}

#[test]
fn held_scene_frames_respect_materialization_gate_and_overflow_bounds() {
    let sys_id = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_04, 1190_04).with_scene_epoch(1);
    let ep = wire.enter_space(Some(sys_id));
    let sync_evs = wire.sync_scene(2, Some(sys_id), true);
    assert!(sync_evs.is_empty());
    assert!(!wire.scene_entered);

    // Held notification referencing an entity that will NOT appear in SetState must still be
    // dropped by the per-ball materialization gate when flushed at scene entry.
    let ghost_ball = 77_001_i64;
    let unmat_notif = TypedNotification::new(
        "OnDamageStateChange",
        vec![PyRep::Integer(ghost_ball), PyRep::Tuple(vec![])],
    )
    .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(2))));
    assert!(matches!(
        wire.gate_typed_notification(unmat_notif),
        GateNotificationOutcome::Held(None)
    ));

    // Delivering SetState without `ghost_ball` flushes held frames through the gate, dropping `unmat_notif`.
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep, 30, true, dummy_updates("SetState", 30))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(2))))
    )
    .is_some());
    let drained = drain_ready_held_frames(&mut wire);
    assert!(drained.frames.is_empty());
    assert_eq!(wire.sequencer_counters().dropped_dependent_notifications, 1);

    // Changing scene epoch to 3 invalidates and drops any stale held epoch-2 frames.
    let stale_future_hold = TypedNotification::new("OnCombatMessage", vec![])
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(3))));
    assert!(matches!(
        wire.gate_typed_notification(stale_future_hold),
        GateNotificationOutcome::Held(None)
    ));
    let pruned = wire.sync_scene(4, Some(30_000_144), false);
    assert_eq!(pruned.len(), 1);
    assert_eq!(wire.held_scene_frame_count(), 0);
    assert!(pruned[0].reason.starts_with("stale_scene_epoch:3<4"));

    // Bounded overflow: pushing beyond MAX_HELD_SCENE_FRAMES evicts the oldest held frame
    // and surfaces a `held_scene_overflow` ledger event instead of dropping silently (`BF-01`).
    let total_overflow_pushes = MAX_HELD_SCENE_FRAMES + 4;
    let mut overflow_evictions = Vec::new();
    for idx in 0..total_overflow_pushes {
        let n = TypedNotification::new("OnCombatMessage", vec![PyRep::Integer(idx as i64)])
            .with_scene_scope(Some(OutboundSceneScope::space(Some(30_000_144), Some(5))));
        match wire.gate_typed_notification(n) {
            GateNotificationOutcome::Held(ev_opt) => {
                if let Some(ev) = ev_opt {
                    overflow_evictions.push(ev);
                }
            }
            other => panic!("expected held frame under overflow test, got {other:?}"),
        }
    }
    assert_eq!(wire.held_scene_frame_count(), MAX_HELD_SCENE_FRAMES);
    assert_eq!(overflow_evictions.len(), 4);
    assert!(overflow_evictions
        .iter()
        .all(|ev| ev.kind == SequencerLedgerEventKind::Dropped && ev.reason == "held_scene_overflow"));
}

#[test]
fn solar_system_scope_delivers_docked_in_matching_system_and_drops_foreign_system() {
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let mut wire = SessionWireState::new(1190_05, 1190_05).with_scene_epoch(3);
    wire.leave_space();
    assert!(wire.sync_scene(3, Some(sys_a), false).is_empty());

    let same_sys = TypedNotification::new("OnCfgDataChanged", vec![])
        .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_a), Some(3))));
    assert!(matches!(
        wire.gate_typed_notification(same_sys),
        GateNotificationOutcome::Emit(_)
    ));

    let other_sys = TypedNotification::new("OnCfgDataChanged", vec![])
        .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_b), Some(3))));
    assert!(matches!(
        wire.gate_typed_notification(other_sys),
        GateNotificationOutcome::Dropped(_)
    ));

    let space_only = TypedNotification::new("OnDamageMessage", vec![])
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_a), Some(3))));
    assert!(matches!(
        wire.gate_typed_notification(space_only),
        GateNotificationOutcome::Dropped(_)
    ));
}

fn make_test_set_state_batch(
    sys_id: u32,
    ball_id: i64,
    epoch: u32,
    stamp: u32,
    scope: Option<OutboundSceneScope>,
) -> DestinyBatch {
    let mut bp = eve_destiny::Ballpark::new(sys_id);
    bp.current_tick = stamp as u64;
    bp.add_ball(eve_destiny::Ball {
        id: ball_id,
        type_id: 587,
        position: eve_destiny::DVec3::ZERO,
        velocity: eve_destiny::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 3.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    });
    let payload = eve_destiny::wire::encode_set_state_payload(&bp, ball_id);
    DestinyBatch::new(epoch, stamp, true, named_update(stamp, "SetState", vec![payload]))
        .with_scene_scope(scope)
}

#[test]
fn held_non_zero_epoch_set_state_and_followup_flush_when_scene_sync_awaits_entry() {
    let sys_old = 30_000_140_u32;
    let sys_new = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_06, 1190_06).with_scene_epoch(2);
    let ep_old = wire.enter_space(Some(sys_old));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep_old, 10, true, dummy_updates("SetState", 10))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_old), Some(2))))
    )
    .is_some());

    // Pre-queue notification BEFORE SetState and a held SetState + delta with a non-zero
    // foreign destiny epoch (99) under future scene_epoch 3.
    assert!(matches!(
        wire.gate_typed_notification(
            TypedNotification::new("OnCombatMessage", vec![PyRep::String("pre".into())])
                .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3))))
        ),
        GateNotificationOutcome::Held(None)
    ));

    assert!(matches!(
        wire.accept_destiny_batch_with_reason(make_test_set_state_batch(
            sys_new,
            6001,
            99,
            20,
            Some(OutboundSceneScope::space(Some(sys_new), Some(3))),
        )),
        AcceptDestinyOutcome::Held(None)
    ));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(
            DestinyBatch::new(99, 21, false, dummy_updates("Stop", 21))
                .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3))))
        ),
        AcceptDestinyOutcome::Held(None)
    ));
    assert_eq!(wire.held_scene_frame_count(), 3);

    // Scene advances (`enter_space` + `sync_scene_with_ballpark` via OutboundSceneSync) while
    // `scene_entered` starts false; held SetState must drain, rebind epoch 99 -> active, and flush all 3
    // in strict FIFO order without dropping earlier held frames.
    let active_ep = wire.enter_space(Some(sys_new));
    let outcome = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 1,
            response_frame: PyRep::None,
            session_change_frame: Some(PyRep::String("jump".into())),
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::None,
            notifications: Vec::new(),
        },
        Some(OutboundSceneSync {
            scene_epoch: 3,
            solar_system_id: Some(sys_new),
            is_in_space: true,
            has_ballpark: true,
        }),
    );
    assert!(wire.scene_entered);
    assert_eq!(wire.held_scene_frame_count(), 0);
    assert!(
        outcome
            .events
            .iter()
            .all(|e| e.kind != SequencerLedgerEventKind::Dropped),
        "expected zero drops when flushing held frames, got {:?}",
        outcome.events
    );
    let pos_set_state = outcome
        .frames
        .iter()
        .position(|f| matches!(
            f,
            OutFrame::DoDestinyUpdate { epoch, stamp: 20, has_set_state: true, .. } if *epoch == active_ep
        ))
        .expect("SetState frame");
    let pos_combat = outcome
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnCombatMessage"))
        .expect("pre-queued OnCombatMessage frame");
    let pos_stop = outcome
        .frames
        .iter()
        .position(|f| matches!(
            f,
            OutFrame::DoDestinyUpdate { epoch, stamp: 21, has_set_state: false, .. } if *epoch == active_ep
        ))
        .expect("post-SetState Stop frame");
    assert!(
        pos_set_state < pos_combat && pos_combat < pos_stop,
        "expected SetState < OnCombatMessage < Stop, got ({pos_set_state}, {pos_combat}, {pos_stop})"
    );
}

#[test]
fn structure_gunner_without_ballpark_delivers_space_notifications_without_set_state() {
    let sys_id = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_07, 1190_07).with_scene_epoch(4);
    wire.leave_space();
    assert!(wire
        .sync_scene_with_ballpark(4, Some(sys_id), true, false)
        .is_empty());
    assert!(wire.in_space);
    assert!(wire.scene_entered);
    assert!(matches!(wire.destiny, DestinyStream::Detached));

    let combat = TypedNotification::new("OnDamageMessage", vec![])
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(4))));
    assert!(matches!(
        wire.gate_typed_notification(combat),
        GateNotificationOutcome::Emit(_)
    ));

    // Stepping down from structure gunner drops subsequent space-scoped notifications.
    assert!(wire
        .sync_scene_with_ballpark(5, Some(sys_id), false, false)
        .is_empty());
    let combat_after = TypedNotification::new("OnDamageMessage", vec![])
        .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(5))));
    assert!(matches!(
        wire.gate_typed_notification(combat_after),
        GateNotificationOutcome::Dropped(_)
    ));
}

#[test]
fn held_pre_set_state_lower_stamp_destiny_drains_before_post_set_state_higher_stamp() {
    let sys_old = 30_000_140_u32;
    let sys_new = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_08, 1190_08).with_scene_epoch(2);
    let ep_old = wire.enter_space(Some(sys_old));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep_old, 10, true, dummy_updates("SetState", 10))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_old), Some(2))))
    )
    .is_some());

    // Queue a held pre-SetState delta (stamp 21), then held SetState (stamp 20), then held post-SetState delta (stamp 22).
    // Even though stamp 21 < stamp 22, draining must stop at SetState(20) and flush 21 before 22 with zero stamp_regression/CRT drops.
    let future_scope = Some(OutboundSceneScope::space(Some(sys_new), Some(3)));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(
            DestinyBatch::new(99, 21, false, dummy_updates("Stop", 21)).with_scene_scope(future_scope)
        ),
        AcceptDestinyOutcome::Held(None)
    ));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(make_test_set_state_batch(
            sys_new,
            6002,
            99,
            20,
            future_scope,
        )),
        AcceptDestinyOutcome::Held(None)
    ));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(
            DestinyBatch::new(
                99,
                22,
                false,
                named_update(22, "SetSpeedFraction", vec![PyRep::Integer(6002), PyRep::Real(0.5)])
            )
            .with_scene_scope(future_scope)
        ),
        AcceptDestinyOutcome::Held(None)
    ));

    let active_ep = wire.enter_space(Some(sys_new));
    let outcome = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 2,
            response_frame: PyRep::None,
            session_change_frame: None,
            item_change_before_session_change: false,
            pending_tick_batches: Vec::new(),
            destiny_drain: DestinyDrain::None,
            notifications: Vec::new(),
        },
        Some(OutboundSceneSync {
            scene_epoch: 3,
            solar_system_id: Some(sys_new),
            is_in_space: true,
            has_ballpark: true,
        }),
    );

    assert!(wire.scene_entered);
    assert_eq!(wire.held_scene_frame_count(), 0);
    assert!(
        outcome
            .events
            .iter()
            .all(|e| e.kind != SequencerLedgerEventKind::Dropped),
        "expected zero stamp_regression/CRT drops when draining held batches, got {:?}",
        outcome.events
    );
    let stamps: Vec<(usize, u32)> = outcome
        .frames
        .iter()
        .enumerate()
        .filter_map(|(idx, f)| match f {
            OutFrame::DoDestinyUpdate { epoch, stamp, .. } if *epoch == active_ep => {
                Some((idx, *stamp))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        stamps.iter().map(|&(_, stamp)| stamp).collect::<Vec<_>>(),
        vec![20, 21, 22]
    );
}

#[test]
fn held_scene_overflow_increments_scene_drop_count() {
    let sys_id = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_09, 1190_09).with_scene_epoch(1);
    let ep = wire.enter_space(Some(sys_id));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep, 10, true, dummy_updates("SetState", 10))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_id), Some(1))))
    )
    .is_some());

    let future_scope = Some(OutboundSceneScope::space(Some(sys_id), Some(2)));
    for idx in 0..MAX_HELD_SCENE_FRAMES {
        assert!(matches!(
            wire.gate_typed_notification(
                TypedNotification::new("OnCombatMessage", vec![PyRep::Integer(idx as i64)])
                    .with_scene_scope(future_scope)
            ),
            GateNotificationOutcome::Held(None)
        ));
    }
    assert_eq!(wire.scene_drop_count(), 0);

    match wire.gate_typed_notification(
        TypedNotification::new("OnCombatMessage", vec![PyRep::Integer(999)])
            .with_scene_scope(future_scope),
    ) {
        GateNotificationOutcome::Held(Some(ev)) => {
            assert_eq!(ev.kind, SequencerLedgerEventKind::Dropped);
            assert_eq!(ev.reason, "held_scene_overflow");
        }
        other => panic!("expected Held(Some(overflow)), got {other:?}"),
    }
    assert_eq!(wire.scene_drop_count(), 1);
}

fn rpc_with_drain(
    call_id: u64,
    destiny_drain: DestinyDrain,
    notifications: Vec<TypedNotification>,
) -> RpcCompletion {
    RpcCompletion {
        call_id,
        response_frame: PyRep::None,
        session_change_frame: None,
        item_change_before_session_change: false,
        pending_tick_batches: Vec::new(),
        destiny_drain,
        notifications,
    }
}

#[test]
fn bf01_held_set_state_and_same_stamp_held_delta_finalize_before_coalesce() {
    let sys_old = 30_000_140_u32;
    let sys_new = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_10, 1190_10).with_scene_epoch(2);
    let ep_old = wire.enter_space(Some(sys_old));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep_old, 10, true, dummy_updates("SetState", 10))
            .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_old), Some(2))))
    )
    .is_some());

    let future_scope = Some(OutboundSceneScope::space(Some(sys_new), Some(3)));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(make_test_set_state_batch(
            sys_new,
            6001,
            99,
            20,
            future_scope,
        )),
        AcceptDestinyOutcome::Held(None)
    ));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(
            DestinyBatch::new(
                99,
                20,
                false,
                PyRep::List(vec![
                    named_entry(20, "OnSpecialFX", vec![PyRep::Integer(8888)]),
                    named_entry(20, "AddBalls2", vec![PyRep::Integer(7001)]),
                ]),
            )
            .with_scene_scope(future_scope),
        ),
        AcceptDestinyOutcome::Held(None)
    ));
    assert!(matches!(
        wire.gate_typed_notification(
            TypedNotification::new(
                "OnDamageStateChange",
                vec![PyRep::Integer(7001), PyRep::Tuple(vec![])],
            )
            .with_scene_scope(future_scope),
        ),
        GateNotificationOutcome::Held(None)
    ));

    let _ep_new = wire.enter_space(Some(sys_new));
    let outcome = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(10, DestinyDrain::None, Vec::new()),
        Some(OutboundSceneSync {
            scene_epoch: 3,
            solar_system_id: Some(sys_new),
            is_in_space: true,
            has_ballpark: true,
        }),
    );

    assert!(wire.materialized.contains(&7001));
    assert!(
        outcome.events.iter().any(|ev| {
            ev.kind == SequencerLedgerEventKind::Dropped
                && ev.packet_kind.as_deref() == Some("OnSpecialFX")
        }),
        "expected unmaterialized OnSpecialFX in held same-stamp delta to drop, got {:?}",
        outcome.events
    );
    assert!(
        outcome
            .events
            .iter()
            .any(|ev| ev.kind == SequencerLedgerEventKind::Coalesced && ev.stamp == 20),
        "expected stamp-20 Coalesced event, got {:?}",
        outcome.events
    );
    let actions: Vec<Vec<&str>> = outcome.frames.iter().filter_map(frame_actions).collect();
    assert_eq!(actions, vec![vec!["SetState", "AddBalls2"]]);
    assert!(outcome.frames.iter().any(|f| matches!(
        f,
        OutFrame::Notification { event, .. } if event == "OnDamageStateChange"
    )));
}

#[test]
fn bf01_live_updates_and_same_stamp_held_delta_finalize_before_coalesce() {
    let sys_id = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_11, 1190_11).with_scene_epoch(1);
    let ep = wire.enter_space(Some(sys_id));
    let scope_ep1 = Some(OutboundSceneScope::space(Some(sys_id), Some(1)));
    assert!(sequence_tick(
        &mut wire,
        make_test_set_state_batch(sys_id, 6001, ep, 19, scope_ep1),
    )
    .is_some());

    let scope_ep2 = Some(OutboundSceneScope::space(Some(sys_id), Some(2)));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(
            DestinyBatch::new(
                ep,
                20,
                false,
                PyRep::List(vec![
                    named_entry(20, "OnSpecialFX", vec![PyRep::Integer(8888)]),
                    named_entry(20, "AddBalls2", vec![PyRep::Integer(7002)]),
                ]),
            )
            .with_scene_scope(scope_ep2),
        ),
        AcceptDestinyOutcome::Held(None)
    ));

    let outcome = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(
            11,
            DestinyDrain::Updates(
                DestinyBatch::new(
                    ep,
                    20,
                    false,
                    named_update(20, "Stop", vec![PyRep::Integer(6001)]),
                )
                .with_scene_scope(scope_ep2),
            ),
            vec![TypedNotification::new(
                "OnDamageStateChange",
                vec![PyRep::Integer(7002), PyRep::Tuple(vec![])],
            )
            .with_scene_scope(scope_ep2)],
        ),
        Some(OutboundSceneSync {
            scene_epoch: 2,
            solar_system_id: Some(sys_id),
            is_in_space: true,
            has_ballpark: true,
        }),
    );

    assert!(wire.materialized.contains(&7002));
    assert!(
        outcome.events.iter().any(|ev| {
            ev.kind == SequencerLedgerEventKind::Dropped
                && ev.packet_kind.as_deref() == Some("OnSpecialFX")
        }),
        "expected unmaterialized OnSpecialFX in held delta after Updates(S) to drop, got {:?}",
        outcome.events
    );
    assert!(
        outcome
            .events
            .iter()
            .any(|ev| ev.kind == SequencerLedgerEventKind::Coalesced && ev.stamp == 20),
        "expected stamp-20 Coalesced event, got {:?}",
        outcome.events
    );
    let actions: Vec<Vec<&str>> = outcome.frames.iter().filter_map(frame_actions).collect();
    assert_eq!(actions, vec![vec!["Stop", "AddBalls2"]]);
    assert!(outcome.frames.iter().any(|f| matches!(
        f,
        OutFrame::Notification { event, .. } if event == "OnDamageStateChange"
    )));
}

#[test]
fn bf02_attach_sequence_scene_scoped_add_and_remove_defers_remove_balls() {
    let sys_id = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_12, 1190_12).with_scene_epoch(1);
    let scope = Some(OutboundSceneScope::space(Some(sys_id), Some(1)));

    let mut set_state = make_test_set_state_batch(sys_id, 6001, 1, 100, scope);
    if let PyRep::List(ref mut items) = set_state.updates {
        items.push(named_entry(100, "AddBalls2", vec![PyRep::Integer(7003)]));
    }
    let followup_remove = DestinyBatch::new(
        1,
        100,
        false,
        PyRep::List(vec![make_remove_balls_entry(100, &[7003])]),
    )
    .with_scene_scope(scope);

    let outcome = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(
            12,
            DestinyDrain::AttachSequence {
                epoch: 1,
                set_state,
                followups: vec![followup_remove],
            },
            Vec::new(),
        ),
        Some(OutboundSceneSync {
            scene_epoch: 1,
            solar_system_id: Some(sys_id),
            is_in_space: true,
            has_ballpark: true,
        }),
    );

    let destiny_stamps_and_actions: Vec<(u32, Vec<&str>)> = outcome
        .frames
        .iter()
        .filter_map(|f| match f {
            OutFrame::DoDestinyUpdate { stamp, .. } => {
                frame_actions(f).map(|actions| (*stamp, actions))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        destiny_stamps_and_actions,
        vec![
            (100, vec!["SetState", "AddBalls2"]),
            (101, vec!["RemoveBalls"]),
        ]
    );
    assert!(outcome
        .events
        .iter()
        .all(|ev| ev.kind != SequencerLedgerEventKind::Dropped));
}

#[test]
fn p2_02_held_future_set_state_stays_held_across_pre_attach_scene_sync() {
    let sys_a = 30_000_140_u32;
    let sys_b = 30_000_142_u32;
    let mut wire = SessionWireState::new(1190_13, 1190_13).with_scene_epoch(1);
    wire.leave_space();
    assert!(wire.sync_scene(1, Some(sys_a), false).is_empty());

    let future_scope = Some(OutboundSceneScope::space(Some(sys_b), Some(2)));
    assert!(matches!(
        wire.accept_destiny_batch_with_reason(make_test_set_state_batch(
            sys_b,
            6004,
            99,
            30,
            future_scope,
        )),
        AcceptDestinyOutcome::Held(None)
    ));
    assert_eq!(wire.held_scene_frame_count(), 1);

    // Scene sync arrives before Attach / enter_space (destiny stream still Detached).
    // Held future SetState must stay Held rather than being flushed into Detached and dropped.
    let pre_attach_sync = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(13, DestinyDrain::None, Vec::new()),
        Some(OutboundSceneSync {
            scene_epoch: 2,
            solar_system_id: Some(sys_b),
            is_in_space: true,
            has_ballpark: true,
        }),
    );
    assert!(matches!(wire.destiny, DestinyStream::Detached));
    assert_eq!(wire.scene_drop_count(), 0);
    assert_eq!(wire.held_scene_frame_count(), 1);
    assert!(pre_attach_sync
        .events
        .iter()
        .all(|ev| ev.kind != SequencerLedgerEventKind::Dropped));

    // Once enter_space moves destiny -> AwaitingSetState, the held SetState flushes cleanly.
    let active_ep = wire.enter_space(Some(sys_b));
    let post_attach = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(14, DestinyDrain::None, Vec::new()),
        None,
    );
    assert_eq!(wire.held_scene_frame_count(), 0);
    assert!(wire.scene_entered);
    assert!(matches!(
        wire.destiny,
        DestinyStream::Live {
            epoch,
            set_state_stamp: 30,
            last_stamp: 30
        } if epoch == active_ep
    ));
    assert!(wire.materialized.contains(&6004));
    assert!(post_attach.frames.iter().any(|f| matches!(
        f,
        OutFrame::DoDestinyUpdate { epoch, stamp: 30, has_set_state: true, .. } if *epoch == active_ep
    )));
}

/// Regression guard for #1617: solar-system broadcasts deliver before ballpark entry.
#[test]
fn solar_system_scope_delivers_before_ballpark_entry_while_gating_space_and_stale_epochs() {
    let mut wire = SessionWireState::new(1, 1001);
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;

    // Simulate post-Undock pre-beyonce window: in space in sys_a with ballpark pending,
    // `destiny == Detached` and `scene_entered == false`.
    let sync_ep2 = OutboundSceneSync {
        scene_epoch: 2,
        solar_system_id: Some(sys_a),
        is_in_space: true,
        has_ballpark: true,
    };
    let out = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(
            1,
            DestinyDrain::None,
            vec![
                TypedNotification::new("OnSystemCriminalFlagUpdates", Vec::new())
                    .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_a), Some(2)))),
                TypedNotification::new("OnDamageStateChange", Vec::new())
                    .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_a), Some(2)))),
                TypedNotification::new("OnSystemDisapprovalFlagUpdates", Vec::new())
                    .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_a), Some(1)))),
                TypedNotification::new("OnSystemCriminalFlagUpdates", Vec::new())
                    .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_b), Some(2)))),
                TypedNotification::new("OnSystemDisapprovalFlagUpdates", Vec::new())
                    .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_a), Some(3)))),
            ],
        ),
        Some(sync_ep2),
    );

    assert!(!wire.scene_entered);
    assert!(matches!(wire.destiny, DestinyStream::Detached));
    // Only the matching-epoch, matching-system solar_system broadcast delivers immediately.
    let events: Vec<&str> = out
        .frames
        .iter()
        .filter_map(|f| match f {
            OutFrame::Notification { event, .. } => Some(event.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(events, vec!["OnSystemCriminalFlagUpdates"]);
    // Held: epoch-2 space notification (awaiting SetState) + epoch-3 future solar_system notification.
    assert_eq!(wire.held_scene_frame_count(), 2);
    // Dropped: epoch-1 stale solar_system + sys_b foreign solar_system.
    let dropped_count = out
        .events
        .iter()
        .filter(|ev| ev.kind == SequencerLedgerEventKind::Dropped)
        .count();
    assert_eq!(dropped_count, 2);

    // Advancing to epoch 3 before ballpark attach flushes the epoch-3 solar_system frame
    // immediately while dropping the stale epoch-2 space frame.
    let sync_ep3 = OutboundSceneSync {
        scene_epoch: 3,
        solar_system_id: Some(sys_a),
        is_in_space: true,
        has_ballpark: true,
    };
    let out_ep3 = sequence_server_rpc_in_scene_with_events(
        &mut wire,
        rpc_with_drain(2, DestinyDrain::None, Vec::new()),
        Some(sync_ep3),
    );
    let events_ep3: Vec<&str> = out_ep3
        .frames
        .iter()
        .filter_map(|f| match f {
            OutFrame::Notification { event, .. } => Some(event.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(events_ep3, vec!["OnSystemDisapprovalFlagUpdates"]);
    assert_eq!(wire.held_scene_frame_count(), 0);
}

#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "ballpark-scoped notification")]
fn raw_notify_of_marked_ballpark_event_panics_without_scene_in_debug() {
    let ctx = ServerContext::new();
    let _ = ctx.notify(
        NotificationTarget::Character(90_000_001),
        "OnSpecialFX",
        "charid",
        Vec::new(),
    );
}

#[test]
fn marked_ballpark_typed_notifications_attach_scene_and_drop_for_docked_or_wrong_system() {
    use crate::router::notify_registry::{push_typed_session_notification, OnTarget};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let uid = ctx.db.with_snapshot(|s| s.characters[&char_id].user_id);
    ctx.db
        .acquire_online_lock(uid, 9_101, Some(char_id), true, ctx.now_unix())
        .expect("acquire_online_lock");
    let _ = ctx.drain_notifications_for_char(char_id);

    let target_notif = OnTarget {
        scene: BallparkScene::try_with_epoch(sys_a, 7).expect("non-zero scene"),
        arg0: PyRep::String("lost".into()),
        arg1: PyRep::Integer(100_000_010),
        reason: None,
    };
    let emitted = ctx.emit_typed(NotificationTarget::Character(char_id), target_notif.clone());
    assert_eq!(emitted.len(), 1);
    assert_eq!(
        emitted[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_a), Some(7)))
    );

    let pending = ctx.drain_notifications_for_char(char_id);
    assert_eq!(pending.len(), 1);
    let wired = TypedNotification::new(pending[0].event.clone(), pending[0].args.clone())
        .with_scene_scope(pending[0].scene_scope);

    // 1. Docked recipient wire drops the marked ballpark event.
    let mut wire_docked = SessionWireState::new(9_101, 2_001);
    let _ = wire_docked.sync_scene(7, Some(sys_a), false);
    match wire_docked.gate_typed_notification(wired.clone()) {
        GateNotificationOutcome::Dropped(ev) => {
            assert!(ev.reason.starts_with("scene_mismatch:"), "got {ev:?}");
        }
        other => panic!("docked wire must drop marked OnTarget, got {other:?}"),
    }

    // 2. Wrong-system in-space wire drops the marked ballpark event.
    let mut wire_wrong_sys = SessionWireState::new(9_102, 2_002);
    let ep_wrong = wire_wrong_sys.enter_space(Some(sys_b));
    let _ = wire_wrong_sys.accept_destiny_batch(DestinyBatch::new(
        ep_wrong,
        1,
        true,
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::String("SetState".into()), PyRep::Tuple(vec![])]),
        ])]),
    ));
    let _ = wire_wrong_sys.sync_scene(7, Some(sys_b), true);
    match wire_wrong_sys.gate_typed_notification(wired.clone()) {
        GateNotificationOutcome::Dropped(ev) => {
            assert!(ev.reason.starts_with("scene_mismatch:"), "got {ev:?}");
        }
        other => panic!("wrong-system wire must drop marked OnTarget, got {other:?}"),
    }

    // 3. Matching entered solar system and scene epoch emits cleanly.
    let mut wire_ok = SessionWireState::new(9_103, 2_003);
    let ep_ok = wire_ok.enter_space(Some(sys_a));
    let _ = wire_ok.accept_destiny_batch(DestinyBatch::new(
        ep_ok,
        1,
        true,
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::String("SetState".into()), PyRep::Tuple(vec![])]),
        ])]),
    ));
    let _ = wire_ok.sync_scene(7, Some(sys_a), true);
    assert!(matches!(
        wire_ok.gate_typed_notification(wired),
        GateNotificationOutcome::Emit(n) if n.event == "OnTarget"
    ));

    // 4. Session-push path also drops for docked/wrong-system and keeps for undocked matching system.
    let mut docked_sess = SessionState::new(9_104);
    docked_sess.solar_system_id = Some(sys_a);
    docked_sess.station_id = Some(60_003_760);
    push_typed_session_notification(&mut docked_sess, target_notif.clone());
    assert!(docked_sess.drain_notifications().is_empty());

    let mut space_sess = SessionState::new(9_105);
    space_sess.solar_system_id = Some(sys_a);
    push_typed_session_notification(&mut space_sess, target_notif);
    assert_eq!(space_sess.drain_notifications().len(), 1);
}

#[test]
fn try_solar_system_rejects_zero_and_emit_in_ballpark_and_pending_push_enforce_scope() {
    use crate::router::notify_registry::{
        push_pending_session_notification, OnGodmaShipEffect, OnTarget,
    };
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    assert!(BallparkScene::try_solar_system(0).is_none());
    assert!(BallparkScene::try_with_epoch(0, 5).is_none());
    assert!(BallparkScene::try_from_outbound_scope(
        OutboundSceneScope::space(None, Some(5)),
        0
    )
    .is_none());
    let from_scope =
        BallparkScene::try_from_outbound_scope(OutboundSceneScope::space(Some(30_000_142), Some(9)), 0)
            .expect("non-zero scope solar system");
    assert_eq!(
        from_scope,
        BallparkScene::try_with_epoch(30_000_142, 9).expect("non-zero scene")
    );

    let ctx = ServerContext::new();
    let char_id = 90_000_001_i64;
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))
        .expect("seeded char 90_000_001");
    ctx.db
        .acquire_online_lock(uid, 9_101, Some(char_id), true, ctx.now_unix())
        .expect("online lock");
    let sys_a = 30_000_142_u32;
    let sys_b = 30_000_144_u32;
    let _ = ctx.drain_notifications_for_char(char_id);

    // Non-ballpark event with `scene_scope() == None` emitted via `emit_typed_in_ballpark`
    // inherits both `solar_system_id` and `scene_epoch` from `BallparkScene`.
    let godma_emitted = ctx.emit_typed_in_ballpark(
        NotificationTarget::Character(char_id),
        BallparkScene::try_with_epoch(sys_a, 11).expect("non-zero scene"),
        OnGodmaShipEffect {
            item_id: 700_001,
            effect: PyRep::Integer(16),
            start_or_time: PyRep::Integer(0),
            start: Some(PyRep::Integer(1)),
            active: Some(PyRep::Integer(1)),
            environment: Some(PyRep::Tuple(vec![])),
            start_time: Some(PyRep::Integer(0)),
            duration: Some(PyRep::Real(1000.0)),
            repeat: Some(PyRep::Integer(0)),
            error: Some(PyRep::None),
            actual_stop_time: None,
        },
    );
    assert_eq!(godma_emitted.len(), 1);
    assert_eq!(
        godma_emitted[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_a), Some(11)))
    );

    // `emit_typed_in_ballpark` merges `scene.scene_epoch` when marked event carries `scene_epoch: None`.
    let merged_emitted = ctx.emit_typed_in_ballpark(
        NotificationTarget::Character(char_id),
        BallparkScene::try_with_epoch(sys_a, 13).expect("non-zero scene"),
        OnTarget {
            scene: BallparkScene::try_solar_system(sys_a).expect("non-zero scene"),
            arg0: PyRep::String("lost".into()),
            arg1: PyRep::Integer(700_002),
            reason: None,
        },
    );
    assert_eq!(merged_emitted.len(), 1);
    assert_eq!(
        merged_emitted[0].scene_scope,
        Some(OutboundSceneScope::space(Some(sys_a), Some(13)))
    );

    // `push_pending_session_notification` applies the same in-space & solar-system gate as `push_typed_session_notification`.
    let pending_ballpark = merged_emitted.into_iter().next().expect("pending notif");
    let mut docked_sess = SessionState::new(9_201);
    docked_sess.solar_system_id = Some(sys_a);
    docked_sess.structure_id = Some(1_040_000_000_001);
    docked_sess.ship_id = Some(100_000_001);
    push_pending_session_notification(&mut docked_sess, pending_ballpark.clone());
    assert!(docked_sess.drain_notifications().is_empty());

    let mut wrong_sys_sess = SessionState::new(9_202);
    wrong_sys_sess.solar_system_id = Some(sys_b);
    push_pending_session_notification(&mut wrong_sys_sess, pending_ballpark.clone());
    assert!(wrong_sys_sess.drain_notifications().is_empty());

    let mut matching_sess = SessionState::new(9_203);
    matching_sess.solar_system_id = Some(sys_a);
    push_pending_session_notification(&mut matching_sess, pending_ballpark);
    assert_eq!(matching_sess.drain_notifications().len(), 1);

    // `OutboundSceneScope::solar_system(Some(sys_a), None)` (`requires_in_space: false`) still enforces system match
    // across docked and unsystemed sessions (`F-03`).
    let mut sys_scoped_notif = crate::router::notify_registry::build_pending_registered_notification(
        char_id,
        Some(uid),
        "OnDungeonCompleted",
        "charid",
        vec![PyRep::Integer(1)],
        0,
    )
    .expect("registered OnDungeonCompleted");
    sys_scoped_notif.scene_scope = Some(OutboundSceneScope::solar_system(Some(sys_a), None));

    let mut wrong_sys_docked = SessionState::new(9_204);
    wrong_sys_docked.solar_system_id = Some(sys_b);
    wrong_sys_docked.station_id = Some(60_003_760);
    push_pending_session_notification(&mut wrong_sys_docked, sys_scoped_notif.clone());
    assert!(wrong_sys_docked.drain_notifications().is_empty());

    let mut no_sys_sess = SessionState::new(9_205);
    no_sys_sess.solar_system_id = None;
    push_pending_session_notification(&mut no_sys_sess, sys_scoped_notif.clone());
    assert!(no_sys_sess.drain_notifications().is_empty());

    let mut same_sys_docked = SessionState::new(9_206);
    same_sys_docked.solar_system_id = Some(sys_a);
    same_sys_docked.station_id = Some(60_003_760);
    push_pending_session_notification(&mut same_sys_docked, sys_scoped_notif);
    assert_eq!(same_sys_docked.drain_notifications().len(), 1);
}

#[test]
fn session_wire_state_scene_matches_scope_rejects_in_space_without_system_id() {
    let mut wire = SessionWireState::new(1190_09, 1190_09);
    let _ = wire.enter_space(Some(30_000_142));
    assert!(wire.in_space);
    assert!(!wire.scene_matches_scope(OutboundSceneScope::space(None, None)));
    assert!(!wire.scene_matches_scope(OutboundSceneScope::space(None, Some(1))));
    assert!(wire.scene_matches_scope(OutboundSceneScope::space(Some(30_000_142), None)));
    assert!(!wire.scene_matches_scope(OutboundSceneScope::space(Some(30_000_144), None)));
    assert!(wire.scene_matches_scope(OutboundSceneScope::solar_system(None, None)));
    assert!(!wire.scene_matches_scope(OutboundSceneScope::solar_system(Some(30_000_144), None)));
}
