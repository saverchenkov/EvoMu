//! `eve-server` — Main entry point for the `EvoMu` EVE Online server emulator.
//!
//! Exposes:
//! - TCP Port `26002`: Modern MessagePack/JSON Client Bridge listener (`evomu_bridge.py`).
//! - TCP Port `26000`: Legacy `eveMarshal` / `machoNet` handshake & packet listener.
//! - TCP Port `26004`: ESI / SSO HTTP listener (when `feature_flags.enable_esi_http` is enabled).

#![forbid(unsafe_code)]

use anyhow::{bail, Result};
use eve_machonet::{MachoAddress, MachoPacket, MachoPacketType, SessionState, VersionExchange};
use eve_proto::{marshal, BridgeEnvelope, PyRep, MAX_FRAME_SIZE};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::time::timeout;
use tracing::{error, info, warn};

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);
const MAX_CONCURRENT_CONNECTIONS: usize = 512;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "eve_server=info,eve_destiny=info".into()),
        )
        .init();

    let cfg = eve_server::config::ServerConfig::load_default_or_env()
        .map_err(|e| anyhow::anyhow!(e))?;
    let ctx = Arc::new(ServerContext::from_config(&cfg).map_err(|e| anyhow::anyhow!(e))?);
    if let Ok(admin_token) = std::env::var("EVOMU_ADMIN_TOKEN") {
        if !admin_token.trim().is_empty() {
            ctx.db
                .provision_gm_account("admin", admin_token.trim(), 90_000_003, "ISD Polaris");
            info!("Provisioned GM account 'admin' via EVOMU_ADMIN_TOKEN");
        }
    }
    let conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    // Pre-spawn Jita (30000142) solar system actor inside the ServerContext registry
    let _jita = ctx.systems.get_or_spawn(30000142);

    info!(
        loaded_types = ctx.sde.types.len(),
        loaded_systems = ctx.sde.solar_systems.len(),
        sde_path = %cfg.sde_path,
        world_db_path = %cfg.world_db_path,
        esi_port = cfg.esi_port,
        enable_quasar_gateway = cfg.feature_flags.enable_quasar_gateway,
        enable_esi_http = cfg.feature_flags.enable_esi_http,
        enable_living_world = cfg.feature_flags.enable_living_world,
        "Initialized EvoMu Simulation Core & SDE Registry"
    );

    let bridge_listener =
        TcpListener::bind((cfg.bind_address.as_str(), cfg.bridge_port)).await?;
    let macho_listener =
        TcpListener::bind((cfg.bind_address.as_str(), cfg.macho_port)).await?;
    let esi_listener = if cfg.feature_flags.enable_esi_http {
        let l = TcpListener::bind((cfg.bind_address.as_str(), cfg.esi_port)).await?;
        info!(
            bind_address = %cfg.bind_address,
            esi_port = cfg.esi_port,
            "Listening on ESI / SSO HTTP TCP"
        );
        Some(l)
    } else {
        None
    };

    info!(
        bind_address = %cfg.bind_address,
        bridge_port = cfg.bridge_port,
        "Listening on Python Client Bridge (JSON/MessagePack)"
    );
    info!(
        bind_address = %cfg.bind_address,
        macho_port = cfg.macho_port,
        "Listening on Legacy machoNet / eveMarshal TCP"
    );

    loop {
        tokio::select! {
            Ok((stream, addr)) = bridge_listener.accept() => {
                let Ok(permit) = Arc::clone(&conn_limiter).try_acquire_owned() else {
                    warn!(%addr, "Connection limit reached; rejecting Client Bridge connection");
                    continue;
                };
                let ctx_clone = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let _permit = permit;
                    info!(%addr, "Accepted Client Bridge connection");
                    if let Err(e) = handle_bridge_client(stream, ctx_clone).await {
                        error!(%addr, error = %e, "Client Bridge connection closed with error");
                    }
                });
            }
            Ok((stream, addr)) = macho_listener.accept() => {
                let Ok(permit) = Arc::clone(&conn_limiter).try_acquire_owned() else {
                    warn!(%addr, "Connection limit reached; rejecting machoNet connection");
                    continue;
                };
                let ctx_clone = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let _permit = permit;
                    info!(%addr, "Accepted legacy machoNet connection");
                    if let Err(e) = handle_macho_client(stream, ctx_clone).await {
                        error!(%addr, error = %e, "machoNet connection closed with error");
                    }
                });
            }
            Ok((mut stream, addr)) = async {
                match &esi_listener {
                    Some(l) => l.accept().await,
                    None => std::future::pending().await,
                }
            } => {
                tokio::spawn(async move {
                    let body = b"{\"status\":\"esi_listener_active\",\"esi_phase\":\"pending\"}\n";
                    let header = format!(
                        "HTTP/1.1 501 Not Implemented\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(body).await;
                    info!(%addr, "Served ESI listener status response");
                });
            }
        }
    }
}

/// Reads a length-prefixed frame with strict `MAX_FRAME_SIZE` validation BEFORE allocating memory.
/// Waiting for the first byte of a new frame has a 5-minute idle timeout (`IDLE_CONNECTION_TIMEOUT`)
/// so abandoned sockets cannot hold a connection slot indefinitely; once the first header byte arrives,
/// the remaining header and body must complete within `IO_TIMEOUT`.
async fn read_bounded_frame<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut R,
) -> Result<Option<Vec<u8>>> {
    const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(300);
    let mut len_buf = [0u8; 4];
    match timeout(IDLE_CONNECTION_TIMEOUT, stream.read_exact(&mut len_buf[..1])).await? {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    timeout(IO_TIMEOUT, stream.read_exact(&mut len_buf[1..])).await??;

    let frame_len = u32::from_le_bytes(len_buf) as usize;
    if frame_len == 0 {
        bail!("Zero-length frame is invalid");
    }
    if frame_len > MAX_FRAME_SIZE {
        bail!(
            "Frame length {} exceeds MAX_FRAME_SIZE ({})",
            frame_len,
            MAX_FRAME_SIZE
        );
    }

    let mut payload = vec![0u8; frame_len];
    timeout(IO_TIMEOUT, stream.read_exact(&mut payload)).await??;
    Ok(Some(payload))
}

async fn write_bounded_frame<W: tokio::io::AsyncWrite + Unpin>(
    stream: &mut W,
    payload: &[u8],
) -> Result<()> {
    if payload.len() > MAX_FRAME_SIZE {
        bail!("Outbound frame exceeds MAX_FRAME_SIZE");
    }
    timeout(IO_TIMEOUT, async {
        stream
            .write_all(&(payload.len() as u32).to_le_bytes())
            .await?;
        stream.write_all(payload).await
    })
    .await??;
    Ok(())
}

/// Handles connections from the Python Client Bridge (`evomu_bridge.py`) using length-prefixed JSON.
/// Uses a dedicated reader task feeding a cancel-safe `mpsc::channel` so 1 Hz `sys_rx` broadcast ticks
/// inside `tokio::select!` can never cancel an in-flight `read_exact` call.
async fn handle_bridge_client(stream: TcpStream, ctx: Arc<ServerContext>) -> Result<()> {
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    // New sessions start strictly unauthenticated (`user_id: None`, `role: 0`)
    let mut session = SessionState::new(session_id);
    let mut sys_rx = ctx.systems.get_or_spawn(30000142).subscribe();

    let (mut reader, mut writer) = stream.into_split();
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>>>(16);
    let reader_task = tokio::spawn(async move {
        loop {
            match read_bounded_frame(&mut reader).await {
                Ok(Some(payload)) => {
                    if frame_tx.send(Ok(payload)).await.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = frame_tx.send(Err(e)).await;
                    break;
                }
            }
        }
    });

    let res: Result<()> = async {
        loop {
            tokio::select! {
                frame_opt = frame_rx.recv() => {
                    let Some(frame_res) = frame_opt else {
                        break;
                    };
                    let payload = frame_res?;
                    let envelope: BridgeEnvelope = serde_json::from_slice(&payload)?;
                    if let BridgeEnvelope::RpcRequest {
                        req_id,
                        service,
                        bound_id,
                        method,
                        args,
                        kwargs,
                        caller_trace,
                    } = envelope
                    {
                        let prev_session = session.clone();
                        let result = ctx
                            .dispatch_rpc(
                                &mut session,
                                &service,
                                bound_id,
                                &method,
                                &args,
                                &kwargs,
                                caller_trace.as_deref(),
                            )
                            .await;
                        if session.solar_system_id != prev_session.solar_system_id {
                            if let Some(new_sys) = session.solar_system_id {
                                let handle = ctx.systems.get_or_spawn(new_sys);
                                info!(solar_system_id = handle.solar_system_id, "Subscribed session to solar system broadcast");
                                sys_rx = handle.subscribe();
                            }
                        }
                        let response = BridgeEnvelope::RpcResponse { req_id, result };
                        let out_bytes = serde_json::to_vec(&response)?;
                        write_bounded_frame(&mut writer, &out_bytes).await?;

                        // Emit OnSessionChanged whenever an RPC mutated session state
                        let diff = prev_session.compute_diff(&session);
                        if !diff.is_empty() {
                            let changes_dict = PyRep::Dict(
                                diff.into_iter()
                                    .map(|(k, (old_v, new_v))| {
                                        (PyRep::String(k), PyRep::Tuple(vec![old_v, new_v]))
                                    })
                                    .collect(),
                            );
                            let session_event = BridgeEnvelope::ServerEvent {
                                tick: 0,
                                event: "OnSessionChanged".to_string(),
                                args: vec![PyRep::Bool(false), changes_dict],
                            };
                            let sess_bytes = serde_json::to_vec(&session_event)?;
                            write_bounded_frame(&mut writer, &sess_bytes).await?;
                        }
                    }
                }
                Ok(tick_msg) = sys_rx.recv() => {
                    if session.character_id.is_some()
                        && session.station_id.is_none()
                        && session.solar_system_id == Some(tick_msg.solar_system_id)
                        && !tick_msg.events.is_empty()
                    {
                        let event_json = serde_json::to_string(&tick_msg.events)?;
                        let server_event = BridgeEnvelope::ServerEvent {
                            tick: tick_msg.tick,
                            event: "DoDestinyUpdate".to_string(),
                            args: vec![PyRep::String(event_json)],
                        };
                        let out_bytes = serde_json::to_vec(&server_event)?;
                        write_bounded_frame(&mut writer, &out_bytes).await?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    reader_task.abort();
    if let Some(uid) = session.user_id {
        ctx.db.release_online_lock(uid, session.session_id);
    }
    ctx.release_session_bindings(session.session_id);
    res
}

/// Handles legacy `machoNet` connections on port 26000, performing the `VersionExchange`
/// handshake and routing `MachoPacket` (`CallReq`, `PingReq`) frames to `ServerContext::dispatch_rpc`.
async fn handle_macho_client(mut stream: TcpStream, ctx: Arc<ServerContext>) -> Result<()> {
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let mut session = SessionState::new(session_id);
    let ver = VersionExchange::default().to_pyrep();
    let encoded = marshal::encode(&ver);
    write_bounded_frame(&mut stream, &encoded).await?;

    let res: Result<()> = async {
        while let Some(frame) = read_bounded_frame(&mut stream).await? {
            let decoded = marshal::decode(&frame)?;

            if let Some(pkt) = MachoPacket::from_pyrep(&decoded) {
                match pkt.packet_type {
                    MachoPacketType::PingReq => {
                        let rsp = MachoPacket {
                            packet_type: MachoPacketType::PingRsp,
                            source: pkt.destination,
                            destination: pkt.source,
                            user_id: session.user_id,
                            payload: pkt.payload,
                            named_payload: HashMap::new(),
                        };
                        write_bounded_frame(&mut stream, &marshal::encode(&rsp.to_pyrep())).await?;
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

                        let rsp_pkt = match rpc_res {
                            Ok(val) => MachoPacket {
                                packet_type: MachoPacketType::CallRsp,
                                source: pkt.destination.clone(),
                                destination: pkt.source.clone(),
                                user_id: session.user_id,
                                payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(val))]),
                                named_payload: HashMap::new(),
                            },
                            Err(err) => MachoPacket {
                                packet_type: MachoPacketType::ErrorResponse,
                                source: pkt.destination.clone(),
                                destination: pkt.source.clone(),
                                user_id: session.user_id,
                                payload: PyRep::Tuple(vec![
                                    PyRep::Integer(MachoPacketType::CallReq as i64),
                                    PyRep::Integer(1),
                                    PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::keyval(
                                        [
                                            ("code", PyRep::String(err.code)),
                                            ("message", PyRep::String(err.message)),
                                        ],
                                    )))]),
                                ]),
                                named_payload: HashMap::new(),
                            },
                        };
                        write_bounded_frame(&mut stream, &marshal::encode(&rsp_pkt.to_pyrep()))
                            .await?;

                        if let Some(sess_change_payload) =
                            prev_session.to_session_change_pyrep(&session, vec![888444])
                        {
                            let sess_pkt = MachoPacket {
                                packet_type: MachoPacketType::SessionChangeNotification,
                                source: MachoAddress::Node {
                                    node_id: 888444,
                                    service: None,
                                    call_id: 0,
                                },
                                destination: MachoAddress::Client {
                                    client_id: session.session_id,
                                    call_id: 0,
                                    service: None,
                                },
                                user_id: session.user_id,
                                payload: sess_change_payload,
                                named_payload: HashMap::new(),
                            };
                            write_bounded_frame(
                                &mut stream,
                                &marshal::encode(&sess_pkt.to_pyrep()),
                            )
                            .await?;
                        }
                    }
                    _ => {
                        let ack = PyRep::None;
                        write_bounded_frame(&mut stream, &marshal::encode(&ack)).await?;
                    }
                }
                continue;
            }

            // Handle low-level VersionExchangeClient / VIPKey / QueueCheck handshake frames
            let reply = match &decoded {
                PyRep::Tuple(items) if items.len() == 6 => PyRep::Object {
                    type_name: "macho.AuthenticationReq".into(),
                    args: Box::new(PyRep::Tuple(vec![PyRep::ascii_bytes("ok")])),
                },
                _ => PyRep::None,
            };
            let reply_bytes = marshal::encode(&reply);
            write_bounded_frame(&mut stream, &reply_bytes).await?;
        }
        Ok(())
    }
    .await;

    if let Some(uid) = session.user_id {
        ctx.db.release_online_lock(uid, session.session_id);
    }
    ctx.release_session_bindings(session.session_id);
    res
}

fn extract_macho_call_req(
    payload: &PyRep,
) -> (Option<u64>, String, Vec<PyRep>, HashMap<String, PyRep>) {
    let mut bound_id = None;
    let mut method = String::new();
    let mut args = Vec::new();
    let mut kwargs = HashMap::new();

    let outer = match payload {
        PyRep::Tuple(items) => items.as_slice(),
        _ => return (bound_id, method, args, kwargs),
    };
    let Some(first) = outer.first() else {
        return (bound_id, method, args, kwargs);
    };
    let inner_call = match first {
        PyRep::Tuple(t) if t.len() >= 2 => {
            bound_id = t[0].as_i64().filter(|&v| v > 0).map(|v| v as u64);
            match &t[1] {
                PyRep::SubStream(s) => s.as_ref(),
                other => other,
            }
        }
        PyRep::SubStream(s) => s.as_ref(),
        other => other,
    };

    if let PyRep::Tuple(call_items) = inner_call {
        // Support both 4-tuple `(remoteObject, method, args, kwargs)` and 3-tuple `(method, args, kwargs)`
        let offset = if call_items.first().and_then(|v| v.as_str()).is_some() {
            0
        } else {
            1
        };
        if let Some(m) = call_items.get(offset).and_then(|v| v.as_str()) {
            method = m.to_owned();
        }
        if let Some(PyRep::Tuple(a) | PyRep::List(a)) = call_items.get(offset + 1) {
            args = a.clone();
        }
        if let Some(PyRep::Dict(entries)) = call_items.get(offset + 2) {
            for (k, v) in entries {
                if let Some(ks) = k.as_str() {
                    kwargs.insert(ks.to_owned(), v.clone());
                }
            }
        }
    }

    (bound_id, method, args, kwargs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_live_dual_protocol_tcp_listeners() -> Result<()> {
        let ctx = Arc::new(ServerContext::new());
        let bridge_listener = TcpListener::bind("127.0.0.1:0").await?;
        let bridge_addr = bridge_listener.local_addr()?;
        let macho_listener = TcpListener::bind("127.0.0.1:0").await?;
        let macho_addr = macho_listener.local_addr()?;

        let ctx_bridge = Arc::clone(&ctx);
        let bridge_task = tokio::spawn(async move {
            if let Ok((stream, _)) = bridge_listener.accept().await {
                let _ = handle_bridge_client(stream, ctx_bridge).await;
            }
        });

        let ctx_macho = Arc::clone(&ctx);
        let macho_task = tokio::spawn(async move {
            if let Ok((stream, _)) = macho_listener.accept().await {
                let _ = handle_macho_client(stream, ctx_macho).await;
            }
        });

        // 1. Mode A: Python Client Bridge TCP round-trip (Auth -> SelectCharacterID ->OnSessionChanged -> QuasarGateway -> Market)
        let mut b_stream = TcpStream::connect(bridge_addr).await?;
        let auth_req = BridgeEnvelope::RpcRequest {
            req_id: 1,
            service: "authentication".into(),
            bound_id: None,
            method: "Login".into(),
            args: vec![
                PyRep::String("demo".into()),
                PyRep::String("demo-secret".into()),
            ],
            kwargs: HashMap::new(),
            caller_trace: Some("tcp-test".into()),
        };
        write_bounded_frame(&mut b_stream, &serde_json::to_vec(&auth_req)?).await?;
        let rsp_frame = read_bounded_frame(&mut b_stream).await?.expect("auth rsp");
        let BridgeEnvelope::RpcResponse { req_id, result } = serde_json::from_slice(&rsp_frame)?
        else {
            panic!("Expected RpcResponse");
        };
        assert_eq!(req_id, 1);
        assert!(result.is_ok());

        // Also read OnSessionChanged emitted after authentication
        let sess_frame = read_bounded_frame(&mut b_stream)
            .await?
            .expect("session event");
        let BridgeEnvelope::ServerEvent { event, .. } = serde_json::from_slice(&sess_frame)?
        else {
            panic!("Expected ServerEvent");
        };
        assert_eq!(event, "OnSessionChanged");
        drop(b_stream);

        // 2. Mode B: Legacy machoNet TCP round-trip (VersionExchange -> PingReq -> CallReq)
        let mut m_stream = TcpStream::connect(macho_addr).await?;
        let ver_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("version exchange");
        let ver_pyrep = marshal::decode(&ver_frame)?;
        assert_eq!(ver_pyrep, VersionExchange::default().to_pyrep());

        let call_pkt = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id: 1,
                call_id: 42,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: 888444,
                service: Some("authentication".into()),
                call_id: 42,
            },
            user_id: None,
            payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::String("Login".into()),
                PyRep::Tuple(vec![
                    PyRep::String("demo".into()),
                    PyRep::String("demo-secret".into()),
                ]),
                PyRep::Dict(vec![]),
            ])))]),
            named_payload: HashMap::new(),
        };
        write_bounded_frame(&mut m_stream, &marshal::encode(&call_pkt.to_pyrep())).await?;
        let call_rsp_frame = read_bounded_frame(&mut m_stream).await?.expect("call rsp");
        let call_rsp_pyrep = marshal::decode(&call_rsp_frame)?;
        let parsed_rsp = MachoPacket::from_pyrep(&call_rsp_pyrep).expect("MachoPacket CallRsp");
        assert_eq!(parsed_rsp.packet_type, MachoPacketType::CallRsp);

        let sess_notif_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("session change notification");
        let sess_notif_pyrep = marshal::decode(&sess_notif_frame)?;
        let parsed_notif =
            MachoPacket::from_pyrep(&sess_notif_pyrep).expect("MachoPacket SessionChange");
        assert_eq!(
            parsed_notif.packet_type,
            MachoPacketType::SessionChangeNotification
        );
        drop(m_stream);

        let _ = bridge_task.await;
        let _ = macho_task.await;
        Ok(())
    }
}
