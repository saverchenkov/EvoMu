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

const MAX_HELD_SCENE_FRAMES: usize = 256;

/// Scene and epoch provenance attached to space-scoped outbound notifications and destiny
/// batches (`GH #1190` Required Behaviour 3, `GH #1565`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundSceneScope {
    /// Monotonic transition epoch from [`crate::router::ServerContext::session_scene_epoch`].
    /// `None` means pinned by `(scene_sys_id, requires_in_space)` without a specific epoch.
    pub scene_epoch: Option<u64>,
    /// Solar system ID where the frame was produced.
    pub scene_sys_id: Option<u32>,
    /// `true` when delivery requires the session to be undocked in space (or active structure gunner).
    pub requires_in_space: bool,
}

impl OutboundSceneScope {
    pub const fn space(scene_sys_id: Option<u32>, scene_epoch: Option<u64>) -> Self {
        Self {
            scene_epoch,
            scene_sys_id,
            requires_in_space: true,
        }
    }

    pub const fn solar_system(scene_sys_id: Option<u32>, scene_epoch: Option<u64>) -> Self {
        Self {
            scene_epoch,
            scene_sys_id,
            requires_in_space: false,
        }
    }

    pub const fn with_epoch(mut self, scene_epoch: u64) -> Self {
        self.scene_epoch = Some(scene_epoch);
        self
    }
}

#[derive(Debug, Clone)]
enum HeldSceneFrame {
    Destiny(DestinyBatch),
    Notification(TypedNotification),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SceneScopeDecision {
    Allow,
    Hold,
    Drop { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutboundSceneSync {
    pub scene_epoch: u64,
    pub solar_system_id: Option<u32>,
    pub is_in_space: bool,
    pub has_ballpark: bool,
}

/// Outcome of scene-epoch and materialisation gating on a [`TypedNotification`] (`#1190`).
#[derive(Debug, Clone, PartialEq)]
pub enum GateNotificationOutcome {
    Emit(TypedNotification),
    Held(Option<SequencerLedgerEvent>),
    Dropped(SequencerLedgerEvent),
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
    held_frames: VecDeque<HeldSceneFrame>,
    scene_drop_count: u64,
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

    pub fn with_scene_epoch(mut self, scene_epoch: u64) -> Self {
        self.scene_epoch = scene_epoch;
        self
    }

    pub fn held_scene_frame_count(&self) -> usize {
        self.held_frames.len()
    }

    pub fn scene_drop_count(&self) -> u64 {
        self.scene_drop_count
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

    fn set_scene_solar_system(&mut self, solar_system_id: Option<u32>) {
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

    fn scene_matches_scope(&self, scope: OutboundSceneScope) -> bool {
        if scope.requires_in_space && !self.in_space {
            return false;
        }
        if let Some(sys_id) = scope.scene_sys_id {
            if self.solar_system_id != Some(sys_id) {
                return false;
            }
        }
        true
    }

    fn evaluate_scene_scope(
        &self,
        scope: OutboundSceneScope,
        has_set_state: bool,
    ) -> SceneScopeDecision {
        if let Some(scope_epoch) = scope.scene_epoch {
            if scope_epoch < self.scene_epoch {
                return SceneScopeDecision::Drop {
                    reason: format!("stale_scene_epoch:{scope_epoch}<{}", self.scene_epoch),
                };
            }
            if scope_epoch > self.scene_epoch {
                return SceneScopeDecision::Hold;
            }
        }
        if !self.scene_matches_scope(scope) {
            return SceneScopeDecision::Drop {
                reason: format!(
                    "scene_mismatch:in_space={},sys={:?},req_space={},req_sys={:?}",
                    self.in_space,
                    self.solar_system_id,
                    scope.requires_in_space,
                    scope.scene_sys_id
                ),
            };
        }
        if !self.scene_entered && !has_set_state {
            return SceneScopeDecision::Hold;
        }
        SceneScopeDecision::Allow
    }

    fn hold_scene_frame(&mut self, frame: HeldSceneFrame) -> Option<SequencerLedgerEvent> {
        let mut evicted = None;
        if self.held_frames.len() >= MAX_HELD_SCENE_FRAMES {
            if let Some(oldest) = self.held_frames.pop_front() {
                let (epoch, stamp, packet_kind) = match oldest {
                    HeldSceneFrame::Destiny(b) => (b.epoch, b.stamp, Some("DoDestinyUpdate".to_string())),
                    HeldSceneFrame::Notification(n) => (self.current_epoch(), 0, Some(n.event)),
                };
                evicted = Some(SequencerLedgerEvent {
                    kind: SequencerLedgerEventKind::Dropped,
                    epoch,
                    stamp,
                    reason: "held_scene_overflow".to_string(),
                    packet_kind,
                });
            }
        }
        self.held_frames.push_back(frame);
        evicted
    }

    /// Synchronizes authoritative session scene epoch/location onto `SessionWireState` and drops
    /// any held frames invalidated by the transition (`#1190` RB3).
    pub fn sync_scene(
        &mut self,
        scene_epoch: u64,
        solar_system_id: Option<u32>,
        in_space: bool,
    ) -> Vec<SequencerLedgerEvent> {
        self.sync_scene_with_ballpark(scene_epoch, solar_system_id, in_space, in_space)
    }

    /// Synchronizes authoritative session scene epoch/location while distinguishing scene
    /// notification eligibility (`in_space`, e.g. active structure gunners) from whether a
    /// ballpark `SetState` is expected before releasing incremental scene frames (`has_ballpark`).
    pub fn sync_scene_with_ballpark(
        &mut self,
        scene_epoch: u64,
        solar_system_id: Option<u32>,
        in_space: bool,
        has_ballpark: bool,
    ) -> Vec<SequencerLedgerEvent> {
        if scene_epoch > self.scene_epoch {
            self.scene_epoch = scene_epoch;
        }
        self.set_scene_solar_system(solar_system_id);
        self.in_space = in_space;
        self.scene_entered = !has_ballpark || matches!(self.destiny, DestinyStream::Live { .. });
        self.prune_incompatible_held_frames()
    }

    fn prune_incompatible_held_frames(&mut self) -> Vec<SequencerLedgerEvent> {
        let mut dropped = Vec::new();
        let mut kept = VecDeque::with_capacity(self.held_frames.len());
        while let Some(frame) = self.held_frames.pop_front() {
            let (scope_opt, has_set_state, ep, stamp, pkt) = match &frame {
                HeldSceneFrame::Destiny(b) => (
                    b.scene_scope,
                    b.has_set_state,
                    b.epoch,
                    b.stamp,
                    Some("DoDestinyUpdate".to_string()),
                ),
                HeldSceneFrame::Notification(n) => {
                    (n.scene_scope, false, self.current_epoch(), 0, Some(n.event.clone()))
                }
            };
            if let Some(scope) = scope_opt {
                if let SceneScopeDecision::Drop { reason } =
                    self.evaluate_scene_scope(scope, has_set_state)
                {
                    self.scene_drop_count += 1;
                    dropped.push(SequencerLedgerEvent {
                        kind: SequencerLedgerEventKind::Dropped,
                        epoch: ep,
                        stamp,
                        reason,
                        packet_kind: pkt,
                    });
                    continue;
                }
            }
            kept.push_back(frame);
        }
        self.held_frames = kept;
        dropped
    }

    /// Evaluates scene-epoch scope first (drop stale/mismatched, hold newer/pre-entry frames) and
    /// then per-ball materialization gating on a [`TypedNotification`] (`#1190`, `#1084`).
    pub fn gate_typed_notification(&mut self, notif: TypedNotification) -> GateNotificationOutcome {
        if let Some(scope) = notif.scene_scope {
            match self.evaluate_scene_scope(scope, false) {
                SceneScopeDecision::Allow => {}
                SceneScopeDecision::Drop { reason } => {
                    self.scene_drop_count += 1;
                    let stamp = match self.destiny {
                        DestinyStream::Live { last_stamp, .. } => last_stamp,
                        _ => 0,
                    };
                    return GateNotificationOutcome::Dropped(SequencerLedgerEvent {
                        kind: SequencerLedgerEventKind::Dropped,
                        epoch: self.current_epoch(),
                        stamp,
                        reason,
                        packet_kind: Some(notif.event),
                    });
                }
                SceneScopeDecision::Hold => {
                    let evicted = self.hold_scene_frame(HeldSceneFrame::Notification(notif));
                    return GateNotificationOutcome::Held(evicted);
                }
            }
        }
        if let Some(drop_ev) = self.gate_notification(&notif.event, &notif.args) {
            GateNotificationOutcome::Dropped(drop_ev)
        } else {
            GateNotificationOutcome::Emit(notif)
        }
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

    pub fn with_scene_scope(mut self, scene_scope: Option<OutboundSceneScope>) -> Self {
        self.scene_scope = scene_scope;
        self
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

impl TypedNotification {
    pub fn new(event: impl Into<String>, args: Vec<PyRep>) -> Self {
        Self {
            event: event.into(),
            args,
            service: None,
            scene_scope: None,
        }
    }

    pub fn with_service(mut self, service: Option<String>) -> Self {
        self.service = service;
        self
    }

    pub fn with_scene_scope(mut self, scene_scope: Option<OutboundSceneScope>) -> Self {
        self.scene_scope = scene_scope;
        self
    }
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
    sequence_with_events_in_scene(state, done, None)
}

fn push_typed_notif_frame(notif: TypedNotification, out: &mut Vec<OutFrame>) {
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

/// Drains held newer-epoch or pre-scene-entry frames that have now become eligible for delivery,
/// enforcing per-ball materialisation gating in FIFO order (`#1190` RB3).
pub fn drain_ready_held_frames(state: &mut SessionWireState) -> SequenceOutcome {
    let mut frames = Vec::new();
    let mut events = Vec::new();
    drain_ready_held_frames_into(state, &mut frames, &mut events);
    SequenceOutcome { frames, events }
}

fn drain_ready_held_frames_into(
    state: &mut SessionWireState,
    out: &mut Vec<OutFrame>,
    events: &mut Vec<SequencerLedgerEvent>,
) {
    loop {
        if state.held_frames.is_empty() {
            return;
        }
        let was_entered = state.scene_entered;
        let mut pending = std::mem::take(&mut state.held_frames);
        let mut still_held = VecDeque::new();
        while let Some(item) = pending.pop_front() {
            let (scope_opt, has_set_state, ep, stamp, pkt) = match &item {
                HeldSceneFrame::Destiny(b) => (
                    b.scene_scope,
                    b.has_set_state,
                    b.epoch,
                    b.stamp,
                    Some("DoDestinyUpdate".to_string()),
                ),
                HeldSceneFrame::Notification(n) => (
                    n.scene_scope,
                    false,
                    state.current_epoch(),
                    0,
                    Some(n.event.clone()),
                ),
            };
            if let Some(scope) = scope_opt {
                match state.evaluate_scene_scope(scope, has_set_state) {
                    SceneScopeDecision::Allow => {}
                    SceneScopeDecision::Drop { reason } => {
                        state.scene_drop_count += 1;
                        events.push(SequencerLedgerEvent {
                            kind: SequencerLedgerEventKind::Dropped,
                            epoch: ep,
                            stamp,
                            reason,
                            packet_kind: pkt,
                        });
                        continue;
                    }
                    SceneScopeDecision::Hold => {
                        still_held.push_back(item);
                        continue;
                    }
                }
            }
            match item {
                HeldSceneFrame::Destiny(mut batch) => {
                    if let Some(active_ep) = state.destiny.epoch() {
                        batch.epoch = active_ep;
                    }
                    for sub_batch in normalize_server_destiny_batch(state, batch) {
                        let start = out.len();
                        eval_and_push_destiny_batch(state, sub_batch, out, events);
                        finalize_pending_destiny_frames(state, out, start, events);
                    }
                }
                HeldSceneFrame::Notification(notif) => {
                    if let Some(drop_ev) = state.gate_notification(&notif.event, &notif.args) {
                        events.push(drop_ev);
                    } else {
                        push_typed_notif_frame(notif, out);
                    }
                }
            }
        }
        if !still_held.is_empty() {
            still_held.extend(std::mem::take(&mut state.held_frames));
            state.held_frames = still_held;
        }
        if !(!was_entered && state.scene_entered) {
            break;
        }
    }
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
    for notif in done.notifications {
        match state.gate_typed_notification(notif) {
            GateNotificationOutcome::Emit(allowed) => push_typed_notif_frame(allowed, &mut out),
            GateNotificationOutcome::Held(held_ev) => {
                if let Some(ev) = held_ev {
                    events.push(ev);
                }
            }
            GateNotificationOutcome::Dropped(drop_ev) => events.push(drop_ev),
        }
    }

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
    scene_epoch: u64,
    solar_system_id: Option<u32>,
    in_space: bool,
    scene_entered: bool,
}

impl ServerDestinyNormalizer {
    fn from_state(state: &SessionWireState) -> Self {
        Self::from_state_with_scene(state, None)
    }

    fn from_state_with_scene(
        state: &SessionWireState,
        post_rpc_scene: Option<(u64, Option<u32>, bool)>,
    ) -> Self {
        let (scene_epoch, solar_system_id, in_space, scene_entered) =
            if let Some((ep, sys, space)) = post_rpc_scene {
                (
                    state.scene_epoch.max(ep),
                    sys,
                    space,
                    !space || matches!(state.destiny, DestinyStream::Live { .. }),
                )
            } else {
                (
                    state.scene_epoch,
                    state.solar_system_id,
                    state.in_space,
                    state.scene_entered,
                )
            };
        Self {
            stream: state.destiny,
            sim_materialized: state.materialized.clone(),
            active_add_stamps: state.guard.last_addballs_stamp.clone(),
            removed_tombstone_stamps: state.guard.removed_tombstone_stamps.clone(),
            synthetic_remove_floor: state.synthetic_remove_floor,
            scene_epoch,
            solar_system_id,
            in_space,
            scene_entered,
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
            if scope.scene_epoch.is_some_and(|ep| ep != self.scene_epoch) {
                return vec![batch];
            }
            let effective_in_space =
                self.in_space || (allow_attach_from_detached && matches!(self.stream, DestinyStream::Detached));
            let matches_scene = (!scope.requires_in_space || effective_in_space)
                && scope
                    .scene_sys_id
                    .is_none_or(|sys| self.solar_system_id == Some(sys));
            if !matches_scene || (!self.scene_entered && !batch.has_set_state) {
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
    let mut norm = ServerDestinyNormalizer::from_state_with_scene(
        state,
        post_rpc_scene
            .filter(|_| !is_detach)
            .map(|s| (s.scene_epoch, s.solar_system_id, s.has_ballpark)),
    );

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

    fn named_entry(stamp: u32, action: &str, args: Vec<PyRep>) -> PyRep {
        PyRep::Tuple(vec![
            PyRep::Integer(i64::from(stamp)),
            PyRep::Tuple(vec![PyRep::String(action.into()), PyRep::Tuple(args)]),
        ])
    }

    fn named_update(stamp: u32, action: &str, args: Vec<PyRep>) -> PyRep {
        PyRep::List(vec![named_entry(stamp, action, args)])
    }

    fn frame_actions(frame: &OutFrame) -> Option<Vec<&str>> {
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

        let same_sys = TypedNotification::new("OnSolarSystemUpdate", vec![])
            .with_scene_scope(Some(OutboundSceneScope::solar_system(Some(sys_a), Some(3))));
        assert!(matches!(
            wire.gate_typed_notification(same_sys),
            GateNotificationOutcome::Emit(_)
        ));

        let other_sys = TypedNotification::new("OnSolarSystemUpdate", vec![])
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

        let mut bp_new = eve_destiny::Ballpark::new(sys_new);
        bp_new.current_tick = 20;
        bp_new.add_ball(eve_destiny::Ball {
            id: 6001,
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
        let set_state_6001 = eve_destiny::wire::encode_set_state_payload(&bp_new, 6001);
        assert!(matches!(
            wire.accept_destiny_batch_with_reason(
                DestinyBatch::new(99, 20, true, named_update(20, "SetState", vec![set_state_6001]))
                    .with_scene_scope(Some(OutboundSceneScope::space(Some(sys_new), Some(3))))
            ),
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
        // `scene_entered` starts false; held SetState must drain, rebind epoch 99 -> active, and flush all 3.
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
        assert!(outcome.frames.iter().any(|f| matches!(
            f,
            OutFrame::DoDestinyUpdate { epoch, has_set_state: true, .. } if *epoch == active_ep
        )));
        assert!(outcome.frames.iter().any(|f| matches!(
            f,
            OutFrame::DoDestinyUpdate { epoch, has_set_state: false, .. } if *epoch == active_ep
        )));
        assert!(outcome
            .frames
            .iter()
            .any(|f| matches!(f, OutFrame::Notification { event, .. } if event == "OnCombatMessage")));
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
}
