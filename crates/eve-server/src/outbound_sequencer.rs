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

use crate::flight_recorder::{FlightRecorder, SessionSequencerCounters};
use crate::sequencer_invariants::{InvariantValidatorMode, SequencerGuardState};
use eve_destiny::BallId;
use eve_proto::PyRep;
use std::collections::HashSet;
use std::sync::Arc;

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
    pub materialized: HashSet<BallId>,
    flight_recorder: Option<Arc<FlightRecorder>>,
    guard: SequencerGuardState,
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
            materialized: HashSet::new(),
            flight_recorder: None,
            guard: SequencerGuardState::default(),
        }
    }

    pub fn with_flight_recorder(mut self, flight_recorder: Arc<FlightRecorder>) -> Self {
        self.flight_recorder = Some(flight_recorder);
        self
    }

    pub fn set_flight_recorder(&mut self, flight_recorder: Option<Arc<FlightRecorder>>) {
        self.flight_recorder = flight_recorder;
    }

    pub fn flight_recorder(&self) -> Option<&FlightRecorder> {
        self.flight_recorder.as_deref()
    }

    pub fn set_invariant_validator_mode(&mut self, mode: InvariantValidatorMode) {
        self.guard.mode = mode;
    }

    pub fn invariant_validator_mode(&self) -> InvariantValidatorMode {
        self.guard.mode
    }

    pub fn sequencer_counters(&self) -> &SessionSequencerCounters {
        &self.guard.counters
    }

    pub fn seed_materialized_entities<I: IntoIterator<Item = BallId>>(&mut self, ids: I) {
        for id in ids {
            if id > 0 {
                self.materialized.insert(id);
            }
        }
    }

    /// Populates `materialized` from an already-live ballpark observer view (used only when
    /// attaching `ClientWireSession` / `dispatch_call_on_wire` mid-flight to an already-live session).
    pub fn seed_materialized_from_ballpark(
        &mut self,
        ctx: &crate::router::ServerContext,
        solar_system_id: u32,
        observer_ship_id: BallId,
    ) {
        if let Some(handle) = ctx.systems.get_handle(solar_system_id) {
            if let Ok(bp) = handle.ballpark.read() {
                if bp.balls.contains_key(&observer_ship_id) {
                    for b in bp.visible_balls_for_observer(observer_ship_id) {
                        self.materialized.insert(b.id);
                    }
                }
            }
        }
    }

    /// Evaluates a standalone outgoing `MachoNet` notification against `materialized` (`GH #1084`).
    pub fn gate_notification(&mut self, event: &str, args: &[PyRep]) -> Option<SequencerLedgerEvent> {
        let stamp = match self.destiny {
            DestinyStream::Live { last_stamp, .. } => last_stamp,
            _ => 0,
        };
        self.guard.gate_notification(
            &self.materialized,
            self.flight_recorder.as_deref(),
            self.session_id,
            self.current_epoch(),
            stamp,
            event,
            args,
        )
    }

    /// Validates a single outgoing `DoDestinyUpdate` frame against all 4 invariants (`GH #1084`).
    pub fn validate_outgoing_destiny_frame(
        &mut self,
        epoch: u32,
        stamp: u32,
        updates: &PyRep,
    ) -> Vec<String> {
        self.guard.validate_outgoing_destiny_frame(
            self.flight_recorder.as_deref(),
            self.session_id,
            epoch,
            stamp,
            updates,
        )
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
        self.materialized.clear();
        self.guard.reset_epoch_state();
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
        self.materialized.clear();
        self.guard.reset_epoch_state();
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
    pub packet_kind: Option<String>,
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
                    let trusted_stamp = last_stamp.max(set_state_stamp);
                    if batch_stamp < set_state_stamp || batch_stamp < last_stamp {
                        let reason = if batch_stamp < set_state_stamp {
                            format!("stamp_below_set_state:{batch_stamp}<{set_state_stamp}")
                        } else {
                            format!("stamp_regression:{batch_stamp}<{last_stamp}")
                        };
                        self.guard.note_prevented_jolt_counter();
                        crate::critical!(
                            session_id = self.session_id,
                            epoch = batch_epoch,
                            batch_stamp = batch_stamp,
                            trusted_stamp = trusted_stamp,
                            reason = &reason,
                            flight_recorder = self.flight_recorder.as_deref(),
                        );
                        return AcceptDestinyOutcome::Dropped {
                            epoch: batch_epoch,
                            stamp: batch_stamp,
                            reason,
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
                        packet_kind: None,
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
                packet_kind: None,
            });
        }
    }
}

fn finalize_pending_destiny_frames(
    state: &mut SessionWireState,
    out: &mut Vec<OutFrame>,
    start_idx: usize,
    events: &mut Vec<SequencerLedgerEvent>,
) {
    let tail = out.split_off(start_idx);
    for mut frame in tail {
        if let OutFrame::DoDestinyUpdate {
            epoch,
            stamp,
            updates,
            ..
        } = &mut frame
        {
            let keep = state.guard.reconcile_destiny_updates_with_gate(
                &mut state.materialized,
                state.flight_recorder.as_deref(),
                state.session_id,
                *epoch,
                *stamp,
                updates,
                events,
            );
            if !keep {
                continue;
            }
            state.guard.validate_outgoing_destiny_frame(
                state.flight_recorder.as_deref(),
                state.session_id,
                *epoch,
                *stamp,
                updates,
            );
        }
        out.push(frame);
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
            let pre_start = out.len();
            for tick_batch in pending_ticks.drain(..) {
                eval_and_push_destiny_batch(state, tick_batch, &mut out, &mut events);
            }
            if let Some(batch) = pre_detach {
                eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
            }
            finalize_pending_destiny_frames(state, &mut out, pre_start, &mut events);
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
    let step4_start = out.len();
    for tick_batch in pending_ticks {
        eval_and_push_destiny_batch(state, tick_batch, &mut out, &mut events);
    }

    match destiny_drain {
        DestinyDrain::Attach { epoch, set_state } => {
            if matches!(state.destiny, DestinyStream::Detached) {
                state.in_space = true;
                state.materialized.clear();
                state.guard.reset_epoch_state();
                state.destiny = DestinyStream::AwaitingSetState { epoch };
            }
            eval_and_push_destiny_batch(state, set_state, &mut out, &mut events);
        }
        DestinyDrain::Updates(batch) => {
            eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
        }
        DestinyDrain::None | DestinyDrain::Detach { .. } => {}
    }
    finalize_pending_destiny_frames(state, &mut out, step4_start, &mut events);

    if let Some(sess_frame) = deferred_session_change.take() {
        out.push(OutFrame::SessionChange { frame: sess_frame });
    }
    if let Some(rsp) = pending_call_rsp.take() {
        out.push(rsp);
    }

    // 5. Post-RPC notifications in causal emission order (ORD-5, ORD-6), gated against `materialized`.
    for notif in done.notifications {
        if let Some(drop_ev) = state.gate_notification(&notif.event, &notif.args) {
            events.push(drop_ev);
            continue;
        }
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
    sequence_tick_with_events(state, batch).0
}

/// Sequences a standalone 1 Hz tick broadcast batch, returning all gate/backstep ledger events.
pub fn sequence_tick_with_events(
    state: &mut SessionWireState,
    batch: DestinyBatch,
) -> (Option<OutFrame>, Vec<SequencerLedgerEvent>) {
    match state.accept_destiny_batch_with_reason(batch) {
        AcceptDestinyOutcome::Accepted(mut accepted) => {
            let mut events = Vec::new();
            let keep = state.guard.reconcile_destiny_updates_with_gate(
                &mut state.materialized,
                state.flight_recorder.as_deref(),
                state.session_id,
                accepted.epoch,
                accepted.stamp,
                &mut accepted.updates,
                &mut events,
            );
            if !keep {
                return (None, events);
            }
            state.guard.validate_outgoing_destiny_frame(
                state.flight_recorder.as_deref(),
                state.session_id,
                accepted.epoch,
                accepted.stamp,
                &accepted.updates,
            );
            (
                Some(OutFrame::DoDestinyUpdate {
                    epoch: accepted.epoch,
                    stamp: accepted.stamp,
                    has_set_state: accepted.has_set_state,
                    updates: accepted.updates,
                    wait_for_bubble: accepted.wait_for_bubble,
                }),
                events,
            )
        }
        AcceptDestinyOutcome::Empty => (None, Vec::new()),
        AcceptDestinyOutcome::Dropped {
            epoch,
            stamp,
            reason,
        } => (
            None,
            vec![SequencerLedgerEvent {
                kind: SequencerLedgerEventKind::Dropped,
                epoch,
                stamp,
                reason,
                packet_kind: None,
            }],
        ),
    }
}

/// Sequences a standalone 1 Hz tick broadcast batch, returning an optional `Dropped` ledger event
/// when the batch is rejected (`#1077 §A`).
pub fn sequence_tick_with_event(
    state: &mut SessionWireState,
    batch: DestinyBatch,
) -> (Option<OutFrame>, Option<SequencerLedgerEvent>) {
    let (frame, events) = sequence_tick_with_events(state, batch);
    (frame, events.into_iter().next())
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

    fn named_update(stamp: u32, action: &str, args: Vec<PyRep>) -> PyRep {
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(i64::from(stamp)),
            PyRep::Tuple(vec![PyRep::String(action.into()), PyRep::Tuple(args)]),
        ])])
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
                }],
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
                }],
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
}
