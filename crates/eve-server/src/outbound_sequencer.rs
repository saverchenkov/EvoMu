//! Per-session outbound wire sequencer (`OutboundSequencer`) and `DestinyStream` state machine (`C-3`, `C-4`).
//!
//! Enforces client-server wire ordering invariants (`ORD-1..ORD-8`, `#927`, `#928`, `#883`, `#844`, `#845`):
//! - `DestinyStream` explicitly tracks `Detached`, `AwaitingSetState { epoch }`, and
//!   `Live { epoch, set_state_stamp, last_stamp }`.
//! - Entering space or changing solar systems advances `epoch` and transitions to
//!   `AwaitingSetState { epoch }`.
//! - When leaving space (`DestinyDrain::Detach`), buffered ticks and pre-detach updates are
//!   flushed while the space ballpark is still live before `SessionChangeNotification` and `CallRsp`.
//! - `SessionChangeNotification` (when present) precedes `CallRsp` (`#883`, `#928`) so the
//!   client's `MachoNetService.TransportReader` stamps `CallRsp` with the post-change
//!   `receivedVersion`. On an in-space ship switch in the same system while `Live` (`#970`),
//!   `DoDestinyUpdate(SetState)` precedes `SessionChangeNotification`, which precedes `CallRsp`.
//! - In `AwaitingSetState { epoch }`, non-`SetState` batches are ignored without resetting back
//!   to `Detached` (`#845`), while the first `SetState` batch for `epoch` transitions to
//!   `Live { epoch, set_state_stamp, last_stamp }`.
//! - In `Live { epoch, set_state_stamp, last_stamp }`, non-`SetState` batches for the current
//!   `epoch` are dropped only when `stamp < set_state_stamp` (superseded by a newer `SetState`
//!   snapshot), never merely because a direct post-RPC drain observed a higher tick (`#844`).
//! - On broadcast lag (`RecvError::Lagged`), `on_broadcast_lagged` transitions `Live` back to
//!   `AwaitingSetState { epoch }` so an immediate `SetState` resync restores ballpark state (`ORD-5`).

use eve_proto::PyRep;

/// Per-session destiny stream lifecycle state (`C-3`, `ORD-3`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DestinyStream {
    /// Not in space (character selection, docked in station/structure, or logged off).
    #[default]
    Detached,
    /// In space for `epoch`, waiting for the initial (or resync) `SetState` batch before any
    /// incremental destiny updates may be emitted to the client.
    AwaitingSetState { epoch: u32 },
    /// `SetState` has been delivered for `epoch` at `set_state_stamp`.
    Live {
        epoch: u32,
        set_state_stamp: u32,
        last_stamp: u32,
    },
}

impl DestinyStream {
    /// Returns the active space epoch if in `AwaitingSetState` or `Live`.
    pub fn epoch(&self) -> Option<u32> {
        match *self {
            Self::Detached => None,
            Self::AwaitingSetState { epoch } | Self::Live { epoch, .. } => Some(epoch),
        }
    }

    /// Returns `true` when the stream has delivered `SetState` for the current epoch.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Live { .. })
    }

    /// Returns `true` when the stream is in `AwaitingSetState` or `Live`.
    pub fn is_attached(&self) -> bool {
        !matches!(self, Self::Detached)
    }
}

/// Per-session wire sequencing state owned exclusively by the session's outbound loop (`C-3`).
#[derive(Debug, Clone)]
pub struct SessionWireState {
    pub session_id: u64,
    pub client_id: u64,
    pub destiny: DestinyStream,
    next_epoch: u32,
    pub solar_system_id: Option<u32>,
    pub in_space: bool,
    pub last_client_tick: Option<u64>,
}

impl SessionWireState {
    pub fn new(session_id: u64, client_id: u64) -> Self {
        Self {
            session_id,
            client_id,
            destiny: DestinyStream::Detached,
            next_epoch: 1,
            solar_system_id: None,
            in_space: false,
            last_client_tick: None,
        }
    }

    pub fn note_client_tick(&mut self, tick: u64) {
        self.last_client_tick = Some(match self.last_client_tick {
            Some(prev) => prev.max(tick),
            None => tick,
        });
    }

    fn allocate_epoch(&mut self) -> u32 {
        let ep = self.next_epoch;
        self.next_epoch = self.next_epoch.wrapping_add(1).max(1);
        ep
    }

    /// Returns the current epoch if attached, or `0` when `Detached`.
    pub fn current_epoch(&self) -> u32 {
        self.destiny.epoch().unwrap_or(0)
    }

    /// Transitions the session into space in `solar_system_id`, allocating a fresh epoch and
    /// entering `DestinyStream::AwaitingSetState { epoch }`.
    pub fn enter_space(&mut self, solar_system_id: Option<u32>) -> u32 {
        let ep = self.allocate_epoch();
        self.in_space = true;
        self.solar_system_id = solar_system_id;
        self.destiny = DestinyStream::AwaitingSetState { epoch: ep };
        ep
    }

    /// Ensures the session has an active space epoch (`AwaitingSetState` or `Live`).
    /// If currently `Detached`, transitions to `AwaitingSetState` with a newly allocated epoch.
    pub fn ensure_in_space(&mut self, solar_system_id: Option<u32>) -> u32 {
        self.in_space = true;
        if solar_system_id.is_some() {
            self.solar_system_id = solar_system_id;
        }
        match self.destiny {
            DestinyStream::Detached => self.enter_space(self.solar_system_id),
            DestinyStream::AwaitingSetState { epoch } | DestinyStream::Live { epoch, .. } => epoch,
        }
    }

    /// Transitions the session out of space (docking or logoff), setting `DestinyStream::Detached`.
    pub fn leave_space(&mut self) {
        self.in_space = false;
        self.destiny = DestinyStream::Detached;
    }

    /// Synchronizes `SessionWireState` with session location changes across an RPC.
    ///
    /// - If `!is_in_space`, transitions to `Detached`.
    /// - If `is_in_space` and (`!was_in_space` or `solar_system_id` changed), allocates a new
    ///   epoch and transitions to `AwaitingSetState { epoch }`.
    pub fn sync_location_transition(
        &mut self,
        was_in_space: bool,
        is_in_space: bool,
        prev_sys_id: Option<u32>,
        new_sys_id: Option<u32>,
    ) {
        if !is_in_space {
            self.leave_space();
            self.solar_system_id = new_sys_id;
            return;
        }
        let sys_changed = prev_sys_id != new_sys_id || self.solar_system_id != new_sys_id;
        if !was_in_space || sys_changed {
            self.enter_space(new_sys_id);
        } else {
            self.in_space = true;
            self.solar_system_id = new_sys_id;
        }
    }

    /// Marks the destiny stream as needing a `SetState` resync in the current epoch after a
    /// broadcast channel lag (`RecvError::Lagged`, `ORD-5`).
    pub fn on_broadcast_lagged(&mut self) -> Option<u32> {
        match self.destiny {
            DestinyStream::Detached => None,
            DestinyStream::AwaitingSetState { epoch } | DestinyStream::Live { epoch, .. } => {
                self.destiny = DestinyStream::AwaitingSetState { epoch };
                Some(epoch)
            }
        }
    }
}

/// Detailed outcome of evaluating a [`DestinyBatch`] against [`SessionWireState`].
#[derive(Debug, Clone, PartialEq)]
pub enum AcceptDestinyOutcome {
    Accepted(DestinyBatch),
    Empty,
    Dropped { epoch: u32, stamp: u32, reason: String },
}

/// Ledger event emitted by `outbound_sequencer` for flight-recorder attribution (`#1077 §A`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequencerLedgerEventKind {
    Dropped,
    Coalesced,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SequencerLedgerEvent {
    pub kind: SequencerLedgerEventKind,
    pub epoch: u32,
    pub stamp: u32,
    pub reason: String,
}

/// Full sequencing output including both outbound wire frames and drop/coalesce ledger events.
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceOutcome {
    pub frames: Vec<OutFrame>,
    pub events: Vec<SequencerLedgerEvent>,
}

impl SessionWireState {
    /// Evaluates a candidate `DestinyBatch` against the `DestinyStream` state machine (`ORD-3`,
    /// `#845`, `#1077 §A`), enforcing strict per-epoch stamp monotonicity for non-`SetState` batches.
    pub fn accept_destiny_batch_with_reason(
        &mut self,
        batch: DestinyBatch,
    ) -> AcceptDestinyOutcome {
        if batch.is_empty() {
            return AcceptDestinyOutcome::Empty;
        }
        let batch_epoch = batch.epoch;
        let batch_stamp = batch.stamp;
        match self.destiny {
            DestinyStream::Detached => AcceptDestinyOutcome::Dropped {
                epoch: batch_epoch,
                stamp: batch_stamp,
                reason: "destiny_stream_detached".to_string(),
            },
            DestinyStream::AwaitingSetState { epoch } => {
                if batch_epoch != epoch {
                    return AcceptDestinyOutcome::Dropped {
                        epoch: batch_epoch,
                        stamp: batch_stamp,
                        reason: format!("epoch_mismatch:{batch_epoch}!={epoch}"),
                    };
                }
                if !batch.has_set_state {
                    // #845 (B7): Do not emit pre-SetState deltas and do NOT reset back to Detached.
                    return AcceptDestinyOutcome::Dropped {
                        epoch: batch_epoch,
                        stamp: batch_stamp,
                        reason: "awaiting_set_state".to_string(),
                    };
                }
                self.destiny = DestinyStream::Live {
                    epoch,
                    set_state_stamp: batch_stamp,
                    last_stamp: batch_stamp,
                };
                AcceptDestinyOutcome::Accepted(batch)
            }
            DestinyStream::Live {
                epoch,
                set_state_stamp,
                last_stamp,
            } => {
                if batch_epoch != epoch {
                    return AcceptDestinyOutcome::Dropped {
                        epoch: batch_epoch,
                        stamp: batch_stamp,
                        reason: format!("epoch_mismatch:{batch_epoch}!={epoch}"),
                    };
                }
                if batch.has_set_state {
                    let new_last = last_stamp.max(batch_stamp);
                    self.destiny = DestinyStream::Live {
                        epoch,
                        set_state_stamp: batch_stamp,
                        last_stamp: new_last,
                    };
                    AcceptDestinyOutcome::Accepted(batch)
                } else {
                    if batch_stamp < set_state_stamp {
                        return AcceptDestinyOutcome::Dropped {
                            epoch: batch_epoch,
                            stamp: batch_stamp,
                            reason: format!(
                                "stamp_below_set_state:{batch_stamp}<{set_state_stamp}"
                            ),
                        };
                    }
                    // #1077 §A: Reject any non-SetState batch strictly below last_stamp for this session.
                    if batch_stamp < last_stamp {
                        return AcceptDestinyOutcome::Dropped {
                            epoch: batch_epoch,
                            stamp: batch_stamp,
                            reason: format!("stamp_regression:{batch_stamp}<{last_stamp}"),
                        };
                    }
                    self.destiny = DestinyStream::Live {
                        epoch,
                        set_state_stamp,
                        last_stamp: batch_stamp,
                    };
                    AcceptDestinyOutcome::Accepted(batch)
                }
            }
        }
    }

    /// Evaluates a candidate `DestinyBatch` against the `DestinyStream` state machine (`ORD-3`,
    /// `#845`, `#1077 §A`), updating `self.destiny` and returning `Some(batch)` if and only if the
    /// batch may be emitted on the wire.
    pub fn accept_destiny_batch(&mut self, batch: DestinyBatch) -> Option<DestinyBatch> {
        match self.accept_destiny_batch_with_reason(batch) {
            AcceptDestinyOutcome::Accepted(accepted) => Some(accepted),
            AcceptDestinyOutcome::Empty | AcceptDestinyOutcome::Dropped { .. } => None,
        }
    }
}

/// A batch of destiny updates tagged with its space `epoch`, destiny `stamp`, and whether it
/// contains a `SetState` initialization/resync event.
#[derive(Debug, Clone, PartialEq)]
pub struct DestinyBatch {
    pub epoch: u32,
    pub stamp: u32,
    pub has_set_state: bool,
    pub updates: PyRep,
    pub wait_for_bubble: bool,
}

impl DestinyBatch {
    pub fn new(epoch: u32, stamp: u32, has_set_state: bool, updates: PyRep) -> Self {
        Self {
            epoch,
            stamp,
            has_set_state,
            updates,
            wait_for_bubble: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.updates {
            PyRep::List(items) | PyRep::Tuple(items) => items.is_empty(),
            PyRep::None => true,
            _ => false,
        }
    }
}

/// Destiny drain outcome associated with an `RpcCompletion` (`C-3`).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum DestinyDrain {
    #[default]
    None,
    /// Ballpark attach or re-attach with a `SetState` batch.
    Attach { epoch: u32, set_state: DestinyBatch },
    /// Incremental post-RPC destiny updates in the current space epoch.
    Updates(DestinyBatch),
    /// Space -> station/structure transition: optional pre-detach updates (e.g. `OnDockingAccepted`)
    /// emitted before `SessionChangeNotification` detaches the ballpark.
    Detach { pre_detach: Option<DestinyBatch> },
}

/// A typed notification queued for delivery after the RPC response (`C-3`).
#[derive(Debug, Clone, PartialEq)]
pub struct TypedNotification {
    pub event: String,
    pub args: Vec<PyRep>,
    pub service: Option<String>,
}

/// All outbound artifacts produced by completing a single `CallReq` (`C-3`).
#[derive(Debug, Clone, PartialEq)]
pub struct RpcCompletion {
    pub call_id: u64,
    /// Encoded `CallRsp` or `ErrorResponse` wire packet `PyRep`.
    pub response_frame: PyRep,
    /// Optional `SessionChangeNotification` wire packet `PyRep`.
    pub session_change_frame: Option<PyRep>,
    /// Tick broadcasts drained from `sys_rx` prior to the post-RPC direct drain.
    pub pending_tick_batches: Vec<DestinyBatch>,
    /// Post-RPC direct observer drain (`None`, `Attach`, `Updates`, or `Detach`).
    pub destiny_drain: DestinyDrain,
    /// Notifications drained for this session after the RPC.
    pub notifications: Vec<TypedNotification>,
}

/// Canonical outbound wire frames emitted by `OutboundSequencer` (`C-3`).
#[derive(Debug, Clone, PartialEq)]
pub enum OutFrame {
    CallRsp {
        call_id: u64,
        frame: PyRep,
    },
    SessionChange {
        frame: PyRep,
    },
    DoDestinyUpdate {
        epoch: u32,
        stamp: u32,
        has_set_state: bool,
        updates: PyRep,
        wait_for_bubble: bool,
    },
    Notification {
        event: String,
        args: Vec<PyRep>,
    },
    ServiceCall {
        service: String,
        method: String,
        args: Vec<PyRep>,
    },
}

fn merge_destiny_updates_pyrep(existing: &mut PyRep, incoming: PyRep) -> usize {
    let incoming_items = match incoming {
        PyRep::List(items) | PyRep::Tuple(items) => items,
        PyRep::None => return 0,
        other => vec![other],
    };
    let merged_count = incoming_items.len();
    match existing {
        PyRep::List(items) | PyRep::Tuple(items) => {
            items.extend(incoming_items);
        }
        other => {
            let prev = std::mem::replace(other, PyRep::None);
            let mut combined = if matches!(prev, PyRep::None) {
                Vec::with_capacity(merged_count)
            } else {
                vec![prev]
            };
            combined.extend(incoming_items);
            *other = PyRep::List(combined);
        }
    }
    merged_count
}

fn eval_and_push_destiny_batch(
    state: &mut SessionWireState,
    batch: DestinyBatch,
    out: &mut Vec<OutFrame>,
    events: &mut Vec<SequencerLedgerEvent>,
) {
    match state.accept_destiny_batch_with_reason(batch) {
        AcceptDestinyOutcome::Accepted(accepted) => {
            if let Some(OutFrame::DoDestinyUpdate {
                epoch,
                stamp,
                has_set_state,
                updates,
                wait_for_bubble,
            }) = out.last_mut()
            {
                if *epoch == accepted.epoch && *stamp == accepted.stamp {
                    *has_set_state |= accepted.has_set_state;
                    *wait_for_bubble |= accepted.wait_for_bubble;
                    let merged_entries = merge_destiny_updates_pyrep(updates, accepted.updates);
                    events.push(SequencerLedgerEvent {
                        kind: SequencerLedgerEventKind::Coalesced,
                        epoch: accepted.epoch,
                        stamp: accepted.stamp,
                        reason: format!(
                            "equal_stamp_merge:stamp={}:merged_{merged_entries}_entries",
                            accepted.stamp
                        ),
                    });
                    return;
                }
            }
            out.push(OutFrame::DoDestinyUpdate {
                epoch: accepted.epoch,
                stamp: accepted.stamp,
                has_set_state: accepted.has_set_state,
                updates: accepted.updates,
                wait_for_bubble: accepted.wait_for_bubble,
            });
        }
        AcceptDestinyOutcome::Empty => {}
        AcceptDestinyOutcome::Dropped {
            epoch,
            stamp,
            reason,
        } => {
            events.push(SequencerLedgerEvent {
                kind: SequencerLedgerEventKind::Dropped,
                epoch,
                stamp,
                reason,
            });
        }
    }
}

/// Pure function mapping `(SessionWireState, RpcCompletion)` to the exact sequence of outbound
/// wire frames (`C-3`, `ORD-1..ORD-6`).
pub fn sequence(state: &mut SessionWireState, done: RpcCompletion) -> Vec<OutFrame> {
    sequence_with_events(state, done).frames
}

/// Full sequencing variant returning both outbound wire frames and `dropped` / `coalesced`
/// ledger events for `FlightRecorder` attribution (`#1077 §A`).
pub fn sequence_with_events(state: &mut SessionWireState, done: RpcCompletion) -> SequenceOutcome {
    let mut out = Vec::with_capacity(
        2 + done.pending_tick_batches.len() + done.notifications.len(),
    );
    let mut events = Vec::new();

    let mut pending_call_rsp = Some(OutFrame::CallRsp {
        call_id: done.call_id,
        frame: done.response_frame,
    });

    // 1. Pre-detach destiny updates (e.g. OnDockingAccepted when transitioning space -> station)
    //    must be flushed while the client ballpark is still live, before SessionChangeNotification
    //    tears down the space ballpark and transitions DestinyStream -> Detached.
    let mut pending_ticks = done.pending_tick_batches;
    let mut destiny_drain = done.destiny_drain;
    if matches!(destiny_drain, DestinyDrain::Detach { .. }) {
        if let DestinyDrain::Detach { pre_detach } = std::mem::take(&mut destiny_drain) {
            for tick_batch in pending_ticks.drain(..) {
                eval_and_push_destiny_batch(state, tick_batch, &mut out, &mut events);
            }
            if let Some(batch) = pre_detach {
                eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
            }
            state.leave_space();
        }
    }

    // 2. SessionChangeNotification precedes CallRsp (#883, #928) so MachoNet's TransportReader
    //    advances `receivedVersion` before stamping `CallRsp`.
    //    Exception (#970, #992): when an in-space ship switch occurs in the same solar system while
    //    `state.destiny` is already `Live` (`DestinyDrain::Updates(batch)` — either `SetState` or
    //    incremental `RemoveBall`/`AddBalls2`/`OnSlimItemChange` handoff) with a session change,
    //    emit `DoDestinyUpdate` first so `michelle` has the new ball and `ego`, then
    //    `SessionChangeNotification`, then `CallRsp`.
    let mut deferred_session_change = done.session_change_frame;
    let in_space_ship_switch_updates = deferred_session_change.is_some()
        && state.destiny.is_live()
        && matches!(&destiny_drain, DestinyDrain::Updates(_));
    let attach_before_call_rsp = matches!(&destiny_drain, DestinyDrain::Attach { .. });
    if !in_space_ship_switch_updates {
        if let Some(sess_frame) = deferred_session_change.take() {
            out.push(OutFrame::SessionChange { frame: sess_frame });
        }
        // 3. CallRsp / ErrorResponse follows any SessionChangeNotification (#883, #928),
        //    except initial ballpark Attach (`DoDestinyUpdate(SetState)`) must precede CallRsp
        //    so `beyonce.MachoBindObject` / `GetFormations` primes `michelle` first (#1083).
        if !attach_before_call_rsp {
            if let Some(rsp) = pending_call_rsp.take() {
                out.push(rsp);
            }
        }
    }

    // 4. Buffered tick broadcasts followed by post-RPC direct destiny drain (ORD-3, #845, #1077 §A).
    for tick_batch in pending_ticks {
        eval_and_push_destiny_batch(state, tick_batch, &mut out, &mut events);
    }

    match destiny_drain {
        DestinyDrain::Attach { epoch, set_state } => {
            if matches!(state.destiny, DestinyStream::Detached) {
                state.in_space = true;
                state.destiny = DestinyStream::AwaitingSetState { epoch };
            }
            eval_and_push_destiny_batch(state, set_state, &mut out, &mut events);
        }
        DestinyDrain::Updates(batch) => {
            eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
        }
        DestinyDrain::None | DestinyDrain::Detach { .. } => {}
    }

    if let Some(sess_frame) = deferred_session_change.take() {
        out.push(OutFrame::SessionChange { frame: sess_frame });
    }
    if let Some(rsp) = pending_call_rsp.take() {
        out.push(rsp);
    }

    // 5. Post-RPC notifications in causal emission order (ORD-5, ORD-6).
    for notif in done.notifications {
        if let Some(service) = notif.service {
            out.push(OutFrame::ServiceCall {
                service,
                method: notif.event,
                args: notif.args,
            });
        } else {
            out.push(OutFrame::Notification {
                event: notif.event,
                args: notif.args,
            });
        }
    }

    SequenceOutcome {
        frames: out,
        events,
    }
}

/// Sequences a standalone 1 Hz tick broadcast batch against `SessionWireState` (`ORD-3`, `#845`, `#1077 §A`).
pub fn sequence_tick(state: &mut SessionWireState, batch: DestinyBatch) -> Option<OutFrame> {
    sequence_tick_with_event(state, batch).0
}

/// Sequences a standalone 1 Hz tick broadcast batch, returning an optional `Dropped` ledger event
/// when the batch is rejected (`#1077 §A`).
pub fn sequence_tick_with_event(
    state: &mut SessionWireState,
    batch: DestinyBatch,
) -> (Option<OutFrame>, Option<SequencerLedgerEvent>) {
    match state.accept_destiny_batch_with_reason(batch) {
        AcceptDestinyOutcome::Accepted(accepted) => (
            Some(OutFrame::DoDestinyUpdate {
                epoch: accepted.epoch,
                stamp: accepted.stamp,
                has_set_state: accepted.has_set_state,
                updates: accepted.updates,
                wait_for_bubble: accepted.wait_for_bubble,
            }),
            None,
        ),
        AcceptDestinyOutcome::Empty => (None, None),
        AcceptDestinyOutcome::Dropped {
            epoch,
            stamp,
            reason,
        } => (
            None,
            Some(SequencerLedgerEvent {
                kind: SequencerLedgerEventKind::Dropped,
                epoch,
                stamp,
                reason,
            }),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_updates(label: &str, stamp: u32) -> PyRep {
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
                }],
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
                }],
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
                }],
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
                                }],
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
}
