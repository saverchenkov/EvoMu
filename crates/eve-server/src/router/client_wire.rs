//! In-process `machoNet` client wire dispatcher (`C-2`, `#926`).
//!
//! Wraps [`crate::macho_listener::execute_call_req_wire`] and [`crate::macho_listener::out_frame_to_wire_pyrep`]
//! so tests and client harnesses invoke the exact socket-loop path (reply normalization, bound object
//! representation, notification suppression and SDE normalization, session-change enrichment, destiny
//! framing, and `ErrorResponse` mapping) and verify `blue.marshal` round-trip encoding on every frame.

#![forbid(unsafe_code)]

use crate::macho_listener::{
    execute_call_req_wire, out_frame_to_wire_pyrep, CallReqWireRefs, PendingDeferredShipId,
};
use crate::outbound_sequencer::{OutFrame, SessionWireState};
use crate::router::{PendingMachoNotification, ServerContext};
use crate::system_actor::TickBroadcast;
use eve_machonet::{MachoAddress, SessionState, MACHO_PROXY_NODE_ID};
use eve_proto::{marshal, BridgeRpcError, PyRep};
use std::collections::HashMap;
use tokio::sync::broadcast;

/// Wire-level outcome of executing one `CallReq` through the server socket-loop path (`C-2`, `#926`).
#[derive(Debug, Clone)]
pub struct ClientWireCallOutcome {
    /// Post-`machoNet:26000` reply `PyRep` or socket-loop enriched [`BridgeRpcError`].
    pub rpc_result: std::result::Result<PyRep, BridgeRpcError>,
    /// Mapped `ErrorResponse.payload` body when `rpc_result` is `Err`, matching
    /// [`crate::macho_listener::map_call_req_error_body`].
    pub error_wire_body: Option<PyRep>,
    /// Exact ordered outbound sequencer frames (`CallRsp`, `DoDestinyUpdate`, `SessionChange`, `Notification`).
    pub out_frames: Vec<OutFrame>,
    /// Exact materialized `MachoPacket` `PyRep` frames matching `macho_listener::write_out_frames`.
    pub wire_packets: Vec<PyRep>,
    /// Any `blue.marshal` encode/decode round-trip errors detected on emitted wire packets.
    pub marshal_errors: Vec<String>,
    /// Suppressed and SDE-normalized `(event, args)` notifications emitted during this call.
    pub notifications: Vec<(String, Vec<PyRep>)>,
    /// Raw per-character notifications consumed from `ServerContext::pending_notifications`.
    pub drained_char_notifications: Vec<PendingMachoNotification>,
    /// `true` iff this call failed with an unmapped [`BridgeRpcError`] that fell back to wire
    /// `CustomInfo` (`#1195`).
    pub used_unmapped_custom_info_fallback: bool,
}

/// Persistent client wire session mirroring a single TCP client's `machoNet` connection state (`C-2`).
pub struct ClientWireSession<'c> {
    pub ctx: &'c ServerContext,
    pub session: SessionState,
    pub client_id: u64,
    next_call_id: u64,
    pub sys_rx: broadcast::Receiver<TickBroadcast>,
    pub subscribed_sys_id: Option<u32>,
    pub wire_state: SessionWireState,
    pub deferred_ship_id: Option<PendingDeferredShipId>,
}

fn session_has_active_ballpark(ctx: &ServerContext, session: &SessionState) -> bool {
    session.character_id.is_some()
        && session.solar_system_id.is_some()
        && session.station_id.is_none()
        && (session.structure_id.is_none()
            || session
                .character_id
                .zip(session.solar_system_id)
                .is_some_and(|(cid, sys_id)| {
                    ctx.is_space_ballpark_bootstrapped(session.session_id, cid, sys_id)
                }))
}

fn initial_wire_tick_subscription(
    ctx: &ServerContext,
    session: &SessionState,
) -> (Option<u32>, broadcast::Receiver<TickBroadcast>) {
    if session_has_active_ballpark(ctx, session) {
        if let Some(sid) = session
            .solar_system_id
            .filter(|&s| s > 0 && ctx.sde.solar_systems.contains_key(&s))
        {
            return (Some(sid), ctx.systems.get_or_spawn(sid).subscribe());
        }
    }
    // Pre-character, unauthenticated, and docked sessions without View Outside hold a closed
    // channel until undock or space entry attaches a live solar system subscription.
    let (_tx, closed_rx) = broadcast::channel(1);
    (None, closed_rx)
}

fn build_wire_state_for_session(
    ctx: &ServerContext,
    session: &SessionState,
    client_id: u64,
) -> SessionWireState {
    let mut wire_state = SessionWireState::new(session.session_id, client_id)
        .with_flight_recorder(std::sync::Arc::clone(&ctx.flight_recorder));
    let scene_epoch = ctx.session_scene_epoch(session.session_id);
    let has_ballpark = session_has_active_ballpark(ctx, session);
    let is_scene_in_space = has_ballpark
        || (session.character_id.is_some()
            && session.station_id.is_none()
            && session.solar_system_id.is_some()
            && session.structure_id.is_some()
            && session.structure_id == session.ship_id);
    if has_ballpark {
        let ep = wire_state.enter_space(session.solar_system_id);
        wire_state.destiny = crate::outbound_sequencer::DestinyStream::Live {
            epoch: ep,
            set_state_stamp: 0,
            last_stamp: 0,
        };
        if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
            wire_state.seed_materialized_from_ballpark(ctx, sys_id, ship_id);
        }
    }
    let _ = wire_state.sync_scene_with_ballpark(
        scene_epoch,
        session.solar_system_id,
        is_scene_in_space,
        has_ballpark,
    );
    wire_state
}

impl<'c> ClientWireSession<'c> {
    /// Creates a fresh authenticated/pre-login wire session with `live_notify_listener = true`,
    /// matching `macho_listener::handle_macho_client`.
    pub fn new(ctx: &'c ServerContext, session_id: u64) -> Self {
        let mut session = SessionState::new(session_id);
        session.live_notify_listener = true;
        Self::from_session(ctx, session)
    }

    /// Wraps an existing `SessionState` on the client wire path, preserving session fields while
    /// enabling `live_notify_listener = true` matching `macho_listener::handle_macho_client`.
    pub fn from_session(ctx: &'c ServerContext, mut session: SessionState) -> Self {
        let client_id = 1_000_000_000_000_u64.saturating_add(session.session_id);
        session.live_notify_listener = true;
        let (subscribed_sys_id, sys_rx) = initial_wire_tick_subscription(ctx, &session);
        let wire_state = build_wire_state_for_session(ctx, &session, client_id);
        if ctx.flight_recorder.get_wire_state(session.session_id).is_none() {
            ctx.flight_recorder.record_wire_state(&wire_state);
        }
        Self {
            ctx,
            session,
            client_id,
            next_call_id: 1,
            sys_rx,
            subscribed_sys_id,
            wire_state,
            deferred_ship_id: None,
        }
    }

    /// Consumes this wire session and returns the inner `SessionState`.
    pub fn into_session(self) -> SessionState {
        self.session
    }

    /// Dispatches a single RPC call through the exact socket-loop path and returns all ordered frames.
    pub async fn call_wire(
        &mut self,
        service: &str,
        bound_id: Option<u64>,
        method: &str,
        args: &[PyRep],
        kwargs: &HashMap<String, PyRep>,
    ) -> ClientWireCallOutcome {
        let call_id = self.next_call_id;
        self.next_call_id = self.next_call_id.saturating_add(1);
        let mut wire_refs = CallReqWireRefs {
            client_id: self.client_id,
            sys_rx: &mut self.sys_rx,
            subscribed_sys_id: &mut self.subscribed_sys_id,
            wire_state: &mut self.wire_state,
            deferred_ship_id: &mut self.deferred_ship_id,
        };
        execute_wire_step(
            self.ctx,
            &mut self.session,
            &mut wire_refs,
            call_id,
            service,
            bound_id,
            method,
            args,
            kwargs,
        )
        .await
    }

    /// Flushes a pending deferred `shipid: [None, shipID]` `SessionChangeNotification` if its 350 ms
    /// deadline has elapsed (`GH #1085`).
    #[cfg(any(test, feature = "testkit"))]
    pub fn poll_deferred_ship_id(&mut self) -> (Vec<OutFrame>, Vec<PyRep>) {
        let mut wire_refs = CallReqWireRefs {
            client_id: self.client_id,
            sys_rx: &mut self.sys_rx,
            subscribed_sys_id: &mut self.subscribed_sys_id,
            wire_state: &mut self.wire_state,
            deferred_ship_id: &mut self.deferred_ship_id,
        };
        let Some(frame) = crate::macho_listener::flush_deferred_ship_id_if_due(
            self.ctx,
            &self.session,
            &mut wire_refs,
            tokio::time::Instant::now(),
        ) else {
            return (Vec::new(), Vec::new());
        };
        let pkt = out_frame_to_wire_pyrep(
            MACHO_PROXY_NODE_ID,
            self.client_id,
            self.session.user_id,
            &frame,
        );
        let bytes = marshal::encode(&pkt);
        if let Err(err) = marshal::decode(&bytes) {
            panic!("deferred shipid frame failed marshal round-trip: {err}");
        }
        (vec![frame], vec![pkt])
    }

    /// Drains any pending background tick `DoDestinyUpdate` frames, session changes, and queued
    /// notifications without issuing a dummy RPC (`GH #1077 §C`).
    #[cfg(any(test, feature = "testkit"))]
    pub fn drain_pending_frames(&mut self) -> (Vec<OutFrame>, Vec<PyRep>) {
        let mut wire_refs = CallReqWireRefs {
            client_id: self.client_id,
            sys_rx: &mut self.sys_rx,
            subscribed_sys_id: &mut self.subscribed_sys_id,
            wire_state: &mut self.wire_state,
            deferred_ship_id: &mut self.deferred_ship_id,
        };
        let (out_frames, wire_packets) =
            crate::macho_listener::drain_pending_tick_frames_for_wire_session(
                self.ctx,
                &mut self.session,
                &mut wire_refs,
            );
        for (idx, pkt) in wire_packets.iter().enumerate() {
            let bytes = marshal::encode(pkt);
            if let Err(err) = marshal::decode(&bytes) {
                panic!("pending frame[{idx}] failed marshal round-trip: {err}");
            }
        }
        (out_frames, wire_packets)
    }
}

/// Stateless one-shot helper that routes a single `(&ServerContext, &mut SessionState)` RPC call
/// through the exact `machoNet` socket-loop path (`execute_call_req_wire` + frame materialization).
pub async fn dispatch_call_on_wire(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> ClientWireCallOutcome {
    let prev_live = session.live_notify_listener;
    session.live_notify_listener = true;
    let client_id = 1_000_000_000_000_u64.saturating_add(session.session_id);
    let (mut subscribed_sys_id, mut sys_rx) = initial_wire_tick_subscription(ctx, session);
    let mut wire_state = build_wire_state_for_session(ctx, session, client_id);
    let mut deferred_ship_id = None;
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
        deferred_ship_id: &mut deferred_ship_id,
    };
    let out = execute_wire_step(
        ctx,
        session,
        &mut wire_refs,
        1,
        service,
        bound_id,
        method,
        args,
        kwargs,
    )
    .await;
    session.live_notify_listener = prev_live;
    out
}

#[allow(clippy::too_many_arguments)]
async fn execute_wire_step(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut CallReqWireRefs<'_>,
    call_id: u64,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> ClientWireCallOutcome {
    let req_source = MachoAddress::Client {
        client_id: wire.client_id,
        call_id,
        service: None,
    };
    let req_dest = MachoAddress::Node {
        node_id: MACHO_PROXY_NODE_ID,
        service: Some(service.to_string()),
        call_id,
    };
    let raw = execute_call_req_wire(
        ctx,
        session,
        wire,
        &req_source,
        &req_dest,
        service,
        bound_id,
        method,
        args,
        kwargs,
    )
    .await;

    let mut wire_packets = Vec::with_capacity(raw.out_frames.len());
    let mut marshal_errors = Vec::new();
    for (idx, frame) in raw.out_frames.iter().enumerate() {
        let pkt = out_frame_to_wire_pyrep(
            MACHO_PROXY_NODE_ID,
            wire.client_id,
            session.user_id,
            frame,
        );
        let bytes = marshal::encode(&pkt);
        if let Err(err) = marshal::decode(&bytes) {
            marshal_errors.push(format!(
                "frame[{idx}] on {service}.{method} failed marshal round-trip: {err}"
            ));
        }
        wire_packets.push(pkt);
    }

    ClientWireCallOutcome {
        rpc_result: raw.rpc_result,
        error_wire_body: raw.error_wire_body,
        out_frames: raw.out_frames,
        wire_packets,
        marshal_errors,
        notifications: raw.notifications,
        drained_char_notifications: raw.drained_char_notifications,
        used_unmapped_custom_info_fallback: raw.used_unmapped_custom_info_fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outbound_sequencer::{DestinyStream, OutFrame};
    use crate::system_actor::SystemLifecycleState;
    use eve_db::{CharacterRecord, InventoryFlag, InventoryItemRecord};

    #[tokio::test]
    async fn prechar_and_docked_wire_sessions_keep_systems_cold_and_undock_subscribes_set_state() {
        let ctx = ServerContext::new();
        let sys_id = 30_000_142_u32;
        let station_id = 60_003_760_i64;
        let user_id = 91_542_u64;
        let char_id = 91_542_001_i64;
        let ship_id = 101_542_001_i64;
        let session_id = 91_542_010_u64;

        assert!(ctx.sde.solar_systems.contains_key(&sys_id));
        assert!(ctx.sde.stations.contains_key(&station_id));

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.insert(
                char_id,
                CharacterRecord {
                    character_id: char_id,
                    user_id,
                    name: "WireColdPilot".into(),
                    corporation_id: 1_000_009,
                    station_id: Some(station_id),
                    structure_id: None,
                    solar_system_id: sys_id,
                    constellation_id: 20_000_020,
                    region_id: 10_000_002,
                    active_ship_id: ship_id,
                    home_station_id: station_id,
                    clone_grade: 2,
                    version: 1,
                    system_epoch: 1,
                    ..Default::default()
                },
            );
            snap.items.insert(
                ship_id,
                InventoryItemRecord {
                    item_id: ship_id,
                    type_id: 587,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        });
        let oneshot_session_id = 91_542_011_u64;
        ctx.db
            .acquire_online_lock(
                user_id,
                oneshot_session_id,
                Some(char_id),
                true,
                ctx.now_unix_secs(),
            )
            .expect("acquire online lock for one-shot wire session");

        let kw = HashMap::new();

        // 1. Pre-character wire session must not spawn system 0 or Jita.
        let mut pre_char_wire = ClientWireSession::new(&ctx, 91_542_009);
        assert!(pre_char_wire.subscribed_sys_id.is_none());
        assert!(ctx.systems.get_handle(0).is_none());
        assert_eq!(
            ctx.systems.system_lifecycle_state(0),
            SystemLifecycleState::Cold
        );
        let time_out = pre_char_wire
            .call_wire("machoNet", None, "GetTime", &[], &kw)
            .await;
        assert!(time_out.rpc_result.is_ok());
        assert!(ctx.systems.get_handle(0).is_none());
        assert!(ctx.systems.get_handle(sys_id).is_none());

        // 2. Docked wire session (`from_session`) and one-shot `dispatch_call_on_wire` must not
        //    spawn system 0 or the docked solar system.
        let mut docked_sess = SessionState::new(session_id);
        docked_sess.user_id = Some(user_id);
        docked_sess.character_id = Some(char_id);
        docked_sess.corporation_id = Some(1_000_009);
        docked_sess.solar_system_id = Some(sys_id);
        docked_sess.station_id = Some(station_id);
        docked_sess.ship_id = Some(ship_id);
        docked_sess.role = eve_machonet::service_roles::ROLE_PLAYER_DEFAULT;

        let mut oneshot_sess = docked_sess.clone();
        oneshot_sess.session_id = oneshot_session_id;
        let bal_out = dispatch_call_on_wire(
            &ctx,
            &mut oneshot_sess,
            "account",
            None,
            "GetCashBalance",
            &[],
            &kw,
        )
        .await;
        assert!(bal_out.rpc_result.is_ok());
        assert!(ctx.systems.get_handle(0).is_none());
        assert!(ctx.systems.get_handle(sys_id).is_none());

        ctx.db
            .acquire_online_lock(
                user_id,
                session_id,
                Some(char_id),
                true,
                ctx.now_unix_secs(),
            )
            .expect("acquire online lock for persistent docked wire session");

        let mut wire = ClientWireSession::from_session(&ctx, docked_sess);
        assert!(wire.subscribed_sys_id.is_none());
        assert!(matches!(wire.wire_state.destiny, DestinyStream::Detached));
        assert!(ctx.systems.get_handle(0).is_none());
        assert!(ctx.systems.get_handle(sys_id).is_none());
        assert_eq!(
            ctx.systems.system_lifecycle_state(sys_id),
            SystemLifecycleState::Cold
        );

        // 3. Undocking through `call_wire` subscribes to `sys_id` and emits `SetState` without
        //    ever spawning system 0.
        let undock_out = wire.call_wire("ship", None, "Undock", &[], &kw).await;
        assert!(
            undock_out.rpc_result.is_ok(),
            "ship.Undock failed: {:?}",
            undock_out.rpc_result
        );
        assert!(undock_out.marshal_errors.is_empty());
        assert!(ctx.systems.get_handle(0).is_none());
        assert_eq!(wire.subscribed_sys_id, Some(sys_id));
        assert!(ctx.systems.get_handle(sys_id).is_some());
        assert!(
            undock_out.out_frames.iter().any(|f| matches!(
                f,
                OutFrame::DoDestinyUpdate {
                    has_set_state: true,
                    ..
                }
            )),
            "undock through call_wire must emit SetState, got {:?}",
            undock_out.out_frames
        );
        assert!(matches!(wire.wire_state.destiny, DestinyStream::Live { .. }));
    }
}
