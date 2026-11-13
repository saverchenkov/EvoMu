//! In-process `machoNet` client wire dispatcher (`C-2`, `#926`).
//!
//! Wraps [`crate::macho_listener::execute_call_req_wire`] and [`crate::macho_listener::out_frame_to_wire_pyrep`]
//! so tests and client harnesses invoke the exact socket-loop path (reply normalization, bound object
//! representation, notification suppression and SDE normalization, session-change enrichment, destiny
//! framing, and `ErrorResponse` mapping) and verify `blue.marshal` round-trip encoding on every frame.

#![forbid(unsafe_code)]

use crate::macho_listener::{
    execute_call_req_wire, out_frame_to_wire_pyrep, CallReqWireRefs,
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
        let subscribed_sys_id = session.solar_system_id;
        let sys_rx = ctx
            .systems
            .get_or_spawn(subscribed_sys_id.unwrap_or(0))
            .subscribe();
        let mut wire_state = SessionWireState::new(session.session_id, client_id);
        if session.character_id.is_some()
            && session.solar_system_id.is_some()
            && session.station_id.is_none()
            && session.structure_id.is_none()
        {
            let ep = wire_state.enter_space(session.solar_system_id);
            wire_state.destiny = crate::outbound_sequencer::DestinyStream::Live {
                epoch: ep,
                set_state_stamp: 0,
                last_stamp: 0,
            };
        }
        Self {
            ctx,
            session,
            client_id,
            next_call_id: 1,
            sys_rx,
            subscribed_sys_id,
            wire_state,
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
    let mut subscribed_sys_id = session.solar_system_id;
    let mut sys_rx = ctx
        .systems
        .get_or_spawn(subscribed_sys_id.unwrap_or(0))
        .subscribe();
    let mut wire_state = SessionWireState::new(session.session_id, client_id);
    if session.character_id.is_some()
        && session.solar_system_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_none()
    {
        let ep = wire_state.enter_space(session.solar_system_id);
        wire_state.destiny = crate::outbound_sequencer::DestinyStream::Live {
            epoch: ep,
            set_state_stamp: 0,
            last_stamp: 0,
        };
    }
    let mut wire_refs = CallReqWireRefs {
        client_id,
        sys_rx: &mut sys_rx,
        subscribed_sys_id: &mut subscribed_sys_id,
        wire_state: &mut wire_state,
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
    }
}
