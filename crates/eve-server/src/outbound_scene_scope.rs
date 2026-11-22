//! Scene and epoch scoping for outbound destiny frames and notifications (`GH #1190` RB3, `GH #1565`).

#![forbid(unsafe_code)]

use crate::outbound_sequencer::{
    eval_and_push_destiny_batch, finalize_pending_destiny_frames, normalize_server_destiny_batch,
    DestinyBatch, DestinyStream, OutFrame, SequenceOutcome, SequencerLedgerEvent,
    SequencerLedgerEventKind, SessionWireState, TypedNotification,
};
use crate::router::cache_notifications::PendingMachoNotification;
use crate::router::notify_registry::{
    build_pending_typed_notification, TypedNotification as TypedEvent,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::VecDeque;

pub(crate) const MAX_HELD_SCENE_FRAMES: usize = 256;

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
pub(crate) enum HeldSceneFrame {
    Destiny(DestinyBatch),
    Notification(TypedNotification),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SceneScopeDecision {
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

pub(crate) struct SceneNormalizationView {
    pub(crate) scene_epoch: u64,
    pub(crate) solar_system_id: Option<u32>,
    pub(crate) in_space: bool,
    pub(crate) scene_entered: bool,
}

impl SceneNormalizationView {
    pub(crate) fn from_wire(
        state: &SessionWireState,
        post_rpc_scene: Option<OutboundSceneSync>,
    ) -> Self {
        if let Some(sync) = post_rpc_scene {
            Self {
                scene_epoch: state.scene_epoch.max(sync.scene_epoch),
                solar_system_id: sync.solar_system_id,
                in_space: sync.has_ballpark,
                scene_entered: !sync.has_ballpark
                    || matches!(state.destiny, DestinyStream::Live { .. }),
            }
        } else {
            Self {
                scene_epoch: state.scene_epoch,
                solar_system_id: state.solar_system_id,
                in_space: state.in_space,
                scene_entered: state.scene_entered,
            }
        }
    }

    pub(crate) fn should_pass_raw_to_accept(
        &self,
        scope: OutboundSceneScope,
        has_set_state: bool,
        allow_attach_from_detached: bool,
        stream_detached: bool,
    ) -> bool {
        if scope.scene_epoch.is_some_and(|ep| ep != self.scene_epoch) {
            return true;
        }
        let effective_in_space = self.in_space || (allow_attach_from_detached && stream_detached);
        let matches_scene = (!scope.requires_in_space || effective_in_space)
            && scope
                .scene_sys_id
                .is_none_or(|sys| self.solar_system_id == Some(sys));
        !matches_scene || (!self.scene_entered && !has_set_state)
    }
}

impl DestinyBatch {
    pub fn with_scene_scope(mut self, scene_scope: Option<OutboundSceneScope>) -> Self {
        self.scene_scope = scene_scope;
        self
    }
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

impl SessionWireState {
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

    pub(crate) fn scene_matches_scope(&self, scope: OutboundSceneScope) -> bool {
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

    pub(crate) fn evaluate_scene_scope(
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

    pub(crate) fn hold_scene_frame(
        &mut self,
        frame: HeldSceneFrame,
    ) -> Option<SequencerLedgerEvent> {
        let mut evicted = None;
        if self.held_frames.len() >= MAX_HELD_SCENE_FRAMES {
            if let Some(oldest) = self.held_frames.pop_front() {
                self.scene_drop_count += 1;
                let (epoch, stamp, packet_kind) = match oldest {
                    HeldSceneFrame::Destiny(b) => {
                        (b.epoch, b.stamp, Some("DoDestinyUpdate".to_string()))
                    }
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

    pub(crate) fn clear_held_scene_frames(&mut self) {
        self.held_frames.clear();
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
    pub fn gate_typed_notification(
        &mut self,
        notif: TypedNotification,
    ) -> GateNotificationOutcome {
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
}

pub(crate) fn typed_notification_to_out_frame(notif: TypedNotification) -> OutFrame {
    if let Some(service) = notif.service {
        OutFrame::ServiceCall {
            service,
            method: notif.event,
            args: notif.args,
        }
    } else {
        OutFrame::Notification {
            event: notif.event,
            args: notif.args,
        }
    }
}

pub(crate) fn gate_typed_notification_to_out_frame(
    state: &mut SessionWireState,
    notif: TypedNotification,
) -> (Option<OutFrame>, Option<SequencerLedgerEvent>) {
    match state.gate_typed_notification(notif) {
        GateNotificationOutcome::Emit(allowed) => {
            (Some(typed_notification_to_out_frame(allowed)), None)
        }
        GateNotificationOutcome::Held(ev) => (None, ev),
        GateNotificationOutcome::Dropped(ev) => (None, Some(ev)),
    }
}

pub(crate) fn gate_and_push_typed_notifications(
    state: &mut SessionWireState,
    notifs: Vec<TypedNotification>,
    out: &mut Vec<OutFrame>,
    events: &mut Vec<SequencerLedgerEvent>,
) {
    for notif in notifs {
        let (frame_opt, ev_opt) = gate_typed_notification_to_out_frame(state, notif);
        if let Some(ev) = ev_opt {
            events.push(ev);
        }
        if let Some(frame) = frame_opt {
            out.push(frame);
        }
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

pub(crate) fn drain_ready_held_frames_into(
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
                    if !was_entered && state.scene_entered {
                        break;
                    }
                }
                HeldSceneFrame::Notification(notif) => {
                    if let Some(drop_ev) = state.gate_notification(&notif.event, &notif.args) {
                        events.push(drop_ev);
                    } else {
                        out.push(typed_notification_to_out_frame(notif));
                    }
                }
            }
        }
        let entered_now = !was_entered && state.scene_entered;
        if entered_now {
            still_held.extend(pending);
            still_held.extend(std::mem::take(&mut state.held_frames));
            state.held_frames = still_held;
            continue;
        }
        if !still_held.is_empty() {
            still_held.extend(std::mem::take(&mut state.held_frames));
            state.held_frames = still_held;
        }
        break;
    }
}

pub(crate) const fn space_scope(
    solar_system_id: Option<u32>,
    scene_epoch: Option<u64>,
) -> Option<OutboundSceneScope> {
    Some(OutboundSceneScope::space(solar_system_id, scene_epoch))
}

pub(crate) const fn ballpark_space_scope(solar_system_id: u32) -> Option<OutboundSceneScope> {
    Some(OutboundSceneScope::space(
        if solar_system_id != 0 {
            Some(solar_system_id)
        } else {
            None
        },
        None,
    ))
}

pub fn is_active_structure_gunner(session: &SessionState) -> bool {
    session.character_id.is_some()
        && session.station_id.is_none()
        && session.solar_system_id.is_some()
        && session.structure_id.is_some()
        && session.structure_id == session.ship_id
}

pub(crate) fn session_space_scope(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<OutboundSceneScope> {
    space_scope(
        session.solar_system_id,
        Some(ctx.session_scene_epoch(session.session_id)),
    )
}

pub(crate) fn session_scene_sync(
    ctx: &ServerContext,
    session: &SessionState,
    is_scene_in_space: bool,
    is_in_space: bool,
) -> OutboundSceneSync {
    OutboundSceneSync {
        scene_epoch: ctx.session_scene_epoch(session.session_id),
        solar_system_id: session.solar_system_id,
        is_in_space: is_scene_in_space,
        has_ballpark: is_in_space,
    }
}

impl PendingMachoNotification {
    pub fn with_scene_scope(mut self, scene_scope: Option<OutboundSceneScope>) -> Self {
        self.scene_scope = scene_scope;
        self
    }
}

/// Scene-scoped variant of [`build_pending_typed_notification`] attaching [`OutboundSceneScope`] (`#1190` RB3).
pub fn build_pending_typed_notification_in_scene<N: TypedEvent>(
    recipient_char_id: i64,
    user_id_opt: Option<u64>,
    id_type: &str,
    created_filetime: i64,
    scene_scope: Option<OutboundSceneScope>,
    notif: N,
) -> PendingMachoNotification {
    build_pending_typed_notification(recipient_char_id, user_id_opt, id_type, created_filetime, notif)
        .with_scene_scope(scene_scope)
}

impl ServerContext {
    /// Resolves `location_id` as a valid solar system ID from SDE metadata (`#1190`).
    pub fn resolve_solar_system_location_id(&self, location_id: i64) -> Option<u32> {
        u32::try_from(location_id)
            .ok()
            .filter(|id| self.sde.solar_systems.contains_key(id))
    }

    /// Returns the active online session's transition `scene_epoch` for `char_id`, if any (`#1190`).
    pub fn active_session_scene_epoch_for_char(&self, char_id: i64) -> Option<u64> {
        let uid = self
            .db
            .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id))?;
        self.db
            .get_online_session(uid)
            .filter(|os| os.character_id == Some(char_id))
            .map(|os| self.session_scene_epoch(os.session_id))
    }

    /// Resolves a space [`OutboundSceneScope`] bound to `sys_id` and `char_id`'s current `scene_epoch` (`#1190`, `#1565`).
    pub fn character_space_scene_scope(
        &self,
        char_id: i64,
        sys_id: Option<u32>,
    ) -> OutboundSceneScope {
        let resolved_sys = sys_id.or_else(|| {
            self.db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
        });
        let epoch = self.active_session_scene_epoch_for_char(char_id);
        OutboundSceneScope::space(resolved_sys, epoch)
    }

    /// Scene-scoped variant of [`Self::emit_typed`] carrying explicit [`OutboundSceneScope`] (`#1190` RB3).
    pub fn emit_typed_in_scene<N: TypedEvent>(
        &self,
        target: NotificationTarget,
        scene_scope: Option<OutboundSceneScope>,
        notif: N,
    ) -> Vec<PendingMachoNotification> {
        self.notify_in_scene(target, N::EVENT, N::DEFAULT_ID_TYPE, notif.into_args(), scene_scope)
    }

    pub(crate) fn default_notify_scene_scope(
        &self,
        target: &NotificationTarget,
    ) -> Option<OutboundSceneScope> {
        match target {
            NotificationTarget::SolarSystem(sys_id) => {
                Some(OutboundSceneScope::solar_system(Some(*sys_id), None))
            }
            _ => None,
        }
    }

    pub(crate) fn bind_recipient_scene_scope(
        &self,
        scene_scope: Option<OutboundSceneScope>,
        cid: i64,
        uid: Option<u64>,
    ) -> Option<OutboundSceneScope> {
        scene_scope.map(|mut scope| {
            if scope.scene_epoch.is_none() {
                // Never fall back to `active_session_scene_epoch_for_char` here: `notify_in_scene`
                // holds `pending_notifications` (`RANK_LEAF = 60`), which outranks `db.with_snapshot` (40).
                scope.scene_epoch = uid
                    .and_then(|u| self.db.get_online_session(u))
                    .filter(|os| os.character_id == Some(cid))
                    .map(|os| self.session_scene_epoch(os.session_id));
            }
            scope
        })
    }
}

/// Stamps `scene_scope.scene_epoch` from `db` (`WorldRepository::active_scene_epoch_for_char_and_user`)
/// without holding any `ServerContext` `RANK_LEAF` lock (`#1190`).
pub(crate) fn bind_db_recipient_scene_scope(
    db: &eve_db::WorldRepository,
    scene_scope: Option<OutboundSceneScope>,
    cid: i64,
    uid: Option<u64>,
) -> Option<OutboundSceneScope> {
    scene_scope.map(|mut scope| {
        if scope.scene_epoch.is_none() {
            scope.scene_epoch = uid.and_then(|u| db.active_scene_epoch_for_char_and_user(cid, u));
        }
        scope
    })
}

impl ServerContext {

    pub(crate) fn space_char_sys_in_snapshot(
        &self,
        snap: &eve_db::WorldStateSnapshot,
        char_id: i64,
    ) -> Option<u32> {
        snap.characters
            .get(&char_id)
            .filter(|c| {
                c.station_id.is_none()
                    && c.structure_id.is_none_or(|st| c.active_ship_id == st)
            })
            .map(|c| c.solar_system_id)
    }

    pub(crate) fn item_or_char_space_sys(&self, item_id: i64, char_id: i64) -> Option<u32> {
        self.db.with_snapshot(|snap| {
            snap.items
                .get(&item_id)
                .and_then(|it| self.resolve_solar_system_location_id(it.location_id))
                .or_else(|| self.space_char_sys_in_snapshot(snap, char_id))
        })
    }

    pub(crate) fn notify_damage_state_change_in_scene(
        &self,
        target: NotificationTarget,
        item_id: i64,
        dmg: PyRep,
    ) -> Vec<PendingMachoNotification> {
        let sys_opt = match target {
            NotificationTarget::SolarSystem(sid) => Some(sid),
            NotificationTarget::Character(cid) => self.item_or_char_space_sys(item_id, cid),
            _ => None,
        };
        let scope = space_scope(sys_opt, None);
        self.notify_in_scene(
            target,
            "OnDamageStateChange",
            "itemid",
            vec![PyRep::Integer(item_id), dmg],
            scope,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn notify_godma_ship_effect_in_scene(
        &self,
        target: NotificationTarget,
        item_id: i64,
        effect_id: i32,
        active: bool,
        duration_ms: i64,
        repeat: i32,
    ) -> Vec<PendingMachoNotification> {
        let sys_opt = match target {
            NotificationTarget::SolarSystem(sid) => Some(sid),
            NotificationTarget::Character(cid) => {
                self.db.with_snapshot(|s| self.space_char_sys_in_snapshot(s, cid))
            }
            _ => None,
        };
        let now_ticks = self.now_system_sim_filetime_ticks(sys_opt);
        let flag = if active { 1 } else { 0 };
        let env = PyRep::Tuple(vec![
            PyRep::Integer(item_id), PyRep::None, PyRep::Integer(item_id),
            PyRep::None, PyRep::None, PyRep::List(Vec::new()), PyRep::Integer(effect_id as i64),
        ]);
        let scope = if matches!(target, NotificationTarget::SolarSystem(_))
            || (effect_id != 16 && sys_opt.is_some())
        {
            space_scope(sys_opt, None)
        } else {
            None
        };
        self.notify_in_scene(
            target,
            "OnGodmaShipEffect",
            "charid",
            vec![
                PyRep::Integer(item_id), PyRep::Integer(effect_id as i64), PyRep::Integer(now_ticks),
                PyRep::Integer(flag), PyRep::Integer(flag), env, PyRep::Integer(now_ticks),
                PyRep::Real((duration_ms.max(0)) as f64), PyRep::Integer(repeat as i64),
                PyRep::Integer(0), PyRep::None,
            ],
            scope,
        )
    }

    pub(crate) fn emit_special_fx_in_scene(
        &self,
        char_id: i64,
        ship_id: i64,
        sfx_args: Vec<PyRep>,
    ) {
        let sfx_scope = space_scope(self.item_or_char_space_sys(ship_id, char_id), None);
        self.notify_in_scene(
            NotificationTarget::Character(char_id),
            "OnSpecialFX",
            "charid",
            sfx_args.clone(),
            sfx_scope,
        );
        for (obs_cid, _) in self.space_observer_chars_for_ship(ship_id, char_id) {
            self.notify_in_scene(
                NotificationTarget::Character(obs_cid),
                "OnSpecialFX",
                "charid",
                sfx_args.clone(),
                sfx_scope,
            );
        }
    }

    pub(crate) fn notify_target_events_in_scene(
        &self,
        char_id: i64,
        list: Vec<PyRep>,
    ) -> Vec<PendingMachoNotification> {
        let sys_opt = self
            .db
            .with_snapshot(|snap| self.space_char_sys_in_snapshot(snap, char_id));
        self.notify_in_scene(
            NotificationTarget::Character(char_id),
            "OnMultiEvent",
            "charid",
            vec![PyRep::List(list)],
            space_scope(sys_opt, None),
        )
    }
}

#[cfg(test)]
#[path = "outbound_scene_scope_tests.rs"]
mod tests;
