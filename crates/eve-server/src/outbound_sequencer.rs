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
pub use crate::outbound_scene_scope::{
    drain_ready_held_frames, GateNotificationOutcome, OutboundSceneScope, OutboundSceneSync,
};
use crate::outbound_scene_scope::{
    drain_ready_held_frames_into, gate_and_push_typed_notifications, HeldSceneFrame,
    SceneNormalizationView, SceneScopeDecision,
};
use crate::sequencer_invariants::{InvariantValidatorMode, SequencerGuardState};
use eve_destiny::BallId;
use eve_proto::PyRep;
use std::collections::{HashSet, VecDeque};
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
    pub scene_epoch: u64,
    pub scene_entered: bool,
    pub last_client_tick: Option<u64>,
    pub materialized: HashSet<BallId>,
    flight_recorder: Option<Arc<FlightRecorder>>,
    guard: SequencerGuardState,
    synthetic_remove_floor: Option<(u32, u32)>,
    last_wire_epoch_and_stamp: Option<(u32, u32)>,
    pub(crate) held_frames: VecDeque<HeldSceneFrame>,
    pub(crate) scene_drop_count: u64,
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
            scene_epoch: 0,
            scene_entered: true,
            last_client_tick: None,
            materialized: HashSet::new(),
            flight_recorder: None,
            guard: SequencerGuardState::default(),
            synthetic_remove_floor: None,
            last_wire_epoch_and_stamp: None,
            held_frames: VecDeque::new(),
            scene_drop_count: 0,
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
                self.guard.seed_validator_materialized(id);
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
            let view = handle.view();
            if view.balls.contains_key(&observer_ship_id) {
                for b in view.visible_balls_for_observer(observer_ship_id) {
                    self.materialized.insert(b.id);
                    self.guard.seed_validator_materialized(b.id);
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

    pub(crate) fn set_scene_solar_system(&mut self, solar_system_id: Option<u32>) {
        self.solar_system_id = solar_system_id;
    }

    /// Transitions the session into space in `solar_system_id`, allocating a fresh epoch and
    /// entering `DestinyStream::AwaitingSetState { epoch }`.
    pub fn enter_space(&mut self, solar_system_id: Option<u32>) -> u32 {
        let ep = self.allocate_epoch();
        self.in_space = true;
        self.set_scene_solar_system(solar_system_id);
        self.scene_entered = false;
        self.materialized.clear();
        self.guard.reset_epoch_state();
        self.synthetic_remove_floor = None;
        self.last_wire_epoch_and_stamp = None;
        self.destiny = DestinyStream::AwaitingSetState { epoch: ep };
        ep
    }

    /// Ensures the session has an active space epoch (`AwaitingSetState` or `Live`).
    /// If currently `Detached`, transitions to `AwaitingSetState` with a newly allocated epoch.
    pub fn ensure_in_space(&mut self, solar_system_id: Option<u32>) -> u32 {
        self.in_space = true;
        if solar_system_id.is_some() {
            self.set_scene_solar_system(solar_system_id);
        }
        match self.destiny {
            DestinyStream::Detached => self.enter_space(self.solar_system_id),
            DestinyStream::AwaitingSetState { epoch } | DestinyStream::Live { epoch, .. } => epoch,
        }
    }

    /// Returns `true` when this wire session still needs its initial `SetState` batch
    /// delivered on the wire (either not yet `Live`, or initialized synthetically by
    /// `ClientWireSession` at `set_state_stamp: 0` without any accepted `SetState` batch).
    pub fn needs_initial_set_state(&self) -> bool {
        match self.destiny {
            DestinyStream::Detached | DestinyStream::AwaitingSetState { .. } => true,
            DestinyStream::Live {
                set_state_stamp: 0, ..
            } => self.last_wire_epoch_and_stamp.is_none(),
            DestinyStream::Live { .. } => false,
        }
    }

    /// Transitions the session out of space (docking or logoff), setting `DestinyStream::Detached`.
    pub fn leave_space(&mut self) {
        self.in_space = false;
        self.scene_entered = true;
        self.materialized.clear();
        self.guard.reset_epoch_state();
        self.synthetic_remove_floor = None;
        self.last_wire_epoch_and_stamp = None;
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
            self.set_scene_solar_system(new_sys_id);
            return;
        }
        let sys_changed = prev_sys_id != new_sys_id || self.solar_system_id != new_sys_id;
        if !was_in_space || sys_changed {
            self.enter_space(new_sys_id);
        } else {
            self.in_space = true;
            self.set_scene_solar_system(new_sys_id);
        }
    }

    /// Marks the destiny stream as needing a `SetState` resync in the current epoch after a
    /// broadcast channel lag (`RecvError::Lagged`, `ORD-5`).
    pub fn on_broadcast_lagged(&mut self) -> Option<u32> {
        match self.destiny {
            DestinyStream::Detached => None,
            DestinyStream::AwaitingSetState { epoch } | DestinyStream::Live { epoch, .. } => {
                self.scene_entered = false;
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
    Held(Option<SequencerLedgerEvent>),
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
        mut batch: DestinyBatch,
    ) -> AcceptDestinyOutcome {
        if batch.is_empty() {
            return AcceptDestinyOutcome::Empty;
        }
        if let Some(scope) = batch.scene_scope {
            match self.evaluate_scene_scope(scope, batch.has_set_state) {
                SceneScopeDecision::Allow => {}
                SceneScopeDecision::Drop { reason } => {
                    self.scene_drop_count += 1;
                    return AcceptDestinyOutcome::Dropped {
                        epoch: batch.epoch,
                        stamp: batch.stamp,
                        reason,
                    };
                }
                SceneScopeDecision::Hold => {
                    let evicted = self.hold_scene_frame(HeldSceneFrame::Destiny(batch));
                    return AcceptDestinyOutcome::Held(evicted);
                }
            }
        }
        let batch_epoch = batch.epoch;
        let batch_stamp = batch.stamp;
        let prev_wire_stamp = self
            .last_wire_epoch_and_stamp
            .filter(|&(ep, _)| ep == batch_epoch)
            .map(|(_, st)| st)
            .unwrap_or(0);
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
                // Issue #1169: SetState must remain stamped at Ballpark::current_tick globally; if
                // this single session's wire already emitted a higher stamp in this epoch (e.g. an
                // FX-floored delta before broadcast lag resync), clamp only this session's wire
                // SetState up to `prev_wire_stamp` so client monotonicity (`INV_MONOTONIC_STAMP`)
                // holds without inflating `Ballpark::current_tick`.
                let wire_stamp = if batch_stamp < prev_wire_stamp {
                    restamp_batch_entries(&mut batch, prev_wire_stamp);
                    prev_wire_stamp
                } else {
                    batch_stamp
                };
                self.destiny = DestinyStream::Live {
                    epoch,
                    set_state_stamp: wire_stamp,
                    last_stamp: wire_stamp,
                };
                self.scene_entered = true;
                self.last_wire_epoch_and_stamp = Some((epoch, wire_stamp));
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
                    let wire_floor = prev_wire_stamp.max(last_stamp).max(set_state_stamp);
                    let wire_stamp = if batch_stamp < wire_floor {
                        restamp_batch_entries(&mut batch, wire_floor);
                        wire_floor
                    } else {
                        batch_stamp
                    };
                    self.destiny = DestinyStream::Live {
                        epoch,
                        set_state_stamp: wire_stamp,
                        last_stamp: wire_stamp,
                    };
                    self.last_wire_epoch_and_stamp = Some((epoch, wire_stamp));
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
                    self.last_wire_epoch_and_stamp = Some((epoch, batch_stamp));
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
            AcceptDestinyOutcome::Empty
            | AcceptDestinyOutcome::Held(_)
            | AcceptDestinyOutcome::Dropped { .. } => None,
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
    pub scene_scope: Option<OutboundSceneScope>,
}

impl DestinyBatch {
    pub fn new(epoch: u32, stamp: u32, has_set_state: bool, updates: PyRep) -> Self {
        Self {
            epoch,
            stamp,
            has_set_state,
            updates,
            wait_for_bubble: false,
            scene_scope: None,
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
    /// Ballpark attach followed by deferred follow-up batches (e.g. same-tick `RemoveBall*` split to `stamp + 1`).
    AttachSequence {
        epoch: u32,
        set_state: DestinyBatch,
        followups: Vec<DestinyBatch>,
    },
    /// Incremental post-RPC destiny updates in the current space epoch.
    Updates(DestinyBatch),
    /// Multi-frame incremental post-RPC destiny updates ordered by stamp.
    UpdatesSequence(Vec<DestinyBatch>),
    /// Space -> station/structure transition: optional pre-detach updates (e.g. `OnDockingAccepted`)
    /// emitted before `SessionChangeNotification` detaches the ballpark.
    Detach { pre_detach: Option<DestinyBatch> },
    /// Multi-frame pre-detach updates emitted before `SessionChangeNotification` detaches the ballpark.
    DetachSequence(Vec<DestinyBatch>),
}

impl DestinyDrain {
    fn is_detach(&self) -> bool {
        matches!(self, Self::Detach { .. } | Self::DetachSequence(_))
    }

    fn is_updates(&self) -> bool {
        matches!(self, Self::Updates(_) | Self::UpdatesSequence(_))
    }

    fn is_attach(&self) -> bool {
        matches!(self, Self::Attach { .. } | Self::AttachSequence { .. })
    }

    fn from_updates_batches(mut batches: Vec<DestinyBatch>) -> Self {
        match batches.len() {
            0 => Self::None,
            1 => Self::Updates(batches.swap_remove(0)),
            _ => Self::UpdatesSequence(batches),
        }
    }

    fn from_attach_batches(epoch: u32, mut batches: Vec<DestinyBatch>) -> Self {
        if batches.is_empty() {
            Self::None
        } else {
            let set_state = batches.remove(0);
            if batches.is_empty() {
                Self::Attach { epoch, set_state }
            } else {
                Self::AttachSequence {
                    epoch,
                    set_state,
                    followups: batches,
                }
            }
        }
    }

    fn from_detach_batches(mut batches: Vec<DestinyBatch>) -> Self {
        match batches.len() {
            0 => Self::Detach { pre_detach: None },
            1 => Self::Detach {
                pre_detach: Some(batches.swap_remove(0)),
            },
            _ => Self::DetachSequence(batches),
        }
    }
}

/// A typed notification queued for delivery after the RPC response (`C-3`).
#[derive(Debug, Clone, PartialEq)]
pub struct TypedNotification {
    pub event: String,
    pub args: Vec<PyRep>,
    pub service: Option<String>,
    pub scene_scope: Option<OutboundSceneScope>,
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
    /// When `true` for a non-space-attach/updates call, emit `OnItemChange` notifications prior
    /// to `SessionChangeNotification` so docked ship swaps deliver hull items first (`#1085`).
    pub item_change_before_session_change: bool,
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

pub(crate) fn eval_and_push_destiny_batch(
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
        AcceptDestinyOutcome::Held(held_ev) => {
            if let Some(ev) = held_ev {
                events.push(ev);
            }
        }
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

pub(crate) fn finalize_pending_destiny_frames(
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
    sequence_with_events_in_scene(state, done, None)
}

fn sequence_with_events_in_scene(
    state: &mut SessionWireState,
    mut done: RpcCompletion,
    post_rpc_scene: Option<OutboundSceneSync>,
) -> SequenceOutcome {
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
    let detach_frame = destiny_drain.is_detach();
    if detach_frame {
        let pre_start = out.len();
        for tick_batch in pending_ticks.drain(..) {
            eval_and_push_destiny_batch(state, tick_batch, &mut out, &mut events);
        }
        match std::mem::take(&mut destiny_drain) {
            DestinyDrain::Detach { pre_detach } => {
                if let Some(batch) = pre_detach {
                    eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
                }
            }
            DestinyDrain::DetachSequence(batches) => {
                for batch in batches {
                    eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
                }
            }
            _ => {}
        }
        finalize_pending_destiny_frames(state, &mut out, pre_start, &mut events);
        state.leave_space();
    }

    if let Some(sync) = post_rpc_scene {
        events.extend(state.sync_scene_with_ballpark(
            sync.scene_epoch,
            sync.solar_system_id,
            sync.is_in_space,
            sync.has_ballpark,
        ));
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
        && destiny_drain.is_updates();
    let attach_before_call_rsp = destiny_drain.is_attach();
    if !in_space_ship_switch_updates {
        if done.item_change_before_session_change && !detach_frame {
            // Docked ship swap (#1085): emit hull `OnItemChange` before `shipid`
            // `SessionChangeNotification` so client inventory has the target hull before `MakeShipActive`.
            let mut item_changes = Vec::new();
            let mut remaining = Vec::with_capacity(done.notifications.len());
            for notif in std::mem::take(&mut done.notifications) {
                let is_junkyard_tombstone = notif.event == "OnItemChange"
                    && notif.args.first().is_some_and(|item| {
                        item.dbrow_get("locationID")
                            .or_else(|| item.dict_get("locationID"))
                            .and_then(|v| v.as_i64())
                            == Some(6)
                    });
                if notif.event == "OnItemChange" && !is_junkyard_tombstone {
                    item_changes.push(notif);
                } else {
                    remaining.push(notif);
                }
            }
            done.notifications = remaining;
            for notif in item_changes {
                out.push(OutFrame::Notification {
                    event: notif.event,
                    args: notif.args,
                });
            }
        }
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
    if destiny_drain.is_attach() {
        // When attaching to a ballpark, sim clock rebase notices must precede the first
        // ballpark frame (`pending_ticks` / `SetState`) so `michelle` initializes on the rebased clock (#1080, #1085).
        let mut clock_first = Vec::new();
        let mut remaining = Vec::with_capacity(done.notifications.len());
        for notif in std::mem::take(&mut done.notifications) {
            if notif.event == "DoSimClockRebase" {
                clock_first.push(notif);
            } else {
                remaining.push(notif);
            }
        }
        done.notifications = remaining;
        for notif in clock_first {
            out.push(OutFrame::Notification {
                event: notif.event,
                args: notif.args,
            });
        }
    }
    let step4_start = out.len();
    for tick_batch in pending_ticks {
        eval_and_push_destiny_batch(state, tick_batch, &mut out, &mut events);
    }

    match destiny_drain {
        DestinyDrain::Attach { epoch, set_state } => {
            if matches!(state.destiny, DestinyStream::Detached) {
                state.in_space = true;
                state.scene_entered = false;
                state.materialized.clear();
                state.guard.reset_epoch_state();
                state.synthetic_remove_floor = None;
                state.destiny = DestinyStream::AwaitingSetState { epoch };
            }
            eval_and_push_destiny_batch(state, set_state, &mut out, &mut events);
        }
        DestinyDrain::AttachSequence {
            epoch,
            set_state,
            followups,
        } => {
            if matches!(state.destiny, DestinyStream::Detached) {
                state.in_space = true;
                state.scene_entered = false;
                state.materialized.clear();
                state.guard.reset_epoch_state();
                state.synthetic_remove_floor = None;
                state.destiny = DestinyStream::AwaitingSetState { epoch };
            }
            eval_and_push_destiny_batch(state, set_state, &mut out, &mut events);
            for batch in followups {
                eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
            }
        }
        DestinyDrain::Updates(batch) => {
            eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
        }
        DestinyDrain::UpdatesSequence(batches) => {
            for batch in batches {
                eval_and_push_destiny_batch(state, batch, &mut out, &mut events);
            }
        }
        DestinyDrain::None | DestinyDrain::Detach { .. } | DestinyDrain::DetachSequence(_) => {}
    }
    finalize_pending_destiny_frames(state, &mut out, step4_start, &mut events);

    if let Some(sess_frame) = deferred_session_change.take() {
        out.push(OutFrame::SessionChange { frame: sess_frame });
    }
    if let Some(rsp) = pending_call_rsp.take() {
        out.push(rsp);
    }

    // 5. Post-RPC held scene frames (now that scene entry / SetState has completed) followed by
    //    post-RPC notifications in causal emission order (ORD-5, ORD-6), gated by scene-epoch and
    //    `materialized`.
    if state.held_scene_frame_count() > 0 {
        drain_ready_held_frames_into(state, &mut out, &mut events);
    }
    gate_and_push_typed_notifications(state, done.notifications, &mut out, &mut events);

    SequenceOutcome {
        frames: out,
        events,
    }
}

struct PendingNormalizeChunk {
    has_set_state: bool,
    wait_for_bubble: bool,
    entries: Vec<PyRep>,
}

impl PendingNormalizeChunk {
    fn followup() -> Self {
        Self {
            has_set_state: false,
            wait_for_bubble: false,
            entries: Vec::new(),
        }
    }
}

struct ServerDestinyNormalizer {
    stream: DestinyStream,
    sim_materialized: HashSet<BallId>,
    active_add_stamps: std::collections::HashMap<BallId, u32>,
    removed_tombstone_stamps: std::collections::HashMap<BallId, u32>,
    synthetic_remove_floor: Option<(u32, u32)>,
    scene: SceneNormalizationView,
}

impl ServerDestinyNormalizer {
    fn from_state(state: &SessionWireState) -> Self {
        Self::from_state_with_scene(state, None)
    }

    fn from_state_with_scene(
        state: &SessionWireState,
        post_rpc_scene: Option<OutboundSceneSync>,
    ) -> Self {
        Self {
            stream: state.destiny,
            sim_materialized: state.materialized.clone(),
            active_add_stamps: state.guard.last_addballs_stamp.clone(),
            removed_tombstone_stamps: state.guard.removed_tombstone_stamps.clone(),
            synthetic_remove_floor: state.synthetic_remove_floor,
            scene: SceneNormalizationView::from_wire(state, post_rpc_scene),
        }
    }

    fn normalize_batch(
        &mut self,
        mut batch: DestinyBatch,
        allow_attach_from_detached: bool,
    ) -> Vec<DestinyBatch> {
        if batch.is_empty() {
            return Vec::new();
        }
        if let Some(scope) = batch.scene_scope {
            if self.scene.should_pass_raw_to_accept(
                scope,
                batch.has_set_state,
                allow_attach_from_detached,
                matches!(self.stream, DestinyStream::Detached),
            ) {
                return vec![batch];
            }
        }
        if allow_attach_from_detached && matches!(self.stream, DestinyStream::Detached) {
            self.stream = DestinyStream::AwaitingSetState { epoch: batch.epoch };
            self.sim_materialized.clear();
            self.active_add_stamps.clear();
            self.removed_tombstone_stamps.clear();
            self.synthetic_remove_floor = None;
        }

        let original_stamp = batch.stamp;
        let mut clamped = false;
        match self.stream {
            DestinyStream::Detached => return vec![batch],
            DestinyStream::AwaitingSetState { epoch } => {
                if batch.epoch != epoch || !batch.has_set_state {
                    return vec![batch];
                }
            }
            DestinyStream::Live {
                epoch,
                set_state_stamp,
                last_stamp,
            } => {
                if batch.epoch != epoch {
                    return vec![batch];
                }
                if let Some((source_stamp, floor_stamp)) = self.synthetic_remove_floor {
                    if original_stamp >= floor_stamp {
                        self.synthetic_remove_floor = None;
                    } else if original_stamp >= source_stamp {
                        restamp_batch_entries(&mut batch, floor_stamp);
                        clamped = true;
                    }
                }
                if !batch.has_set_state
                    && (batch.stamp < set_state_stamp || batch.stamp < last_stamp)
                {
                    return vec![batch];
                }
            }
        }

        let entries = match batch.updates {
            PyRep::List(items) | PyRep::Tuple(items) => items,
            other => {
                batch.updates = other;
                return vec![batch];
            }
        };

        let initial_stamp = batch.stamp;
        let mut pending: std::collections::BTreeMap<u32, PendingNormalizeChunk> =
            std::collections::BTreeMap::new();
        pending.insert(
            initial_stamp,
            PendingNormalizeChunk {
                has_set_state: batch.has_set_state,
                wait_for_bubble: batch.wait_for_bubble,
                entries,
            },
        );

        let mut emitted = Vec::new();
        let mut max_deferred_stamp: Option<u32> = None;

        while let Some((cur_stamp, mut chunk)) = pending.pop_first() {
            let mut deferred_seen: HashSet<(u32, BallId)> = HashSet::new();
            let mut idx = 0usize;
            while idx < chunk.entries.len() {
                let parsed = {
                    let entry = &chunk.entries[idx];
                    crate::sequencer_invariants::parse_destiny_entry(entry).map(
                        |(st, act, args)| {
                            (st, act.to_string(), args.cloned())
                        },
                    )
                };
                let Some((entry_stamp, action, args_opt)) = parsed else {
                    idx += 1;
                    continue;
                };
                let eff_stamp = entry_stamp.unwrap_or(cur_stamp);

                if action == "SetState" {
                    self.active_add_stamps.clear();
                    self.sim_materialized.clear();
                    self.removed_tombstone_stamps.clear();
                    if let Some(ref args) = args_opt {
                        let mut added = Vec::new();
                        crate::sequencer_invariants::extract_set_state_entities(args, &mut added);
                        for id in added {
                            self.sim_materialized.insert(id);
                        }
                    }
                    idx += 1;
                    continue;
                }

                if matches!(action.as_str(), "AddBalls2" | "AddBalls" | "AddBall") {
                    if let Some(ref args) = args_opt {
                        let mut added = Vec::new();
                        crate::sequencer_invariants::extract_add_balls_entities(
                            &action,
                            args,
                            &mut added,
                        );
                        for id in added {
                            if self
                                .removed_tombstone_stamps
                                .get(&id)
                                .is_some_and(|&tomb| eff_stamp <= tomb)
                            {
                                continue;
                            }
                            self.removed_tombstone_stamps.remove(&id);
                            self.active_add_stamps.insert(id, eff_stamp);
                            self.sim_materialized.insert(id);
                        }
                    }
                    idx += 1;
                    continue;
                }

                if matches!(action.as_str(), "RemoveBalls" | "RemoveBall") {
                    let Some(ref args) = args_opt else {
                        idx += 1;
                        continue;
                    };
                    let mut removed = Vec::new();
                    crate::sequencer_invariants::extract_remove_balls_entities(args, &mut removed);
                    let mut stay_ids = Vec::new();
                    let mut defer_pairs: Vec<(BallId, u32)> = Vec::new();
                    for id in removed {
                        if let Some(&add_stamp) = self.active_add_stamps.get(&id) {
                            if eff_stamp <= add_stamp {
                                let target = add_stamp
                                    .saturating_add(1)
                                    .max(cur_stamp.saturating_add(1));
                                defer_pairs.push((id, target));
                                continue;
                            }
                        }
                        stay_ids.push(id);
                    }
                    if defer_pairs.is_empty() {
                        for &id in &stay_ids {
                            if !self.sim_materialized.remove(&id) {
                                self.removed_tombstone_stamps
                                    .entry(id)
                                    .and_modify(|t| *t = (*t).max(eff_stamp))
                                    .or_insert(eff_stamp);
                            }
                        }
                        idx += 1;
                        continue;
                    }

                    let later_adds = collect_later_added_ids(&chunk.entries[(idx + 1)..]);
                    let has_readd_conflict =
                        defer_pairs.iter().any(|(id, _)| later_adds.contains(id));

                    if has_readd_conflict {
                        let mut tail = chunk.entries.split_off(idx);
                        let tail_added = collect_later_added_ids(&tail);
                        let mut kept = Vec::with_capacity(chunk.entries.len());
                        let mut promoted = Vec::new();
                        for kept_entry in chunk.entries.drain(..) {
                            let promote =
                                crate::sequencer_invariants::extract_dependent_destiny_target_from_entry(
                                    &kept_entry,
                                )
                                .is_some_and(|(_, target)| {
                                    !self.sim_materialized.contains(&target)
                                        && tail_added.contains(&target)
                                });
                            if promote {
                                promoted.push(kept_entry);
                            } else {
                                kept.push(kept_entry);
                            }
                        }
                        chunk.entries = kept;
                        promoted.extend(tail);
                        tail = promoted;

                        let split_target = defer_pairs
                            .iter()
                            .map(|&(_, ts)| ts)
                            .max()
                            .unwrap_or_else(|| cur_stamp.saturating_add(1));
                        max_deferred_stamp = Some(
                            max_deferred_stamp.map_or(split_target, |prev| prev.max(split_target)),
                        );
                        for e in &mut tail {
                            restamp_entry_stamp(e, split_target);
                        }
                        pending
                            .entry(split_target)
                            .or_insert_with(PendingNormalizeChunk::followup)
                            .entries
                            .extend(tail);
                        break;
                    }

                    for &id in &stay_ids {
                        if !self.sim_materialized.remove(&id) {
                            self.removed_tombstone_stamps
                                .entry(id)
                                .and_modify(|t| *t = (*t).max(eff_stamp))
                                .or_insert(eff_stamp);
                        }
                    }
                    let mut grouped: std::collections::BTreeMap<u32, Vec<BallId>> =
                        std::collections::BTreeMap::new();
                    for (id, target) in defer_pairs {
                        if deferred_seen.insert((target, id)) {
                            grouped.entry(target).or_default().push(id);
                        }
                    }

                    if stay_ids.is_empty() && grouped.len() == 1 {
                        let (&target_stamp, ids) = grouped.iter().next().unwrap();
                        let mut removed_entry = chunk.entries.remove(idx);
                        max_deferred_stamp = Some(
                            max_deferred_stamp.map_or(target_stamp, |prev| prev.max(target_stamp)),
                        );
                        if ids.is_empty() {
                            continue;
                        }
                        let deferred_entry = if action == "RemoveBalls" {
                            restamp_entry_stamp(&mut removed_entry, target_stamp);
                            removed_entry
                        } else {
                            make_remove_balls_entry(target_stamp, ids)
                        };
                        pending
                            .entry(target_stamp)
                            .or_insert_with(PendingNormalizeChunk::followup)
                            .entries
                            .push(deferred_entry);
                        continue;
                    }

                    if stay_ids.is_empty() {
                        chunk.entries.remove(idx);
                    } else {
                        chunk.entries[idx] = make_remove_balls_entry(cur_stamp, &stay_ids);
                        idx += 1;
                    }

                    for (target_stamp, ids) in grouped {
                        if ids.is_empty() {
                            continue;
                        }
                        max_deferred_stamp = Some(
                            max_deferred_stamp.map_or(target_stamp, |prev| prev.max(target_stamp)),
                        );
                        pending
                            .entry(target_stamp)
                            .or_insert_with(PendingNormalizeChunk::followup)
                            .entries
                            .push(make_remove_balls_entry(target_stamp, &ids));
                    }
                    continue;
                }

                idx += 1;
            }

            if !chunk.entries.is_empty() {
                match self.stream {
                    DestinyStream::AwaitingSetState { epoch } => {
                        self.stream = DestinyStream::Live {
                            epoch,
                            set_state_stamp: cur_stamp,
                            last_stamp: cur_stamp,
                        };
                    }
                    DestinyStream::Live {
                        epoch,
                        set_state_stamp,
                        last_stamp,
                    } => {
                        let new_set_state = if chunk.has_set_state {
                            cur_stamp
                        } else {
                            set_state_stamp
                        };
                        self.stream = DestinyStream::Live {
                            epoch,
                            set_state_stamp: new_set_state,
                            last_stamp: if chunk.has_set_state {
                                last_stamp.max(cur_stamp)
                            } else {
                                cur_stamp
                            },
                        };
                    }
                    DestinyStream::Detached => {}
                }
                emitted.push(DestinyBatch {
                    epoch: batch.epoch,
                    stamp: cur_stamp,
                    has_set_state: chunk.has_set_state,
                    updates: PyRep::List(chunk.entries),
                    wait_for_bubble: chunk.wait_for_bubble,
                    scene_scope: batch.scene_scope,
                });
            }
        }

        if let Some(floor) = max_deferred_stamp {
            let new_src = match self.synthetic_remove_floor {
                Some((prev_src, _)) => prev_src.min(original_stamp),
                None => original_stamp,
            };
            let new_floor = match self.synthetic_remove_floor {
                Some((_, prev_floor)) => prev_floor.max(floor),
                None => floor,
            };
            self.synthetic_remove_floor = Some((new_src, new_floor));
        } else if clamped && emitted.is_empty() {
            // Preserve floor if nothing emitted.
        }

        emitted
    }
}

fn restamp_damage_state_map(rep: &mut PyRep, new_stamp: u32) {
    let stamp_rep = PyRep::Integer(i64::from(new_stamp));
    let patch_entry = |val: &mut PyRep| {
        if let PyRep::Tuple(outer) | PyRep::List(outer) = val {
            if let Some(PyRep::Tuple(shield_tuple) | PyRep::List(shield_tuple)) =
                outer.first_mut()
            {
                if shield_tuple.len() >= 3 && shield_tuple[2].as_i64().is_some() {
                    shield_tuple[2] = stamp_rep.clone();
                }
            }
        }
    };
    match rep {
        PyRep::Dict(entries) => {
            for (_, val) in entries.iter_mut() {
                patch_entry(val);
            }
        }
        PyRep::List(entries) | PyRep::Tuple(entries) => {
            for item in entries.iter_mut() {
                if let PyRep::Tuple(kv) | PyRep::List(kv) = item {
                    if kv.len() >= 2 {
                        patch_entry(&mut kv[1]);
                    }
                }
            }
        }
        _ => {}
    }
}

fn restamp_state_binary_header(rep: &mut PyRep, new_stamp: u32) {
    match rep {
        PyRep::Bytes(bytes) | PyRep::Buffer(bytes) if bytes.len() >= 5 => {
            bytes[1..5].copy_from_slice(&new_stamp.to_le_bytes());
        }
        _ => {}
    }
}

fn restamp_set_state_args(args: &mut PyRep, new_stamp: u32) {
    match args {
        PyRep::KeyVal(map) => {
            if let Some(v) = map.get_mut("stamp") {
                if v.as_i64().is_some() {
                    *v = PyRep::Integer(i64::from(new_stamp));
                }
            }
            if let Some(v) = map.get_mut("state") {
                restamp_state_binary_header(v, new_stamp);
            }
            if let Some(v) = map.get_mut("damageState") {
                restamp_damage_state_map(v, new_stamp);
            }
        }
        PyRep::Dict(entries) => {
            for (k, v) in entries.iter_mut() {
                match k.as_str() {
                    Some("stamp") if v.as_i64().is_some() => {
                        *v = PyRep::Integer(i64::from(new_stamp));
                    }
                    Some("state") => {
                        restamp_state_binary_header(v, new_stamp);
                    }
                    Some("damageState") => {
                        restamp_damage_state_map(v, new_stamp);
                    }
                    _ => {}
                }
            }
        }
        PyRep::Tuple(items) | PyRep::List(items) => {
            for item in items {
                restamp_set_state_args(item, new_stamp);
            }
        }
        _ => {}
    }
}

fn restamp_entry_stamp(entry: &mut PyRep, new_stamp: u32) {
    let outer = match entry {
        PyRep::Tuple(items) | PyRep::List(items) if items.len() >= 2 => items,
        _ => return,
    };
    if outer[0].as_i64().is_some() {
        outer[0] = PyRep::Integer(i64::from(new_stamp));
    }
    if let PyRep::Tuple(action_pair) | PyRep::List(action_pair) = &mut outer[1] {
        if action_pair.first().and_then(PyRep::as_str) == Some("SetState") {
            if let Some(args) = action_pair.get_mut(1) {
                restamp_set_state_args(args, new_stamp);
            }
        }
    }
}

fn restamp_batch_entries(batch: &mut DestinyBatch, new_stamp: u32) {
    let prev_stamp = batch.stamp;
    batch.stamp = new_stamp;
    if let PyRep::List(items) | PyRep::Tuple(items) = &mut batch.updates {
        for entry in items {
            let matches_batch = crate::sequencer_invariants::parse_destiny_entry(entry)
                .is_some_and(|(st, _, _)| st.is_none_or(|s| s == prev_stamp || s < new_stamp));
            if matches_batch {
                restamp_entry_stamp(entry, new_stamp);
            }
        }
    }
}

fn collect_later_added_ids(entries: &[PyRep]) -> HashSet<BallId> {
    let mut ids = HashSet::new();
    let mut tmp = Vec::new();
    for entry in entries {
        let Some((_, action, Some(args))) = crate::sequencer_invariants::parse_destiny_entry(entry)
        else {
            continue;
        };
        tmp.clear();
        if action == "SetState" {
            crate::sequencer_invariants::extract_set_state_entities(args, &mut tmp);
        } else if matches!(action, "AddBalls2" | "AddBalls" | "AddBall") {
            crate::sequencer_invariants::extract_add_balls_entities(action, args, &mut tmp);
        }
        ids.extend(tmp.iter().copied());
    }
    ids
}

fn make_remove_balls_entry(stamp: u32, ball_ids: &[BallId]) -> PyRep {
    PyRep::Tuple(vec![
        PyRep::Integer(i64::from(stamp)),
        PyRep::Tuple(vec![
            PyRep::String("RemoveBalls".into()),
            PyRep::Tuple(vec![PyRep::List(
                ball_ids.iter().map(|&id| PyRep::Integer(id)).collect(),
            )]),
        ]),
    ])
}

/// Normalizes a server-generated tick `DestinyBatch` for live emission, deferring any
/// `RemoveBall` / `RemoveBalls` whose `stamp <= last_addballs_stamp` into `stamp + 1`
/// and clamping subsequent same-server-tick updates cleanly up to the deferred stamp floor.
pub fn normalize_server_destiny_batch(
    state: &mut SessionWireState,
    batch: DestinyBatch,
) -> Vec<DestinyBatch> {
    let mut norm = ServerDestinyNormalizer::from_state(state);
    let out = norm.normalize_batch(batch, false);
    if !out.is_empty() {
        state.synthetic_remove_floor = norm.synthetic_remove_floor;
    }
    out
}

/// Normalizes a server-generated `RpcCompletion`'s destiny batches (`pending_tick_batches` +
/// `destiny_drain`) so `INV_REMOVE_AFTER_ADDBALLS` and stamp monotonicity hold across same-tick
/// lifecycle sequences, then runs [`sequence_with_events`].
pub fn sequence_server_rpc_with_events(
    state: &mut SessionWireState,
    done: RpcCompletion,
) -> SequenceOutcome {
    sequence_server_rpc_in_scene_with_events(state, done, None)
}

/// Normalizes a server-generated `RpcCompletion`'s destiny batches, synchronizes post-RPC scene
/// transition state `(scene_epoch, solar_system_id, in_space)` after any pre-detach step, and runs
/// [`sequence_with_events_in_scene`] (`#1190` RB3).
pub fn sequence_server_rpc_in_scene_with_events(
    state: &mut SessionWireState,
    mut done: RpcCompletion,
    post_rpc_scene: Option<OutboundSceneSync>,
) -> SequenceOutcome {
    let is_detach = done.destiny_drain.is_detach();
    let mut norm =
        ServerDestinyNormalizer::from_state_with_scene(state, post_rpc_scene.filter(|_| !is_detach));

    let mut normalized_ticks = Vec::with_capacity(done.pending_tick_batches.len());
    for tick_batch in std::mem::take(&mut done.pending_tick_batches) {
        normalized_ticks.extend(norm.normalize_batch(tick_batch, false));
    }
    done.pending_tick_batches = normalized_ticks;

    done.destiny_drain = match std::mem::take(&mut done.destiny_drain) {
        DestinyDrain::None => DestinyDrain::None,
        DestinyDrain::Updates(batch) => {
            DestinyDrain::from_updates_batches(norm.normalize_batch(batch, false))
        }
        DestinyDrain::UpdatesSequence(batches) => {
            let mut out = Vec::new();
            for b in batches {
                out.extend(norm.normalize_batch(b, false));
            }
            DestinyDrain::from_updates_batches(out)
        }
        DestinyDrain::Attach { epoch, set_state } => {
            DestinyDrain::from_attach_batches(epoch, norm.normalize_batch(set_state, true))
        }
        DestinyDrain::AttachSequence {
            epoch,
            set_state,
            followups,
        } => {
            let mut out = norm.normalize_batch(set_state, true);
            for b in followups {
                out.extend(norm.normalize_batch(b, false));
            }
            DestinyDrain::from_attach_batches(epoch, out)
        }
        DestinyDrain::Detach { pre_detach } => {
            let out = match pre_detach {
                Some(b) => norm.normalize_batch(b, false),
                None => Vec::new(),
            };
            DestinyDrain::from_detach_batches(out)
        }
        DestinyDrain::DetachSequence(batches) => {
            let out_batches = batches;
            let mut out = Vec::new();
            for b in out_batches {
                out.extend(norm.normalize_batch(b, false));
            }
            DestinyDrain::from_detach_batches(out)
        }
    };

    if !is_detach {
        state.synthetic_remove_floor = norm.synthetic_remove_floor;
    }

    sequence_with_events_in_scene(state, done, post_rpc_scene)
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
        AcceptDestinyOutcome::Held(held_ev) => (None, held_ev.into_iter().collect()),
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
#[path = "outbound_sequencer_tests.rs"]
pub(crate) mod tests;
