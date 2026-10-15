//! `eve-server` — Main entry point for the `EvoMu` EVE Online server emulator.
//!
//! Exposes:
//! - TCP Port `26001`: Modern MessagePack/JSON Client Bridge listener (`evomu_bridge.py`).
//! - TCP Port `26000`: Legacy `eveMarshal` / `machoNet` handshake & packet listener.

#![forbid(unsafe_code)]

mod router;
mod system_actor;

use anyhow::{bail, Result};
use eve_machonet::{SessionState, VersionExchange};
use eve_proto::{marshal, BridgeEnvelope, PyRep, MAX_FRAME_SIZE};
use router::ServerContext;
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

    let ctx = Arc::new(ServerContext::new());
    let conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    // Pre-spawn Jita (30000142) solar system actor inside the ServerContext registry
    let _jita = ctx.systems.get_or_spawn(30000142);

    info!(
        loaded_types = ctx.sde.types.len(),
        loaded_systems = ctx.sde.solar_systems.len(),
        "Initialized EvoMu Simulation Core & SDE Registry"
    );

    let bridge_listener = TcpListener::bind("127.0.0.1:26001").await?;
    let macho_listener = TcpListener::bind("127.0.0.1:26000").await?;

    info!("Listening on 127.0.0.1:26001 (Python Client Bridge - JSON/MessagePack)");
    info!("Listening on 127.0.0.1:26000 (Legacy machoNet / eveMarshal TCP)");

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
        }
    }
}

/// Reads a length-prefixed frame with strict `MAX_FRAME_SIZE` validation BEFORE allocating memory.
async fn read_bounded_frame(stream: &mut TcpStream) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match timeout(IO_TIMEOUT, stream.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => bail!("Frame header read timed out after {:?}", IO_TIMEOUT),
    }

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

async fn write_bounded_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<()> {
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
async fn handle_bridge_client(mut stream: TcpStream, ctx: Arc<ServerContext>) -> Result<()> {
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    // New sessions start strictly unauthenticated (`user_id: None`, `role: 0`)
    let mut session = SessionState::new(session_id);
    let mut sys_rx = ctx.systems.get_or_spawn(30000142).subscribe();

    let res: Result<()> = async {
        loop {
            tokio::select! {
                frame_res = read_bounded_frame(&mut stream) => {
                    let Some(payload) = frame_res? else {
                        break;
                    };
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
                        let prev_sys = session.solar_system_id;
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
                        if session.solar_system_id != prev_sys {
                            if let Some(new_sys) = session.solar_system_id {
                                let handle = ctx.systems.get_or_spawn(new_sys);
                                info!(solar_system_id = handle.solar_system_id, "Subscribed session to solar system broadcast");
                                sys_rx = handle.subscribe();
                            }
                        }
                        let response = BridgeEnvelope::RpcResponse { req_id, result };
                        let out_bytes = serde_json::to_vec(&response)?;
                        write_bounded_frame(&mut stream, &out_bytes).await?;
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
                        write_bounded_frame(&mut stream, &out_bytes).await?;
                    }
                }
            }
        }
        Ok(())
    }
    .await;

    ctx.release_session_bindings(session.session_id);
    res
}

/// Handles legacy `machoNet` connections on port 26000, performing the `VersionExchange`
/// handshake and remaining open to process subsequent `eveMarshal` packets.
async fn handle_macho_client(mut stream: TcpStream, _ctx: Arc<ServerContext>) -> Result<()> {
    let ver = VersionExchange::default().to_pyrep();
    let encoded = marshal::encode(&ver);
    write_bounded_frame(&mut stream, &encoded).await?;

    while let Some(frame) = read_bounded_frame(&mut stream).await? {
        let decoded = marshal::decode(&frame)?;
        info!(?decoded, "Received machoNet frame");

        // Handle low-level VersionExchangeClient / VIPKey / QueueCheck handshake frames
        let reply = match &decoded {
            PyRep::Tuple(items) if items.len() == 6 => {
                // Client sent its VersionExchange; acknowledge readiness
                PyRep::Object {
                    type_name: "macho.AuthenticationReq".into(),
                    args: Box::new(PyRep::Tuple(vec![PyRep::ascii_bytes("ok")])),
                }
            }
            _ => PyRep::None,
        };
        let reply_bytes = marshal::encode(&reply);
        write_bounded_frame(&mut stream, &reply_bytes).await?;
    }
    Ok(())
}
