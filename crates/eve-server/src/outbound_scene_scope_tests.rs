use super::*;
use crate::outbound_sequencer::tests::{dummy_updates, named_update};
use crate::outbound_sequencer::{
    sequence_server_rpc_in_scene_with_events, sequence_tick, sequence_tick_with_events,
    sequence_with_events, AcceptDestinyOutcome, DestinyBatch, DestinyDrain, DestinyStream,
    OutFrame, RpcCompletion, SequencerLedgerEventKind, SessionWireState, TypedNotification,
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
