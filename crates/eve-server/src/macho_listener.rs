//! Native V24.02 `machoNet` / `GPSTransport` TCP listener & connection state machine (`P-6`).
//!
//! Implements the full 4-step `GPSTransport` handshake over length-prefixed `blue.marshal` TCP frames:
//! 1. `LowLevelVersionExchange` server 7-tuple (`(170472, macho_version=496, user_count, 24.02, build=3698585, "V24.02@ccp", update_info=None)`), matching the installed client's `start.ini` / `eveMachoNetVersion`
//! 2. Command packet (`(None, "QC")` queue check, `(None, "VK", vipKey)` VIP key, `("placebo", {})` `CryptoRequestPacket` -> `"OK CC"`)
//! 3. `CryptoChallengePacket` `(client_challenge, handshake_dict)` -> credential/SSO JWT verification against `WorldRepository`, `passwordVersion`, `CryptoServerHandshake`
//! 4. `CryptoHandshakeResult` -> `CryptoHandshakeAck` + `SessionInitialStateNotification` (type 18) + `SessionChangeNotification` (type 16)
//!
//! Unauthenticated `AuthenticationReq("ok")` or premature `CallReq` packets are rejected with `GPSTransportClosed`.

#![forbid(unsafe_code)]

use crate::outbound_sequencer::{DestinyDrain, DestinyStream, RpcCompletion};
use crate::router::ServerContext;
use anyhow::Result;
use eve_machonet::{
    CryptoChallengePacket, GPSTransportClosed, HandshakeIdentity, HandshakeStepOutput,
    MachoAddress, MachoHandshakeStateMachine, MachoPacket, MachoPacketType, SessionState,
    VersionExchange, MACHO_PROXY_NODE_ID,
};
use eve_proto::{marshal, BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, warn};

fn extract_handshake_client_build(pyrep: &PyRep) -> Option<u32> {
    if let Some(vx) = VersionExchange::from_pyrep(pyrep) {
        return Some(vx.build_version);
    }
    if let Some(pkt) = MachoPacket::from_pyrep(pyrep) {
        if let Some(vx) = VersionExchange::from_pyrep(&pkt.payload) {
            return Some(vx.build_version);
        }
        if let Some(ch) = CryptoChallengePacket::from_pyrep(&pkt.payload) {
            return Some(ch.boot_build);
        }
    }
    if let Some(ch) = CryptoChallengePacket::from_pyrep(pyrep) {
        return Some(ch.boot_build);
    }
    None
}

fn record_and_warn_handshake_failure(
    ctx: &ServerContext,
    session_id: u64,
    user_id: Option<u64>,
    peer_addr: &str,
    client_boot_build: Option<u32>,
    server_boot_build: u32,
    closed: &GPSTransportClosed,
) {
    let is_build_mismatch = closed.reason.contains("Boot Build Mismatch")
        || closed.reason_code.as_deref() == Some("HANDSHAKE_INCOMPATIBLEBUILD");
    ctx.flight_recorder.record_handshake_builds(
        session_id,
        user_id,
        client_boot_build,
        server_boot_build,
        is_build_mismatch,
    );
    if is_build_mismatch {
        let client_build_str = client_boot_build
            .map(|b| b.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        warn!(
            session_id,
            user_id = ?user_id,
            peer_addr = %peer_addr,
            client_build = %client_build_str,
            server_build = server_boot_build,
            reason_code = ?closed.reason_code,
            reason = %closed.reason,
            "Rejected MachoNet client handshake: Boot Build Mismatch"
        );
    }
}

/// Maximum frame length accepted on TCP listeners (4 MiB, matching `eve_proto::marshal::MAX_FRAME_SIZE`) to prevent memory exhaustion.
pub const MAX_MACHO_FRAME_LEN: usize = eve_proto::marshal::MAX_FRAME_SIZE;
/// Maximum frame length and `zlib` inflation budget accepted prior to completing the `GPSTransport` authentication handshake (64 KiB).
pub const MAX_PREAUTH_MACHO_FRAME_LEN: usize = 64 * 1024;
/// Maximum decoded AST / byte-weight nodes accepted prior to completing the `GPSTransport` authentication handshake.
pub const MAX_PREAUTH_DECODED_NODES: usize = 4_096;
/// Idle timeout waiting for the next machoNet frame header on an open connection.
pub const IDLE_READ_TIMEOUT_SECS: u64 = 300;
/// Maximum timeout to finish reading a frame body after its 4-byte length prefix arrives.
pub const FRAME_BODY_TIMEOUT_SECS: u64 = 30;
/// Maximum timeout to finish writing and flushing a single machoNet frame (`#880` / `C18`).
pub const WRITE_FRAME_TIMEOUT_SECS: u64 = 30;
/// Maximum total wall-clock time allowed for an unauthenticated connection to complete the 4-step `GPSTransport` handshake (`#853` / `B15`).
pub const PREAUTH_HANDSHAKE_DEADLINE_SECS: u64 = 15;
/// Maximum concurrent unauthenticated connections permitted per peer IP (`#853` / `B15`).
pub const MAX_PREAUTH_CONNECTIONS_PER_IP: usize = 32;
/// Maximum wall-clock delay before flushing a deferred dock `shipid: [None, shipID]` session change (`GH #1085`).
pub const DEFERRED_SHIP_ID_DELAY_MS: u64 = 350;

/// Pending post-dock `shipid: [None, shipID]` `SessionChangeNotification` held until NPC station
/// hangar `invbroker` (`List`, `ListByFlags`, `GetSelfInvItem`) completes or 350 ms elapses (`GH #1085`, `#1140`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingDeferredShipId {
    pub char_id: i64,
    pub ship_id: i64,
    pub dock_id: i64,
    pub deadline: tokio::time::Instant,
}

fn drain_sys_rx_discarding_lag<T: Clone>(
    sys_rx: &mut tokio::sync::broadcast::Receiver<T>,
) -> u64 {
    let mut lagged_total = 0u64;
    loop {
        match sys_rx.try_recv() {
            Ok(_) => {}
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                lagged_total = lagged_total.saturating_add(skipped);
            }
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
        }
    }
    lagged_total
}

fn tick_broadcast_batch_for_observer(
    ctx: &ServerContext,
    tick_out: &crate::system_actor::TickBroadcast,
    epoch: u32,
    observer_ids: &[i64],
) -> Option<crate::outbound_sequencer::DestinyBatch> {
    for &obs_id in observer_ids {
        if let Some(updates_pyrep) = tick_out.observer_updates_pyrep.get(&obs_id) {
            let evs = tick_out
                .observer_events
                .get(&obs_id)
                .map(Vec::as_slice)
                .unwrap_or(&tick_out.events);
            if let Some(batch) = build_destiny_batch(
                ctx,
                epoch,
                tick_out.tick,
                evs,
                updates_pyrep.clone(),
                observer_ids,
                SelfDestinyWireContext::NonDock,
            ) {
                return Some(batch);
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn drain_pending_tick_batches_for_sys_with_watermark(
    ctx: &ServerContext,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    watermark_seq: Option<u64>,
    pre_batches: &mut Vec<crate::outbound_sequencer::DestinyBatch>,
    post_batches: &mut Vec<crate::outbound_sequencer::DestinyBatch>,
    session_id: u64,
) -> (u32, bool) {
    let mut ep = wire_state.current_epoch();
    let mut effective_was_live = true;
    loop {
        match sys_rx.try_recv() {
            Ok(tick_out) => {
                if !effective_was_live {
                    continue;
                }
                if tick_out.solar_system_id == sys_id {
                    if let Some(batch) =
                        tick_broadcast_batch_for_observer(ctx, &tick_out, ep, observer_ids)
                    {
                        let is_post = watermark_seq
                            .is_some_and(|w| tick_out.broadcast_seq > w);
                        if is_post {
                            post_batches.push(batch);
                        } else {
                            pre_batches.push(batch);
                        }
                    }
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                warn!(
                    session_id,
                    skipped,
                    "Destiny broadcast channel lagged during pre-RPC drain; discarding stale ticks and resyncing SetState (ORD-5)"
                );
                pre_batches.clear();
                post_batches.clear();
                wire_state.on_broadcast_lagged();
                ep = wire_state.current_epoch();
                effective_was_live = false;
            }
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
        }
    }
    (ep, effective_was_live)
}

#[derive(Clone, Copy)]
enum SnapshotCutoff {
    WatermarkSeq(u64),
    MinTick(u64),
}

/// Drains queued `TickBroadcast` messages from `sys_rx`, discarding any message up to
/// `cutoff` (already captured in a synchronous `SetState` / direct drain) and appending
/// strictly newer batches for `observer_ids` to `post_batches`.
///
/// Returns `true` if `TryRecvError::Lagged` broke continuity past `cutoff` or occurred
/// after reading a post-cutoff tick (clearing `post_batches` so callers re-snapshot `SetState`).
#[must_use]
fn drain_sys_rx_after_snapshot(
    ctx: &ServerContext,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    observer_ids: &[i64],
    epoch: u32,
    cutoff: SnapshotCutoff,
    post_batches: &mut Vec<crate::outbound_sequencer::DestinyBatch>,
) -> bool {
    let mut saw_post_cutoff = false;
    let mut pending_lag = false;
    let mut post_loss = false;
    loop {
        match sys_rx.try_recv() {
            Ok(tick_out) => {
                if post_loss {
                    continue;
                }
                if pending_lag {
                    let gap_detected = match cutoff {
                        SnapshotCutoff::WatermarkSeq(w) => {
                            tick_out.broadcast_seq == 0
                                || tick_out.broadcast_seq > w.saturating_add(1)
                        }
                        SnapshotCutoff::MinTick(t) => tick_out.tick > t.saturating_add(1),
                    };
                    if gap_detected {
                        post_batches.clear();
                        post_loss = true;
                        pending_lag = false;
                        continue;
                    }
                    pending_lag = false;
                }
                let is_post = match cutoff {
                    SnapshotCutoff::WatermarkSeq(w) => tick_out.broadcast_seq > w,
                    SnapshotCutoff::MinTick(t) => tick_out.tick > t,
                };
                if is_post {
                    saw_post_cutoff = true;
                    if tick_out.solar_system_id == sys_id {
                        if let Some(batch) =
                            tick_broadcast_batch_for_observer(ctx, &tick_out, epoch, observer_ids)
                        {
                            post_batches.push(batch);
                        }
                    }
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => {
                post_batches.clear();
                if saw_post_cutoff {
                    post_loss = true;
                    pending_lag = false;
                } else {
                    pending_lag = true;
                }
            }
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => {
                if pending_lag {
                    post_batches.clear();
                    post_loss = true;
                }
                break;
            }
        }
    }
    post_loss
}

/// Drains queued `TickBroadcast` messages from `sys_rx`, discarding any message up to
/// `watermark_seq` (already captured in a synchronous `SetState` / direct drain) and
/// appending strictly newer `> watermark_seq` batches for `observer_ids` to `post_batches`.
///
/// Returns `true` if `TryRecvError::Lagged` broke continuity at `watermark_seq + 1` or occurred
/// after reading a post-watermark tick (clearing `post_batches` so callers re-snapshot `SetState`).
/// Lag that only evicts `<= watermark_seq` ticks (with earliest retained broadcast at
/// `1 <= broadcast_seq <= watermark_seq.saturating_add(1)`) preserves post-watermark batches (`false`).
#[must_use]
fn drain_sys_rx_discarding_up_to_seq(
    ctx: &ServerContext,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    observer_ids: &[i64],
    epoch: u32,
    watermark_seq: u64,
    post_batches: &mut Vec<crate::outbound_sequencer::DestinyBatch>,
) -> bool {
    drain_sys_rx_after_snapshot(
        ctx,
        sys_rx,
        sys_id,
        observer_ids,
        epoch,
        SnapshotCutoff::WatermarkSeq(watermark_seq),
        post_batches,
    )
}

const MAX_SET_STATE_RESYNC_ATTEMPTS: usize = 3;

struct ResyncedSetState {
    tick: u64,
    events: Vec<eve_destiny::DestinyEvent>,
    updates_pyrep: PyRep,
    post_batches: Vec<crate::outbound_sequencer::DestinyBatch>,
}

/// Pushes a fresh `SetState` snapshot for `ship_id`, drains it with its `watermark_seq`,
/// and catches up `sys_rx` while checking `drain_sys_rx_discarding_up_to_seq` continuity.
/// Retries boundedly if `sys_rx` suffers post-watermark lag during the resync; leaves `wire_state`
/// in `AwaitingSetState` (`on_broadcast_lagged()`) and returns `None` if retries exhaust.
async fn resync_set_state_with_sys_rx(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    ship_id: i64,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
) -> Option<ResyncedSetState> {
    let ep = wire_state.current_epoch();
    for _ in 0..MAX_SET_STATE_RESYNC_ATTEMPTS {
        let _ = sys_handle.push_set_state(ship_id).await;
        let Ok((tick, events, updates_pyrep, watermark_seq)) =
            sys_handle.drain_observer_updates_with_seq(ship_id).await
        else {
            wire_state.on_broadcast_lagged();
            return None;
        };
        sys_handle.record_warp_stops_from_events(&events);
        let mut post_batches = Vec::new();
        if !drain_sys_rx_discarding_up_to_seq(
            ctx,
            sys_rx,
            sys_id,
            observer_ids,
            ep,
            watermark_seq,
            &mut post_batches,
        ) {
            return Some(ResyncedSetState {
                tick,
                events,
                updates_pyrep,
                post_batches,
            });
        }
    }
    wire_state.on_broadcast_lagged();
    None
}

/// Non-destructively snapshots `SetState` for shared static structure observer `structure_id`
/// without enqueueing into or draining `observer_events[structure_id]` (`#1461` F-2), so
/// concurrent pilots viewing the same structure never steal each other's events or stall ticks.
async fn resync_structure_set_state_with_sys_rx(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    sys_id: u32,
    structure_id: i64,
    observer_ids: &[i64],
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
) -> Option<ResyncedSetState> {
    let ep = wire_state.current_epoch();
    for _ in 0..MAX_SET_STATE_RESYNC_ATTEMPTS {
        let _ = drain_sys_rx_discarding_lag(sys_rx);
        let Ok((tick, _state_kv, updates_pyrep)) = sys_handle.get_set_state(structure_id).await
        else {
            wire_state.on_broadcast_lagged();
            return None;
        };
        let events = vec![eve_destiny::DestinyEvent::SetState {
            observer_ball_id: structure_id,
        }];
        let mut post_batches = Vec::new();
        if !drain_sys_rx_after_snapshot(
            ctx,
            sys_rx,
            sys_id,
            observer_ids,
            ep,
            SnapshotCutoff::MinTick(tick),
            &mut post_batches,
        ) {
            return Some(ResyncedSetState {
                tick,
                events,
                updates_pyrep,
                post_batches,
            });
        }
    }
    wire_state.on_broadcast_lagged();
    None
}

struct ShipSwapDestinySync {
    pre_batches: Vec<crate::outbound_sequencer::DestinyBatch>,
    direct_batch: Option<crate::outbound_sequencer::DestinyBatch>,
    post_batches: Vec<crate::outbound_sequencer::DestinyBatch>,
}

/// Synchronizes Destiny state across an asynchronous same-system in-space ship swap (`notify_rx`).
/// Splits `sys_rx` around `drain_observer_updates_with_seq`'s `watermark_seq`, updates live state on
/// `TryRecvError::Lagged`, and re-snapshots `SetState` whenever `wire_state.destiny` is
/// `AwaitingSetState` without a `SetState` or post-watermark `sys_rx` lag occurred (`#1172`).
#[allow(clippy::too_many_arguments)]
async fn sync_in_space_ship_swap_destiny(
    ctx: &ServerContext,
    sys_rx: &mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    session_id: u64,
    sys_id: u32,
    new_ship_id: i64,
    prev_ship_id: Option<i64>,
    prev_in_same_space: bool,
) -> Option<ShipSwapDestinySync> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (mut tick, mut events, mut updates_pyrep, watermark_seq) = sys_handle
        .drain_observer_updates_with_seq(new_ship_id)
        .await
        .ok()?;
    sys_handle.record_warp_stops_from_events(&events);
    let has_set_state = events
        .iter()
        .any(|e| matches!(e, eve_destiny::DestinyEvent::SetState { .. }));
    let suppressed_ids = [new_ship_id, prev_ship_id.unwrap_or(new_ship_id)];
    let mut pre_batches = Vec::new();
    let mut post_batches = Vec::new();
    let mut ep = wire_state.ensure_in_space(Some(sys_id));
    let mut is_live_same_system = matches!(
        wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::Live { .. }
    ) && prev_in_same_space;
    let mut post_lagged = false;
    if matches!(
        wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::Live { .. }
    ) {
        let (next_ep, next_live) = drain_pending_tick_batches_for_sys_with_watermark(
            ctx,
            sys_rx,
            sys_id,
            &suppressed_ids,
            wire_state,
            Some(watermark_seq),
            &mut pre_batches,
            &mut post_batches,
            session_id,
        );
        ep = next_ep;
        is_live_same_system = is_live_same_system && next_live;
        if !next_live && has_set_state {
            post_lagged = true;
        }
    } else if has_set_state {
        post_lagged = drain_sys_rx_discarding_up_to_seq(
            ctx,
            sys_rx,
            sys_id,
            &suppressed_ids,
            ep,
            watermark_seq,
            &mut post_batches,
        );
    }

    let mut aborted_resync = false;
    if (matches!(
        wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::AwaitingSetState { .. }
    ) && !has_set_state
        && !is_live_same_system)
        || post_lagged
    {
        post_batches.clear();
        if let Some(resynced) = resync_set_state_with_sys_rx(
            ctx,
            &sys_handle,
            sys_rx,
            sys_id,
            new_ship_id,
            &suppressed_ids,
            wire_state,
        )
        .await
        {
            tick = resynced.tick;
            events = resynced.events;
            updates_pyrep = resynced.updates_pyrep;
            post_batches = resynced.post_batches;
            ep = wire_state.current_epoch();
        } else {
            aborted_resync = true;
        }
    }

    let direct_batch = if aborted_resync {
        None
    } else {
        build_destiny_batch(
            ctx,
            ep,
            tick,
            &events,
            updates_pyrep,
            &suppressed_ids,
            SelfDestinyWireContext::NonDock,
        )
    };
    Some(ShipSwapDestinySync {
        pre_batches,
        direct_batch,
        post_batches,
    })
}

pub use crate::router::NEXT_TRANSPORT_SESSION_ID as NEXT_MACHO_SESSION_ID;
/// Global monotonic session-ID counter shared across `machoNet`, Python Bridge, and Quasar gRPC (`ORD-9`).
pub use crate::router::NEXT_TRANSPORT_SESSION_ID as NEXT_SESSION_ID;

static PREAUTH_CONN_COUNTS: std::sync::LazyLock<
    std::sync::Mutex<HashMap<(usize, String), usize>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

struct PreAuthConnectionGuard {
    key: (usize, String),
    active: bool,
}

impl PreAuthConnectionGuard {
    fn try_acquire(ctx: &Arc<ServerContext>, peer_ip: String) -> Option<Self> {
        let key = (Arc::as_ptr(ctx) as usize, peer_ip);
        let mut counts = PREAUTH_CONN_COUNTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let entry = counts.entry(key.clone()).or_insert(0);
        if *entry >= MAX_PREAUTH_CONNECTIONS_PER_IP {
            return None;
        }
        *entry += 1;
        Some(Self { key, active: true })
    }

    fn complete_auth(&mut self) {
        if self.active {
            self.active = false;
            let mut counts = PREAUTH_CONN_COUNTS
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(cnt) = counts.get_mut(&self.key) {
                *cnt = cnt.saturating_sub(1);
                if *cnt == 0 {
                    counts.remove(&self.key);
                }
            }
        }
    }
}

impl Drop for PreAuthConnectionGuard {
    fn drop(&mut self) {
        self.complete_auth();
    }
}

/// Maps a `CallReq` dispatch failure into a V24 `machoNet` `ErrorResponse` body via the
/// total [`eve_proto::ClientError::to_macho_error_body`] wire mapping (`C-5`, `#929`).
#[cfg(test)]
pub(crate) fn map_call_req_error_body(
    service: &str,
    method: &str,
    err: &eve_proto::BridgeRpcError,
) -> PyRep {
    err.to_client_error(service, method)
        .to_macho_error_body(MachoPacketType::CallReq as i64)
}

fn reply_source_address(dest: &MachoAddress) -> MachoAddress {
    match dest {
        MachoAddress::Node { .. } => dest.clone(),
        MachoAddress::Any { service, call_id } => MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: service.clone(),
            call_id: *call_id,
        },
        MachoAddress::Client {
            service, call_id, ..
        } => MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: service.clone(),
            call_id: *call_id,
        },
        MachoAddress::Broadcast { .. } => MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: None,
            call_id: 0,
        },
    }
}

/// Writes a length-prefixed (`u32` little-endian) marshaled `PyRep` frame to an async writer.
pub async fn write_framed_pyrep<W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    rep: &PyRep,
) -> Result<()> {
    write_framed_pyrep_with_timeout(
        stream,
        rep,
        std::time::Duration::from_secs(WRITE_FRAME_TIMEOUT_SECS),
    )
    .await
}

pub(crate) const SLOW_ENCODE_THRESHOLD_MS: u64 = 100;
pub(crate) const SLOW_WRITE_ALL_THRESHOLD_MS: u64 = 25;

fn encode_macho_frame_with_slow_warn(
    rep: &PyRep,
) -> std::result::Result<Vec<u8>, eve_proto::ProtoError> {
    let encode_start = std::time::Instant::now();
    let bytes = marshal::try_encode_with_limits(rep, MAX_MACHO_FRAME_LEN)?;
    let encode_ms = encode_start.elapsed().as_millis() as u64;
    if encode_ms >= SLOW_ENCODE_THRESHOLD_MS {
        warn!(
            target: "rpc.slow",
            stage = "encode",
            bytes = bytes.len(),
            ms = encode_ms,
            "Wire writer marshal::encode exceeded slow-work threshold"
        );
    }
    Ok(bytes)
}

async fn write_encoded_frame_with_timeout<W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    bytes: &[u8],
    write_timeout: std::time::Duration,
) -> Result<()> {
    let frame_len = u32::try_from(bytes.len())
        .map_err(|_| anyhow::anyhow!("machoNet frame length {} exceeds u32", bytes.len()))?;
    let write_start = std::time::Instant::now();
    tokio::time::timeout(write_timeout, async {
        stream.write_u32_le(frame_len).await?;
        stream.write_all(bytes).await?;
        stream.flush().await?;
        Ok::<(), std::io::Error>(())
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Timed out writing machoNet frame after {}ms",
            write_timeout.as_millis()
        )
    })??;
    let write_ms = write_start.elapsed().as_millis() as u64;
    if write_ms >= SLOW_WRITE_ALL_THRESHOLD_MS {
        warn!(
            target: "rpc.slow",
            stage = "write_all",
            bytes = bytes.len(),
            ms = write_ms,
            "Wire writer write_all exceeded slow-work threshold"
        );
    }
    Ok(())
}

pub(crate) async fn write_framed_pyrep_with_timeout<W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    rep: &PyRep,
    write_timeout: std::time::Duration,
) -> Result<()> {
    let bytes = encode_macho_frame_with_slow_warn(rep)
        .map_err(|e| anyhow::anyhow!("Failed to marshal machoNet frame: {e}"))?;
    write_encoded_frame_with_timeout(stream, &bytes, write_timeout).await
}

/// Materializes an [`crate::outbound_sequencer::OutFrame`] into the exact `MachoPacket` `PyRep`
/// written onto the wire (`C-2`, `#926`).
pub fn out_frame_to_wire_pyrep(
    proxy_node_id: u64,
    client_id: u64,
    user_id: Option<u64>,
    frame: &crate::outbound_sequencer::OutFrame,
) -> PyRep {
    use crate::outbound_sequencer::OutFrame;
    match frame {
        OutFrame::CallRsp { frame, .. } | OutFrame::SessionChange { frame } => frame.clone(),
        OutFrame::DoDestinyUpdate {
            updates,
            wait_for_bubble,
            ..
        } => MachoPacket::new_broadcast_notification(
            proxy_node_id,
            client_id,
            user_id,
            "DoDestinyUpdate",
            vec![updates.clone(), PyRep::Bool(*wait_for_bubble)],
        )
        .to_pyrep(),
        OutFrame::Notification { event, args } => MachoPacket::new_broadcast_notification(
            proxy_node_id,
            client_id,
            user_id,
            event,
            args.clone(),
        )
        .to_pyrep(),
        OutFrame::ServiceCall {
            service,
            method,
            args,
        } => crate::router::ServerContext::build_macho_service_call_packet(
            proxy_node_id,
            client_id,
            user_id,
            service,
            method,
            args.clone(),
        )
        .to_pyrep(),
    }
}

async fn write_out_frames<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    proxy_node_id: u64,
    client_id: u64,
    user_id: Option<u64>,
    frames: &[crate::outbound_sequencer::OutFrame],
) -> Result<()> {
    for frame in frames {
        let wire_pyrep = out_frame_to_wire_pyrep(proxy_node_id, client_id, user_id, frame);
        write_framed_pyrep(writer, &wire_pyrep).await?;
    }
    Ok(())
}

// Section 4 (#1086) helpers kept self-contained for clean merge/rebase ergonomics.
fn truncate_internal_error_message(msg: &str, max_chars: usize) -> String {
    let mut chars = msg.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
}

pub(crate) fn build_call_req_internal_error_packet(
    req_source: &MachoAddress,
    req_dest: Option<&MachoAddress>,
    user_id: Option<u64>,
    reason: &str,
) -> MachoPacket {
    let short_reason = truncate_internal_error_message(reason, 512);
    let client_err = eve_proto::ClientError::Internal(short_reason);
    let payload = client_err.to_macho_error_body(MachoPacketType::CallReq as i64);
    MachoPacket {
        packet_type: MachoPacketType::ErrorResponse,
        source: match req_dest {
            Some(dest) => reply_source_address(dest),
            None => MachoAddress::Node {
                node_id: MACHO_PROXY_NODE_ID,
                service: None,
                call_id: 0,
            },
        },
        destination: req_source.clone(),
        user_id,
        payload,
        named_payload: HashMap::new(),
    }
}

async fn write_call_req_internal_error_fallback<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    req_source: &MachoAddress,
    req_dest: Option<&MachoAddress>,
    user_id: Option<u64>,
    reason: &str,
) -> Result<()> {
    let err_pkt = build_call_req_internal_error_packet(req_source, req_dest, user_id, reason);
    write_framed_pyrep(writer, &err_pkt.to_pyrep()).await
}

fn recover_call_req_from_corrupt_payload(
    payload: &[u8],
    max_frame_bytes: usize,
    max_nodes: usize,
) -> Option<(MachoAddress, u64)> {
    marshal::decode_partial_call_header(payload, max_frame_bytes, max_nodes)
        .and_then(|rep| eve_machonet::recover_call_req_source_from_pyrep(&rep))
}

pub(crate) async fn write_out_frames_for_call_req<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    proxy_node_id: u64,
    client_id: u64,
    user_id: Option<u64>,
    req_source: &MachoAddress,
    req_dest: &MachoAddress,
    frames: &[crate::outbound_sequencer::OutFrame],
) -> Result<()> {
    let write_timeout = std::time::Duration::from_secs(WRITE_FRAME_TIMEOUT_SECS);
    let mut call_reply_delivered = false;
    for frame in frames {
        let is_call_rsp = matches!(frame, crate::outbound_sequencer::OutFrame::CallRsp { .. });
        let wire_pyrep = out_frame_to_wire_pyrep(proxy_node_id, client_id, user_id, frame);
        match encode_macho_frame_with_slow_warn(&wire_pyrep) {
            Ok(bytes) => {
                write_encoded_frame_with_timeout(writer, &bytes, write_timeout).await?;
                if is_call_rsp {
                    call_reply_delivered = true;
                }
            }
            Err(e) => {
                warn!(
                    "Failed to marshal outbound frame for CallReq call_id={}: {e}",
                    req_source.call_id()
                );
                if is_call_rsp && req_source.call_id() > 0 {
                    write_call_req_internal_error_fallback(
                        writer,
                        req_source,
                        Some(req_dest),
                        user_id,
                        &format!("Reply serialization failed: {e}"),
                    )
                    .await?;
                    call_reply_delivered = true;
                }
            }
        }
    }
    if !call_reply_delivered && req_source.call_id() > 0 {
        write_call_req_internal_error_fallback(
            writer,
            req_source,
            Some(req_dest),
            user_id,
            "Internal error: CallReq yielded no serializable reply frame",
        )
        .await?;
    }
    Ok(())
}

/// Returns the destiny ballpark observer ball ID when `session` is either undocked in space
/// (`ship_id`) or attached in Upwell "View Outside" (`structure_id`). Never observes a docked
/// ship ball while `structure_id.is_some()`, and returns `None` while docked in an NPC station.
fn ballpark_observer_id_for_session(session: &SessionState) -> Option<i64> {
    if session.character_id.is_none()
        || session.station_id.is_some()
        || session.solar_system_id.is_none()
    {
        return None;
    }
    session.structure_id.or(session.ship_id).filter(|&id| id > 0)
}

fn is_shared_structure_observer(session: &SessionState, observer_id: i64) -> bool {
    session.structure_id == Some(observer_id)
}

fn is_bootstrap_rpc(service: &str, method: &str, was_live: bool, in_space: bool) -> bool {
    matches!(
        (service, method),
        ("charUnboundMgr", "SelectCharacterID")
            | ("charUnboundMgr", "GetCharacterInfo")
            | ("character", "GetCharacterSelectionInfo")
            | ("stationSvc", "Undock")
            | ("structureDocking", "Undock")
            | ("ship", "Undock")
            | ("beyonce", "MachoBindObject")
            | ("beyonce", "GetFormations")
            | ("beyonce", "UpdateStateRequest")
    ) || (in_space && !was_live)
}

fn out_frame_kind_and_stamp(frame: &crate::outbound_sequencer::OutFrame) -> (&'static str, Option<u32>) {
    use crate::outbound_sequencer::OutFrame;
    match frame {
        OutFrame::CallRsp { .. } => ("CallRsp", None),
        OutFrame::SessionChange { .. } => ("SessionChangeNotification", None),
        OutFrame::DoDestinyUpdate { stamp, .. } => ("DoDestinyUpdate", Some(*stamp)),
        OutFrame::Notification { .. } => ("Notification", None),
        OutFrame::ServiceCall { .. } => ("ServiceCall", None),
    }
}

#[allow(clippy::too_many_arguments)]
fn record_out_frame_observability(
    ctx: &ServerContext,
    session_id: u64,
    proxy_node_id: u64,
    client_id: u64,
    user_id: Option<u64>,
    last_client_tick: Option<u64>,
    frame: &crate::outbound_sequencer::OutFrame,
    elapsed_since_request_micros: Option<u64>,
    bootstrap: bool,
    reason: &str,
) {
    let wire_pyrep = out_frame_to_wire_pyrep(proxy_node_id, client_id, user_id, frame);
    let byte_size = eve_proto::marshal::encode(&wire_pyrep).len();
    let (kind, stamp) = out_frame_kind_and_stamp(frame);
    ctx.flight_recorder.record_outbound(
        session_id,
        crate::flight_recorder::OutboundDisposition::Sent,
        kind,
        byte_size,
        stamp,
        reason,
        elapsed_since_request_micros,
        bootstrap,
    );
    if let crate::outbound_sequencer::OutFrame::DoDestinyUpdate { stamp, updates, .. } = frame {
        ctx.flight_recorder.record_destiny_update_stamps(
            session_id,
            *stamp,
            updates,
            last_client_tick,
        );
    }
}

fn record_sequencer_events(
    ctx: &ServerContext,
    session_id: u64,
    events: &[crate::outbound_sequencer::SequencerLedgerEvent],
    elapsed_since_request_micros: Option<u64>,
    bootstrap: bool,
) {
    for ev in events {
        let disposition = match ev.kind {
            crate::outbound_sequencer::SequencerLedgerEventKind::Dropped => {
                crate::flight_recorder::OutboundDisposition::Dropped
            }
            crate::outbound_sequencer::SequencerLedgerEventKind::Coalesced => {
                crate::flight_recorder::OutboundDisposition::Coalesced
            }
        };
        let packet_kind = ev.packet_kind.as_deref().unwrap_or("DoDestinyUpdate");
        ctx.flight_recorder.record_outbound(
            session_id,
            disposition,
            packet_kind,
            0,
            Some(ev.stamp),
            &ev.reason,
            elapsed_since_request_micros,
            bootstrap,
        );
    }
}

async fn emit_tick_batch_with_observability<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    writer: &mut W,
    client_id: u64,
    session: &SessionState,
    batch: crate::outbound_sequencer::DestinyBatch,
    reason: &str,
) -> Result<()> {
    for sub_batch in crate::outbound_sequencer::normalize_server_destiny_batch(wire_state, batch) {
        let (frame_opt, events) =
            crate::outbound_sequencer::sequence_tick_with_events(wire_state, sub_batch);
        ctx.flight_recorder.record_wire_state(wire_state);
        if !events.is_empty() {
            record_sequencer_events(ctx, session.session_id, &events, None, false);
        }
        if let Some(frame) = frame_opt {
            record_out_frame_observability(
                ctx,
                session.session_id,
                MACHO_PROXY_NODE_ID,
                client_id,
                session.user_id,
                wire_state.last_client_tick,
                &frame,
                None,
                false,
                reason,
            );
            write_out_frames(
                writer,
                MACHO_PROXY_NODE_ID,
                client_id,
                session.user_id,
                &[frame],
            )
            .await?;
        }
    }
    Ok(())
}

async fn emit_tick_batches_with_observability<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    writer: &mut W,
    client_id: u64,
    session: &SessionState,
    batches: Vec<crate::outbound_sequencer::DestinyBatch>,
    reason: &str,
) -> Result<()> {
    for batch in batches {
        emit_tick_batch_with_observability(ctx, wire_state, writer, client_id, session, batch, reason)
            .await?;
    }
    Ok(())
}

struct PendingDockCall {
    source: MachoAddress,
    destination: MachoAddress,
    service: String,
    bound_id: Option<u64>,
    method: String,
    call_args: Vec<PyRep>,
    call_kwargs: HashMap<String, PyRep>,
    sleep: std::pin::Pin<Box<tokio::time::Sleep>>,
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_call_req_to_wire<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire_refs: &mut InternalCallReqWireRefs<'_>,
    writer: &mut W,
    user_id_before_call: Option<u64>,
    source: &MachoAddress,
    destination: &MachoAddress,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> Result<()> {
    let client_id = wire_refs.client_id;
    let wire_out = execute_call_req_wire_inner(
        ctx,
        session,
        wire_refs,
        source,
        destination,
        service,
        bound_id,
        method,
        call_args,
        call_kwargs,
    )
    .await;
    let frame_user_id = session.user_id.or(user_id_before_call);
    write_out_frames_for_call_req(
        writer,
        MACHO_PROXY_NODE_ID,
        client_id,
        frame_user_id,
        source,
        destination,
        &wire_out.out_frames,
    )
    .await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelfDestinyWireContext {
    /// Live space ticks, sub-warp/warp RPCs, and cross-system leave-space drains.
    /// Suppresses self `TerminalPlayDestructionEffect` (`#1039`) and self stop
    /// `OnSpecialFX("effects.Warping", 0)` (`#1147`) so V24 `Warp.WarpLoop` exits cleanly once
    /// `ball.mode != DSTBALL_WARP` without `Warping.Stop()` clearing `self.shipModel` early.
    NonDock,
    /// Docking warp-stop settle / pre-dock flush (`#1051`), which preserves stop
    /// `OnSpecialFX("effects.Warping", 0)` alongside `Stop` in the atomic dock-settle batch.
    DockSettle,
}

fn destiny_entry_action<'a>(entry: &'a PyRep) -> Option<(&'a str, &'a [PyRep])> {
    let PyRep::Tuple(outer) = entry else {
        return None;
    };
    if outer.len() != 2 {
        return None;
    }
    let PyRep::Tuple(action) = &outer[1] else {
        return None;
    };
    if action.len() != 2 {
        return None;
    }
    let name = action[0].as_str()?;
    let args = match &action[1] {
        PyRep::Tuple(items) | PyRep::List(items) => items.as_slice(),
        _ => return None,
    };
    Some((name, args))
}

fn is_suppressed_self_terminal_explosion(entry: &PyRep, suppressed_ship_ids: &[i64]) -> bool {
    let Some((name, args)) = destiny_entry_action(entry) else {
        return false;
    };
    name == "TerminalPlayDestructionEffect"
        && matches!(args.first(), Some(PyRep::Integer(ball_id)) if suppressed_ship_ids.contains(ball_id))
}

fn is_suppressed_self_non_dock_warp_stop_fx(entry: &PyRep, suppressed_ship_ids: &[i64]) -> bool {
    let Some((name, args)) = destiny_entry_action(entry) else {
        return false;
    };
    name == "OnSpecialFX"
        && matches!(args.first(), Some(PyRep::Integer(ball_id)) if suppressed_ship_ids.contains(ball_id))
        && args.get(5).and_then(PyRep::as_str) == Some("effects.Warping")
        && matches!(args.get(7), Some(PyRep::Integer(0) | PyRep::Bool(false)))
}

fn filter_destiny_updates_pyrep(
    updates_pyrep: PyRep,
    predicate: impl Fn(&PyRep) -> bool,
) -> PyRep {
    match updates_pyrep {
        PyRep::List(items) => {
            if !items.iter().any(&predicate) {
                return PyRep::List(items);
            }
            PyRep::List(items.into_iter().filter(|entry| !predicate(entry)).collect())
        }
        other => other,
    }
}

/// Suppresses `TerminalPlayDestructionEffect` targeting the observer's own ship (`#1039`).
///
/// Setting `destructionEffectId` on the observer client's own ship (the active `lookAtBall`)
/// causes V24 `ShipOrbitCamera.OnBallRemoved` to activate `ExplosionCamera` during 1-arg
/// `RemoveBall` or during `OnPrimaryViewChanged -> RemoveBallpark -> Release -> RemoveBalls`
/// after a rapid ship -> pod -> station `DeathRespawn`, crashing `GetBallPosition(None)`.
#[cfg(test)]
pub(crate) fn strip_self_terminal_explosions(
    updates_pyrep: PyRep,
    suppressed_ship_ids: &[i64],
) -> PyRep {
    if suppressed_ship_ids.is_empty() {
        return updates_pyrep;
    }
    filter_destiny_updates_pyrep(updates_pyrep, |entry| {
        is_suppressed_self_terminal_explosion(entry, suppressed_ship_ids)
    })
}

/// Suppresses `OnSpecialFX("effects.Warping", start=0)` targeting the observer's own ship on
/// non-dock Destiny batches (`#1147`).
///
/// V24 `Warping.Prepare` (`Warp.py:43-48`) redirects non-ego ships to `effects.WarpOut` and only
/// spawns `WarpLoop` for ego (`shipID == session.shipid`); ego `WarpLoop` self-terminates
/// `'effects.Warping'` at `Warp.py:654` once `ball.mode != DSTBALL_WARP`. Receiving server stop
/// `OnSpecialFX("effects.Warping", 0)` while `ball.mode` is still `DSTBALL_WARP` clears
/// `self.shipModel = None` in `Warping.Stop()`, throwing `AttributeError` at `Warp.py:597`.
#[cfg(test)]
pub(crate) fn strip_self_non_dock_warp_stop_fx(
    updates_pyrep: PyRep,
    suppressed_ship_ids: &[i64],
) -> PyRep {
    if suppressed_ship_ids.is_empty() {
        return updates_pyrep;
    }
    filter_destiny_updates_pyrep(updates_pyrep, |entry| {
        is_suppressed_self_non_dock_warp_stop_fx(entry, suppressed_ship_ids)
    })
}

fn build_destiny_batch(
    ctx: &ServerContext,
    epoch: u32,
    stamp: u64,
    events: &[eve_destiny::DestinyEvent],
    updates_pyrep: PyRep,
    suppressed_ship_ids: &[i64],
    wire_ctx: SelfDestinyWireContext,
) -> Option<crate::outbound_sequencer::DestinyBatch> {
    if events.is_empty() {
        return None;
    }
    let updates_pyrep = if suppressed_ship_ids.is_empty() {
        updates_pyrep
    } else {
        let suppress_warp_stop_fx = wire_ctx == SelfDestinyWireContext::NonDock;
        filter_destiny_updates_pyrep(updates_pyrep, |entry| {
            is_suppressed_self_terminal_explosion(entry, suppressed_ship_ids)
                || (suppress_warp_stop_fx
                    && is_suppressed_self_non_dock_warp_stop_fx(entry, suppressed_ship_ids))
        })
    };
    let has_set_state = events
        .iter()
        .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. }));
    let mut norm_args = crate::router::client_compat::normalize_macho_notification_with_sde(
        "DoDestinyUpdate",
        vec![updates_pyrep, PyRep::Bool(false)],
        Some(&ctx.sde),
    );
    if matches!(norm_args.first(), Some(PyRep::List(items)) if items.is_empty()) {
        return None;
    }
    let updates = if norm_args.is_empty() {
        return None;
    } else {
        norm_args.swap_remove(0)
    };
    let wait_for_bubble = norm_args
        .first()
        .and_then(|v| match v {
            PyRep::Bool(b) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);
    let base_stamp = stamp as u32;
    let batch_stamp = if !has_set_state {
        let mut inferred: Option<u32> = None;
        let mut uniform = true;
        if let PyRep::List(items) | PyRep::Tuple(items) = &updates {
            for entry in items {
                if let Some((Some(entry_stamp), _, _)) =
                    crate::sequencer_invariants::parse_destiny_entry(entry)
                {
                    match inferred {
                        None => inferred = Some(entry_stamp),
                        Some(prev) if prev != entry_stamp => {
                            uniform = false;
                            break;
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        if uniform {
            inferred.filter(|&s| s > base_stamp).unwrap_or(base_stamp)
        } else {
            base_stamp
        }
    } else {
        base_stamp
    };
    Some(crate::outbound_sequencer::DestinyBatch {
        epoch,
        stamp: batch_stamp,
        has_set_state,
        updates,
        wait_for_bubble,
    })
}

fn build_typed_notification(
    ctx: &ServerContext,
    event: String,
    id_type: Option<&str>,
    args: Vec<PyRep>,
) -> Option<crate::outbound_sequencer::TypedNotification> {
    if crate::router::client_compat::should_suppress_macho_notification(&event, &args) {
        return None;
    }
    let norm_args = crate::router::client_compat::normalize_macho_notification_with_sde(
        &event,
        args,
        Some(&ctx.sde),
    );
    Some(crate::outbound_sequencer::TypedNotification {
        event,
        args: norm_args,
        service: id_type
            .and_then(|s| s.strip_prefix("service:"))
            .map(ToOwned::to_owned),
    })
}

/// Active solar-system tick broadcast receiver handle for a session (`#1237` / `HP-10`).
///
/// `handle_macho_client` uses [`TickRxSlot::Optional`], holding `None` whenever the session is
/// pre-auth, at character selection, or docked so no unpolled `broadcast::Receiver<TickBroadcast>`
/// pins ring slots or keeps `SystemHandle::event_tx.receiver_count() > 0` (which blocks LOD-0
/// solar-system hibernation).
///
/// External [`CallReqWireRefs`] callers (`ClientWireSession` / tests) pass [`TickRxSlot::Borrowed`],
/// which drops the solar-system receiver on detach by replacing it with a closed 1-slot channel.
enum TickRxSlot<'a> {
    Optional(&'a mut Option<tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>>),
    Borrowed(&'a mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>),
}

impl TickRxSlot<'_> {
    fn as_mut(
        &mut self,
    ) -> Option<&mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>> {
        match self {
            Self::Optional(slot) => slot.as_mut(),
            Self::Borrowed(rx) => Some(rx),
        }
    }

    fn needs_attach(&self, subscribed_sys_id: Option<u32>, target_sys_id: u32) -> bool {
        match self {
            Self::Optional(slot) => slot.is_none() || subscribed_sys_id != Some(target_sys_id),
            Self::Borrowed(_) => subscribed_sys_id != Some(target_sys_id),
        }
    }

    fn needs_detach(&self, subscribed_sys_id: Option<u32>) -> bool {
        match self {
            Self::Optional(slot) => slot.is_some() || subscribed_sys_id.is_some(),
            Self::Borrowed(_) => subscribed_sys_id.is_some(),
        }
    }

    fn attach(&mut self, ctx: &ServerContext, sys_id: u32) {
        let rx = ctx.systems.get_or_spawn(sys_id).subscribe();
        match self {
            Self::Optional(slot) => **slot = Some(rx),
            Self::Borrowed(slot) => **slot = rx,
        }
    }

    fn detach(&mut self) {
        match self {
            Self::Optional(slot) => {
                **slot = None;
            }
            Self::Borrowed(slot) => {
                let (_tx, closed_rx) = tokio::sync::broadcast::channel(1);
                **slot = closed_rx;
            }
        }
    }
}

/// Clears View Outside wire/stream/guard state when a pilot releases their last `beyonce`
/// binding while still structure-docked (`Enter Hangar`, `#1448` / `#1461`).
fn detach_structure_view_outside_on_beyonce_release(
    ctx: &ServerContext,
    session_id: u64,
    char_id: Option<i64>,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    sys_rx: &mut TickRxSlot<'_>,
    subscribed_sys_id: &mut Option<u32>,
) {
    sys_rx.detach();
    *subscribed_sys_id = None;
    wire_state.leave_space();
    ctx.systems
        .clear_explicit_update_state_guards_for_session(session_id);
    ctx.clear_pending_structure_view_outside_set_state_for_session(session_id);
    if let Some(cid) = char_id {
        ctx.clear_space_ballpark_bootstrapped(cid);
        crate::services::space_destiny::reset_update_state_rate_limit(ctx, session_id, cid);
    }
    ctx.flight_recorder.record_wire_state(wire_state);
}

struct InternalCallReqWireRefs<'a> {
    client_id: u64,
    sys_rx: TickRxSlot<'a>,
    subscribed_sys_id: &'a mut Option<u32>,
    wire_state: &'a mut crate::outbound_sequencer::SessionWireState,
    deferred_ship_id: &'a mut Option<PendingDeferredShipId>,
}

/// Borrowed per-connection wire state updated during [`execute_call_req_wire`] (`C-2`, `C-3`).
pub struct CallReqWireRefs<'a> {
    pub client_id: u64,
    pub sys_rx: &'a mut tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>,
    pub subscribed_sys_id: &'a mut Option<u32>,
    pub wire_state: &'a mut crate::outbound_sequencer::SessionWireState,
    pub deferred_ship_id: &'a mut Option<PendingDeferredShipId>,
}

/// Full wire-level outcome of executing one `CallReq` through the socket-loop handler (`C-2`, `#926`).
#[derive(Debug, Clone)]
pub struct CallReqWireOutcome {
    /// Reply `PyRep` (after `machoNet:26000` shaping) or enriched [`BridgeRpcError`].
    pub rpc_result: std::result::Result<PyRep, BridgeRpcError>,
    /// Mapped wire `ErrorResponse.payload` body when `rpc_result` is `Err`.
    pub error_wire_body: Option<PyRep>,
    /// Exact ordered outbound frames emitted by [`crate::outbound_sequencer::sequence`].
    pub out_frames: Vec<crate::outbound_sequencer::OutFrame>,
    /// Suppressed and SDE-normalized `(event, args)` notifications delivered on this call.
    pub notifications: Vec<(String, Vec<PyRep>)>,
    /// Raw per-character [`crate::router::PendingMachoNotification`] items drained from `ctx` during this call.
    pub drained_char_notifications: Vec<crate::router::PendingMachoNotification>,
}

fn session_owns_char_notifications(ctx: &ServerContext, session: &SessionState) -> bool {
    let (Some(uid), Some(cid)) = (session.user_id, session.character_id) else {
        return false;
    };
    let now_unix = ctx.now_unix_secs();
    if ctx
        .db
        .check_session_revoked(uid, session.session_id, now_unix)
        .is_some()
    {
        return false;
    }
    match ctx.db.get_online_session(uid) {
        Some(os) => os.session_id == session.session_id && os.character_id.is_none_or(|c| c == cid),
        None => true,
    }
}

fn build_call_req_reply_packet(
    req_source: &MachoAddress,
    req_dest: &MachoAddress,
    user_id: Option<u64>,
    character_id: Option<i64>,
    service: &str,
    method: &str,
    rpc_res: std::result::Result<PyRep, BridgeRpcError>,
) -> (
    std::result::Result<PyRep, BridgeRpcError>,
    Option<PyRep>,
    MachoPacket,
) {
    match rpc_res {
        Ok(val) => {
            let mut named_payload = HashMap::new();
            let maybe_substruct = match &val {
                PyRep::SubStruct(inner) => Some(inner),
                PyRep::Tuple(items) => match items.first() {
                    Some(PyRep::SubStruct(inner)) => Some(inner),
                    _ => None,
                },
                _ => None,
            };
            if let Some(inner) = maybe_substruct {
                if let PyRep::SubStream(sub) = inner.as_ref() {
                    if let PyRep::Tuple(oid_pair) = sub.as_ref() {
                        if oid_pair.len() == 2 {
                            named_payload.insert(
                                "OID+".into(),
                                PyRep::Dict(vec![(
                                    oid_pair[0].clone(),
                                    oid_pair[1].clone(),
                                )]),
                            );
                        }
                    }
                }
            }
            let pkt = MachoPacket {
                packet_type: MachoPacketType::CallRsp,
                source: reply_source_address(req_dest),
                destination: req_source.clone(),
                user_id,
                payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(val.clone()))]),
                named_payload,
            };
            (Ok(val), None, pkt)
        }
        Err(err) => {
            let mut client_err = err.to_client_error(service, method);
            if let eve_proto::ClientError::User {
                key: eve_proto::UserErrorKey::InsureShipFailedSingleContract,
                ref mut args,
            } = client_err
            {
                if !args.iter().any(|(k, _)| k == "ownerName") {
                    if let Some(cid) = character_id {
                        args.push(("ownerName".into(), PyRep::Integer(cid)));
                    }
                }
            }
            let payload = client_err.to_macho_error_body(MachoPacketType::CallReq as i64);
            let pkt = MachoPacket {
                packet_type: MachoPacketType::ErrorResponse,
                source: reply_source_address(req_dest),
                destination: req_source.clone(),
                user_id,
                payload: payload.clone(),
                named_payload: HashMap::new(),
            };
            (Err(err), Some(payload), pkt)
        }
    }
}

fn session_owns_charsel_transition(ctx: &ServerContext, session: &SessionState) -> bool {
    session_owns_char_notifications(ctx, session)
        && session.user_id.is_some_and(|uid| {
            ctx.db
                .get_online_session(uid)
                .is_some_and(|os| os.session_id == session.session_id && os.character_id.is_none())
        })
}

/// Executes a single `CallReq` through the exact production socket-loop path:
/// `ctx.dispatch_rpc(..., Some("machoNet:26000"))`, session sync + `SessionChangeNotification` enrichment,
/// `TickBroadcast`/direct `DoDestinyUpdate` synchronization, notification suppression & SDE normalization,
/// `map_call_req_error_body` error mapping, and `outbound_sequencer::sequence` framing (`C-2`, `#926`).
#[allow(clippy::too_many_arguments)]
pub async fn execute_call_req_wire(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut CallReqWireRefs<'_>,
    req_source: &MachoAddress,
    req_dest: &MachoAddress,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> CallReqWireOutcome {
    let mut inner = InternalCallReqWireRefs {
        client_id: wire.client_id,
        sys_rx: TickRxSlot::Borrowed(wire.sys_rx),
        subscribed_sys_id: wire.subscribed_sys_id,
        wire_state: wire.wire_state,
        deferred_ship_id: wire.deferred_ship_id,
    };
    execute_call_req_wire_inner(
        ctx,
        session,
        &mut inner,
        req_source,
        req_dest,
        service,
        bound_id,
        method,
        call_args,
        call_kwargs,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn execute_call_req_wire_inner(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut InternalCallReqWireRefs<'_>,
    req_source: &MachoAddress,
    req_dest: &MachoAddress,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> CallReqWireOutcome {
    let call_started_at = std::time::Instant::now();
    let was_live_before_rpc = wire.wire_state.destiny.is_live();
    let call_id = match (req_dest, req_source) {
        (
            MachoAddress::Node { call_id, .. }
            | MachoAddress::Any { call_id, .. }
            | MachoAddress::Client { call_id, .. },
            _,
        ) if *call_id != 0 => *call_id,
        (
            _,
            MachoAddress::Client { call_id, .. }
            | MachoAddress::Node { call_id, .. }
            | MachoAddress::Any { call_id, .. },
        ) => *call_id,
        _ => 0,
    };
    let mut drained_char_notifications = Vec::new();
    let mut pre_rpc_notifs = Vec::new();
    if session_owns_char_notifications(ctx, session) {
        if let Some(cid) = session.character_id {
            for notif in ctx.drain_notifications_for_char(cid) {
                drained_char_notifications.push(notif.clone());
                pre_rpc_notifs.push(notif);
            }
        }
    }
    let prev_session = session.clone();
    let owned_bound_service =
        bound_id.and_then(|bid| ctx.owned_bound_service_for_metrics(session, bid));
    let effective_service: &str = owned_bound_service.as_deref().unwrap_or(service);
    let pre_rpc_db_loc = session.character_id.and_then(|cid| {
        ctx.db.with_snapshot(|s| {
            s.characters.get(&cid).map(|c| {
                (
                    c.active_ship_id,
                    c.solar_system_id,
                    c.station_id,
                    c.structure_id,
                )
            })
        })
    });
    if effective_service == "beyonce" && method == "UpdateStateRequest" {
        let stream_live = !wire.wire_state.needs_initial_set_state();
        if !stream_live {
            if let Some(cid) = session.character_id {
                crate::services::space_destiny::reset_update_state_rate_limit(
                    ctx,
                    session.session_id,
                    cid,
                );
            }
        }
    }
    let had_beyonce_before_rpc = ctx.has_bound_service(session.session_id, "beyonce");
    let rpc_res = crate::router::dispatch::with_active_rpc_call_id(
        call_id,
        ctx.dispatch_rpc(
            session,
            service,
            bound_id,
            method,
            call_args,
            call_kwargs,
            Some("machoNet:26000"),
        ),
    )
    .await;
    let pending_structure_set_state = session.character_id.and_then(|cid| {
        crate::services::space_destiny::take_pending_structure_set_state(
            ctx,
            session.session_id,
            cid,
        )
    });
    if session_owns_char_notifications(ctx, session) {
        if let Some(cid) = session.character_id {
            for notif in ctx.drain_notifications_for_char(cid) {
                drained_char_notifications.push(notif.clone());
                if !session
                    .notifications
                    .iter()
                    .any(|(ev, a)| ev == &notif.event && a == &notif.args)
                {
                    pre_rpc_notifs.push(notif);
                }
            }
        }
    }
    let post_rpc_db_loc = session.character_id.and_then(|cid| {
        ctx.db.with_snapshot(|s| {
            s.characters.get(&cid).map(|c| {
                (
                    c.active_ship_id,
                    c.solar_system_id,
                    c.station_id,
                    c.structure_id,
                )
            })
        })
    });
    if effective_service == "timeControl"
        || post_rpc_db_loc != pre_rpc_db_loc
        || pre_rpc_notifs.iter().any(|n| n.event == "OnSessionChanged")
        || session
            .notifications
            .iter()
            .any(|(ev, _)| ev == "OnSessionChanged")
    {
        ctx.sync_session(session);
    }
    if rpc_res.is_ok()
        && had_beyonce_before_rpc
        && !ctx.has_bound_service(session.session_id, "beyonce")
        && session.character_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_some()
    {
        detach_structure_view_outside_on_beyonce_release(
            ctx,
            session.session_id,
            session.character_id,
            wire.wire_state,
            &mut wire.sys_rx,
            wire.subscribed_sys_id,
        );
    }

    let owns_char_session = session_owns_char_notifications(ctx, session);
    let prev_stream_active = !matches!(wire.wire_state.destiny, DestinyStream::Detached)
        || wire.subscribed_sys_id.is_some();
    let prev_undocked_space = prev_session.character_id.is_some()
        && prev_session.station_id.is_none()
        && prev_session.structure_id.is_none()
        && prev_session.solar_system_id.is_some();
    let prev_structure_view_outside = prev_session.character_id.is_some()
        && prev_session.station_id.is_none()
        && prev_session.structure_id.is_some()
        && prev_session.solar_system_id.is_some()
        && prev_stream_active;
    let was_in_space = prev_undocked_space || prev_structure_view_outside;
    let is_undocked_space = session.character_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_none()
        && session.solar_system_id.is_some();
    let is_structure_docked = session.character_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_some()
        && session.solar_system_id.is_some();
    let is_beyonce_ballpark_rpc = method == "UpdateStateRequest"
        || (effective_service == "beyonce"
            && (method == "MachoBindObject" || method == "GetFormations"));
    let is_structure_view_outside = is_structure_docked
        && ((prev_structure_view_outside
            && prev_session.station_id.is_none()
            && prev_session.structure_id == session.structure_id
            && prev_session.solar_system_id == session.solar_system_id)
            || (is_beyonce_ballpark_rpc && rpc_res.is_ok()));
    let is_in_space = is_undocked_space || is_structure_view_outside;
    let is_upwell_undock = rpc_res.is_ok()
        && matches!(
            (effective_service, method),
            ("ship" | "structureDocking" | "stationSvc", "Undock")
        )
        && prev_structure_view_outside
        && is_undocked_space
        && session.solar_system_id == prev_session.solar_system_id;
    let target_sys_id = is_in_space.then_some(session.solar_system_id).flatten();
    let mut cross_sys_pre_ticks = Vec::new();
    let mut cross_sys_pre_destiny = None;
    let sys_transition_needed = if let Some(target_sys) = target_sys_id {
        session.solar_system_id != prev_session.solar_system_id
            || wire.sys_rx.needs_attach(*wire.subscribed_sys_id, target_sys)
    } else {
        // When docking from space in the same solar system, keep `sys_rx` attached until the
        // pre-dock drain below flushes final pre-dock ticks, then detach immediately (`#1237`).
        session.solar_system_id != prev_session.solar_system_id
            || (!was_in_space && wire.sys_rx.needs_detach(*wire.subscribed_sys_id))
    };
    if sys_transition_needed {
        if owns_char_session
            && was_in_space
            && is_in_space
            && session.solar_system_id != prev_session.solar_system_id
            && matches!(wire.wire_state.destiny, DestinyStream::Live { .. })
        {
            if let (Some(old_sys_id), Some(old_obs_id)) = (
                prev_session.solar_system_id,
                ballpark_observer_id_for_session(&prev_session)
                    .or_else(|| ballpark_observer_id_for_session(session)),
            ) {
                if is_shared_structure_observer(&prev_session, old_obs_id) {
                    if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        let _ = drain_sys_rx_discarding_lag(sys_rx);
                    }
                } else {
                    let old_handle = ctx.systems.get_or_spawn(old_sys_id);
                    if let Ok((tick, obs_evs, updates_pyrep, watermark_seq)) =
                        old_handle.drain_observer_updates_with_seq(old_obs_id).await
                    {
                        old_handle.record_warp_stops_from_events(&obs_evs);
                        let mut ignored_post = Vec::new();
                        let (ep, live_ok) = if let Some(sys_rx) = wire.sys_rx.as_mut() {
                            drain_pending_tick_batches_for_sys_with_watermark(
                                ctx,
                                sys_rx,
                                old_sys_id,
                                &[old_obs_id],
                                wire.wire_state,
                                Some(watermark_seq),
                                &mut cross_sys_pre_ticks,
                                &mut ignored_post,
                                session.session_id,
                            )
                        } else {
                            (wire.wire_state.current_epoch(), true)
                        };
                        if live_ok {
                            cross_sys_pre_destiny = build_destiny_batch(
                                ctx,
                                ep,
                                tick,
                                &obs_evs,
                                updates_pyrep,
                                &[old_obs_id],
                                SelfDestinyWireContext::NonDock,
                            );
                        } else {
                            cross_sys_pre_ticks.clear();
                        }
                    } else if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        let _ = drain_sys_rx_discarding_lag(sys_rx);
                    }
                }
            }
        }
        if cross_sys_pre_ticks.is_empty() && cross_sys_pre_destiny.is_none() {
            wire.wire_state.leave_space();
        }
        if !is_in_space
            || (was_in_space && session.solar_system_id != prev_session.solar_system_id)
            || is_upwell_undock
        {
            ctx.systems
                .clear_explicit_update_state_guards_for_session(session.session_id);
            ctx.clear_pending_structure_view_outside_set_state_for_session(session.session_id);
        }
        if let Some(new_sys) = target_sys_id {
            wire.sys_rx.attach(ctx, new_sys);
            *wire.subscribed_sys_id = Some(new_sys);
        } else {
            wire.sys_rx.detach();
            *wire.subscribed_sys_id = None;
        }
    }
    let cross_sys_pre_detach =
        !cross_sys_pre_ticks.is_empty() || cross_sys_pre_destiny.is_some();
    let mut was_live = matches!(wire.wire_state.destiny, DestinyStream::Live { .. });
    let is_ballpark_init_rpc = (is_beyonce_ballpark_rpc && is_in_space) || is_upwell_undock;
    let structure_dock_transition =
        is_structure_docked && prev_session.structure_id != session.structure_id;
    let needs_initial = wire.wire_state.needs_initial_set_state();
    if is_ballpark_init_rpc && (needs_initial || is_upwell_undock || structure_dock_transition) {
        if is_upwell_undock {
            ctx.systems
                .clear_explicit_update_state_guards_for_session(session.session_id);
            ctx.clear_pending_structure_view_outside_set_state_for_session(session.session_id);
        }
        // Drain any pre-bind 1Hz ticks buffered in `sys_rx` between `SelectCharacterID` /
        // `View Outside` and `beyonce.MachoBindObject` / `UpdateStateRequest` / Upwell `Undock`
        // so stale ticks (e.g. tick N-1) are never flushed out-of-order after `SetState` (tick N).
        if let Some(sys_rx) = wire.sys_rx.as_mut() {
            let _ = drain_sys_rx_discarding_lag(sys_rx);
        }
        if was_live {
            wire.wire_state.enter_space(session.solar_system_id);
            was_live = false;
        } else {
            wire.wire_state.ensure_in_space(session.solar_system_id);
        }
    }

    let bootstrap_phase = crate::ops_telemetry::classify_bootstrap_phase(effective_service, method);
    let bootstrap_elapsed_ms = bootstrap_phase.map(|phase| {
        let elapsed_ms = call_started_at.elapsed().as_millis() as u64;
        crate::ops_telemetry::record_bootstrap_timing(crate::ops_telemetry::BootstrapPacketTiming {
            phase: phase.to_string(),
            service: effective_service.to_string(),
            method: method.to_string(),
            session_id: session.session_id,
            character_id: session.character_id,
            elapsed_since_request_ms: elapsed_ms,
            timestamp_unix: ctx.now_unix(),
        });
        elapsed_ms
    });

    let (rpc_result, error_wire_body, mut rsp_pkt) = build_call_req_reply_packet(
        req_source,
        req_dest,
        session.user_id,
        session.character_id,
        effective_service,
        method,
        rpc_res,
    );
    if let Some(elapsed_ms) = bootstrap_elapsed_ms {
        rsp_pkt.named_payload.insert(
            "elapsed_since_request_ms".into(),
            PyRep::Integer(elapsed_ms as i64),
        );
    }

    let docking_transition = was_in_space && !is_in_space;
    let death_respawn_docking =
        docking_transition && prev_undocked_space && session.ship_id != prev_session.ship_id;

    let mut pre_dock_destiny = None;
    let mut pre_dock_ticks = Vec::new();
    if owns_char_session
        && !matches!(wire.wire_state.destiny, DestinyStream::Detached)
        && docking_transition
    {
        if death_respawn_docking {
            if let Some(sys_rx) = wire.sys_rx.as_mut() {
                let _ = drain_sys_rx_discarding_lag(sys_rx);
            }
            if let Some(old_sys_id) = prev_session.solar_system_id.or(session.solar_system_id) {
                let sys_handle = ctx.systems.get_or_spawn(old_sys_id);
                if let Some(old_obs_id) = ballpark_observer_id_for_session(&prev_session) {
                    if !is_shared_structure_observer(&prev_session, old_obs_id) {
                        let _ = sys_handle.drain_observer_updates(old_obs_id).await;
                    }
                }
            }
        } else if let (Some(sys_id), Some(obs_id)) = (
            session.solar_system_id.or(prev_session.solar_system_id),
            ballpark_observer_id_for_session(&prev_session)
                .or_else(|| ballpark_observer_id_for_session(session)),
        ) {
            if is_shared_structure_observer(&prev_session, obs_id) {
                if matches!(wire.wire_state.destiny, DestinyStream::Live { .. }) {
                    if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        let mut ignored_post = Vec::new();
                        let (_ep, live_ok) = drain_pending_tick_batches_for_sys_with_watermark(
                            ctx,
                            sys_rx,
                            sys_id,
                            &[obs_id],
                            wire.wire_state,
                            None,
                            &mut pre_dock_ticks,
                            &mut ignored_post,
                            session.session_id,
                        );
                        if !live_ok {
                            pre_dock_ticks.clear();
                        }
                    }
                } else if let Some(sys_rx) = wire.sys_rx.as_mut() {
                    let _ = drain_sys_rx_discarding_lag(sys_rx);
                }
            } else {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok((tick, obs_evs, updates_pyrep, watermark_seq)) =
                    sys_handle.drain_observer_updates_with_seq(obs_id).await
                {
                    let mut ignored_post = Vec::new();
                    let (ep, live_ok) =
                        if matches!(wire.wire_state.destiny, DestinyStream::Live { .. }) {
                            if let Some(sys_rx) = wire.sys_rx.as_mut() {
                                drain_pending_tick_batches_for_sys_with_watermark(
                                    ctx,
                                    sys_rx,
                                    sys_id,
                                    &[obs_id],
                                    wire.wire_state,
                                    Some(watermark_seq),
                                    &mut pre_dock_ticks,
                                    &mut ignored_post,
                                    session.session_id,
                                )
                            } else {
                                (wire.wire_state.current_epoch(), true)
                            }
                        } else {
                            if let Some(sys_rx) = wire.sys_rx.as_mut() {
                                let _ = drain_sys_rx_discarding_lag(sys_rx);
                            }
                            (wire.wire_state.current_epoch(), true)
                        };
                    if live_ok {
                        pre_dock_destiny = build_destiny_batch(
                            ctx,
                            ep,
                            tick,
                            &obs_evs,
                            updates_pyrep,
                            &[obs_id],
                            SelfDestinyWireContext::DockSettle,
                        );
                    } else {
                        pre_dock_ticks.clear();
                    }
                }
            }
        }
    }
    if !is_in_space && wire.sys_rx.needs_detach(*wire.subscribed_sys_id) {
        wire.sys_rx.detach();
        *wire.subscribed_sys_id = None;
    }

    if owns_char_session && docking_transition {
        if let Some(sys_id) = prev_session.solar_system_id.or(session.solar_system_id) {
            let _ = ctx
                .systems
                .get_or_spawn(sys_id)
                .flush_peer_observer_updates(
                    ballpark_observer_id_for_session(&prev_session)
                        .or_else(|| ballpark_observer_id_for_session(session)),
                )
                .await;
        }
    } else if owns_char_session
        && was_in_space
        && prev_session.solar_system_id != session.solar_system_id
    {
        if let Some(old_sys_id) = prev_session.solar_system_id {
            let _ = ctx
                .systems
                .get_or_spawn(old_sys_id)
                .flush_peer_observer_updates(ballpark_observer_id_for_session(&prev_session))
                .await;
        }
    }

    let is_dock_hangar_flush = rpc_result.is_ok()
        && is_dock_hangar_flush_rpc(
            ctx,
            service,
            bound_id,
            method,
            call_args,
            *wire.deferred_ship_id,
        );
    let (session_change_frame, deferred_flush_frame) =
        resolve_deferred_session_change_frame_with_elapsed(
            ctx,
            wire.client_id,
            &prev_session,
            session,
            wire.deferred_ship_id,
            is_dock_hangar_flush,
            bootstrap_elapsed_ms,
        );

    let mut pending_tick_batches = if docking_transition {
        pre_dock_ticks
    } else {
        cross_sys_pre_ticks
    };
    let mut post_drain_tick_batches = Vec::new();
    let mut post_rpc_direct_batch = None;
    let mut effective_was_live = was_live;
    let post_rpc_owns_char = session_owns_char_notifications(ctx, session);
    let post_rpc_charsel_pending = session_owns_charsel_transition(ctx, session);
    if post_rpc_owns_char
        && !post_rpc_charsel_pending
        && !matches!(wire.wire_state.destiny, DestinyStream::Detached)
        && !docking_transition
        && !cross_sys_pre_detach
        && (was_in_space || is_in_space)
    {
        if let (Some(sys_id), Some(observer_id)) = (
            session.solar_system_id.or(prev_session.solar_system_id),
            ballpark_observer_id_for_session(session)
                .or_else(|| ballpark_observer_id_for_session(&prev_session)),
        ) {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            if is_shared_structure_observer(session, observer_id)
                || is_shared_structure_observer(&prev_session, observer_id)
            {
                let lookup_ids = std::slice::from_ref(&observer_id);
                let force_structure_refresh = pending_structure_set_state == Some(observer_id);
                if effective_was_live && !force_structure_refresh {
                    if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        let (_ep, next_live) = drain_pending_tick_batches_for_sys_with_watermark(
                            ctx,
                            sys_rx,
                            sys_id,
                            lookup_ids,
                            wire.wire_state,
                            None,
                            &mut pending_tick_batches,
                            &mut post_drain_tick_batches,
                            session.session_id,
                        );
                        effective_was_live = next_live;
                    }
                }
                if !effective_was_live || force_structure_refresh {
                    if force_structure_refresh {
                        pending_tick_batches.clear();
                    }
                    let allow_structure_resync = !is_structure_docked
                        || session.character_id.is_some_and(|cid| {
                            ctx.is_space_ballpark_bootstrapped(session.session_id, cid, sys_id)
                        });
                    post_drain_tick_batches.clear();
                    if !allow_structure_resync {
                        wire.wire_state.leave_space();
                        wire.sys_rx.detach();
                        *wire.subscribed_sys_id = None;
                    } else if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        let ep = wire.wire_state.current_epoch();
                        if let Some(resynced) = resync_structure_set_state_with_sys_rx(
                            ctx,
                            &sys_handle,
                            sys_rx,
                            sys_id,
                            observer_id,
                            lookup_ids,
                            wire.wire_state,
                        )
                        .await
                        {
                            post_drain_tick_batches = resynced.post_batches;
                            post_rpc_direct_batch = build_destiny_batch(
                                ctx,
                                ep,
                                resynced.tick,
                                &resynced.events,
                                resynced.updates_pyrep,
                                lookup_ids,
                                SelfDestinyWireContext::NonDock,
                            );
                        }
                    }
                }
            } else if let Ok((mut tick, mut obs_evs, mut updates_pyrep, watermark_seq)) =
                // Drain observer events atomically with the system's broadcast sequence watermark
                // FIRST, then drain `sys_rx` splitting `broadcast_seq <= watermark_seq` before the
                // direct drain and `broadcast_seq > watermark_seq` after it (`GH #1172`).
                sys_handle.drain_observer_updates_with_seq(observer_id).await
            {
                sys_handle.record_warp_stops_from_events(&obs_evs);
                let has_set_state = obs_evs
                    .iter()
                    .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. }));
                let has_incremental_ship_handoff = obs_evs.iter().any(|ev| {
                    matches!(
                        ev,
                        eve_destiny::DestinyEvent::AddBalls2 { .. }
                            | eve_destiny::DestinyEvent::RemoveBall { .. }
                    )
                });
                let ship_changed_in_space = prev_undocked_space
                    && is_undocked_space
                    && session.ship_id != prev_session.ship_id;
                let same_system_live_ship_swap = ship_changed_in_space
                    && effective_was_live
                    && prev_session.solar_system_id == session.solar_system_id;
                let suppressed_ids = [observer_id, prev_session.ship_id.unwrap_or(observer_id)];
                let lookup_ids: &[i64] = if ship_changed_in_space {
                    &suppressed_ids
                } else {
                    std::slice::from_ref(&observer_id)
                };

                let mut ep = wire.wire_state.current_epoch();
                let mut post_lagged = false;
                if effective_was_live {
                    if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        let (next_ep, next_live) = drain_pending_tick_batches_for_sys_with_watermark(
                            ctx,
                            sys_rx,
                            sys_id,
                            lookup_ids,
                            wire.wire_state,
                            Some(watermark_seq),
                            &mut pending_tick_batches,
                            &mut post_drain_tick_batches,
                            session.session_id,
                        );
                        ep = next_ep;
                        effective_was_live = next_live;
                        if !next_live && has_set_state {
                            post_lagged = true;
                        }
                    }
                } else if has_set_state {
                    if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        post_lagged = drain_sys_rx_discarding_up_to_seq(
                            ctx,
                            sys_rx,
                            sys_id,
                            lookup_ids,
                            ep,
                            watermark_seq,
                            &mut post_drain_tick_batches,
                        );
                    }
                }

                let mut blocked_invalid_structure_view = false;
                if ((!effective_was_live
                    || (ship_changed_in_space
                        && !same_system_live_ship_swap
                        && !has_incremental_ship_handoff))
                    && !has_set_state)
                    || post_lagged
                {
                    // Either this is a ballpark-init RPC, an in-space ship switch
                    // (e.g. pod ejection `#970`), a `Lagged` resync (`ORD-5`), or a
                    // concurrent 1Hz tick drained `SetState` between `MachoBindObject` and
                    // this post-bind drain (`#845`): synthesize `SetState` so the stream
                    // transitions cleanly into `Live`.
                    // Do not synthesize a View Outside SetState if `space_destiny` rejected the
                    // structure (not bootstrapped in DB / missing or destroyed structure).
                    let allow_structure_resync = !is_structure_docked
                        || session.character_id.is_some_and(|cid| {
                            ctx.is_space_ballpark_bootstrapped(session.session_id, cid, sys_id)
                        });
                    post_drain_tick_batches.clear();
                    if !allow_structure_resync {
                        blocked_invalid_structure_view = true;
                        obs_evs.clear();
                        updates_pyrep = PyRep::None;
                        effective_was_live = false;
                    } else if let Some(sys_rx) = wire.sys_rx.as_mut() {
                        if let Some(resynced) = resync_set_state_with_sys_rx(
                            ctx,
                            &sys_handle,
                            sys_rx,
                            sys_id,
                            observer_id,
                            lookup_ids,
                            wire.wire_state,
                        )
                        .await
                        {
                            tick = resynced.tick;
                            obs_evs = resynced.events;
                            updates_pyrep = resynced.updates_pyrep;
                            post_drain_tick_batches = resynced.post_batches;
                        } else {
                            obs_evs.clear();
                            updates_pyrep = PyRep::None;
                            effective_was_live = false;
                        }
                    } else {
                        obs_evs.clear();
                        updates_pyrep = PyRep::None;
                        effective_was_live = false;
                    }
                }
                if blocked_invalid_structure_view {
                    wire.wire_state.leave_space();
                    wire.sys_rx.detach();
                    *wire.subscribed_sys_id = None;
                } else if !obs_evs.is_empty() {
                    post_rpc_direct_batch = build_destiny_batch(
                        ctx,
                        ep,
                        tick,
                        &obs_evs,
                        updates_pyrep,
                        lookup_ids,
                        SelfDestinyWireContext::NonDock,
                    );
                }
            }
        }
    }
    if post_rpc_owns_char && !docking_transition && (was_in_space || is_in_space) {
        if let Some(sys_id) = session.solar_system_id.or(prev_session.solar_system_id) {
            let _ = ctx
                .systems
                .get_or_spawn(sys_id)
                .flush_peer_observer_updates(
                    ballpark_observer_id_for_session(session)
                        .or_else(|| ballpark_observer_id_for_session(&prev_session)),
                )
                .await;
        }
    }
    if !is_in_space && !docking_transition {
        wire.wire_state.leave_space();
    }
    if !is_in_space {
        ctx.systems
            .clear_explicit_update_state_guards_for_session(session.session_id);
        ctx.clear_pending_structure_view_outside_set_state_for_session(session.session_id);
    }
    if (!is_in_space || post_rpc_charsel_pending)
        && wire.sys_rx.needs_detach(*wire.subscribed_sys_id)
    {
        wire.sys_rx.detach();
        *wire.subscribed_sys_id = None;
    }

    if session_owns_char_notifications(ctx, session) {
        if let Some(cid) = session.character_id {
            for notif in ctx.drain_notifications_for_char(cid) {
                drained_char_notifications.push(notif.clone());
                if !session
                    .notifications
                    .iter()
                    .any(|(ev, a)| ev == &notif.event && a == &notif.args)
                {
                    pre_rpc_notifs.push(notif);
                }
            }
        }
    }
    let sess_change_sent = session_change_frame.is_some();
    let mut typed_notifications =
        Vec::with_capacity(pre_rpc_notifs.len() + session.notifications.len());
    for notif in pre_rpc_notifs {
        if sess_change_sent && notif.event == "OnSessionChanged" {
            continue;
        }
        if let Some(tn) = build_typed_notification(ctx, notif.event, Some(&notif.id_type), notif.args) {
            typed_notifications.push(tn);
        }
    }
    for (event, args) in session.drain_notifications() {
        if sess_change_sent && event == "OnSessionChanged" {
            continue;
        }
        if let Some(tn) = build_typed_notification(ctx, event, None, args) {
            typed_notifications.push(tn);
        }
    }
    let notifications = typed_notifications
        .iter()
        .map(|tn| (tn.event.clone(), tn.args.clone()))
        .collect();

    let destiny_drain = if docking_transition {
        DestinyDrain::Detach {
            pre_detach: pre_dock_destiny,
        }
    } else if cross_sys_pre_detach {
        DestinyDrain::Detach {
            pre_detach: cross_sys_pre_destiny,
        }
    } else if let Some(batch) = post_rpc_direct_batch {
        if !effective_was_live && batch.has_set_state {
            DestinyDrain::Attach {
                epoch: batch.epoch,
                set_state: batch,
            }
        } else {
            DestinyDrain::Updates(batch)
        }
    } else {
        DestinyDrain::None
    };

    let was_docked = prev_session.station_id.or(prev_session.structure_id).is_some();
    let is_docked = session.station_id.or(session.structure_id).is_some();
    let item_change_before_session_change = was_docked
        && is_docked
        && effective_service == "ship"
        && matches!(
            method,
            "Board" | "BoardStoredShip" | "LeaveShip" | "Eject" | "AssembleShip"
        );
    let sequence_outcome = crate::outbound_sequencer::sequence_server_rpc_with_events(
        wire.wire_state,
        RpcCompletion {
            call_id: 0,
            response_frame: rsp_pkt.to_pyrep(),
            session_change_frame,
            pending_tick_batches,
            destiny_drain,
            notifications: typed_notifications,
            item_change_before_session_change,
        },
    );
    let mut seq_events = sequence_outcome.events;
    let mut out_frames = sequence_outcome.frames;
    if matches!(wire.wire_state.destiny, DestinyStream::Live { .. }) {
        for post_batch in post_drain_tick_batches {
            for sub_batch in
                crate::outbound_sequencer::normalize_server_destiny_batch(wire.wire_state, post_batch)
            {
                let (frame_opt, mut evs) =
                    crate::outbound_sequencer::sequence_tick_with_events(wire.wire_state, sub_batch);
                seq_events.append(&mut evs);
                if let Some(frame) = frame_opt {
                    out_frames.push(frame);
                }
            }
        }
    }
    ctx.flight_recorder.record_wire_state(wire.wire_state);
    if let Some(flush_frame) = deferred_flush_frame {
        out_frames.push(crate::outbound_sequencer::OutFrame::SessionChange { frame: flush_frame });
    }
    let req_elapsed_micros = call_started_at.elapsed().as_micros().min(u64::MAX as u128) as u64;
    let bootstrap = is_bootstrap_rpc(
        effective_service,
        method,
        was_live_before_rpc,
        session.solar_system_id.is_some() && session.station_id.is_none() && session.structure_id.is_none(),
    );
    let reason_tag = format!("{effective_service}.{method}");
    record_sequencer_events(
        ctx,
        session.session_id,
        &seq_events,
        Some(req_elapsed_micros),
        bootstrap,
    );
    for frame in &out_frames {
        record_out_frame_observability(
            ctx,
            session.session_id,
            MACHO_PROXY_NODE_ID,
            wire.client_id,
            session.user_id,
            wire.wire_state.last_client_tick,
            frame,
            Some(req_elapsed_micros),
            bootstrap,
            &reason_tag,
        );
    }
    if bootstrap {
        tracing::debug!(
            target: "packet_perf",
            session_id = session.session_id,
            service = %effective_service,
            method = %method,
            elapsed_micros = req_elapsed_micros,
            out_frame_count = out_frames.len(),
            "[PacketPerf] bootstrap outbound annotations"
        );
    }

    CallReqWireOutcome {
        rpc_result,
        error_wire_body,
        out_frames,
        notifications,
        drained_char_notifications,
    }
}

/// Reads and decodes a single length-prefixed (`u32` little-endian) marshaled `PyRep` frame from an async reader.
pub async fn read_framed_pyrep<R: AsyncReadExt + Unpin>(stream: &mut R) -> Result<Option<PyRep>> {
    let frame_len = match tokio::time::timeout(
        std::time::Duration::from_secs(60),
        stream.read_u32_le(),
    )
    .await
    {
        Ok(Ok(len)) => len as usize,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => anyhow::bail!("Timed out waiting for machoNet frame header after 60s"),
    };
    if frame_len == 0 || frame_len > MAX_MACHO_FRAME_LEN {
        anyhow::bail!("Invalid machoNet frame length: {frame_len} bytes");
    }
    let mut payload = vec![0u8; frame_len];
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        stream.read_exact(&mut payload),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Timed out waiting for machoNet frame body after 60s"))??;
    let rep = marshal::decode(&payload)?;
    Ok(Some(rep))
}

/// Bounded read-only grace window after SafeLogoff clears `session.character_id = None` so V24
/// `GameUI.DoCharacterLogoff` (`ProcessSessionReset` / `OnSessionReset`) cleanup/refresh RPCs
/// (`skillMgr2.GetMySkillHandler`, read-only `skillMgr2` getters, `account.GetCashBalance(0)`,
/// `accessGroupBookmarkMgr.GetMyActiveBookmarks`) succeed without spurious `CharacterNotSelected`
/// errors while the connection remains at character selection (`#1360`, `#1386`).
const SAFE_LOGOFF_TEARDOWN_GRACE_SECS: u64 = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SafeLogoffTeardownGrace {
    char_id: i64,
    deadline: tokio::time::Instant,
    temp_bindings_created: bool,
}

fn clear_safe_logoff_teardown_grace(
    ctx: &ServerContext,
    session_id: u64,
    grace: &mut Option<SafeLogoffTeardownGrace>,
) {
    if let Some(prev) = grace.take() {
        if prev.temp_bindings_created {
            if let Ok(mut bound) = ctx.bound_objects.write() {
                bound.retain(|_, e| {
                    !(e.session_id == session_id
                        && e.character_id == prev.char_id
                        && e.service == "skillMgr2")
                });
            }
        }
    }
}

fn is_personal_cash_balance_args(
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> bool {
    let parse_corp_flag = |rep: &PyRep| -> bool {
        match rep {
            PyRep::Bool(b) => *b,
            PyRep::Integer(n) => *n != 0,
            PyRep::Real(r) => *r != 0.0,
            PyRep::String(s) | PyRep::Token(s) => s.starts_with("cash"),
            PyRep::Tuple(items) | PyRep::List(items) => items.first().is_some_and(|v| {
                v.as_bool() == Some(true)
                    || v.as_i64().unwrap_or(0) != 0
                    || v.as_str().is_some_and(|s| s.starts_with("cash"))
            }),
            _ => false,
        }
    };
    let is_corp = call_args
        .first()
        .map(&parse_corp_flag)
        .or_else(|| call_kwargs.get("isCorpWallet").map(&parse_corp_flag))
        .or_else(|| {
            (call_args.is_empty()
                && call_kwargs
                    .get("accountKey")
                    .and_then(|v| v.as_i64())
                    .is_some_and(|k| (1000..=1006).contains(&k)))
            .then_some(true)
        })
        .unwrap_or(false);
    !is_corp
}

#[allow(clippy::too_many_arguments)]
fn is_safe_logoff_teardown_readonly_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    grace_char_id: i64,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> Option<bool> {
    if let Some(bid) = bound_id {
        let is_grace_skill_mgr = ctx.bound_objects.read().ok().is_some_and(|m| {
            m.get(&bid).is_some_and(|e| {
                e.session_id == session.session_id
                    && Some(e.user_id) == session.user_id
                    && e.character_id == grace_char_id
                    && e.service == "skillMgr2"
            })
        });
        if is_grace_skill_mgr
            && matches!(
                method,
                "GetSkills"
                    | "GetSkillQueue"
                    | "GetSkillQueueAndFreePoints"
                    | "GetEndOfTraining"
                    | "GetAttributes"
                    | "GetFreeSkillPoints"
                    | "GetImplants"
                    | "GetBoosters"
                    | "GetSkillHistory"
                    | "GetRespecInfo"
                    | "GetCharacterAttributeModifiers"
                    | "GetSkillRequirements"
            )
        {
            return Some(false);
        }
        return None;
    }
    match (service, method) {
        ("skillMgr2", "GetMySkillHandler") => Some(true),
        (
            "skillMgr2",
            "GetSkills"
            | "GetSkillQueue"
            | "GetSkillQueueAndFreePoints"
            | "GetEndOfTraining"
            | "GetAttributes"
            | "GetFreeSkillPoints"
            | "GetImplants"
            | "GetBoosters"
            | "GetSkillHistory"
            | "GetRespecInfo"
            | "GetCharacterAttributeModifiers"
            | "GetSkillRequirements",
        ) => Some(false),
        ("account", "GetCashBalance") if is_personal_cash_balance_args(call_args, call_kwargs) => {
            Some(false)
        }
        ("accessGroupBookmarkMgr", "GetMyActiveBookmarks")
        | ("bookmark", "GetMyActiveBookmarks" | "GetBookmarks") => Some(false),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve_safe_logoff_teardown_grace(
    ctx: &ServerContext,
    session: &SessionState,
    grace: &mut Option<SafeLogoffTeardownGrace>,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> Option<(i64, bool)> {
    let g = (*grace)?;
    if session.character_id.is_some() || tokio::time::Instant::now() > g.deadline {
        clear_safe_logoff_teardown_grace(ctx, session.session_id, grace);
        return None;
    }
    let Some(uid) = session.user_id else {
        clear_safe_logoff_teardown_grace(ctx, session.session_id, grace);
        return None;
    };
    let now_unix = ctx.now_unix_secs();
    if ctx
        .db
        .check_session_revoked(uid, session.session_id, now_unix)
        .is_some()
    {
        clear_safe_logoff_teardown_grace(ctx, session.session_id, grace);
        return None;
    }
    let owns_charsel_lock = ctx
        .db
        .get_online_session(uid)
        .is_some_and(|os| os.session_id == session.session_id && os.character_id.is_none());
    let owns_char = ctx
        .db
        .with_snapshot(|s| s.characters.get(&g.char_id).is_some_and(|c| c.user_id == uid));
    if !owns_charsel_lock || !owns_char {
        clear_safe_logoff_teardown_grace(ctx, session.session_id, grace);
        return None;
    }
    let creates_temp_binding = is_safe_logoff_teardown_readonly_rpc(
        ctx,
        session,
        g.char_id,
        service,
        bound_id,
        method,
        call_args,
        call_kwargs,
    )?;
    Some((g.char_id, creates_temp_binding))
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_safe_logoff_teardown_grace_rpc<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    session: &mut SessionState,
    grace_char_id: i64,
    client_id: u64,
    writer: &mut W,
    source: &MachoAddress,
    destination: &MachoAddress,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    call_kwargs: &HashMap<String, PyRep>,
) -> Result<bool> {
    let call_id = match (destination, source) {
        (
            MachoAddress::Node { call_id, .. }
            | MachoAddress::Any { call_id, .. }
            | MachoAddress::Client { call_id, .. },
            _,
        ) if *call_id != 0 => *call_id,
        (
            _,
            MachoAddress::Client { call_id, .. }
            | MachoAddress::Node { call_id, .. }
            | MachoAddress::Any { call_id, .. },
        ) => *call_id,
        _ => 0,
    };
    let user_id_before = session.user_id;
    let mut grace_session = session.clone();
    grace_session.character_id = Some(grace_char_id);
    let rpc_res = crate::router::dispatch::with_active_rpc_call_id(
        call_id,
        crate::router::dispatch::with_quasar_session_change_suppressed(ctx.dispatch_rpc(
            &mut grace_session,
            service,
            bound_id,
            method,
            call_args,
            call_kwargs,
            Some("machoNet:26000"),
        )),
    )
    .await;
    if grace_session.user_id.is_none() && session.user_id.is_some() {
        session.user_id = None;
        session.role = 0;
    }
    let succeeded = rpc_res.is_ok();
    let (_rpc_result, _err_body, rsp_pkt) = build_call_req_reply_packet(
        source,
        destination,
        session.user_id.or(user_id_before),
        Some(grace_char_id),
        service,
        method,
        rpc_res,
    );
    let out_frame = crate::outbound_sequencer::OutFrame::CallRsp {
        call_id: 0,
        frame: rsp_pkt.to_pyrep(),
    };
    write_out_frames_for_call_req(
        writer,
        MACHO_PROXY_NODE_ID,
        client_id,
        session.user_id.or(user_id_before),
        source,
        destination,
        &[out_frame],
    )
    .await?;
    Ok(succeeded)
}

struct SafeLogoffWireContext<'a> {
    client_id: u64,
    sys_rx: &'a mut Option<tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>>,
    notify_rx: &'a mut tokio::sync::broadcast::Receiver<(
        i64,
        crate::router::PendingMachoNotification,
    )>,
    subscribed_sys_id: &'a mut Option<u32>,
    wire_state: &'a mut crate::outbound_sequencer::SessionWireState,
    deferred_ship_id: &'a mut Option<PendingDeferredShipId>,
    pending_dock: &'a mut Option<PendingDockCall>,
    safe_logoff_teardown: &'a mut Option<SafeLogoffTeardownGrace>,
}

async fn emit_typed_notifications<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    wire_state: &mut crate::outbound_sequencer::SessionWireState,
    writer: &mut W,
    client_id: u64,
    session: &SessionState,
    pending_notifs: Vec<crate::outbound_sequencer::TypedNotification>,
) -> Result<()> {
    for typed_notif in pending_notifs {
        if !session_owns_char_notifications(ctx, session) {
            break;
        }
        if let Some(drop_ev) = wire_state.gate_notification(&typed_notif.event, &typed_notif.args)
        {
            record_sequencer_events(ctx, session.session_id, &[drop_ev], None, false);
            continue;
        }
        let frame = if let Some(service) = typed_notif.service {
            crate::outbound_sequencer::OutFrame::ServiceCall {
                service,
                method: typed_notif.event,
                args: typed_notif.args,
            }
        } else {
            crate::outbound_sequencer::OutFrame::Notification {
                event: typed_notif.event,
                args: typed_notif.args,
            }
        };
        let wire_pyrep =
            out_frame_to_wire_pyrep(MACHO_PROXY_NODE_ID, client_id, session.user_id, &frame);
        write_framed_pyrep(writer, &wire_pyrep).await?;
    }
    Ok(())
}

/// Flushes pending safe-logoff notifications (including `OnSafeLogoffActivated` while the
/// client still has `session.charid` non-None, matching V24 `GameUI.OnSafeLogoffActivated`)
/// and transitions the connection cleanly back to character selection without `GPSTransportClosed` (#1360).
async fn handle_safe_logoff_to_charsel<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut SafeLogoffWireContext<'_>,
    writer: &mut W,
    reason: &str,
) -> Result<()> {
    let Some(cid) = session.character_id else {
        return Ok(());
    };
    if let Ok(target_cid) = reason.trim().parse::<i64>() {
        if target_cid != cid {
            return Ok(());
        }
    }
    // Guard against a late/stale SafeLogoffCharsel signal firing after a new character was
    // already selected or after another session acquired or revoked the account's online lock.
    if !session_owns_charsel_transition(ctx, session) {
        return Ok(());
    }

    let pending_notifs: Vec<_> = if session_owns_charsel_transition(ctx, session) {
        ctx.drain_notifications_for_char(cid)
            .into_iter()
            .filter(|notif| notif.event != "OnSessionChanged")
            .filter_map(|notif| {
                build_typed_notification(ctx, notif.event, Some(&notif.id_type), notif.args)
            })
            .collect()
    } else {
        return Ok(());
    };
    if !pending_notifs.is_empty() {
        emit_typed_notifications(
            ctx,
            wire.wire_state,
            writer,
            wire.client_id,
            session,
            pending_notifs,
        )
        .await?;
    }
    if !session_owns_charsel_transition(ctx, session) {
        return Ok(());
    }
    ctx.clear_notifications_for_char(cid);
    let _ = session.drain_notifications();

    wire.wire_state.leave_space();
    ctx.systems
        .clear_explicit_update_state_guards_for_session(session.session_id);
    ctx.clear_pending_structure_view_outside_set_state_for_session(session.session_id);
    // Drop the solar-system tick broadcast receiver immediately so `event_tx.receiver_count()`
    // decrements to 0 and LOD-0 solar-system hibernation is not blocked (`#1237` / `HP-10`).
    *wire.sys_rx = None;
    while !matches!(
        wire.notify_rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty
            | tokio::sync::broadcast::error::TryRecvError::Closed)
    ) {}
    *wire.subscribed_sys_id = None;
    *wire.deferred_ship_id = None;
    *wire.pending_dock = None;

    session.character_id = None;
    session.corporation_id = None;
    session.alliance_id = None;
    session.war_faction_id = None;
    session.corp_role = 0;
    session.corp_account_key = 1000;
    session.fleet_id = None;
    crate::router::clear_session_character_state(session);

    ctx.release_session_bindings(session.session_id);
    clear_safe_logoff_teardown_grace(ctx, session.session_id, wire.safe_logoff_teardown);
    *wire.safe_logoff_teardown = Some(SafeLogoffTeardownGrace {
        char_id: cid,
        deadline: tokio::time::Instant::now()
            + std::time::Duration::from_secs(SAFE_LOGOFF_TEARDOWN_GRACE_SECS),
        temp_bindings_created: false,
    });
    ctx.quasar_gateway
        .on_session_changed(session, ctx.now_unix());
    ctx.flight_recorder.record_wire_state(wire.wire_state);
    Ok(())
}

/// Processes a single control signal received on `close_rx`.
/// Returns `Ok(true)` if the connection stays open at character selection, or `Ok(false)`
/// if `GPSTransportClosed` was sent and the connection loop should exit.
async fn handle_session_closer_event<W: AsyncWriteExt + Unpin>(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut SafeLogoffWireContext<'_>,
    writer: &mut W,
    reason_code: String,
    reason: String,
) -> Result<bool> {
    if reason_code == "SafeLogoffCharsel" {
        handle_safe_logoff_to_charsel(ctx, session, wire, writer, &reason).await?;
        Ok(true)
    } else {
        clear_safe_logoff_teardown_grace(ctx, session.session_id, wire.safe_logoff_teardown);
        *wire.sys_rx = None;
        *wire.subscribed_sys_id = None;
        if reason_code == "SafeLogoff" {
            let owned_by_other = session.user_id.is_some_and(|uid| {
                ctx.db
                    .get_online_session(uid)
                    .is_some_and(|os| os.session_id != session.session_id)
            });
            if owned_by_other {
                session.character_id = None;
            }
        }
        let closed = GPSTransportClosed::new(reason, reason_code);
        let _ = write_framed_pyrep(writer, &closed.to_pyrep()).await;
        Ok(false)
    }
}

/// Handles a single native `machoNet` TCP connection through the 4-step V24.01 `GPSTransport`
/// handshake state machine and subsequent authenticated `MachoPacket` RPC dispatch (`P-6`).
pub async fn handle_macho_client(mut stream: TcpStream, ctx: Arc<ServerContext>) -> Result<()> {
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let client_id = 1_000_000_000_000_u64.saturating_add(session_id);
    let peer_addr = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "127.0.0.1:0".to_string());
    let peer_ip = stream
        .peer_addr()
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string());
    let local_ip = stream.local_addr().ok().map(|a| a.ip().to_string());

    let Some(mut preauth_guard) = PreAuthConnectionGuard::try_acquire(&ctx, peer_ip.clone()) else {
        anyhow::bail!(
            "Rejected machoNet pre-auth connection from {peer_ip}: exceeded MAX_PREAUTH_CONNECTIONS_PER_IP ({MAX_PREAUTH_CONNECTIONS_PER_IP})"
        );
    };

    let mut session = SessionState::new(session_id);
    session.address = peer_addr.clone();
    session.live_notify_listener = true;

    let mut ver = VersionExchange::default();
    ver.user_count = ctx.db.online_session_count().max(1);
    let server_boot_build = ver.build_version;
    let mut handshake =
        MachoHandshakeStateMachine::new(ver, MACHO_PROXY_NODE_ID, session_id, client_id);
    let mut client_boot_build: Option<u32> = None;
    handshake.set_config_vals(crate::bug_reporting::build_client_global_config(
        &ctx,
        local_ip.as_deref(),
    ));

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(session_id, close_tx);
    let mut session_guard = crate::router::SessionCleanupGuard::new(Arc::clone(&ctx), session_id);
    struct ExplicitUpdateStateSessionCleanup(Arc<ServerContext>, u64);
    impl Drop for ExplicitUpdateStateSessionCleanup {
        fn drop(&mut self) {
            self.0
                .systems
                .clear_explicit_update_state_guards_for_session(self.1);
            self.0
                .clear_pending_structure_view_outside_set_state_for_session(self.1);
        }
    }
    let _explicit_update_state_cleanup =
        ExplicitUpdateStateSessionCleanup(Arc::clone(&ctx), session_id);

    // Step 1: Server immediately sends `LowLevelVersionExchange` upon TCP connection accept.
    if let Err(e) = write_framed_pyrep(&mut stream, &handshake.server_hello()).await {
        return Err(e);
    }

    // Start detached with zero solar-system tick subscriptions so pre-auth, character-selection,
    // and docked connections neither spawn/wake Jita nor hold unpolled broadcast receivers that
    // pin ring slots and block LOD-0 solar-system hibernation (`#1237` / `HP-10`).
    let mut sys_rx: Option<tokio::sync::broadcast::Receiver<crate::system_actor::TickBroadcast>> =
        None;
    let mut subscribed_sys_id: Option<u32> = None;
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut wire_state = crate::outbound_sequencer::SessionWireState::new(session_id, client_id)
        .with_flight_recorder(Arc::clone(&ctx.flight_recorder));
    ctx.flight_recorder.record_wire_state(&wire_state);

    let (mut reader, mut writer) = stream.into_split();
    let is_authenticated_flag = Arc::new(AtomicBool::new(false));
    let is_authenticated_reader = Arc::clone(&is_authenticated_flag);
    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::channel::<std::result::Result<Vec<u8>, std::io::Error>>(32);
    let reader_task = tokio::spawn(async move {
        let preauth_deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(PREAUTH_HANDSHAKE_DEADLINE_SECS);
        loop {
            let is_authed = is_authenticated_reader.load(Ordering::Acquire);
            let header_timeout = if is_authed {
                std::time::Duration::from_secs(IDLE_READ_TIMEOUT_SECS)
            } else {
                let now = tokio::time::Instant::now();
                if now >= preauth_deadline {
                    let _ = frame_tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "pre-auth handshake deadline ({PREAUTH_HANDSHAKE_DEADLINE_SECS}s) expired"
                            ),
                        )))
                        .await;
                    break;
                }
                preauth_deadline.duration_since(now)
            };
            let frame_len = match tokio::time::timeout(header_timeout, reader.read_u32_le()).await {
                Ok(Ok(len)) => len as usize,
                Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Ok(Err(e)) => {
                    let _ = frame_tx.send(Err(e)).await;
                    break;
                }
                Err(_) => {
                    let _ = frame_tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "timed out waiting for machoNet frame header after {}s",
                                header_timeout.as_secs()
                            ),
                        )))
                        .await;
                    break;
                }
            };
            let max_allowed_frame_len = if is_authenticated_reader.load(Ordering::Acquire) {
                MAX_MACHO_FRAME_LEN
            } else {
                MAX_PREAUTH_MACHO_FRAME_LEN
            };
            if frame_len == 0 || frame_len > max_allowed_frame_len {
                let _ = frame_tx
                    .send(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid frame length {frame_len} bytes"),
                    )))
                    .await;
                break;
            }
            let mut payload = vec![0u8; frame_len];
            let body_timeout = if is_authenticated_reader.load(Ordering::Acquire) {
                std::time::Duration::from_secs(FRAME_BODY_TIMEOUT_SECS)
            } else {
                let now = tokio::time::Instant::now();
                preauth_deadline
                    .checked_duration_since(now)
                    .unwrap_or(std::time::Duration::from_millis(1))
                    .min(std::time::Duration::from_secs(FRAME_BODY_TIMEOUT_SECS))
            };
            match tokio::time::timeout(body_timeout, reader.read_exact(&mut payload)).await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    let _ = frame_tx.send(Err(e)).await;
                    break;
                }
                Err(_) => {
                    let _ = frame_tx
                        .send(Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!(
                                "timed out waiting for machoNet frame body after {FRAME_BODY_TIMEOUT_SECS}s"
                            ),
                        )))
                        .await;
                    break;
                }
            }
            if frame_tx.send(Ok(payload)).await.is_err() {
                break;
            }
        }
    });
    session_guard.set_reader_abort(reader_task.abort_handle());
    let mut pending_dock: Option<PendingDockCall> = None;
    let mut deferred_ship_id: Option<PendingDeferredShipId> = None;
    let mut safe_logoff_teardown: Option<SafeLogoffTeardownGrace> = None;

    loop {
        tokio::select! {
            () = async {
                if let Some(ref mut pending) = pending_dock {
                    pending.sleep.as_mut().await;
                }
            }, if pending_dock.is_some() => {
                if let Some(pending) = pending_dock.take() {
                    let user_id_before = session.user_id;
                    let mut wire_refs = InternalCallReqWireRefs {
                        client_id,
                        sys_rx: TickRxSlot::Optional(&mut sys_rx),
                        subscribed_sys_id: &mut subscribed_sys_id,
                        wire_state: &mut wire_state,
                        deferred_ship_id: &mut deferred_ship_id,
                    };
                    if dispatch_call_req_to_wire(
                        &ctx,
                        &mut session,
                        &mut wire_refs,
                        &mut writer,
                        user_id_before,
                        &pending.source,
                        &pending.destination,
                        &pending.service,
                        pending.bound_id,
                        &pending.method,
                        &pending.call_args,
                        &pending.call_kwargs,
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
            }
            () = async {
                if let Some(pending) = deferred_ship_id {
                    tokio::time::sleep_until(pending.deadline).await;
                }
            }, if deferred_ship_id.is_some() && pending_dock.is_none() => {
                if let Some(frame) = flush_deferred_ship_id_slot(
                    &ctx,
                    &session,
                    client_id,
                    &mut deferred_ship_id,
                    tokio::time::Instant::now(),
                ) {
                    if write_out_frames(
                        &mut writer,
                        MACHO_PROXY_NODE_ID,
                        client_id,
                        session.user_id,
                        &[frame],
                    )
                    .await
                    .is_err()
                    {
                        break;
                    }
                }
            }
            Some((reason_code, reason)) = close_rx.recv() => {
                let mut safe_wire = SafeLogoffWireContext {
                    client_id,
                    sys_rx: &mut sys_rx,
                    notify_rx: &mut notify_rx,
                    subscribed_sys_id: &mut subscribed_sys_id,
                    wire_state: &mut wire_state,
                    deferred_ship_id: &mut deferred_ship_id,
                    pending_dock: &mut pending_dock,
                    safe_logoff_teardown: &mut safe_logoff_teardown,
                };
                if !matches!(
                    handle_session_closer_event(
                        &ctx,
                        &mut session,
                        &mut safe_wire,
                        &mut writer,
                        reason_code,
                        reason,
                    )
                    .await,
                    Ok(true)
                ) {
                    break;
                }
            }
            tick_res = async {
                match sys_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            }, if sys_rx.is_some()
                && !matches!(
                    wire_state.destiny,
                    crate::outbound_sequencer::DestinyStream::Detached
                ) => {
                // Guard against emitting stale space ticks (or pod TerminalExplosion/RemoveBall)
                // after an out-of-band DeathRespawn has already transitioned the session in DB
                // (#1010) or after SafeLogoff cleared online_lock.character_id (#1360), before
                // `notify_rx` / `close_rx` completes local session teardown.
                let mut db_probe = session.clone();
                if session.character_id.is_some() {
                    ctx.sync_session(&mut db_probe);
                }
                let online_char_matches = session.user_id.is_none_or(|uid| {
                    match ctx.db.get_online_session(uid) {
                        Some(os) => {
                            os.session_id == session_id
                                && os.character_id == session.character_id
                        }
                        None => true,
                    }
                });
                let session_obs_id = ballpark_observer_id_for_session(&session);
                let probe_obs_id = ballpark_observer_id_for_session(&db_probe);
                let still_in_same_live_space = online_char_matches
                    && session_obs_id.is_some()
                    && probe_obs_id == session_obs_id
                    && db_probe.solar_system_id == session.solar_system_id
                    && db_probe.solar_system_id == subscribed_sys_id
                    && db_probe.structure_id == session.structure_id;
                match tick_res {
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("machoNet session {session_id} sys_rx lagged by {skipped} ticks; resyncing SetState (ORD-5)");
                        if still_in_same_live_space && session_owns_char_notifications(&ctx, &session) {
                            wire_state.on_broadcast_lagged();
                            if let (Some(sys_id), Some(observer_id), Some(ref mut rx)) =
                                (session.solar_system_id, session_obs_id, sys_rx.as_mut())
                            {
                                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                                let resynced_opt = if is_shared_structure_observer(&session, observer_id) {
                                    resync_structure_set_state_with_sys_rx(
                                        &ctx,
                                        &sys_handle,
                                        rx,
                                        sys_id,
                                        observer_id,
                                        &[observer_id],
                                        &mut wire_state,
                                    )
                                    .await
                                } else {
                                    resync_set_state_with_sys_rx(
                                        &ctx,
                                        &sys_handle,
                                        rx,
                                        sys_id,
                                        observer_id,
                                        &[observer_id],
                                        &mut wire_state,
                                    )
                                    .await
                                };
                                if let Some(resynced) = resynced_opt {
                                    if session_owns_char_notifications(&ctx, &session) {
                                        let ep = wire_state.current_epoch();
                                        let write_failed = if let Some(batch) = build_destiny_batch(
                                            &ctx,
                                            ep,
                                            resynced.tick,
                                            &resynced.events,
                                            resynced.updates_pyrep,
                                            &[observer_id],
                                            SelfDestinyWireContext::NonDock,
                                        ) {
                                            emit_tick_batch_with_observability(
                                                &ctx,
                                                &mut wire_state,
                                                &mut writer,
                                                client_id,
                                                &session,
                                                batch,
                                                "lagged_resync_tick",
                                            )
                                            .await
                                            .is_err()
                                        } else {
                                            false
                                        } || emit_tick_batches_with_observability(
                                            &ctx,
                                            &mut wire_state,
                                            &mut writer,
                                            client_id,
                                            &session,
                                            resynced.post_batches,
                                            "system_tick_broadcast",
                                        )
                                        .await
                                        .is_err();
                                        if write_failed {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        sys_rx = None;
                        subscribed_sys_id = None;
                    }
                    Ok(tick_msg) => {
                        if still_in_same_live_space
                            && session_owns_char_notifications(&ctx, &session)
                            && session.solar_system_id == Some(tick_msg.solar_system_id)
                        {
                            if let Some(observer_id) = session_obs_id {
                                if let Some(updates_pyrep) = tick_msg.observer_updates_pyrep.get(&observer_id) {
                                    let evs = tick_msg
                                        .observer_events
                                        .get(&observer_id)
                                        .map(Vec::as_slice)
                                        .unwrap_or(&tick_msg.events);
                                    let ep = wire_state.current_epoch();
                                    if let Some(batch) = build_destiny_batch(&ctx, ep, tick_msg.tick, evs, updates_pyrep.clone(), &[observer_id], SelfDestinyWireContext::NonDock) {
                                        if emit_tick_batch_with_observability(
                                            &ctx,
                                            &mut wire_state,
                                            &mut writer,
                                            client_id,
                                            &session,
                                            batch,
                                            "system_tick_broadcast",
                                        )
                                        .await
                                        .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            notif_res = notify_rx.recv(), if session.character_id.is_some() => {
                if let Some(cid) = session.character_id {
                    let should_drain = match notif_res {
                        Ok((target_char_id, _)) => target_char_id == cid,
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => false,
                    };
                    if should_drain && session_owns_char_notifications(&ctx, &session) {
                        let prev_session = session.clone();
                        ctx.sync_session(&mut session);
                        ctx.sync_transition_grace_on_session_change(&prev_session, &session);
                        if !session_owns_char_notifications(&ctx, &session) {
                            sys_rx = None;
                            subscribed_sys_id = None;
                            continue;
                        }
                        let mut pending_notifs = Vec::new();
                        if session_owns_char_notifications(&ctx, &session) {
                            for notif in ctx.drain_notifications_for_char(cid) {
                                if notif.event == "OnSessionChanged" {
                                    let notif_prev = session.clone();
                                    ctx.sync_session(&mut session);
                                    ctx.sync_transition_grace_on_session_change(&notif_prev, &session);
                                    continue;
                                }
                                if let Some(typed_notif) = build_typed_notification(
                                    &ctx,
                                    notif.event,
                                    Some(&notif.id_type),
                                    notif.args,
                                ) {
                                    pending_notifs.push(typed_notif);
                                }
                            }
                        }
                        if !session_owns_char_notifications(&ctx, &session) {
                            sys_rx = None;
                            subscribed_sys_id = None;
                            continue;
                        }
                        let prev_stream_active = !matches!(
                            wire_state.destiny,
                            crate::outbound_sequencer::DestinyStream::Detached
                        ) || sys_rx.is_some()
                            || subscribed_sys_id.is_some();
                        let prev_obs_id = ballpark_observer_id_for_session(&prev_session);
                        let was_in_space = prev_session.character_id.is_some()
                            && prev_session.station_id.is_none()
                            && prev_session.solar_system_id.is_some()
                            && (prev_session.structure_id.is_none() || prev_stream_active);
                        let charsel_transition =
                            session_owns_charsel_transition(&ctx, &session);
                        let same_structure_view_outside = prev_stream_active
                            && session.character_id.is_some()
                            && session.station_id.is_none()
                            && session.structure_id.is_some()
                            && prev_session.station_id.is_none()
                            && prev_session.structure_id == session.structure_id
                            && prev_session.solar_system_id == session.solar_system_id;
                        let is_in_space = !charsel_transition
                            && session.character_id.is_some()
                            && session.station_id.is_none()
                            && session.solar_system_id.is_some()
                            && (session.structure_id.is_none() || same_structure_view_outside);
                        let target_sys_id = is_in_space.then_some(session.solar_system_id).flatten();
                        let docking_transition = was_in_space && !is_in_space;
                        let structure_mode_changed =
                            was_in_space && prev_session.structure_id != session.structure_id;
                        let sys_changed = is_in_space
                            && (session.solar_system_id != prev_session.solar_system_id
                                || target_sys_id != subscribed_sys_id
                                || sys_rx.is_none());

                        if docking_transition
                            || structure_mode_changed
                            || sys_changed
                            || (!is_in_space
                                && (sys_rx.is_some()
                                    || subscribed_sys_id.is_some()
                                    || !matches!(
                                        wire_state.destiny,
                                        crate::outbound_sequencer::DestinyStream::Detached
                                    )))
                        {
                            sys_rx = None;
                            subscribed_sys_id = None;
                            if !charsel_transition
                                && !matches!(
                                    wire_state.destiny,
                                    crate::outbound_sequencer::DestinyStream::Detached
                                )
                                && (docking_transition
                                    || structure_mode_changed
                                    || (was_in_space
                                        && session.solar_system_id != prev_session.solar_system_id))
                            {
                                if let Some(old_sys_id) = prev_session.solar_system_id {
                                    let old_sys = ctx.systems.get_or_spawn(old_sys_id);
                                    if session_owns_char_notifications(&ctx, &session) {
                                        if let Some(old_obs_id) = prev_obs_id {
                                            if !is_shared_structure_observer(&prev_session, old_obs_id)
                                            {
                                                // Discard stale old-system observer updates (e.g. pod
                                                // TerminalExplosion / RemoveBall / SetState) on async
                                                // DeathRespawn (#1010) before SessionChangeNotification
                                                // tears down the client ballpark via OnPrimaryViewChanged.
                                                let _ = old_sys
                                                    .drain_observer_updates(old_obs_id)
                                                    .await;
                                            }
                                        }
                                        if session_owns_char_notifications(&ctx, &session) {
                                            let _ = old_sys
                                                .flush_peer_observer_updates(prev_obs_id)
                                                .await;
                                        }
                                    }
                                }
                            }
                            if !session_owns_char_notifications(&ctx, &session) {
                                continue;
                            }
                            wire_state.leave_space();
                            ctx.systems
                                .clear_explicit_update_state_guards_for_session(session_id);
                            ctx.clear_pending_structure_view_outside_set_state_for_session(
                                session_id,
                            );
                            ctx.flight_recorder.record_wire_state(&wire_state);
                            if let Some(new_sys) = target_sys_id {
                                if sys_changed || structure_mode_changed {
                                    sys_rx = Some(ctx.systems.get_or_spawn(new_sys).subscribe());
                                    subscribed_sys_id = Some(new_sys);
                                }
                            }
                        }

                        if !session_owns_char_notifications(&ctx, &session) {
                            sys_rx = None;
                            subscribed_sys_id = None;
                            continue;
                        }
                        let (sess_change_frame, _) = resolve_deferred_session_change_frame(
                            &ctx,
                            client_id,
                            &prev_session,
                            &session,
                            &mut deferred_ship_id,
                            false,
                        );
                        if let Some(sess_frame) = sess_change_frame {
                            // When an in-space ship switch occurs in the same solar system (e.g. pod
                            // ejection on ship destruction `#970`), emit incremental `DoDestinyUpdate`
                            // (`RemoveBall` + `AddBalls2` from `queue_in_space_ship_handoff`) BEFORE
                            // `SessionChangeNotification(shipid)` when `wire_state.destiny` is already
                            // `Live`, avoiding `SetState` (`Park.SetState` triggers `DoBallClear` and
                            // breaks `OverviewWindow` weakrefs to celestial `Ball`s). Only synthesize
                            // `SetState` if the destiny stream is still `AwaitingSetState`.
                            if is_in_space
                                && !docking_transition
                                && !sys_changed
                                && session.structure_id.is_none()
                                && prev_session.structure_id.is_none()
                                && session.ship_id != prev_session.ship_id
                            {
                                if let (Some(new_ship_id), Some(sys_id), Some(ref mut rx)) =
                                    (session.ship_id, session.solar_system_id, sys_rx.as_mut())
                                {
                                    let prev_in_same_space = prev_session.station_id.is_none()
                                        && prev_session.structure_id.is_none()
                                        && prev_session.solar_system_id == session.solar_system_id;
                                    if session_owns_char_notifications(&ctx, &session) {
                                        if let Some(ship_swap) = sync_in_space_ship_swap_destiny(
                                            &ctx,
                                            rx,
                                            &mut wire_state,
                                            session_id,
                                            sys_id,
                                            new_ship_id,
                                            prev_session.ship_id,
                                            prev_in_same_space,
                                        )
                                        .await
                                        {
                                            if !session_owns_char_notifications(&ctx, &session) {
                                                sys_rx = None;
                                                subscribed_sys_id = None;
                                                continue;
                                            }
                                            let swap_write_failed =
                                                emit_tick_batches_with_observability(
                                                    &ctx,
                                                    &mut wire_state,
                                                    &mut writer,
                                                    client_id,
                                                    &session,
                                                    ship_swap.pre_batches,
                                                    "pending_session_change_pre_tick",
                                                )
                                                .await
                                                .is_err()
                                                    || if let Some(batch) = ship_swap.direct_batch {
                                                        emit_tick_batch_with_observability(
                                                            &ctx,
                                                            &mut wire_state,
                                                            &mut writer,
                                                            client_id,
                                                            &session,
                                                            batch,
                                                            "pending_session_change_tick",
                                                        )
                                                        .await
                                                        .is_err()
                                                    } else {
                                                        false
                                                    }
                                                    || emit_tick_batches_with_observability(
                                                        &ctx,
                                                        &mut wire_state,
                                                        &mut writer,
                                                        client_id,
                                                        &session,
                                                        ship_swap.post_batches,
                                                        "system_tick_broadcast",
                                                    )
                                                    .await
                                                    .is_err();
                                            if swap_write_failed {
                                                break;
                                            }
                                        }
                                    }
                                }
                            }
                            if !session_owns_char_notifications(&ctx, &session) {
                                sys_rx = None;
                                subscribed_sys_id = None;
                                continue;
                            }
                            if write_framed_pyrep(&mut writer, &sess_frame).await.is_err() {
                                break;
                            }
                        }
                        let sent_safe_logoff = pending_notifs
                            .iter()
                            .any(|n| n.event == "OnSafeLogoffActivated");
                        if emit_typed_notifications(
                            &ctx,
                            &mut wire_state,
                            &mut writer,
                            client_id,
                            &session,
                            pending_notifs,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        let safe_logoff_charsel_pending =
                            sent_safe_logoff && session_owns_charsel_transition(&ctx, &session);
                        if safe_logoff_charsel_pending {
                            let mut safe_wire = SafeLogoffWireContext {
                                client_id,
                                sys_rx: &mut sys_rx,
                                notify_rx: &mut notify_rx,
                                subscribed_sys_id: &mut subscribed_sys_id,
                                wire_state: &mut wire_state,
                                deferred_ship_id: &mut deferred_ship_id,
                                pending_dock: &mut pending_dock,
                                safe_logoff_teardown: &mut safe_logoff_teardown,
                            };
                            if handle_safe_logoff_to_charsel(
                                &ctx,
                                &mut session,
                                &mut safe_wire,
                                &mut writer,
                                &cid.to_string(),
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                    }
                }
            }
            frame_opt = frame_rx.recv(), if pending_dock.is_none() => {
                let payload = match frame_opt {
                    Some(Ok(bytes)) => bytes,
                    Some(Err(e)) => {
                        warn!("Closing machoNet session {session_id}: {e}");
                        break;
                    }
                    None => break,
                };
                let (max_frame_bytes, max_nodes) = if handshake.is_authenticated() {
                    (MAX_MACHO_FRAME_LEN, eve_proto::MAX_DECODED_NODES)
                } else {
                    (MAX_PREAUTH_MACHO_FRAME_LEN, MAX_PREAUTH_DECODED_NODES)
                };
                let pyrep = match marshal::decode_with_limits(&payload, max_frame_bytes, max_nodes)
                {
                    Ok(r) => r,
                    Err(e) => {
                        let dec = crate::warn_limiter::check_global_warn(
                            crate::warn_limiter::WarnCategory::MarshalOddity,
                            format!("macho_decode:{e}"),
                        );
                        if handshake.is_authenticated() {
                            if let Some((req_source, recovered_call_id)) =
                                recover_call_req_from_corrupt_payload(
                                    &payload,
                                    max_frame_bytes,
                                    max_nodes,
                                )
                            {
                                if dec.should_log {
                                    warn!(
                                        session_id,
                                        recovered_call_id,
                                        count = dec.count,
                                        suppressed = dec.suppressed_for_key,
                                        "Failed to decode authenticated CallReq call_id={recovered_call_id} on machoNet session {session_id}: {e}; returning wrapped Internal error"
                                    );
                                }
                                if write_call_req_internal_error_fallback(
                                    &mut writer,
                                    &req_source,
                                    None,
                                    session.user_id,
                                    &format!("Malformed CallReq payload: {e}"),
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                continue;
                            }
                        }
                        if dec.should_log {
                            warn!(
                                session_id,
                                count = dec.count,
                                suppressed = dec.suppressed_for_key,
                                "Failed to decode marshal payload on machoNet session {session_id}: {e}"
                            );
                        }
                        let closed = GPSTransportClosed::new(
                            format!("Invalid marshal frame: {e}"),
                            "HANDSHAKE_INCOMPATIBLEPROTOCOL",
                        );
                        let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                        break;
                    }
                };

                if !handshake.is_authenticated() {
                    if let Some(detected_build) = extract_handshake_client_build(&pyrep) {
                        client_boot_build = Some(detected_build);
                    }
                    match handshake.step(&pyrep) {
                        Err(closed) => {
                            record_and_warn_handshake_failure(
                                &ctx,
                                session_id,
                                None,
                                &peer_addr,
                                client_boot_build,
                                server_boot_build,
                                &closed,
                            );
                            let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                            break;
                        }
                        Ok(HandshakeStepOutput::SendFramesAndContinue(frames)) => {
                            let mut failed = false;
                            for f in &frames {
                                if write_framed_pyrep(&mut writer, f).await.is_err() {
                                    failed = true;
                                    break;
                                }
                            }
                            if failed {
                                break;
                            }
                        }
                        Ok(HandshakeStepOutput::NeedsAuthentication(challenge)) => {
                            if challenge.boot_build > 0 {
                                client_boot_build = Some(challenge.boot_build);
                            }
                            let now_unix = ctx.now_unix();
                            let username = challenge.user_name.as_deref().unwrap_or("");
                            let token = if let Some(sso_tok) =
                                challenge.user_sso_token.as_deref().filter(|s| !s.is_empty())
                            {
                                sso_tok
                            } else {
                                challenge.user_password.as_deref().unwrap_or("")
                            };

                            let auth_res = match ctx.db.authenticate_at(username, token, now_unix) {
                                Ok(acc) => {
                                    if ctx.is_shutting_down()
                                        && (acc.role & eve_machonet::service_roles::ROLE_GMS) == 0
                                    {
                                        Err(GPSTransportClosed::new(
                                            "Cluster shutdown in progress. Non-GM logins are rejected.",
                                            "ClusterShuttingDown",
                                        ))
                                    } else {
                                        let language_id =
                                            if challenge.user_language_id.trim().is_empty() {
                                                "EN".to_string()
                                            } else {
                                                challenge.user_language_id.clone()
                                            };
                                        Ok(HandshakeIdentity {
                                            user_id: acc.user_id,
                                            role: acc.role,
                                            language_id,
                                            user_type: 30,
                                            address: peer_addr.clone(),
                                            access_token: challenge.user_sso_token.clone(),
                                            computer_hash: Some(format!("evomu-peer-{peer_ip}")),
                                        })
                                    }
                                }
                                Err(eve_db::DbError::AccountBanned { until_unix, reason }) => {
                                    let msg = format!(
                                        "Account is banned until unix={until_unix}: {reason}"
                                    );
                                    Err(GPSTransportClosed::new(msg, "UserBanned")
                                        .with_arg("reason", PyRep::ascii_bytes(&reason))
                                        .with_arg("expiry", PyRep::Integer(until_unix as i64))
                                        .with_arg("until", PyRep::Integer(until_unix as i64)))
                                }
                                Err(eve_db::DbError::AccountSuspended { until_unix, reason }) => {
                                    let msg = format!(
                                        "Account is suspended until unix={until_unix}: {reason}"
                                    );
                                    Err(GPSTransportClosed::new(msg, "AccountSuspended")
                                        .with_arg("reason", PyRep::ascii_bytes(&reason))
                                        .with_arg("expiry", PyRep::Integer(until_unix as i64))
                                        .with_arg("until", PyRep::Integer(until_unix as i64)))
                                }
                                Err(e) => Err(GPSTransportClosed::new(
                                    e.to_string(),
                                    "LoginAuthFailed",
                                )),
                            };

                            let cluster_users = ctx.db.online_session_count();
                            handshake.set_config_vals(
                                crate::bug_reporting::build_client_global_config(
                                    &ctx,
                                    local_ip.as_deref(),
                                ),
                            );
                            match handshake.complete_challenge(&challenge, auth_res, cluster_users)
                            {
                                Ok(frames) => {
                                    let mut failed = false;
                                    for f in &frames {
                                        if write_framed_pyrep(&mut writer, f).await.is_err() {
                                            failed = true;
                                            break;
                                        }
                                    }
                                    if failed {
                                        break;
                                    }
                                }
                                Err(closed) => {
                                    record_and_warn_handshake_failure(
                                        &ctx,
                                        session_id,
                                        None,
                                        &peer_addr,
                                        client_boot_build,
                                        server_boot_build,
                                        &closed,
                                    );
                                    let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                                    break;
                                }
                            }
                        }
                        Ok(HandshakeStepOutput::Authenticated { frames, identity }) => {
                            let auth_started = std::time::Instant::now();
                            let now_unix = ctx.now_unix();
                            if ctx.is_shutting_down()
                                && (identity.role & eve_machonet::service_roles::ROLE_GMS) == 0
                            {
                                let closed = GPSTransportClosed::new(
                                    "Cluster shutdown in progress. Non-GM logins are rejected.",
                                    "ClusterShuttingDown",
                                );
                                let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                                break;
                            }
                            // Re-verify account has not been banned/suspended during the challenge-response round-trip.
                            if let Some(acc) = ctx.db.with_snapshot(|s| {
                                s.accounts
                                    .values()
                                    .find(|a| a.user_id == identity.user_id)
                                    .cloned()
                            }) {
                                if let Err(err) = acc.check_active_restriction(now_unix) {
                                    let code = match err {
                                        eve_db::DbError::AccountBanned { .. } => "UserBanned",
                                        eve_db::DbError::AccountSuspended { .. } => "AccountSuspended",
                                        _ => "LoginAuthFailed",
                                    };
                                    let closed = GPSTransportClosed::new(err.to_string(), code);
                                    let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                                    break;
                                }
                            }

                            ctx.db
                                .clear_revocation_on_fresh_login(identity.user_id, session_id);
                            match ctx.db.acquire_online_lock_with_computer_hash(
                                identity.user_id,
                                session_id,
                                None,
                                identity.computer_hash.clone(),
                                true,
                                now_unix,
                            ) {
                                Ok(displaced) => {
                                    ctx.db.set_online_session_peer_ip(
                                        identity.user_id,
                                        session_id,
                                        &peer_addr,
                                    );
                                    if let Some(d) = displaced {
                                        ctx.disconnect_displaced_session(session_id, &d);
                                    }
                                }
                                Err(err) => {
                                    let closed = GPSTransportClosed::new(
                                        err.to_string(),
                                        "LoginAuthFailed",
                                    );
                                    let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                                    break;
                                }
                            }

                            session.user_id = Some(identity.user_id);
                            session.role = identity.role;
                            session.language_id = identity.language_id.clone();
                            session.address = identity.address.clone();
                            session.user_type = identity.user_type;
                            ctx.flight_recorder.reset_user_telemetry(identity.user_id);
                            ctx.flight_recorder.record_handshake_builds(
                                session_id,
                                Some(identity.user_id),
                                client_boot_build,
                                server_boot_build,
                                false,
                            );
                            is_authenticated_flag.store(true, Ordering::Release);
                            preauth_guard.complete_auth();
                            crate::lifecycle::set_open_sessions(ctx.db.online_session_count() as usize);
                            crate::ops_telemetry::record_bootstrap_timing(
                                crate::ops_telemetry::BootstrapPacketTiming {
                                    phase: "login".to_string(),
                                    service: "authentication".to_string(),
                                    method: "Login".to_string(),
                                    session_id,
                                    character_id: None,
                                    elapsed_since_request_ms: auth_started.elapsed().as_millis() as u64,
                                    timestamp_unix: now_unix,
                                },
                            );

                            let mut failed = false;
                            for f in &frames {
                                if write_framed_pyrep(&mut writer, f).await.is_err() {
                                    failed = true;
                                    break;
                                }
                            }
                            if failed {
                                break;
                            }
                        }
                    }
                    continue;
                }

                // Drain any pending session-closer signal before handling the next authenticated
                // client frame so a concurrent SafeLogoff or displacement cannot race SelectCharacterID.
                let mut close_connection = false;
                while let Ok((reason_code, reason)) = close_rx.try_recv() {
                    let mut safe_wire = SafeLogoffWireContext {
                        client_id,
                        sys_rx: &mut sys_rx,
                        notify_rx: &mut notify_rx,
                        subscribed_sys_id: &mut subscribed_sys_id,
                        wire_state: &mut wire_state,
                        deferred_ship_id: &mut deferred_ship_id,
                        pending_dock: &mut pending_dock,
                        safe_logoff_teardown: &mut safe_logoff_teardown,
                    };
                    if !matches!(
                        handle_session_closer_event(
                            &ctx,
                            &mut session,
                            &mut safe_wire,
                            &mut writer,
                            reason_code,
                            reason,
                        )
                        .await,
                        Ok(true)
                    ) {
                        close_connection = true;
                        break;
                    }
                }
                if close_connection {
                    break;
                }

                // Handshake is complete (`Authenticated`): only valid `MachoPacket`s are processed now.
                let Some(pkt) = MachoPacket::from_pyrep(&pyrep) else {
                    if let Some((req_source, recovered_call_id)) =
                        eve_machonet::recover_call_req_source_from_pyrep(&pyrep)
                    {
                        warn!("Failed to parse authenticated CallReq packet call_id={recovered_call_id} on machoNet session {session_id}; returning wrapped Internal error");
                        if write_call_req_internal_error_fallback(
                            &mut writer,
                            &req_source,
                            None,
                            session.user_id,
                            "Malformed CallReq packet",
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    debug!("Ignoring non-MachoPacket frame after authentication on session {session_id}");
                    continue;
                };

                if let Some(uid) = session.user_id {
                    if let Some((reason_code, reason)) =
                        ctx.db.check_session_revoked(uid, session_id, ctx.now_unix())
                    {
                        if reason_code != "MultiLoginBlocked" {
                            let closed = GPSTransportClosed::new(reason, reason_code);
                            let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                            break;
                        }
                    }
                }

                let had_beyonce_before_release = ctx.has_bound_service(session_id, "beyonce");
                let (released_cnt, is_dedicated_release_notif) =
                    ctx.process_client_bound_object_releases(session_id, &pkt);
                if released_cnt > 0
                    && had_beyonce_before_release
                    && !ctx.has_bound_service(session_id, "beyonce")
                    && session.character_id.is_some()
                    && session.station_id.is_none()
                    && session.structure_id.is_some()
                {
                    let mut sys_rx_slot = TickRxSlot::Optional(&mut sys_rx);
                    detach_structure_view_outside_on_beyonce_release(
                        &ctx,
                        session_id,
                        session.character_id,
                        &mut wire_state,
                        &mut sys_rx_slot,
                        &mut subscribed_sys_id,
                    );
                }
                if is_dedicated_release_notif {
                    continue;
                }

                match pkt.packet_type {
                    MachoPacketType::PingReq => {
                        let mut payload = pkt.payload.clone();
                        let now_ft = ctx.now_blueos_filetime_ticks();
                        let append_ping_times = |times: &mut Vec<PyRep>| {
                            let (t1, t2) = times
                                .last()
                                .and_then(|entry| match entry {
                                    PyRep::Tuple(t) if t.len() >= 2 => {
                                        Some((t[0].as_i64()?, t[1].as_i64()?))
                                    }
                                    _ => None,
                                })
                                .unwrap_or((now_ft, now_ft));
                            for label in [
                                "proxy::handle_message",
                                "proxy::after_tasklet",
                                "proxy::writing",
                                "server::handle_message",
                                "server::after_tasklet",
                                "server::turnaround",
                                "proxy::handle_message",
                                "proxy::after_tasklet",
                                "proxy::writing",
                            ] {
                                times.push(PyRep::Tuple(vec![
                                    PyRep::Integer(t1),
                                    PyRep::Integer(t2),
                                    PyRep::ascii_bytes(label),
                                ]));
                            }
                        };
                        match &mut payload {
                            PyRep::Tuple(items) => {
                                if let Some(PyRep::List(times)) = items.first_mut() {
                                    append_ping_times(times);
                                }
                            }
                            PyRep::List(times) => append_ping_times(times),
                            _ => {}
                        }
                        let rsp = MachoPacket {
                            packet_type: MachoPacketType::PingRsp,
                            source: reply_source_address(&pkt.destination),
                            destination: pkt.source.clone(),
                            user_id: session.user_id,
                            payload,
                            named_payload: HashMap::new(),
                        };
                        if write_framed_pyrep(&mut writer, &rsp.to_pyrep()).await.is_err() {
                            break;
                        }
                    }
                    MachoPacketType::CallReq => {
                        let service = match &pkt.destination {
                            MachoAddress::Node { service, .. }
                            | MachoAddress::Any { service, .. }
                            | MachoAddress::Client { service, .. } => {
                                service.clone().unwrap_or_else(|| "machoNet".into())
                            }
                            MachoAddress::Broadcast { broadcast_id, .. } => broadcast_id.clone(),
                        };

                        let (bound_id, method, call_args, call_kwargs) =
                            extract_macho_call_req(&pkt.payload);
                        let dock_plan = if session_owns_char_notifications(&ctx, &session) {
                            let effective_dock_service = bound_id
                                .and_then(|bid| ctx.owned_bound_service_for_metrics(&session, bid));
                            crate::services::space_warp_jump::prepare_dock_warp_settle(
                                &ctx,
                                &session,
                                effective_dock_service.as_deref().unwrap_or(&service),
                                &method,
                                &call_args,
                            )
                            .await
                        } else {
                            pending_dock = None;
                            None
                        };
                        if let Some(plan) = dock_plan {
                            if !session_owns_char_notifications(&ctx, &session) {
                                pending_dock = None;
                            } else {
                                let handle = ctx.systems.get_or_spawn(plan.sys_id);
                                if let Ok((tick, events, updates_pyrep, watermark_seq)) =
                                    handle.drain_observer_updates_with_seq(plan.ship_id).await
                                {
                                    if session_owns_char_notifications(&ctx, &session) {
                                        let mut pre_batches = Vec::new();
                                        let mut post_batches = Vec::new();
                                        let ep = if matches!(
                                            wire_state.destiny,
                                            crate::outbound_sequencer::DestinyStream::Live { .. }
                                        ) {
                                            if let Some(ref mut rx) = sys_rx {
                                                drain_pending_tick_batches_for_sys_with_watermark(
                                                    &ctx,
                                                    rx,
                                                    plan.sys_id,
                                                    &[plan.ship_id],
                                                    &mut wire_state,
                                                    Some(watermark_seq),
                                                    &mut pre_batches,
                                                    &mut post_batches,
                                                    session_id,
                                                )
                                                .0
                                            } else {
                                                wire_state.current_epoch()
                                            }
                                        } else {
                                            wire_state.current_epoch()
                                        };
                                        let settle_write_failed = emit_tick_batches_with_observability(
                                            &ctx,
                                            &mut wire_state,
                                            &mut writer,
                                            client_id,
                                            &session,
                                            pre_batches,
                                            "pending_dock_settle_pre_tick",
                                        )
                                        .await
                                        .is_err()
                                            || if !events.is_empty() || plan.stopped_now {
                                                if let Some(batch) = build_destiny_batch(
                                                    &ctx,
                                                    ep,
                                                    tick,
                                                    &events,
                                                    updates_pyrep,
                                                    &[plan.ship_id],
                                                    SelfDestinyWireContext::DockSettle,
                                                ) {
                                                    emit_tick_batch_with_observability(
                                                        &ctx,
                                                        &mut wire_state,
                                                        &mut writer,
                                                        client_id,
                                                        &session,
                                                        batch,
                                                        "pending_dock_settle_tick",
                                                    )
                                                    .await
                                                    .is_err()
                                                } else {
                                                    false
                                                }
                                            } else {
                                                false
                                            }
                                            || emit_tick_batches_with_observability(
                                                &ctx,
                                                &mut wire_state,
                                                &mut writer,
                                                client_id,
                                                &session,
                                                post_batches,
                                                "system_tick_broadcast",
                                            )
                                            .await
                                            .is_err();
                                        if settle_write_failed {
                                            break;
                                        }
                                    }
                                }
                                if plan.stopped_now && session_owns_char_notifications(&ctx, &session) {
                                    let _ = handle.flush_peer_observer_updates(Some(plan.ship_id)).await;
                                }
                                if !session_owns_char_notifications(&ctx, &session) {
                                    pending_dock = None;
                                } else if plan.settle_deadline > tokio::time::Instant::now() {
                                    if let Some(prev_pending) = pending_dock.take() {
                                        let user_id_before = session.user_id;
                                        let mut wire_refs = InternalCallReqWireRefs {
                                            client_id,
                                            sys_rx: TickRxSlot::Optional(&mut sys_rx),
                                            subscribed_sys_id: &mut subscribed_sys_id,
                                            wire_state: &mut wire_state,
                                            deferred_ship_id: &mut deferred_ship_id,
                                        };
                                        if dispatch_call_req_to_wire(
                                            &ctx,
                                            &mut session,
                                            &mut wire_refs,
                                            &mut writer,
                                            user_id_before,
                                            &prev_pending.source,
                                            &prev_pending.destination,
                                            &prev_pending.service,
                                            prev_pending.bound_id,
                                            &prev_pending.method,
                                            &prev_pending.call_args,
                                            &prev_pending.call_kwargs,
                                        )
                                        .await
                                        .is_err()
                                        {
                                            break;
                                        }
                                    }
                                    pending_dock = Some(PendingDockCall {
                                        source: pkt.source,
                                        destination: pkt.destination,
                                        service,
                                        bound_id,
                                        method,
                                        call_args,
                                        call_kwargs,
                                        sleep: Box::pin(tokio::time::sleep_until(plan.settle_deadline)),
                                    });
                                    continue;
                                }
                            }
                        }
                        if safe_logoff_teardown.is_some() {
                            let exits_teardown_grace = session.character_id.is_some()
                                || (bound_id.is_none()
                                    && matches!(
                                        (service.as_str(), method.as_str()),
                                        ("charUnboundMgr", "SelectCharacterID")
                                            | ("authentication", _)
                                            | ("userSvc", "UserLogOffCharacter")
                                    ));
                            if exits_teardown_grace {
                                clear_safe_logoff_teardown_grace(
                                    &ctx,
                                    session_id,
                                    &mut safe_logoff_teardown,
                                );
                            } else if let Some((grace_cid, creates_temp_binding)) =
                                resolve_safe_logoff_teardown_grace(
                                    &ctx,
                                    &session,
                                    &mut safe_logoff_teardown,
                                    &service,
                                    bound_id,
                                    &method,
                                    &call_args,
                                    &call_kwargs,
                                )
                            {
                                match dispatch_safe_logoff_teardown_grace_rpc(
                                    &ctx,
                                    &mut session,
                                    grace_cid,
                                    client_id,
                                    &mut writer,
                                    &pkt.source,
                                    &pkt.destination,
                                    &service,
                                    bound_id,
                                    &method,
                                    &call_args,
                                    &call_kwargs,
                                )
                                .await
                                {
                                    Ok(succeeded) => {
                                        if creates_temp_binding && succeeded {
                                            if let Some(ref mut g) = safe_logoff_teardown {
                                                g.temp_bindings_created = true;
                                            }
                                        }
                                        if session.user_id.is_none() {
                                            clear_safe_logoff_teardown_grace(
                                                &ctx,
                                                session_id,
                                                &mut safe_logoff_teardown,
                                            );
                                        }
                                    }
                                    Err(_) => break,
                                }
                                continue;
                            }
                        }
                        let user_id_before = session.user_id;
                        let mut wire_refs = InternalCallReqWireRefs {
                            client_id,
                            sys_rx: TickRxSlot::Optional(&mut sys_rx),
                            subscribed_sys_id: &mut subscribed_sys_id,
                            wire_state: &mut wire_state,
                            deferred_ship_id: &mut deferred_ship_id,
                        };
                        if dispatch_call_req_to_wire(
                            &ctx,
                            &mut session,
                            &mut wire_refs,
                            &mut writer,
                            user_id_before,
                            &pkt.source,
                            &pkt.destination,
                            &service,
                            bound_id,
                            &method,
                            &call_args,
                            &call_kwargs,
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    MachoPacketType::ResolveReq => {
                        let rsp = MachoPacket {
                            packet_type: MachoPacketType::ResolveRsp,
                            source: MachoAddress::Node {
                                node_id: MACHO_PROXY_NODE_ID,
                                service: None,
                                call_id: 0,
                            },
                            destination: pkt.source.clone(),
                            user_id: session.user_id,
                            payload: PyRep::Integer(MACHO_PROXY_NODE_ID as i64),
                            named_payload: HashMap::new(),
                        };
                        if write_framed_pyrep(&mut writer, &rsp.to_pyrep()).await.is_err() {
                            break;
                        }
                    }
                    MachoPacketType::AuthenticationReq => {
                        let err_pkt = MachoPacket {
                            packet_type: MachoPacketType::ErrorResponse,
                            source: reply_source_address(&pkt.destination),
                            destination: pkt.source.clone(),
                            user_id: session.user_id,
                            payload: PyRep::Tuple(vec![
                                PyRep::Integer(MachoPacketType::AuthenticationReq as i64),
                                PyRep::Integer(1),
                                PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::keyval([
                                    ("code", PyRep::String("AlreadyAuthenticated".into())),
                                    (
                                        "message",
                                        PyRep::String(
                                            "GPSTransport handshake is already complete".into(),
                                        ),
                                    ),
                                ])))]),
                            ]),
                            named_payload: HashMap::new(),
                        };
                        let _ = write_framed_pyrep(&mut writer, &err_pkt.to_pyrep()).await;
                    }
                    _ => {
                        debug!(
                            "Unhandled MachoPacket type {:?} on session {session_id}",
                            pkt.packet_type
                        );
                    }
                }
            }
        }
    }

    // Drop any active solar-system tick receiver before async disconnect cleanup (`#1237`).
    drop(sys_rx);
    let _ = subscribed_sys_id;
    reader_task.abort();
    ctx.flight_recorder.remove_wire_state(session_id);
    ctx.flight_recorder.remove_session_telemetry(session_id);
    ctx.systems
        .clear_explicit_update_state_guards_for_session(session_id);
    ctx.clear_pending_structure_view_outside_set_state_for_session(session_id);
    ctx.cleanup_disconnected_client_session(&mut session).await;
    crate::lifecycle::set_open_sessions(ctx.db.online_session_count() as usize);

    Ok(())
}

pub(crate) fn enrich_session_change_scoped_roles(
    ctx: &ServerContext,
    prev_session: &SessionState,
    session: &SessionState,
    sess_change_payload: &mut PyRep,
) {
    let should_enrich = prev_session.character_id != session.character_id
        || prev_session.corporation_id != session.corporation_id
        || prev_session.corp_role != session.corp_role;
    if !should_enrich {
        return;
    }
    let (Some(char_id), Some(corp_id)) = (session.character_id, session.corporation_id) else {
        return;
    };
    let now_unix = ctx.now_unix();
    let (is_ceo, roles_all, roles_hq, roles_base, roles_other) = ctx.db.with_snapshot(|snap| {
        if !snap.corporations.contains_key(&corp_id) {
            return (false, 0u64, 0u64, 0u64, 0u64);
        }
        let (is_ceo, eff_roles) =
            crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles(
                snap, corp_id, char_id, 0, now_unix,
            );
        if is_ceo {
            return (true, u64::MAX, u64::MAX, u64::MAX, u64::MAX);
        }
        let ra = snap
            .corp_role_assignments
            .get(&(corp_id, char_id))
            .cloned()
            .unwrap_or_default();
        let (pend_hq, pend_base, pend_other) = snap
            .corp_governance
            .get(&corp_id)
            .and_then(|g| g.pending_scoped_role_grants.get(&char_id))
            .filter(|&&(_, _, _, eff)| now_unix >= eff)
            .map_or((0, 0, 0), |&(hq, base, other, _)| (hq, base, other));
        (
            false,
            eff_roles,
            ra.roles_at_hq | pend_hq,
            ra.roles_at_base | pend_base,
            ra.roles_at_other | pend_other,
        )
    });
    if !is_ceo && roles_all == 0 && roles_hq == 0 && roles_base == 0 && roles_other == 0 {
        return;
    }
    let PyRep::Tuple(ref mut outer) = sess_change_payload else {
        return;
    };
    let Some(PyRep::Tuple(ref mut inner)) = outer.get_mut(1) else {
        return;
    };
    let Some(PyRep::Dict(ref mut entries)) = inner.get_mut(1) else {
        return;
    };
    let mut upsert = |key: &str, val: u64| {
        if val == 0 {
            return;
        }
        let already = entries
            .iter()
            .any(|(k, _)| k.as_str() == Some(key));
        if !already {
            entries.push((
                PyRep::ascii_bytes(key),
                PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(val as i64)]),
            ));
        }
    };
    upsert("rolesAtAll", roles_all);
    upsert("rolesAtHQ", roles_hq);
    upsert("rolesAtBase", roles_base);
    upsert("rolesAtOther", roles_other);
}

pub use eve_machonet::extract_macho_call_req;

/// Filters `raw_payload` through [`eve_machonet::filter_session_change_payload`], logs dropped keys at
/// debug level, skips emission when every key is dropped, and wraps the surviving diff in a
/// `SessionChangeNotification` `MachoPacket` `PyRep` (`GH #1085`).
pub fn build_filtered_session_change_packet(
    client_id: u64,
    user_id: Option<u64>,
    session_id: u64,
    raw_payload: PyRep,
) -> Option<PyRep> {
    build_filtered_session_change_packet_with_elapsed(
        client_id,
        user_id,
        session_id,
        raw_payload,
        None,
    )
}

fn build_filtered_session_change_packet_with_elapsed(
    client_id: u64,
    user_id: Option<u64>,
    session_id: u64,
    raw_payload: PyRep,
    elapsed_since_request_ms: Option<u64>,
) -> Option<PyRep> {
    let (filtered_payload, dropped) = eve_machonet::filter_session_change_payload(raw_payload);
    if !dropped.is_empty() {
        debug!(
            session_id,
            client_id,
            dropped_keys = ?dropped,
            "dropped non-distributed SessionChangeNotification keys: {dropped:?}"
        );
    }
    let sess_change_payload = filtered_payload?;
    let mut named_payload = HashMap::new();
    named_payload.insert("channel".into(), PyRep::ascii_bytes("sessionchange"));
    if let Some(elapsed_ms) = elapsed_since_request_ms {
        named_payload.insert(
            "elapsed_since_request_ms".into(),
            PyRep::Integer(elapsed_ms as i64),
        );
    }
    let sess_pkt = MachoPacket {
        packet_type: MachoPacketType::SessionChangeNotification,
        source: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: None,
            call_id: 0,
        },
        destination: MachoAddress::Client {
            client_id,
            call_id: 0,
            service: None,
        },
        user_id,
        payload: sess_change_payload,
        named_payload,
    };
    Some(sess_pkt.to_pyrep())
}

fn build_session_change_wire_frame(
    ctx: &ServerContext,
    client_id: u64,
    prev_session: &SessionState,
    session: &SessionState,
    elapsed_since_request_ms: Option<u64>,
) -> Option<PyRep> {
    let mut raw_payload = eve_machonet::SessionChange::from_diff(
        prev_session,
        session,
        vec![MACHO_PROXY_NODE_ID as i64],
    )
    .to_pyrep()?;
    enrich_session_change_scoped_roles(ctx, prev_session, session, &mut raw_payload);
    build_filtered_session_change_packet_with_elapsed(
        client_id,
        session.user_id,
        session.session_id,
        raw_payload,
        elapsed_since_request_ms,
    )
}

pub(crate) fn build_deferred_ship_id_frame(
    client_id: u64,
    user_id: Option<u64>,
    session_id: u64,
    ship_id: i64,
) -> Option<PyRep> {
    let raw_payload = eve_machonet::SessionChange::new(
        session_id,
        vec![(
            "shipid".to_string(),
            (PyRep::None, PyRep::Integer(ship_id)),
        )],
        vec![MACHO_PROXY_NODE_ID as i64],
    )
    .to_pyrep()?;
    build_filtered_session_change_packet(client_id, user_id, session_id, raw_payload)
}

/// Returns true when a completed RPC fetches the docked NPC station hangar inventory so a
/// deferred `shipid: [None, shipID]` `SessionChangeNotification` can flush right after `CallRsp`
/// (`GH #1085`, `#1140`).
fn is_dock_hangar_flush_rpc(
    ctx: &ServerContext,
    service: &str,
    bound_id: Option<u64>,
    method: &str,
    call_args: &[PyRep],
    pending_ship: Option<PendingDeferredShipId>,
) -> bool {
    let Some(pending) = pending_ship else {
        return false;
    };
    match method {
        // Bound object RPCs on the live V24 wire arrive with `service == "machoNet"`; confirm
        // via `ctx.bound_objects` that the target is the docked NPC station's `invbroker` object.
        "List" | "ListByFlags" | "GetSelfInvItem" => {
            matches!(service, "invbroker" | "machoNet")
                && bound_id.is_some_and(|bid| {
                    ctx.bound_objects
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&bid)
                        .is_some_and(|entry| {
                            entry.service == "invbroker" && entry.object_id == pending.dock_id
                        })
                })
        }
        "MachoBindObject" if service == "invbroker" => {
            let target_id = match call_args.first() {
                Some(PyRep::Integer(id)) => Some(*id),
                Some(PyRep::Tuple(items)) => items.first().and_then(PyRep::as_i64),
                _ => None,
            };
            let sub_method = match call_args.get(1) {
                Some(PyRep::Tuple(call_tuple)) => call_tuple.first().and_then(|m| m.as_str()),
                _ => None,
            };
            target_id == Some(pending.dock_id)
                && matches!(sub_method, Some("List" | "ListByFlags" | "GetSelfInvItem"))
        }
        _ => false,
    }
}

fn resolve_deferred_session_change_frame(
    ctx: &ServerContext,
    client_id: u64,
    prev_session: &SessionState,
    session: &SessionState,
    deferred_ship_id: &mut Option<PendingDeferredShipId>,
    flush_now: bool,
) -> (Option<PyRep>, Option<PyRep>) {
    resolve_deferred_session_change_frame_with_elapsed(
        ctx,
        client_id,
        prev_session,
        session,
        deferred_ship_id,
        flush_now,
        None,
    )
}

fn resolve_deferred_session_change_frame_with_elapsed(
    ctx: &ServerContext,
    client_id: u64,
    prev_session: &SessionState,
    session: &SessionState,
    deferred_ship_id: &mut Option<PendingDeferredShipId>,
    flush_now: bool,
    elapsed_since_request_ms: Option<u64>,
) -> (Option<PyRep>, Option<PyRep>) {
    let was_in_space = prev_session.character_id.is_some()
        && prev_session.station_id.is_none()
        && prev_session.structure_id.is_none()
        && prev_session.solar_system_id.is_some();
    let is_in_space = session.character_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_none()
        && session.solar_system_id.is_some();
    let docking_transition = was_in_space && !is_in_space;
    let death_respawn_docking = docking_transition && session.ship_id != prev_session.ship_id;
    let cur_dock_id = session.station_id.or(session.structure_id);

    let mut effective_prev = prev_session.clone();
    let mut effective_next = session.clone();
    let mut deferred_flush_frame = None;

    if let Some(pending) = *deferred_ship_id {
        let still_same_docked_ship = session.character_id == Some(pending.char_id)
            && session.ship_id == Some(pending.ship_id)
            && cur_dock_id == Some(pending.dock_id)
            && prev_session.character_id == Some(pending.char_id)
            && prev_session.ship_id == Some(pending.ship_id)
            && prev_session.station_id.or(prev_session.structure_id) == Some(pending.dock_id);
        if still_same_docked_ship {
            effective_prev.ship_id = None;
            effective_next.ship_id = None;
            if flush_now {
                *deferred_ship_id = None;
                deferred_flush_frame = build_deferred_ship_id_frame(
                    client_id,
                    session.user_id,
                    session.session_id,
                    pending.ship_id,
                );
            }
        } else {
            *deferred_ship_id = None;
            if prev_session.character_id == Some(pending.char_id)
                && prev_session.ship_id == Some(pending.ship_id)
            {
                effective_prev.ship_id = None;
            }
        }
    } else if docking_transition && !death_respawn_docking && session.structure_id.is_none() {
        // Only defer `shipid` when docking at an NPC station. On Upwell structure docks
        // (`structure_id` set), `godma.StateManager.Prime` sees `primeStructure = True`,
        // immediately calls `dogmaIM.GetAllInfo`, and forwards `session.shipid` into
        // `clientDogmaLocation._MakeShipActive` before any `invbroker` call, which logs
        // `Unexpectedly got shipID = None` if `shipid` was masked (`GH #1140`, `#1141`).
        if let (Some(char_id), Some(ship_id), Some(dock_id)) =
            (session.character_id, session.ship_id, session.station_id)
        {
            effective_next.ship_id = None;
            *deferred_ship_id = Some(PendingDeferredShipId {
                char_id,
                ship_id,
                dock_id,
                deadline: tokio::time::Instant::now()
                    + std::time::Duration::from_millis(DEFERRED_SHIP_ID_DELAY_MS),
            });
        }
    }

    let sess_frame = build_session_change_wire_frame(
        ctx,
        client_id,
        &effective_prev,
        &effective_next,
        elapsed_since_request_ms,
    );
    (sess_frame, deferred_flush_frame)
}

fn flush_deferred_ship_id_slot(
    ctx: &ServerContext,
    session: &SessionState,
    client_id: u64,
    deferred_ship_id: &mut Option<PendingDeferredShipId>,
    now: tokio::time::Instant,
) -> Option<crate::outbound_sequencer::OutFrame> {
    let pending = (*deferred_ship_id)?;
    if now < pending.deadline {
        return None;
    }
    *deferred_ship_id = None;
    let mut probe = session.clone();
    if probe.character_id.is_some() {
        ctx.sync_session(&mut probe);
    }
    let still_docked = probe.character_id == Some(pending.char_id)
        && probe.ship_id == Some(pending.ship_id)
        && probe.station_id.or(probe.structure_id) == Some(pending.dock_id);
    if !still_docked {
        return None;
    }
    build_deferred_ship_id_frame(client_id, probe.user_id, probe.session_id, pending.ship_id)
        .map(|frame| crate::outbound_sequencer::OutFrame::SessionChange { frame })
}

#[cfg(any(test, feature = "testkit"))]
pub(crate) fn flush_deferred_ship_id_if_due(
    ctx: &ServerContext,
    session: &SessionState,
    wire: &mut CallReqWireRefs<'_>,
    now: tokio::time::Instant,
) -> Option<crate::outbound_sequencer::OutFrame> {
    flush_deferred_ship_id_slot(ctx, session, wire.client_id, wire.deferred_ship_id, now)
}

/// Drains any pending `TickBroadcast` destiny updates, session changes, and queued notifications
/// for an idle `ClientWireSession` without issuing an RPC (`GH #1077 §C`).
#[cfg(any(test, feature = "testkit"))]
pub(crate) fn drain_pending_tick_frames_for_wire_session(
    ctx: &ServerContext,
    session: &mut SessionState,
    wire: &mut CallReqWireRefs<'_>,
) -> (Vec<crate::outbound_sequencer::OutFrame>, Vec<PyRep>) {
    let prev_stream_active = !matches!(
        wire.wire_state.destiny,
        crate::outbound_sequencer::DestinyStream::Detached
    ) || wire.subscribed_sys_id.is_some();
    let was_in_space = session.character_id.is_some()
        && session.station_id.is_none()
        && session.solar_system_id.is_some()
        && (session.structure_id.is_none() || prev_stream_active);
    let prev_session = session.clone();
    if session.character_id.is_some() {
        ctx.sync_session(session);
        ctx.sync_transition_grace_on_session_change(&prev_session, session);
    }
    let mut out_frames = Vec::new();
    let (session_change_frame, _) = resolve_deferred_session_change_frame(
        ctx,
        wire.client_id,
        &prev_session,
        session,
        wire.deferred_ship_id,
        false,
    );
    let sess_change_sent = session_change_frame.is_some();
    if let Some(frame) = session_change_frame {
        out_frames.push(crate::outbound_sequencer::OutFrame::SessionChange { frame });
    }
    if let Some(flush_frame) =
        flush_deferred_ship_id_if_due(ctx, session, wire, tokio::time::Instant::now())
    {
        out_frames.push(flush_frame);
    }
    let same_structure_view_outside = prev_stream_active
        && session.character_id.is_some()
        && session.station_id.is_none()
        && session.structure_id.is_some()
        && prev_session.station_id.is_none()
        && prev_session.structure_id == session.structure_id
        && prev_session.solar_system_id == session.solar_system_id;
    let is_in_space = session.character_id.is_some()
        && session.station_id.is_none()
        && session.solar_system_id.is_some()
        && (session.structure_id.is_none() || same_structure_view_outside);
    if was_in_space
        && (!is_in_space
            || prev_session.solar_system_id != session.solar_system_id
            || prev_session.structure_id != session.structure_id)
    {
        wire.wire_state.leave_space();
        ctx.systems
            .clear_explicit_update_state_guards_for_session(session.session_id);
        ctx.clear_pending_structure_view_outside_set_state_for_session(session.session_id);
    }
    if !is_in_space {
        if wire.subscribed_sys_id.is_some() {
            let (_tx, closed_rx) = tokio::sync::broadcast::channel(1);
            *wire.sys_rx = closed_rx;
            *wire.subscribed_sys_id = None;
        }
    } else {
        let sys_id = session.solar_system_id.unwrap();
        if *wire.subscribed_sys_id != Some(sys_id) {
            *wire.sys_rx = ctx.systems.get_or_spawn(sys_id).subscribe();
            *wire.subscribed_sys_id = Some(sys_id);
        }
        if let Some(observer_id) = ballpark_observer_id_for_session(session) {
            let mut pending_batches = Vec::new();
            let mut ignored_post = Vec::new();
            let (_ep, effective_was_live) = drain_pending_tick_batches_for_sys_with_watermark(
                ctx,
                wire.sys_rx,
                sys_id,
                &[observer_id],
                wire.wire_state,
                None,
                &mut pending_batches,
                &mut ignored_post,
                session.session_id,
            );
            if effective_was_live {
                for batch in pending_batches {
                    for sub_batch in crate::outbound_sequencer::normalize_server_destiny_batch(
                        wire.wire_state,
                        batch,
                    ) {
                        let (frame_opt, tick_events) =
                            crate::outbound_sequencer::sequence_tick_with_events(
                                wire.wire_state,
                                sub_batch,
                            );
                        if !tick_events.is_empty() {
                            record_sequencer_events(
                                ctx,
                                session.session_id,
                                &tick_events,
                                None,
                                false,
                            );
                        }
                        if let Some(frame) = frame_opt {
                            out_frames.push(frame);
                        }
                    }
                }
            }
        }
    }
    ctx.flight_recorder.record_wire_state(wire.wire_state);
    let mut notifs = Vec::new();
    if session_owns_char_notifications(ctx, session) {
        if let Some(cid) = session.character_id {
            for n in ctx.drain_notifications_for_char(cid) {
                notifs.push((n.event, Some(n.id_type), n.args));
            }
        }
    }
    for (ev, args) in session.drain_notifications() {
        if !notifs.iter().any(|(e, _, a)| e == &ev && a == &args) {
            notifs.push((ev, None, args));
        }
    }
    for (event, id_type, args) in notifs {
        if sess_change_sent && event == "OnSessionChanged" {
            continue;
        }
        if let Some(tn) = build_typed_notification(ctx, event, id_type.as_deref(), args) {
            if let Some(drop_ev) = wire.wire_state.gate_notification(&tn.event, &tn.args) {
                record_sequencer_events(ctx, session.session_id, &[drop_ev], None, false);
                continue;
            }
            if let Some(service) = tn.service {
                out_frames.push(crate::outbound_sequencer::OutFrame::ServiceCall {
                    service,
                    method: tn.event,
                    args: tn.args,
                });
            } else {
                out_frames.push(crate::outbound_sequencer::OutFrame::Notification {
                    event: tn.event,
                    args: tn.args,
                });
            }
        }
    }
    let wire_packets = out_frames
        .iter()
        .map(|frame| {
            out_frame_to_wire_pyrep(
                MACHO_PROXY_NODE_ID,
                wire.client_id,
                session.user_id,
                frame,
            )
        })
        .collect();
    (out_frames, wire_packets)
}

#[cfg(test)]
#[path = "macho_listener_tests.rs"]
mod macho_listener_tests;
#[cfg(test)] #[path = "macho_listener_extra_tests.rs"] mod macho_listener_extra_tests;
#[cfg(test)]
#[path = "macho_listener_preauth_bounds_tests.rs"]
mod macho_listener_preauth_bounds_tests;

