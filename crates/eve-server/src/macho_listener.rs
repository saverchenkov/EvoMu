//! Native V24.01 `machoNet` / `GPSTransport` TCP listener & connection state machine (`P-6`).
//!
//! Implements the full 4-step `GPSTransport` handshake over length-prefixed `blue.marshal` TCP frames:
//! 1. `LowLevelVersionExchange` server 7-tuple (`(170472, macho_version=496, user_count, 24.01, build=3624425, "V24.01@ccp", update_info=None)`), matching the installed client's `start.ini` / `eveMachoNetVersion`
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
    GPSTransportClosed, HandshakeIdentity, HandshakeStepOutput, MachoAddress,
    MachoHandshakeStateMachine, MachoPacket, MachoPacketType, SessionState, VersionExchange,
    MACHO_PROXY_NODE_ID,
};
use eve_proto::{marshal, BridgeRpcError, PyRep};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, warn};

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

/// Maps a `CallReq` dispatch failure into a V24 `machoNet` `ErrorResponse` body, preserving
/// client-handled `UserError` keys (including `InsureShip*` and `DockingApproach` prompts)
/// instead of collapsing them into `"CustomInfo"`.
pub(crate) fn map_call_req_error_body(
    service: &str,
    method: &str,
    err: &BridgeRpcError,
) -> PyRep {
    let err_msg = format!("{}: {}", err.code, err.message);
    if matches!(
        err.code.as_str(),
        "UnimplementedRPC" | "InternalError" | "DatabasePersistenceError"
    ) {
        eve_machonet::wrapped_runtime_error_body(MachoPacketType::CallReq as i64, &err_msg)
    } else if matches!(
        err.code.as_str(),
        "FakeItemNotFound" | "CharacterNotAtStation"
    ) {
        eve_machonet::wrapped_runtime_error_body(MachoPacketType::CallReq as i64, &err.code)
    } else if matches!(
        err.code.as_str(),
        "InvalidBoundObject" | "BoundObjectNotOwned" | "UnMachoDestination"
    ) {
        eve_machonet::wrapped_named_user_error_body(
            MachoPacketType::CallReq as i64,
            "UnMachoDestination",
            &err_msg,
        )
    } else if err.code == "SlashError" {
        eve_machonet::wrapped_slash_error_body(MachoPacketType::CallReq as i64, &err.message)
    } else if matches!(
        err.code.as_str(),
        "UnknownSlashCommand" | "InvalidSlashArgument" | "InvalidSlashArguments"
    ) || (matches!(service.trim(), "slash" | "publicQaToolsServer") && method == "SlashCmd")
    {
        eve_machonet::wrapped_slash_error_body(MachoPacketType::CallReq as i64, &err_msg)
    } else if matches!(
        err.code.as_str(),
        "NotEnoughCargoSpace"
            | "NotEnoughCargoSpaceOverload"
            | "NotEnoughDroneBaySpace"
            | "NotEnoughDroneBaySpaceOverload"
            | "NotEnoughFighterBaySpace"
            | "NotEnoughSpecialBaySpace"
            | "NotEnoughSpecialBaySpaceOverload"
            | "NotEnoughSpace"
            | "NotEnoughChargeSpace"
            | "NoSpaceForThat"
            | "NoSpaceForThatOverload"
    ) {
        let parse_val = |prefix: &str| -> Option<f64> {
            let start = err.message.find(prefix)? + prefix.len();
            let rest = &err.message[start..];
            let end = rest.find(',').unwrap_or(rest.len());
            rest[..end].trim().parse::<f64>().ok()
        };
        let free = parse_val("free=").unwrap_or(0.0);
        let required = parse_val("required=").unwrap_or(0.0);
        eve_machonet::wrapped_user_error_with_dict_body(
            MachoPacketType::CallReq as i64,
            err.code.trim(),
            vec![
                (PyRep::ascii_bytes("free"), PyRep::Real(free)),
                (PyRep::ascii_bytes("required"), PyRep::Real(required)),
            ],
        )
    } else if err.code == "MaxOpenOrdersReached" {
        let (cur_cnt, max_cnt) = err
            .message
            .rsplit_once('(')
            .and_then(|(_, rest)| rest.strip_suffix(')'))
            .and_then(|pair| pair.split_once('/'))
            .and_then(|(c, m)| {
                Some((
                    c.trim().parse::<i64>().ok()?,
                    m.trim().parse::<i64>().ok()?,
                ))
            })
            .unwrap_or((0, 0));
        eve_machonet::wrapped_user_error_with_dict_body(
            MachoPacketType::CallReq as i64,
            "MarketExceededOrderCount",
            vec![
                (PyRep::ascii_bytes("curCnt"), PyRep::Integer(cur_cnt)),
                (PyRep::ascii_bytes("maxCnt"), PyRep::Integer(max_cnt)),
            ],
        )
    } else if err.code == "InsureShipFailedSingleContract" {
        let owner_id = err
            .message
            .split_once("ownerName=")
            .and_then(|(_, rest)| {
                let digits: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '-')
                    .collect();
                digits.parse::<i64>().ok()
            })
            .or_else(|| err.message.trim().parse::<i64>().ok())
            .filter(|&id| id > 0)
            .unwrap_or(90_000_001);
        eve_machonet::wrapped_user_error_with_dict_body(
            MachoPacketType::CallReq as i64,
            "InsureShipFailedSingleContract",
            vec![(PyRep::ascii_bytes("ownerName"), PyRep::Integer(owner_id))],
        )
    } else if matches!(
        err.code.as_str(),
        "InsureShipInvalidCost" | "InsureShipFailed"
    ) {
        eve_machonet::wrapped_user_error_with_dict_body(
            MachoPacketType::CallReq as i64,
            err.code.trim(),
            vec![],
        )
    } else if matches!(
        err.code.as_str(),
        "OutsideDockingPerimeter" | "DockingApproach"
    ) {
        eve_machonet::wrapped_user_error_with_dict_body(
            MachoPacketType::CallReq as i64,
            "DockingApproach",
            vec![],
        )
    } else {
        eve_machonet::wrapped_user_error_body(MachoPacketType::CallReq as i64, &err_msg)
    }
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

pub(crate) async fn write_framed_pyrep_with_timeout<W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    rep: &PyRep,
    write_timeout: std::time::Duration,
) -> Result<()> {
    let bytes = marshal::encode(rep);
    tokio::time::timeout(write_timeout, async {
        stream.write_u32_le(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
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
    Ok(())
}

async fn write_out_frames<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    proxy_node_id: u64,
    client_id: u64,
    user_id: Option<u64>,
    frames: &[crate::outbound_sequencer::OutFrame],
) -> Result<()> {
    use crate::outbound_sequencer::OutFrame;
    for frame in frames {
        match frame {
            OutFrame::CallRsp { frame, .. } | OutFrame::SessionChange { frame } => {
                write_framed_pyrep(writer, frame).await?;
            }
            OutFrame::DoDestinyUpdate {
                updates,
                wait_for_bubble,
                ..
            } => {
                let pkt = MachoPacket::new_broadcast_notification(
                    proxy_node_id,
                    client_id,
                    user_id,
                    "DoDestinyUpdate",
                    vec![updates.clone(), PyRep::Bool(*wait_for_bubble)],
                );
                write_framed_pyrep(writer, &pkt.to_pyrep()).await?;
            }
            OutFrame::Notification { event, args } => {
                let pkt = MachoPacket::new_broadcast_notification(
                    proxy_node_id,
                    client_id,
                    user_id,
                    event,
                    args.clone(),
                );
                write_framed_pyrep(writer, &pkt.to_pyrep()).await?;
            }
        }
    }
    Ok(())
}

fn build_destiny_batch(
    ctx: &ServerContext,
    epoch: u32,
    stamp: u64,
    events: &[eve_destiny::DestinyEvent],
    updates_pyrep: PyRep,
) -> Option<crate::outbound_sequencer::DestinyBatch> {
    if events.is_empty() {
        return None;
    }
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
    Some(crate::outbound_sequencer::DestinyBatch {
        epoch,
        stamp: stamp as u32,
        has_set_state,
        updates,
        wait_for_bubble,
    })
}

fn build_typed_notification(
    ctx: &ServerContext,
    event: String,
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
    })
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
    let mut handshake =
        MachoHandshakeStateMachine::new(ver, MACHO_PROXY_NODE_ID, session_id, client_id);
    handshake.set_config_vals(crate::bug_reporting::build_client_global_config(
        &ctx,
        local_ip.as_deref(),
    ));

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(session_id, close_tx);
    let mut session_guard = crate::router::SessionCleanupGuard::new(Arc::clone(&ctx), session_id);

    // Step 1: Server immediately sends `LowLevelVersionExchange` upon TCP connection accept.
    if let Err(e) = write_framed_pyrep(&mut stream, &handshake.server_hello()).await {
        return Err(e);
    }

    let mut sys_rx = ctx.systems.get_or_spawn(30000142).subscribe();
    let mut subscribed_sys_id: Option<u32> = None;
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut wire_state = crate::outbound_sequencer::SessionWireState::new(session_id, client_id);

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

    loop {
        tokio::select! {
            Some((reason_code, reason)) = close_rx.recv() => {
                let closed = GPSTransportClosed::new(reason, reason_code);
                let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                break;
            }
            tick_res = sys_rx.recv(), if !matches!(wire_state.destiny, crate::outbound_sequencer::DestinyStream::Detached) => {
                match tick_res {
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!("machoNet session {session_id} sys_rx lagged by {skipped} ticks; resyncing SetState (ORD-5)");
                        wire_state.on_broadcast_lagged();
                        if session.character_id.is_some()
                            && session.station_id.is_none()
                            && session.structure_id.is_none()
                        {
                            if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                                let _ = sys_handle.push_set_state(ship_id).await;
                                if let Ok((tick, obs_evs, updates_pyrep)) = sys_handle.drain_observer_updates(ship_id).await {
                                    let ep = wire_state.current_epoch();
                                    if let Some(batch) = build_destiny_batch(&ctx, ep, tick, &obs_evs, updates_pyrep) {
                                        if let Some(frame) = crate::outbound_sequencer::sequence_tick(
                                            &mut wire_state,
                                            batch,
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
                                }
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                    Ok(tick_msg) => {
                        if session.character_id.is_some()
                            && session.station_id.is_none()
                            && session.structure_id.is_none()
                            && session.solar_system_id == Some(tick_msg.solar_system_id)
                        {
                            if let Some(ship_id) = session.ship_id {
                                if let Some(updates_pyrep) = tick_msg.observer_updates_pyrep.get(&ship_id) {
                                    let evs = tick_msg
                                        .observer_events
                                        .get(&ship_id)
                                        .map(Vec::as_slice)
                                        .unwrap_or(&tick_msg.events);
                                    let ep = wire_state.current_epoch();
                                    if let Some(batch) = build_destiny_batch(&ctx, ep, tick_msg.tick, evs, updates_pyrep.clone()) {
                                        if let Some(frame) = crate::outbound_sequencer::sequence_tick(
                                            &mut wire_state,
                                            batch,
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
                    if should_drain {
                        let prev_session = session.clone();
                        ctx.sync_session(&mut session);
                        if session.solar_system_id != prev_session.solar_system_id
                            || session.solar_system_id != subscribed_sys_id
                        {
                            wire_state.leave_space();
                            if let Some(new_sys) = session.solar_system_id {
                                sys_rx = ctx.systems.get_or_spawn(new_sys).subscribe();
                            }
                            subscribed_sys_id = session.solar_system_id;
                        }
                        let mut write_failed = false;
                        for notif in ctx.drain_notifications_for_char(cid) {
                            if notif.event == "OnSessionChanged" {
                                let inner_prev_sys = session.solar_system_id;
                                ctx.sync_session(&mut session);
                                if session.solar_system_id != inner_prev_sys
                                    || session.solar_system_id != subscribed_sys_id
                                {
                                    wire_state.leave_space();
                                    if let Some(new_sys) = session.solar_system_id {
                                        sys_rx = ctx.systems.get_or_spawn(new_sys).subscribe();
                                    }
                                    subscribed_sys_id = session.solar_system_id;
                                }
                                if session.station_id.is_some() || session.structure_id.is_some() {
                                    wire_state.leave_space();
                                }
                                continue;
                            }
                            if let Some(typed_notif) = build_typed_notification(&ctx, notif.event, notif.args) {
                                let notif_pkt = MachoPacket::new_broadcast_notification(
                                    MACHO_PROXY_NODE_ID,
                                    client_id,
                                    session.user_id,
                                    &typed_notif.event,
                                    typed_notif.args,
                                );
                                if write_framed_pyrep(&mut writer, &notif_pkt.to_pyrep()).await.is_err() {
                                    write_failed = true;
                                    break;
                                }
                            }
                        }
                        if write_failed {
                            break;
                        }
                        if session.solar_system_id != prev_session.solar_system_id
                            || session.solar_system_id != subscribed_sys_id
                        {
                            wire_state.leave_space();
                            if let Some(new_sys) = session.solar_system_id {
                                sys_rx = ctx.systems.get_or_spawn(new_sys).subscribe();
                            }
                            subscribed_sys_id = session.solar_system_id;
                        }
                        if session.station_id.is_some() || session.structure_id.is_some() {
                            wire_state.leave_space();
                        }
                        if let Some(sess_change_payload) = prev_session
                            .to_session_change_pyrep(&session, vec![MACHO_PROXY_NODE_ID as i64])
                        {
                            let mut named_payload = HashMap::new();
                            named_payload
                                .insert("channel".into(), PyRep::ascii_bytes("sessionchange"));
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
                                user_id: session.user_id,
                                payload: sess_change_payload,
                                named_payload,
                            };
                            if write_framed_pyrep(&mut writer, &sess_pkt.to_pyrep()).await.is_err() {
                                break;
                            }
                            if session.station_id.is_none()
                                && session.structure_id.is_none()
                                && session.ship_id != prev_session.ship_id
                            {
                                if let (Some(new_ship_id), Some(sys_id)) =
                                    (session.ship_id, session.solar_system_id)
                                {
                                    let sys_handle = ctx.systems.get_or_spawn(sys_id);
                                    if let Ok((mut tick, mut events, mut updates_pyrep)) =
                                        sys_handle.drain_observer_updates(new_ship_id).await
                                    {
                                        let has_set_state = events.iter().any(|e| {
                                            matches!(e, eve_destiny::DestinyEvent::SetState { .. })
                                        });
                                        if !matches!(
                                            wire_state.destiny,
                                            crate::outbound_sequencer::DestinyStream::Detached
                                        ) && !has_set_state
                                        {
                                            if sys_handle.push_set_state(new_ship_id).await.is_ok() {
                                                if let Ok((t2, ev2, up2)) =
                                                    sys_handle.drain_observer_updates(new_ship_id).await
                                                {
                                                    tick = t2;
                                                    events = ev2;
                                                    updates_pyrep = up2;
                                                }
                                            }
                                        }
                                        let ep = wire_state.ensure_in_space(session.solar_system_id);
                                        if let Some(batch) =
                                            build_destiny_batch(&ctx, ep, tick, &events, updates_pyrep)
                                        {
                                            if let Some(frame) = crate::outbound_sequencer::sequence_tick(
                                                &mut wire_state,
                                                batch,
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
                                    }
                                }
                            }
                        }
                    }
                }
            }
            frame_opt = frame_rx.recv() => {
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
                        warn!("Failed to decode marshal payload on machoNet session {session_id}: {e}");
                        let closed = GPSTransportClosed::new(
                            format!("Invalid marshal frame: {e}"),
                            "HANDSHAKE_INCOMPATIBLEPROTOCOL",
                        );
                        let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                        break;
                    }
                };

                if !handshake.is_authenticated() {
                    match handshake.step(&pyrep) {
                        Err(closed) => {
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
                                    let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                                    break;
                                }
                            }
                        }
                        Ok(HandshakeStepOutput::Authenticated { frames, identity }) => {
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
                                        if d.session_id != session_id {
                                            if let Some(disp_cid) = d.character_id {
                                                ctx.logoff_character_sync(disp_cid, None);
                                            }
                                            ctx.disconnect_session(
                                                d.session_id,
                                                "ACL_DISPLACED",
                                                "Displaced by concurrent login on same account",
                                            );
                                            ctx.release_session_bindings(d.session_id);
                                        }
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
                            is_authenticated_flag.store(true, Ordering::Release);
                            preauth_guard.complete_auth();

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

                // Handshake is complete (`Authenticated`): only valid `MachoPacket`s are processed now.
                let Some(pkt) = MachoPacket::from_pyrep(&pyrep) else {
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

                let (_released_cnt, is_dedicated_release_notif) =
                    ctx.process_client_bound_object_releases(session_id, &pkt);
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
                        let mut pre_rpc_notifs = session
                            .character_id
                            .map(|cid| ctx.drain_notifications_for_char(cid))
                            .unwrap_or_default();
                        let prev_session = session.clone();
                        let rpc_res = ctx
                            .dispatch_rpc(
                                &mut session,
                                &service,
                                bound_id,
                                &method,
                                &call_args,
                                &call_kwargs,
                                Some("machoNet:26000"),
                            )
                            .await;
                        if let Some(cid) = session.character_id {
                            for notif in ctx.drain_notifications_for_char(cid) {
                                if !session
                                    .notifications
                                    .iter()
                                    .any(|(ev, a)| ev == &notif.event && a == &notif.args)
                                {
                                    pre_rpc_notifs.push(notif);
                                }
                            }
                        }
                        if service == "timeControl"
                            || pre_rpc_notifs.iter().any(|n| n.event == "OnSessionChanged")
                            || session
                                .notifications
                                .iter()
                                .any(|(ev, _)| ev == "OnSessionChanged")
                        {
                            ctx.sync_session(&mut session);
                        }

                        if session.solar_system_id != prev_session.solar_system_id
                            || session.solar_system_id != subscribed_sys_id
                        {
                            wire_state.leave_space();
                            if let Some(new_sys) = session.solar_system_id {
                                sys_rx = ctx.systems.get_or_spawn(new_sys).subscribe();
                            }
                            subscribed_sys_id = session.solar_system_id;
                        }
                        let was_live = matches!(wire_state.destiny, DestinyStream::Live { .. });
                        let is_ballpark_init_rpc = (method == "UpdateStateRequest"
                            || (service == "beyonce" && method == "MachoBindObject"))
                            && session.station_id.is_none()
                            && session.structure_id.is_none();
                        if is_ballpark_init_rpc && !was_live {
                            // Drain any pre-bind 1Hz ticks buffered in `sys_rx` between `SelectCharacterID`
                            // and `beyonce.MachoBindObject` / `UpdateStateRequest` so stale ticks (e.g. tick N-1)
                            // are never flushed out-of-order after the synchronous post-bind `SetState` drain (tick N).
                            let _ = drain_sys_rx_discarding_lag(&mut sys_rx);
                            wire_state.ensure_in_space(session.solar_system_id);
                        }

                        let rsp_pkt = match rpc_res {
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
                                MachoPacket {
                                    packet_type: MachoPacketType::CallRsp,
                                    source: reply_source_address(&pkt.destination),
                                    destination: pkt.source.clone(),
                                    user_id: session.user_id,
                                    payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(val))]),
                                    named_payload,
                                }
                            }
                            Err(mut err) => {
                                if err.code == "InsureShipFailedSingleContract"
                                    && !err.message.contains("ownerName=")
                                {
                                    if let Some(cid) = session.character_id {
                                        err.message = format!(
                                            "InsureShipFailedSingleContract (ownerName={cid})"
                                        );
                                    }
                                }
                                let payload = map_call_req_error_body(&service, &method, &err);
                                MachoPacket {
                                    packet_type: MachoPacketType::ErrorResponse,
                                    source: reply_source_address(&pkt.destination),
                                    destination: pkt.source.clone(),
                                    user_id: session.user_id,
                                    payload,
                                    named_payload: HashMap::new(),
                                }
                            }
                        };

                        let was_in_space = prev_session.station_id.is_none()
                            && prev_session.structure_id.is_none();
                        let is_in_space =
                            session.station_id.is_none() && session.structure_id.is_none();
                        let docking_transition = was_in_space && !is_in_space;

                        let mut pre_dock_destiny = None;
                        if !matches!(wire_state.destiny, DestinyStream::Detached)
                            && docking_transition
                        {
                            if let (Some(sys_id), Some(ship_id)) = (
                                session.solar_system_id.or(prev_session.solar_system_id),
                                session.ship_id.or(prev_session.ship_id),
                            ) {
                                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                                if let Ok((tick, obs_evs, updates_pyrep)) =
                                    sys_handle.drain_observer_updates(ship_id).await
                                {
                                    let ep = wire_state.current_epoch();
                                    pre_dock_destiny = build_destiny_batch(
                                        &ctx,
                                        ep,
                                        tick,
                                        &obs_evs,
                                        updates_pyrep,
                                    );
                                }
                            }
                        }

                        if docking_transition {
                            if let Some(sys_id) =
                                session.solar_system_id.or(prev_session.solar_system_id)
                            {
                                let _ = ctx
                                    .systems
                                    .get_or_spawn(sys_id)
                                    .flush_peer_observer_updates(
                                        session.ship_id.or(prev_session.ship_id),
                                    )
                                    .await;
                            }
                        } else if was_in_space
                            && prev_session.solar_system_id != session.solar_system_id
                        {
                            if let Some(old_sys_id) = prev_session.solar_system_id {
                                let _ = ctx
                                    .systems
                                    .get_or_spawn(old_sys_id)
                                    .flush_peer_observer_updates(prev_session.ship_id)
                                    .await;
                            }
                        }

                        let session_change_frame = prev_session
                            .to_session_change_pyrep(&session, vec![MACHO_PROXY_NODE_ID as i64])
                            .map(|mut sess_change_payload| {
                                enrich_session_change_scoped_roles(
                                    &ctx,
                                    &prev_session,
                                    &session,
                                    &mut sess_change_payload,
                                );
                                let mut named_payload = HashMap::new();
                                named_payload
                                    .insert("channel".into(), PyRep::ascii_bytes("sessionchange"));
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
                                    user_id: session.user_id,
                                    payload: sess_change_payload,
                                    named_payload,
                                };
                                sess_pkt.to_pyrep()
                            });

                        let mut pending_tick_batches = Vec::new();
                        let mut post_rpc_direct_batch = None;
                        let mut effective_was_live = was_live;
                        if !matches!(wire_state.destiny, DestinyStream::Detached)
                            && !docking_transition
                            && (was_in_space || is_in_space)
                        {
                            if let (Some(sys_id), Some(ship_id)) = (
                                session.solar_system_id.or(prev_session.solar_system_id),
                                session.ship_id.or(prev_session.ship_id),
                            ) {
                                let mut ep = wire_state.current_epoch();
                                // Drain any buffered 1Hz tick broadcasts that occurred before this RPC's
                                // synchronous observer drain so ticks older than the direct drain are
                                // never discarded (`#844`), and explicitly recover from `Lagged` (`ORD-5`).
                                if effective_was_live {
                                    loop {
                                        match sys_rx.try_recv() {
                                            Ok(tick_out) => {
                                                if !effective_was_live {
                                                    continue;
                                                }
                                                if tick_out.solar_system_id == sys_id {
                                                    if let Some(updates_pyrep) =
                                                        tick_out.observer_updates_pyrep.get(&ship_id)
                                                    {
                                                        let evs = tick_out
                                                            .observer_events
                                                            .get(&ship_id)
                                                            .map(Vec::as_slice)
                                                            .unwrap_or(&tick_out.events);
                                                        if let Some(batch) = build_destiny_batch(
                                                            &ctx,
                                                            ep,
                                                            tick_out.tick,
                                                            evs,
                                                            updates_pyrep.clone(),
                                                        ) {
                                                            pending_tick_batches.push(batch);
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
                                                pending_tick_batches.clear();
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
                                }

                                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                                if let Ok((mut tick, mut obs_evs, mut updates_pyrep)) =
                                    sys_handle.drain_observer_updates(ship_id).await
                                {
                                    let mut has_set_state = obs_evs
                                        .iter()
                                        .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. }));
                                    if !effective_was_live && !has_set_state {
                                        // Either this is a ballpark-init RPC, a `Lagged` resync (`ORD-5`), or a
                                        // concurrent 1Hz tick drained `SetState` between `MachoBindObject` and
                                        // this post-bind drain (`#845`): synthesize `SetState` so the stream
                                        // transitions cleanly into `Live`.
                                        let _ = sys_handle.push_set_state(ship_id).await;
                                        if let Ok((retry_tick, retry_evs, retry_pyrep)) =
                                            sys_handle.drain_observer_updates(ship_id).await
                                        {
                                            tick = retry_tick;
                                            obs_evs = retry_evs;
                                            updates_pyrep = retry_pyrep;
                                            has_set_state = obs_evs
                                                .iter()
                                                .any(|ev| matches!(ev, eve_destiny::DestinyEvent::SetState { .. }));
                                        }
                                        let _ = drain_sys_rx_discarding_lag(&mut sys_rx);
                                    }
                                    if !obs_evs.is_empty() {
                                        if !effective_was_live && has_set_state {
                                            let _ = drain_sys_rx_discarding_lag(&mut sys_rx);
                                        }
                                        post_rpc_direct_batch =
                                            build_destiny_batch(&ctx, ep, tick, &obs_evs, updates_pyrep);
                                    }
                                }
                            }
                        }
                        if !docking_transition && (was_in_space || is_in_space) {
                            if let Some(sys_id) =
                                session.solar_system_id.or(prev_session.solar_system_id)
                            {
                                let _ = ctx
                                    .systems
                                    .get_or_spawn(sys_id)
                                    .flush_peer_observer_updates(
                                        session.ship_id.or(prev_session.ship_id),
                                    )
                                    .await;
                            }
                        }
                        if !is_in_space && !docking_transition {
                            wire_state.leave_space();
                        }

                        if let Some(cid) = session.character_id {
                            for notif in ctx.drain_notifications_for_char(cid) {
                                if !session
                                    .notifications
                                    .iter()
                                    .any(|(ev, a)| ev == &notif.event && a == &notif.args)
                                {
                                    pre_rpc_notifs.push(notif);
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
                            if let Some(tn) =
                                build_typed_notification(&ctx, notif.event, notif.args)
                            {
                                typed_notifications.push(tn);
                            }
                        }
                        for (event, args) in session.drain_notifications() {
                            if sess_change_sent && event == "OnSessionChanged" {
                                continue;
                            }
                            if let Some(tn) = build_typed_notification(&ctx, event, args) {
                                typed_notifications.push(tn);
                            }
                        }

                        let destiny_drain = if docking_transition {
                            DestinyDrain::Detach {
                                pre_detach: pre_dock_destiny,
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

                        let out_frames = crate::outbound_sequencer::sequence(
                            &mut wire_state,
                            RpcCompletion {
                                call_id: 0,
                                response_frame: rsp_pkt.to_pyrep(),
                                session_change_frame,
                                pending_tick_batches,
                                destiny_drain,
                                notifications: typed_notifications,
                            },
                        );
                        if write_out_frames(
                            &mut writer,
                            MACHO_PROXY_NODE_ID,
                            client_id,
                            session.user_id,
                            &out_frames,
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

    reader_task.abort();
    ctx.cleanup_disconnected_client_session(&mut session).await;

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

#[cfg(test)]
#[path = "macho_listener_tests.rs"]
mod macho_listener_tests;
#[cfg(test)] #[path = "macho_listener_extra_tests.rs"] mod macho_listener_extra_tests;
#[cfg(test)]
#[path = "macho_listener_preauth_bounds_tests.rs"]
mod macho_listener_preauth_bounds_tests;

