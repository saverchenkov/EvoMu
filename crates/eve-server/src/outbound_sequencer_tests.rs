use super::*;

pub(crate) fn dummy_updates(label: &str, stamp: u32) -> PyRep {
    PyRep::List(vec![PyRep::Tuple(vec![
        PyRep::Integer(i64::from(stamp)),
        PyRep::Tuple(vec![PyRep::String(label.to_string()), PyRep::Tuple(vec![])]),
    ])])
}

#[test]
fn destiny_stream_awaiting_set_state_ignores_non_set_state_without_detaching_b7() {
    let mut wire = SessionWireState::new(1, 1001);
    let epoch = wire.enter_space(Some(30_000_142));
    assert_eq!(wire.destiny, DestinyStream::AwaitingSetState { epoch });

    // #845 (B7): A tick or empty/non-SetState drain before SetState must NOT transition back
    // to Detached and must NOT emit DoDestinyUpdate before SetState.
    let pre_tick = DestinyBatch::new(epoch, 10, false, dummy_updates("AddBalls2", 10));
    assert!(sequence_tick(&mut wire, pre_tick).is_none());
    assert_eq!(
        wire.destiny,
        DestinyStream::AwaitingSetState { epoch },
        "Session must remain in AwaitingSetState until SetState arrives"
    );

    // Now SetState arrives (e.g. from the tick broadcast or post-bind drain) -> transitions to Live.
    let set_state = DestinyBatch::new(epoch, 11, true, dummy_updates("SetState", 11));
    let frame = sequence_tick(&mut wire, set_state).expect("SetState must be emitted");
    assert!(matches!(
        frame,
        OutFrame::DoDestinyUpdate {
            has_set_state: true,
            stamp: 11,
            ..
        }
    ));
    assert_eq!(
        wire.destiny,
        DestinyStream::Live {
            epoch,
            set_state_stamp: 11,
            last_stamp: 11,
        }
    );
}

#[test]
fn destiny_stream_live_rejects_stamp_regression_and_coalesces_equal_stamp() {
    let mut wire = SessionWireState::new(2, 1002);
    let epoch = wire.enter_space(Some(30_000_142));
    let set_state = DestinyBatch::new(epoch, 100, true, dummy_updates("SetState", 100));
    assert!(sequence_tick(&mut wire, set_state).is_some());

    // Direct post-RPC drain at tick 105 advances last_stamp to 105.
    let direct = DestinyBatch::new(epoch, 105, false, dummy_updates("GotoDirection", 105));
    assert!(sequence_tick(&mut wire, direct).is_some());
    assert_eq!(
        wire.destiny,
        DestinyStream::Live {
            epoch,
            set_state_stamp: 100,
            last_stamp: 105,
        }
    );

    // #1077 §A: Tick broadcast strictly below last_sent (104 < 105) MUST be rejected with reason.
    let tick_104 = DestinyBatch::new(epoch, 104, false, dummy_updates("AddBalls2", 104));
    let (emitted, drop_ev) = sequence_tick_with_event(&mut wire, tick_104);
    assert!(
        emitted.is_none(),
        "Strict stamp regression (104 < 105) must be dropped"
    );
    let drop_ev = drop_ev.expect("drop event must be recorded");
    assert_eq!(drop_ev.kind, SequencerLedgerEventKind::Dropped);
    assert_eq!(drop_ev.stamp, 104);
    assert!(
        drop_ev.reason.contains("stamp_regression:104<105"),
        "unexpected drop reason: {}",
        drop_ev.reason
    );

    // A batch older than set_state_stamp (99 < 100) or wrong epoch is also dropped with reason.
    let stale_99 = DestinyBatch::new(epoch, 99, false, dummy_updates("Stop", 99));
    let (emitted_99, drop_99) = sequence_tick_with_event(&mut wire, stale_99);
    assert!(emitted_99.is_none());
    assert!(drop_99.unwrap().reason.contains("stamp_below_set_state"));

    let wrong_epoch = DestinyBatch::new(epoch + 1, 106, false, dummy_updates("Stop", 106));
    let (emitted_ep, drop_ep) = sequence_tick_with_event(&mut wire, wrong_epoch);
    assert!(emitted_ep.is_none());
    assert!(drop_ep.unwrap().reason.contains("epoch_mismatch"));

    // #1077 §A: Equal-stamp updates (pending tick 105 + direct drain 105) coalesce into one frame.
    let pending_105 = DestinyBatch::new(epoch, 105, false, dummy_updates("AddBalls2", 105));
    let direct_105 = DestinyBatch::new(epoch, 105, false, dummy_updates("OnSpecialFX", 105));
    let outcome = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 77,
            response_frame: PyRep::Integer(77),
            session_change_frame: None,
            pending_tick_batches: vec![pending_105],
            destiny_drain: DestinyDrain::Updates(direct_105),
            notifications: vec![],
            item_change_before_session_change: false,
        },
    );
    let destiny_frames: Vec<&OutFrame> = outcome
        .frames
        .iter()
        .filter(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }))
        .collect();
    assert_eq!(
        destiny_frames.len(),
        1,
        "equal-stamp updates must coalesce into a single DoDestinyUpdate frame"
    );
    if let OutFrame::DoDestinyUpdate {
        stamp,
        updates: PyRep::List(items),
        ..
    } = destiny_frames[0]
    {
        assert_eq!(*stamp, 105);
        assert_eq!(items.len(), 2, "coalesced frame must preserve both updates");
    } else {
        panic!("expected DoDestinyUpdate with PyRep::List updates");
    }
    assert!(outcome.events.iter().any(|ev| {
        ev.kind == SequencerLedgerEventKind::Coalesced
            && ev.stamp == 105
            && ev.reason.contains("equal_stamp_merge")
    }));
}

#[test]
fn broadcast_lag_transitions_to_awaiting_set_state_until_resync_ord5() {
    let mut wire = SessionWireState::new(3, 1003);
    let epoch = wire.enter_space(Some(30_000_142));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(epoch, 50, true, dummy_updates("SetState", 50))
    )
    .is_some());
    assert!(wire.destiny.is_live());

    // Lagged broadcast channel transitions back to AwaitingSetState for the same epoch.
    assert_eq!(wire.on_broadcast_lagged(), Some(epoch));
    assert_eq!(wire.destiny, DestinyStream::AwaitingSetState { epoch });

    // Incremental tick before resync SetState is suppressed.
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(epoch, 60, false, dummy_updates("GotoDirection", 60))
    )
    .is_none());

    // Resync SetState at stamp 61 restores Live state.
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(epoch, 61, true, dummy_updates("SetState", 61))
    )
    .is_some());
    assert_eq!(
        wire.destiny,
        DestinyStream::Live {
            epoch,
            set_state_stamp: 61,
            last_stamp: 61,
        }
    );
}

#[test]
fn in_space_ship_switch_emits_set_state_before_session_change_970() {
    let mut wire = SessionWireState::new(97, 970);
    let epoch = wire.enter_space(Some(30_000_142));
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(epoch, 10, true, dummy_updates("SetState", 10))
    )
    .is_some());
    assert!(wire.destiny.is_live());

    // Pod ejection / ship switch in the same solar system while already `Live`:
    // `DestinyDrain::Updates(batch)` with `batch.has_set_state == true` must be sequenced
    // BEFORE `SessionChangeNotification` (`#970`).
    let pod_set_state = DestinyBatch::new(epoch, 11, true, dummy_updates("SetState", 11));
    let frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 7,
            response_frame: PyRep::Integer(7),
            session_change_frame: Some(PyRep::String("shipid_to_capsule".into())),
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Updates(pod_set_state),
            notifications: vec![TypedNotification {
                event: "OnItemChange".into(),
                args: vec![],
                service: None,
                scene_scope: None,
            }],
            item_change_before_session_change: false,
        },
    );
    assert!(
        frames.iter().any(|f| matches!(f, OutFrame::CallRsp { call_id: 7, .. })),
        "expected CallRsp for call_id 7"
    );
    let dest_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, stamp: 11, .. }))
        .expect("SetState DoDestinyUpdate frame");
    let sess_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::SessionChange { .. }))
        .expect("SessionChange frame");
    let rsp_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::CallRsp { call_id: 7, .. }))
        .expect("CallRsp frame");
    let notif_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { .. }))
        .expect("Notification frame");
    assert!(
        dest_idx < sess_idx && sess_idx < rsp_idx && rsp_idx < notif_idx,
        "expected DoDestinyUpdate(SetState) -> SessionChange -> CallRsp -> Notification, got {dest_idx}, {sess_idx}, {rsp_idx}, {notif_idx}"
    );
}

#[test]
fn session_change_precedes_call_rsp_and_docking_detach_precedes_session_change_928_883() {
    // 1. Session-change RPC with Attach (e.g. SelectCharacterID / Undock / Station Board):
    //    SessionChangeNotification -> Attach(SetState) -> CallRsp -> Notification (#1083).
    let mut wire = SessionWireState::new(88, 883);
    let attach_ep = wire.enter_space(Some(30_000_142));
    let frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 1,
            response_frame: PyRep::Integer(1),
            session_change_frame: Some(PyRep::String("undock_sess".into())),
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Attach {
                epoch: attach_ep,
                set_state: DestinyBatch::new(attach_ep, 20, true, dummy_updates("SetState", 20)),
            },
            notifications: vec![TypedNotification {
                event: "OnItemChange".into(),
                args: vec![],
                service: None,
                scene_scope: None,
            }],
            item_change_before_session_change: false,
        },
    );
    assert!(matches!(frames.first(), Some(OutFrame::SessionChange { .. })));
    let sess_idx = frames.iter().position(|f| matches!(f, OutFrame::SessionChange { .. })).unwrap();
    let rsp_idx = frames.iter().position(|f| matches!(f, OutFrame::CallRsp { call_id: 1, .. })).unwrap();
    let dest_idx = frames.iter().position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. })).unwrap();
    let notif_idx = frames.iter().position(|f| matches!(f, OutFrame::Notification { .. })).unwrap();
    assert!(sess_idx < dest_idx && dest_idx < rsp_idx && rsp_idx < notif_idx);

    // 2. Docking detach (space -> station):
    //    DoDestinyUpdate(OnDockingAccepted) -> SessionChangeNotification -> CallRsp -> Notification.
    let dock_frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 2,
            response_frame: PyRep::Integer(2),
            session_change_frame: Some(PyRep::String("dock_sess".into())),
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Detach {
                pre_detach: Some(DestinyBatch::new(
                    attach_ep,
                    21,
                    false,
                    dummy_updates("OnDockingAccepted", 21),
                )),
            },
            notifications: vec![TypedNotification {
                event: "OnCharNowInStation".into(),
                args: vec![],
                service: None,
                scene_scope: None,
            }],
            item_change_before_session_change: false,
        },
    );
    assert_eq!(wire.destiny, DestinyStream::Detached);
    let d_idx = dock_frames.iter().position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. })).unwrap();
    let s_idx = dock_frames.iter().position(|f| matches!(f, OutFrame::SessionChange { .. })).unwrap();
    let r_idx = dock_frames.iter().position(|f| matches!(f, OutFrame::CallRsp { call_id: 2, .. })).unwrap();
    let n_idx = dock_frames.iter().position(|f| matches!(f, OutFrame::Notification { .. })).unwrap();
    assert!(d_idx < s_idx && s_idx < r_idx && r_idx < n_idx);

    // 3. No-session-change UpdateStateRequest while already Live with SetState:
    //    CallRsp still precedes DoDestinyUpdate(SetState).
    let ep2 = wire.enter_space(Some(30_000_142));
    assert!(sequence_tick(&mut wire, DestinyBatch::new(ep2, 30, true, dummy_updates("SetState", 30))).is_some());
    let usr_frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 3,
            response_frame: PyRep::None,
            session_change_frame: None,
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                ep2,
                31,
                true,
                dummy_updates("SetState", 31),
            )),
            notifications: vec![],
            item_change_before_session_change: false,
        },
    );
    assert!(matches!(usr_frames.first(), Some(OutFrame::CallRsp { call_id: 3, .. })));
    assert!(matches!(usr_frames.get(1), Some(OutFrame::DoDestinyUpdate { has_set_state: true, stamp: 31, .. })));
}

#[test]
fn property_random_interleavings_preserve_ord_invariants() {
    // Deterministic PRNG (xorshift64) testing 500 random event traces of length 60 across
    // {EnterSpace, LeaveSpace, RpcAttach, RpcUpdate, RpcDetach, TickSetState, TickDelta, Lagged}.
    let mut seed: u64 = 0xC3_0927_0844_0845;
    let mut next_u64 = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };

    for trace_idx in 0..500 {
        let mut wire = SessionWireState::new(trace_idx + 1, 10_000 + trace_idx);
        let mut call_id = 0u64;
        let mut stamp = 1u32;
        // Track per-epoch whether SetState has been emitted on the wire and its latest stamp.
        let mut seen_set_state_for_epoch: std::collections::HashMap<u32, u32> =
            std::collections::HashMap::new();

        for _step in 0..60 {
            let op = next_u64() % 8;
            stamp = stamp.saturating_add((next_u64() % 3) as u32);
            match op {
                0 => {
                    // Enter space / jump solar system
                    let sys_id = 30_000_142 + (next_u64() % 5) as u32;
                    let ep = wire.enter_space(Some(sys_id));
                    assert_eq!(wire.destiny, DestinyStream::AwaitingSetState { epoch: ep });
                }
                1 => {
                    // Leave space (dock)
                    wire.leave_space();
                    assert_eq!(wire.destiny, DestinyStream::Detached);
                }
                2 => {
                    // RPC Attach with SetState
                    call_id += 1;
                    let ep = wire.ensure_in_space(Some(30_000_142));
                    let batch =
                        DestinyBatch::new(ep, stamp, true, dummy_updates("SetState", stamp));
                    let frames = sequence(
                        &mut wire,
                        RpcCompletion {
                            call_id,
                            response_frame: PyRep::Integer(call_id as i64),
                            session_change_frame: Some(PyRep::String("sess".into())),
                            pending_tick_batches: vec![],
                            destiny_drain: DestinyDrain::Attach {
                                epoch: ep,
                                set_state: batch,
                            },
                            notifications: vec![TypedNotification {
                                event: "OnItemChange".into(),
                                args: vec![],
                                service: None,
                                scene_scope: None,
                            }],
                            item_change_before_session_change: false,
                        },
                    );
                    // #1083: SessionChangeNotification precedes Attach SetState, which precedes CallRsp and Notifications.
                    let sess_pos = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::SessionChange { .. }))
                        .expect("SessionChange frame");
                    let rsp_pos = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::CallRsp { call_id: cid, .. } if *cid == call_id))
                        .expect("CallRsp frame");
                    let notif_pos = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::Notification { .. }))
                        .expect("Notification frame");
                    assert!(
                        sess_pos < rsp_pos && rsp_pos < notif_pos,
                        "SessionChange ({sess_pos}) must precede CallRsp ({rsp_pos}) before Notification ({notif_pos})"
                    );
                    if let Some(dest_pos) = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }))
                    {
                        assert!(
                            dest_pos < rsp_pos && rsp_pos < notif_pos,
                            "Attach DoDestinyUpdate ({dest_pos}) must precede CallRsp ({rsp_pos}) before Notification ({notif_pos})"
                        );
                    }
                    for f in &frames {
                        if let OutFrame::DoDestinyUpdate {
                            epoch,
                            stamp: s,
                            has_set_state,
                            ..
                        } = *f
                        {
                            if has_set_state {
                                seen_set_state_for_epoch.insert(epoch, s);
                            } else {
                                let ss = seen_set_state_for_epoch.get(&epoch).copied();
                                assert!(
                                    ss.is_some_and(|ss_stamp| s >= ss_stamp),
                                    "ORD-3 violated: non-SetState emitted without prior SetState in epoch {epoch}"
                                );
                            }
                        }
                    }
                }
                3 => {
                    // RPC with pending tick (possibly older stamp than direct drain) + direct Updates
                    call_id += 1;
                    let ep = wire.current_epoch();
                    let tick_stamp = stamp.saturating_sub(1).max(1);
                    let tick_batch = DestinyBatch::new(
                        ep,
                        tick_stamp,
                        false,
                        dummy_updates("AddBalls2", tick_stamp),
                    );
                    let direct_batch = DestinyBatch::new(
                        ep,
                        stamp,
                        false,
                        dummy_updates("GotoDirection", stamp),
                    );
                    let frames = sequence(
                        &mut wire,
                        RpcCompletion {
                            call_id,
                            response_frame: PyRep::Integer(call_id as i64),
                            session_change_frame: None,
                            pending_tick_batches: vec![tick_batch],
                            destiny_drain: DestinyDrain::Updates(direct_batch),
                            notifications: vec![],
                            item_change_before_session_change: false,
                        },
                    );
                    assert!(matches!(frames.first(), Some(OutFrame::CallRsp { call_id: cid, .. }) if *cid == call_id));
                    for f in &frames {
                        if let OutFrame::DoDestinyUpdate {
                            epoch,
                            stamp: s,
                            has_set_state,
                            ..
                        } = *f
                        {
                            if has_set_state {
                                seen_set_state_for_epoch.insert(epoch, s);
                            } else {
                                let ss = seen_set_state_for_epoch.get(&epoch).copied();
                                assert!(
                                    ss.is_some_and(|ss_stamp| s >= ss_stamp),
                                    "ORD-3 violated: non-SetState at {s} emitted without prior SetState in epoch {epoch}"
                                );
                            }
                        }
                    }
                }
                4 => {
                    // RPC Detach (docking)
                    call_id += 1;
                    let ep = wire.current_epoch();
                    let pre = DestinyBatch::new(
                        ep,
                        stamp,
                        false,
                        dummy_updates("OnDockingAccepted", stamp),
                    );
                    let frames = sequence(
                        &mut wire,
                        RpcCompletion {
                            call_id,
                            response_frame: PyRep::Integer(call_id as i64),
                            session_change_frame: Some(PyRep::String("dock_sess".into())),
                            pending_tick_batches: vec![],
                            destiny_drain: DestinyDrain::Detach {
                                pre_detach: Some(pre),
                            },
                            notifications: vec![],
                            item_change_before_session_change: false,
                        },
                    );
                    assert_eq!(wire.destiny, DestinyStream::Detached);
                    // Pre-detach DoDestinyUpdate (if any) must precede SessionChange, which must precede CallRsp (#883, #928).
                    let sess_pos = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::SessionChange { .. }))
                        .expect("Docking SessionChange frame");
                    let rsp_pos = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::CallRsp { call_id: cid, .. } if *cid == call_id))
                        .expect("Docking CallRsp frame");
                    let dest_pos = frames
                        .iter()
                        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }));
                    if let Some(dp) = dest_pos {
                        assert!(dp < sess_pos, "Docking DoDestinyUpdate must precede SessionChange");
                    }
                    assert!(
                        sess_pos < rsp_pos,
                        "Docking SessionChange ({sess_pos}) must precede CallRsp ({rsp_pos})"
                    );
                    for f in &frames {
                        if let OutFrame::DoDestinyUpdate {
                            epoch,
                            stamp: s,
                            has_set_state,
                            ..
                        } = *f
                        {
                            if !has_set_state {
                                let ss = seen_set_state_for_epoch.get(&epoch).copied();
                                assert!(ss.is_some_and(|ss_stamp| s >= ss_stamp));
                            }
                        }
                    }
                }
                5 => {
                    // Standalone Tick with SetState
                    let ep = wire.current_epoch();
                    let b =
                        DestinyBatch::new(ep, stamp, true, dummy_updates("SetState", stamp));
                    if let Some(OutFrame::DoDestinyUpdate {
                        epoch,
                        stamp: s,
                        has_set_state,
                        ..
                    }) = sequence_tick(&mut wire, b)
                    {
                        assert!(has_set_state);
                        seen_set_state_for_epoch.insert(epoch, s);
                    }
                }
                6 => {
                    // Standalone Tick with delta (possibly slightly older stamp than last_stamp)
                    let ep = wire.current_epoch();
                    let delta_stamp = if next_u64() % 2 == 0 {
                        stamp.saturating_sub(1).max(1)
                    } else {
                        stamp
                    };
                    let b = DestinyBatch::new(
                        ep,
                        delta_stamp,
                        false,
                        dummy_updates("AddBalls2", delta_stamp),
                    );
                    if let Some(OutFrame::DoDestinyUpdate {
                        epoch,
                        stamp: s,
                        has_set_state,
                        ..
                    }) = sequence_tick(&mut wire, b)
                    {
                        assert!(!has_set_state);
                        let ss = seen_set_state_for_epoch.get(&epoch).copied();
                        assert!(
                            ss.is_some_and(|ss_stamp| s >= ss_stamp),
                            "ORD-3 violated on standalone tick"
                        );
                    }
                }
                _ => {
                    // Broadcast channel lag -> transitions back to AwaitingSetState
                    if let Some(ep) = wire.on_broadcast_lagged() {
                        seen_set_state_for_epoch.remove(&ep);
                    }
                }
            }
        }
    }
}

pub(crate) fn named_entry(stamp: u32, action: &str, args: Vec<PyRep>) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(i64::from(stamp)),
        PyRep::Tuple(vec![PyRep::String(action.into()), PyRep::Tuple(args)]),
    ])
}

pub(crate) fn named_update(stamp: u32, action: &str, args: Vec<PyRep>) -> PyRep {
    PyRep::List(vec![named_entry(stamp, action, args)])
}

pub(crate) fn frame_actions(frame: &OutFrame) -> Option<Vec<&str>> {
    let OutFrame::DoDestinyUpdate { updates, .. } = frame else { return None };
    let PyRep::List(items) = updates else { return Some(Vec::new()) };
    Some(items.iter().filter_map(|e| crate::sequencer_invariants::parse_destiny_entry(e).map(|(_, a, _)| a)).collect())
}

#[test]
fn entity_materialization_gate_orders_same_dispatch_and_drops_unknown_balls() {
    let fr = Arc::new(FlightRecorder::new());
    let mut wire = SessionWireState::new(1084_01, 9001).with_flight_recorder(Arc::clone(&fr));
    let ep = wire.enter_space(Some(30_000_142));

    let set_state = DestinyBatch::new(
        ep,
        10,
        true,
        named_update(
            10,
            "SetState",
            vec![PyRep::keyval([("ego", PyRep::Integer(5001))])],
        ),
    );
    assert!(sequence_tick(&mut wire, set_state).is_some());
    assert!(wire.materialized.contains(&5001));

    // Unmaterialized ball 8888: OnSlimItemChange notification & OnSpecialFX update are dropped (never queued).
    let unmat_fx = DestinyBatch::new(ep, 11, false, named_update(11, "OnSpecialFX", vec![PyRep::Integer(8888)]));
    let outcome1 = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 1,
            response_frame: PyRep::None,
            session_change_frame: None,
            pending_tick_batches: vec![unmat_fx],
            destiny_drain: DestinyDrain::None,
            notifications: vec![TypedNotification {
                event: "OnSlimItemChange".into(),
                args: vec![PyRep::Integer(8888), PyRep::keyval([("itemID", PyRep::Integer(8888))])],
                service: None,
                scene_scope: None,
            }],
            item_change_before_session_change: false,
        },
    );
    assert!(!outcome1
        .frames
        .iter()
        .any(|f| matches!(f, OutFrame::DoDestinyUpdate { .. } | OutFrame::Notification { .. })));
    assert_eq!(outcome1.events.len(), 2);
    assert!(outcome1
        .events
        .iter()
        .all(|ev| ev.kind == SequencerLedgerEventKind::Dropped
            && ev.reason.contains("entity-not-materialized")));

    // Same dispatch: dependent OnSlimItemChange coalesced before AddBalls2(8888) AND notification OnDamageStateChange(8888)
    // must both succeed, with AddBalls2 emitted BEFORE the dependent updates!
    let premature_slim = DestinyBatch::new(
        ep,
        12,
        false,
        named_update(12, "OnSlimItemChange", vec![PyRep::Integer(8888), PyRep::None]),
    );
    let add_8888 = DestinyBatch::new(
        ep,
        12,
        false,
        named_update(12, "AddBalls2", vec![PyRep::Integer(8888)]),
    );
    let outcome2 = sequence_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 2,
            response_frame: PyRep::None,
            session_change_frame: None,
            pending_tick_batches: vec![premature_slim],
            destiny_drain: DestinyDrain::Updates(add_8888),
            notifications: vec![TypedNotification {
                event: "OnDamageStateChange".into(),
                args: vec![PyRep::Integer(8888), PyRep::Tuple(vec![])],
                service: None,
                scene_scope: None,
            }],
            item_change_before_session_change: false,
        },
    );
    // Only the stamp-12 DoDestinyUpdate (with AddBalls2 before OnSlimItemChange) + stamp-12 OnDamageStateChange arrive;
    // none of the earlier dropped events were queued.
    let destiny_pos = outcome2
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { .. }))
        .expect("DoDestinyUpdate present");
    let notif_pos = outcome2
        .frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnDamageStateChange"))
        .expect("OnDamageStateChange present after AddBalls2");
    assert!(destiny_pos < notif_pos);
    if let OutFrame::DoDestinyUpdate {
        updates: PyRep::List(items),
        ..
    } = &outcome2.frames[destiny_pos]
    {
        assert_eq!(items.len(), 2);
        let names: Vec<&str> = items
            .iter()
            .filter_map(|it| match it {
                PyRep::Tuple(v) => match &v[1] {
                    PyRep::Tuple(inner) => inner[0].as_str(),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["AddBalls2", "OnSlimItemChange"]);
    } else {
        panic!("expected DoDestinyUpdate with List updates");
    }

    let counters = fr.session_sequencer_counters(1084_01);
    assert_eq!(counters.dropped_dependent_notifications, 2);
    assert_eq!(counters.dropped_dependent_by_name.get("OnSpecialFX"), Some(&1));
    assert_eq!(counters.dropped_dependent_by_name.get("OnSlimItemChange"), Some(&1));

    wire.leave_space();
    assert!(wire.materialized.is_empty());
}

#[test]
fn forced_stamp_backstep_emits_single_crt_marker_and_zero_frames() {
    let fr = Arc::new(FlightRecorder::new());
    let unique_sid = 98_108_409_u64;
    let mut wire = SessionWireState::new(unique_sid, 9009).with_flight_recorder(Arc::clone(&fr));
    let ep = wire.enter_space(Some(30_000_142));

    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(
            ep,
            200,
            true,
            named_update(200, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(1))])]),
        ),
    )
    .is_some());
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep, 210, false, named_update(210, "AddBalls2", vec![PyRep::Integer(2)])),
    )
    .is_some());

    let (frame, events) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(ep, 209, false, named_update(209, "Stop", vec![PyRep::Integer(2)])),
    );
    assert!(frame.is_none());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SequencerLedgerEventKind::Dropped);
    assert!(events[0].reason.contains("stamp_regression:209<210"));

    let counters = fr.session_sequencer_counters(unique_sid);
    assert_eq!(counters.prevented_jolts, 1);
    let jolts = fr.get_session_prevented_jolts(unique_sid, 10);
    assert_eq!(jolts.len(), 1);
    assert_eq!(jolts[0].batch_stamp, 209);
    assert_eq!(jolts[0].trusted_stamp, 210);

    let session_tag = format!("session={unique_sid} ");
    let ring_lines = crate::log_ring_buffer::global_log_ring().snapshot_recent(500);
    let crt_matches: Vec<&String> = ring_lines
        .iter()
        .filter(|line| line.contains("CRT") && line.contains(&session_tag))
        .collect();
    assert_eq!(
        crt_matches.len(),
        1,
        "forced backstep must produce exactly 1 CRT marker for session, got {crt_matches:?}"
    );
}

#[test]
fn docked_ship_swap_emits_item_change_before_session_change_1085() {
    // parity: notify:OnItemChange
    // parity: notify:OnSessionChanged
    // Positive: docked ship swap with item_change_before_session_change=true emits
    // OnItemChange -> SessionChange -> CallRsp -> trailing notifications.
    let mut wire = SessionWireState::new(1085, 1085);
    let frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 10,
            response_frame: PyRep::None,
            session_change_frame: Some(PyRep::String("shipid_swap".into())),
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::None,
            notifications: vec![
                TypedNotification {
                    event: "OnItemChange".into(),
                    args: vec![PyRep::Integer(100_000_002)],
                    service: None,
                    scene_scope: None,
                },
                TypedNotification {
                    event: "OnActiveShipModelChange".into(),
                    args: vec![],
                    service: None,
                    scene_scope: None,
                },
            ],
            item_change_before_session_change: true,
        },
    );
    let item_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnItemChange"))
        .unwrap();
    let sess_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::SessionChange { .. }))
        .unwrap();
    let rsp_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::CallRsp { call_id: 10, .. }))
        .unwrap();
    let model_idx = frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnActiveShipModelChange"))
        .unwrap();
    assert!(item_idx < sess_idx && sess_idx < rsp_idx && rsp_idx < model_idx);

    // Negative: when flag is false, OnItemChange stays after CallRsp; and in-space live
    // DestinyDrain::Updates keeps SetState -> SessionChange -> CallRsp -> OnItemChange even if set.
    let ep = wire.enter_space(Some(30_000_142));
    assert!(sequence_tick(&mut wire, DestinyBatch::new(ep, 40, true, dummy_updates("SetState", 40))).is_some());
    let space_frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 11,
            response_frame: PyRep::None,
            session_change_frame: Some(PyRep::String("in_space_board".into())),
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                ep,
                41,
                true,
                dummy_updates("SetState", 41),
            )),
            notifications: vec![TypedNotification {
                event: "OnItemChange".into(),
                args: vec![],
                service: None,
                scene_scope: None,
            }],
            item_change_before_session_change: true,
        },
    );
    let d_pos = space_frames
        .iter()
        .position(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, .. }))
        .unwrap();
    let s_pos = space_frames
        .iter()
        .position(|f| matches!(f, OutFrame::SessionChange { .. }))
        .unwrap();
    let r_pos = space_frames
        .iter()
        .position(|f| matches!(f, OutFrame::CallRsp { call_id: 11, .. }))
        .unwrap();
    let i_pos = space_frames
        .iter()
        .position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnItemChange"))
        .unwrap();
    assert!(d_pos < s_pos && s_pos < r_pos && r_pos < i_pos);
}

#[test]
fn attach_hoists_do_sim_clock_rebase_before_pending_ticks_and_set_state_1085() {
    // parity: notify:DoSimClockRebase
    let mut wire = SessionWireState::new(1086, 1086);
    let ep = wire.enter_space(Some(30_000_142));
    let frames = sequence(
        &mut wire,
        RpcCompletion {
            call_id: 20,
            response_frame: PyRep::Integer(20),
            session_change_frame: Some(PyRep::String("undock".into())),
            pending_tick_batches: vec![DestinyBatch::new(
                ep,
                50,
                false,
                dummy_updates("AddBalls2", 50),
            )],
            destiny_drain: DestinyDrain::Attach {
                epoch: ep,
                set_state: DestinyBatch::new(ep, 51, true, dummy_updates("SetState", 51)),
            },
            notifications: vec![
                TypedNotification {
                    event: "OnCharNoLongerInStation".into(),
                    args: vec![],
                    service: None,
                    scene_scope: None,
                },
                TypedNotification {
                    event: "DoSimClockRebase".into(),
                    args: vec![],
                    service: None,
                    scene_scope: None,
                },
            ],
            item_change_before_session_change: false,
        },
    );
    let sess_pos = frames.iter().position(|f| matches!(f, OutFrame::SessionChange { .. })).unwrap();
    let rebase_pos = frames.iter().position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "DoSimClockRebase")).unwrap();
    let set_state_pos = frames.iter().position(|f| matches!(f, OutFrame::DoDestinyUpdate { has_set_state: true, .. })).unwrap();
    let rsp_pos = frames.iter().position(|f| matches!(f, OutFrame::CallRsp { call_id: 20, .. })).unwrap();
    let trailing_pos = frames.iter().position(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnCharNoLongerInStation")).unwrap();
    assert!(sess_pos < rebase_pos && rebase_pos < set_state_pos && set_state_pos < rsp_pos && rsp_pos < trailing_pos);
}

#[test]
fn server_normalizer_splits_same_tick_add_then_remove_preserving_strict_invariants() {
    let mut wire = SessionWireState::new(98_113_501, 9101);
    let ep = wire.enter_space(Some(30_000_142));

    let init = DestinyBatch::new(
        ep,
        10,
        true,
        named_update(10, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(1))])]),
    );
    assert!(sequence_tick(&mut wire, init).is_some());

    // Same-stamp AddBalls2(202) + OnDamageStateChange(202) + RemoveBall(202) at stamp 11.
    let batch = DestinyBatch::new(
        ep,
        11,
        false,
        PyRep::List(vec![
            named_entry(11, "AddBalls2", vec![PyRep::Integer(202)]),
            named_entry(11, "OnDamageStateChange", vec![PyRep::Integer(202), PyRep::Integer(0)]),
            named_entry(11, "RemoveBall", vec![PyRep::Integer(202)]),
        ]),
    );
    let split = normalize_server_destiny_batch(&mut wire, batch);
    assert_eq!(split.len(), 2, "same-tick Add+Remove must split into S and S+1");
    assert_eq!(split[0].stamp, 11);
    assert_eq!(split[1].stamp, 12);

    let mut frames = Vec::new();
    for b in split {
        let (frame_opt, _) = sequence_tick_with_events(&mut wire, b);
        if let Some(f) = frame_opt {
            frames.push(f);
        }
    }
    assert_eq!(frames.len(), 2);
    assert_eq!(frame_actions(&frames[1]), Some(vec!["RemoveBalls"]));
    assert!(!wire.materialized.contains(&202));
    assert_eq!(wire.sequencer_counters().invariant_violations, 0);

    // Next update still arriving at server tick 11 must clamp up to 12 instead of dropping as a jolt.
    let same_tick_followup = DestinyBatch::new(
        ep,
        11,
        false,
        named_update(11, "AddBalls2", vec![PyRep::Integer(303)]),
    );
    let followup_batches = normalize_server_destiny_batch(&mut wire, same_tick_followup);
    assert_eq!(followup_batches.len(), 1);
    assert_eq!(followup_batches[0].stamp, 12);
    let f_followup = sequence_tick(&mut wire, followup_batches.into_iter().next().unwrap())
        .expect("clamped same-server-tick followup must not drop as jolt");
    match f_followup {
        OutFrame::DoDestinyUpdate { stamp, .. } => assert_eq!(stamp, 12),
        other => panic!("expected DoDestinyUpdate, got {other:?}"),
    }
    assert_eq!(wire.sequencer_counters().prevented_jolts, 0);

    // Genuine regression below source tick 11 (< 11) must still trigger prevented_jolt drop.
    let genuine_backstep = DestinyBatch::new(
        ep,
        10,
        false,
        named_update(10, "Stop", vec![PyRep::Integer(303)]),
    );
    for b in normalize_server_destiny_batch(&mut wire, genuine_backstep) {
        assert!(sequence_tick(&mut wire, b).is_none());
    }
    assert_eq!(wire.sequencer_counters().prevented_jolts, 1);
}

#[test]
fn server_normalizer_partitions_remove_balls_and_handles_readd_and_detach() {
    let mut wire = SessionWireState::new(98_113_502, 9102);
    let ep = wire.enter_space(Some(30_000_142));

    // Attach batch containing SetState(ego=1) + AddBalls2(202) + RemoveBall(202) at stamp 1.
    let attach_outcome = sequence_server_rpc_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 1,
            response_frame: PyRep::Integer(1),
            session_change_frame: None,
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Attach {
                epoch: ep,
                set_state: DestinyBatch::new(
                    ep,
                    1,
                    true,
                    PyRep::List(vec![
                        named_entry(1, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(1))])]),
                        named_entry(1, "AddBalls2", vec![PyRep::Integer(202)]),
                        named_entry(1, "RemoveBall", vec![PyRep::Integer(202)]),
                    ]),
                ),
            },
            notifications: vec![],
            item_change_before_session_change: false,
        },
    );
    let stamps: Vec<u32> = attach_outcome
        .frames
        .iter()
        .filter_map(|f| match f {
            OutFrame::DoDestinyUpdate { stamp, .. } => Some(*stamp),
            _ => None,
        })
        .collect();
    assert_eq!(stamps, vec![1, 2]);
    let attach_actions: Vec<_> = attach_outcome.frames.iter().filter_map(frame_actions).collect();
    assert_eq!(attach_actions.get(1).map(Vec::as_slice), Some(["RemoveBalls"].as_slice()));
    assert!(!wire.materialized.contains(&202));

    // At stamp 10, materialize 555 cleanly.
    assert!(sequence_tick(
        &mut wire,
        DestinyBatch::new(ep, 10, false, named_update(10, "AddBalls2", vec![PyRep::Integer(555)])),
    )
    .is_some());

    // At stamp 12, partial RemoveBalls([555, 666]) after AddBalls2(666) must keep 555 at 12
    // and defer only 666 to 13, while Add->Remove->Add for 777 preserves causal re-add at 13.
    let outcome = sequence_server_rpc_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 2,
            response_frame: PyRep::Integer(2),
            session_change_frame: None,
            pending_tick_batches: vec![DestinyBatch::new(
                ep,
                12,
                false,
                PyRep::List(vec![
                    named_entry(12, "AddBalls2", vec![PyRep::Integer(666)]),
                    make_remove_balls_entry(12, &[555, 666]),
                    named_entry(12, "AddBalls2", vec![PyRep::Integer(777)]),
                    make_remove_balls_entry(12, &[777]),
                    named_entry(12, "AddBalls2", vec![PyRep::Integer(777)]),
                ]),
            )],
            destiny_drain: DestinyDrain::Detach {
                pre_detach: Some(DestinyBatch::new(
                    ep,
                    12,
                    false,
                    PyRep::List(vec![
                        named_entry(12, "AddBalls2", vec![PyRep::Integer(888)]),
                        make_remove_balls_entry(12, &[888]),
                    ]),
                )),
            },
            notifications: vec![],
            item_change_before_session_change: false,
        },
    );
    let destiny_stamps: Vec<u32> = outcome
        .frames
        .iter()
        .filter_map(|f| match f {
            OutFrame::DoDestinyUpdate { stamp, .. } => Some(*stamp),
            _ => None,
        })
        .collect();
    assert_eq!(destiny_stamps, vec![12, 13, 14]);
    assert!(matches!(wire.destiny, DestinyStream::Detached));
    assert_eq!(wire.sequencer_counters().invariant_violations, 0);
}

#[test]
fn direct_drain_remove_tombstones_reordered_same_stamp_addballs2_and_raw_remove_is_flagged() {
    // 1. Sequenced pipeline: direct drain RemoveBalls(9001) at stamp 50 followed by
    //    TickBroadcast AddBalls2(9001) at stamp 50 must neither materialize 9001 nor emit
    //    a ghost removal / resurrect 9001.
    let mut wire = SessionWireState::new(98_117_201, 9201);
    let ep = wire.enter_space(Some(30_000_142));
    let init = DestinyBatch::new(
        ep,
        49,
        true,
        named_update(49, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(1))])]),
    );
    assert!(sequence_tick(&mut wire, init).is_some());
    assert!(!wire.materialized.contains(&9001));

    let direct_drain_outcome = sequence_server_rpc_with_events(
        &mut wire,
        RpcCompletion {
            call_id: 11,
            response_frame: PyRep::Integer(1),
            session_change_frame: None,
            pending_tick_batches: vec![],
            destiny_drain: DestinyDrain::Updates(DestinyBatch::new(
                ep,
                50,
                false,
                PyRep::List(vec![make_remove_balls_entry(50, &[9001])]),
            )),
            notifications: vec![],
            item_change_before_session_change: false,
        },
    );
    assert!(
        !direct_drain_outcome
            .frames
            .iter()
            .any(|f| matches!(f, OutFrame::DoDestinyUpdate { .. })),
        "unmaterialized direct-drain RemoveBalls(9001) must be suppressed cleanly"
    );

    let (tick_frame, tick_events) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(
            ep,
            50,
            false,
            named_update(50, "AddBalls2", vec![PyRep::Integer(9001)]),
        ),
    );
    assert!(
        tick_frame.is_none(),
        "reordered same-stamp TickBroadcast AddBalls2(9001) after tombstone must be suppressed"
    );
    assert!(!wire.materialized.contains(&9001));
    assert!(
        tick_events
            .iter()
            .any(|e| e.reason.starts_with("tombstoned-addballs:")),
        "expected tombstone drop ledger event, got {tick_events:?}"
    );
    assert_eq!(wire.sequencer_counters().invariant_violations, 0);

    // Legitimate later-stamp (51) AddBalls2(9001) clears tombstone and materializes 9001.
    let (respawn_frame, _) = sequence_tick_with_events(
        &mut wire,
        DestinyBatch::new(
            ep,
            51,
            false,
            named_update(51, "AddBalls2", vec![PyRep::Integer(9001)]),
        ),
    );
    assert!(respawn_frame.is_some());
    assert!(wire.materialized.contains(&9001));

    // 2. Raw validator path: emitting RemoveBalls(9001) at stamp 50 without prior
    //    materialization must NEVER pass unflagged.
    let mut raw_wire = SessionWireState::new(98_117_202, 9202);
    raw_wire.set_invariant_validator_mode(InvariantValidatorMode::RecordAndEmit);
    let raw_ep = raw_wire.enter_space(Some(30_000_142));
    let raw_init = named_update(49, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(1))])]);
    assert!(raw_wire
        .validate_outgoing_destiny_frame(raw_ep, 49, &raw_init)
        .is_empty());

    let raw_remove = PyRep::List(vec![make_remove_balls_entry(50, &[9001])]);
    let violations = raw_wire.validate_outgoing_destiny_frame(raw_ep, 50, &raw_remove);
    assert_eq!(
        violations,
        vec![crate::sequencer_invariants::INV_REMOVE_AFTER_ADDBALLS]
    );
    assert_eq!(raw_wire.sequencer_counters().invariant_violations, 1);
}

#[test]
fn reattach_set_state_preserves_same_stamp_and_clamps_below_wire_floor() {
    let fr = Arc::new(FlightRecorder::new());
    let mut wire = SessionWireState::new(98_116_901, 9601).with_flight_recorder(Arc::clone(&fr));
    wire.set_invariant_validator_mode(InvariantValidatorMode::RecordAndEmit);
    let ep = wire.enter_space(Some(30_000_142));

    let init = DestinyBatch::new(
        ep,
        40,
        true,
        named_update(40, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(5001))])]),
    );
    let init_frame = sequence_tick(&mut wire, init).expect("initial SetState accepted");
    assert!(matches!(
        init_frame,
        OutFrame::DoDestinyUpdate { stamp: 40, has_set_state: true, .. }
    ));

    // Equal-stamp re-attachment SetState at current_tick (= 40) is accepted cleanly without bumping.
    let same_tick_reattach = DestinyBatch::new(
        ep,
        40,
        true,
        named_update(40, "SetState", vec![PyRep::keyval([("ego", PyRep::Integer(5001))])]),
    );
    let reattach_frame =
        sequence_tick(&mut wire, same_tick_reattach).expect("same-stamp SetState accepted");
    assert!(matches!(
        reattach_frame,
        OutFrame::DoDestinyUpdate { stamp: 40, has_set_state: true, .. }
    ));
    assert_eq!(
        wire.destiny,
        DestinyStream::Live {
            epoch: ep,
            set_state_stamp: 40,
            last_stamp: 40,
        }
    );

    // Suppose an FX-floored delta bumped this wire's last emitted stamp to 43 while ballpark tick is 41.
    let fx_delta = DestinyBatch::new(ep, 43, false, dummy_updates("OnSpecialFX", 43));
    assert!(sequence_tick(&mut wire, fx_delta).is_some());

    // Subsequent lag transition + SetState stamped at current_tick = 41 must clamp ONLY this session's
    // SetState wire stamp (including KeyVal["stamp"], binary state header bytes[1..5], and damageState
    // stamps) up to 43 so INV_MONOTONIC_STAMP and V24 ReadFullStateFromStream consistency hold.
    assert_eq!(wire.on_broadcast_lagged(), Some(ep));
    let mut bp = eve_destiny::Ballpark::new(30_000_142);
    bp.current_tick = 41;
    bp.add_ball(eve_destiny::Ball {
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
    if let Some(cs) = bp.combat_states.get_mut(&5001) {
        cs.shield_hp = 120.0;
        cs.max_shield_hp = 200.0;
    }
    let set_state_payload = eve_destiny::wire::encode_set_state_payload(&bp, 5001);
    let lag_resync = DestinyBatch::new(
        ep,
        41,
        true,
        named_update(41, "SetState", vec![set_state_payload]),
    );
    let (resync_frame, _) = sequence_tick_with_events(&mut wire, lag_resync);
    match resync_frame.expect("lag resync SetState emitted") {
        OutFrame::DoDestinyUpdate { stamp, has_set_state, updates, .. } => {
            assert!(has_set_state);
            assert_eq!(stamp, 43);
            let PyRep::List(entries) = updates else {
                panic!("expected List of updates");
            };
            let PyRep::Tuple(entry) = &entries[0] else {
                panic!("expected tuple entry");
            };
            assert_eq!(entry[0], PyRep::Integer(43));
            let PyRep::Tuple(action_pair) = &entry[1] else {
                panic!("expected action pair");
            };
            assert_eq!(action_pair[0].as_str(), Some("SetState"));
            let PyRep::Tuple(args) = &action_pair[1] else {
                panic!("expected args tuple");
            };
            let PyRep::KeyVal(kv) = &args[0] else {
                panic!("expected SetState KeyVal");
            };
            assert_eq!(kv.get("stamp"), Some(&PyRep::Integer(43)));
            let Some(PyRep::Bytes(state_bytes)) = kv.get("state") else {
                panic!("expected binary state bytes");
            };
            let (_, header_stamp, decoded_balls) =
                eve_destiny::wire::decode_add_balls(state_bytes).expect("decode SetState binary buffer");
            assert_eq!(header_stamp, 43, "SetState binary bytes[1..5] must match clamped stamp");
            assert_eq!(decoded_balls.len(), 1);
            let Some(PyRep::Dict(damage_entries)) = kv.get("damageState") else {
                panic!("expected damageState Dict");
            };
            assert!(!damage_entries.is_empty());
            for (_, dmg_val) in damage_entries {
                let PyRep::Tuple(outer) = dmg_val else {
                    panic!("expected damage tuple");
                };
                let PyRep::Tuple(shield_tuple) = &outer[0] else {
                    panic!("expected shield inner tuple");
                };
                assert_eq!(
                    shield_tuple[2],
                    PyRep::Integer(43),
                    "damageState shield stamp must match clamped SetState wire stamp"
                );
            }
        }
        other => panic!("expected DoDestinyUpdate, got {other:?}"),
    }
    assert_eq!(wire.sequencer_counters().invariant_violations, 0);
    assert_eq!(
        wire.destiny,
        DestinyStream::Live {
            epoch: ep,
            set_state_stamp: 43,
            last_stamp: 43,
        }
    );
}
