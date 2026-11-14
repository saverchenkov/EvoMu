//! `eve-server` — Main entry point for the `EvoMu` EVE Online server emulator.
//!
//! Exposes:
//! - TCP Port `26002`: Modern MessagePack/JSON Client Bridge listener (`evomu_bridge.py`).
//! - TCP Port `26000`: Native V24.01 `eveMarshal` / `machoNet` `GPSTransport` handshake & packet listener (`P-6`).
//! - TCP Port `26004`: ESI / SSO OAuth2 + PKCE (`S256`) HTTP listener (`ESI-1`).
//! - TCP Port `26005`: Native V24.01 Quasar Public Gateway HTTP/2 gRPC listener (`EVE_PUBLIC_GATEWAY_ADDRESS=127.0.0.1:26005`).

#![forbid(unsafe_code)]

use anyhow::{bail, Result};
use eve_machonet::SessionState;
use eve_proto::{BridgeEnvelope, PyRep, MAX_FRAME_SIZE};
use eve_server::macho_listener::handle_macho_client;
use eve_server::quasar_grpc::handle_quasar_grpc_connection;
use eve_server::router::ServerContext;
use eve_server::sso::handle_http_connection;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Semaphore};
use tokio::time::{timeout, timeout_at, Instant};
use tracing::{error, info, warn};

use eve_server::router::NEXT_TRANSPORT_SESSION_ID as NEXT_SESSION_ID;
const MAX_CONCURRENT_CONNECTIONS: usize = 512;
const SHUTDOWN_RPC_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const IO_TIMEOUT: Duration = Duration::from_secs(30);
const XMPP_IDLE_READ_TIMEOUT: Duration = Duration::from_secs(300);
const XMPP_STANZA_TIMEOUT: Duration = Duration::from_secs(15);
const XMPP_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_XMPP_STANZA_BYTES: usize = 64 * 1024;

#[tokio::main]
async fn main() -> Result<()> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "eve_server=info,eve_destiny=info".into());
    let fmt_layer = tracing_subscriber::fmt::layer();
    let ring_layer = eve_server::log_ring_buffer::LogRingLayer::new(
        eve_server::log_ring_buffer::global_log_ring(),
    );

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(ring_layer)
        .init();

    let mut cfg = eve_server::config::ServerConfig::load_default_or_env()
        .map_err(|e| anyhow::anyhow!(e))?;
    if let Some(cli_xmpp_port) = eve_server::config::ServerConfig::parse_cli_xmpp_port(std::env::args().skip(1))
        .map_err(|e| anyhow::anyhow!(e))?
    {
        cfg.xmpp_port = cli_xmpp_port;
    }
    let xmpp_listener = TcpListener::bind((cfg.bind_address.as_str(), cfg.xmpp_port))
        .await
        .map_err(|e| anyhow::anyhow!("failed to bind XMPP listener on {}:{}: {e}", cfg.bind_address, cfg.xmpp_port))?;
    let bound_xmpp_port = xmpp_listener.local_addr()?.port();
    cfg.xmpp_port = bound_xmpp_port;
    let ctx = Arc::new(ServerContext::from_config(&cfg).map_err(|e| anyhow::anyhow!(e))?);
    eve_server::starter_characters::provision_starter_characters(&ctx, &cfg).await;
    if let Some(token) = parse_admin_token(std::env::var("EVOMU_ADMIN_TOKEN").ok().as_deref()) {
        ctx.db
            .provision_gm_account("admin", token, 90_000_003, "ISD Polaris");
        info!("Provisioned GM account 'admin' via EVOMU_ADMIN_TOKEN");
    }
    let conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let http_conn_limiter = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

    if !ctx.db.is_write_locked_by_current_thread() {
        let _ = ctx.db.with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::set_clean_shutdown_marker(conn, false)
        });
    }

    // Pre-spawn Jita (30000142) solar system actor inside the ServerContext registry
    let _jita = ctx.systems.get_or_spawn(30000142);

    // Dedicated 1 Hz Destiny physics & NPC AI loop, isolated from mining and SQLite timers
    // so warp decelerations complete strictly in real time.
    ctx.dedicated_physics_ticker_active
        .store(true, std::sync::atomic::Ordering::Release);
    let physics_handle = {
        let physics_ctx = Arc::clone(&ctx);
        let mut physics_shutdown = shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut last_step = std::time::Instant::now();
            let mut physics_ticker = tokio::time::interval(Duration::from_secs(1));
            physics_ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            physics_ticker.tick().await;
            let mut panic_count = 0u64;
            loop {
                tokio::select! {
                    _ = physics_ticker.tick() => {}
                    _ = physics_shutdown.changed() => {
                        if *physics_shutdown.borrow() {
                            break;
                        }
                        continue;
                    }
                }
                let elapsed = eve_server::sp6_lifecycle::clamp_physics_ticker_backlog(
                    last_step.elapsed().as_secs(),
                );
                last_step = std::time::Instant::now();
                if !physics_ctx.is_clock_paused() {
                    if let Err(panic_msg) = eve_server::sp6_lifecycle::catch_unwind_future(
                        eve_server::sp6_lifecycle::step_active_systems_physics_tick(
                            &physics_ctx,
                            elapsed,
                        ),
                    )
                    .await
                    {
                        panic_count += 1;
                        error!(
                            panic_count,
                            panic = %panic_msg,
                            "physics_ticker caught panic during tick; continuing loop"
                        );
                    }
                }
            }
        })
    };

    // World tick: advance live server clock (`tick_wall_second`, honoring pause/pause_on_tick/acceleration)
    // and run the unified 1 Hz cluster tick pipeline.
    let cluster_tick_handle = {
        let tick_ctx = Arc::clone(&ctx);
        let mut tick_shutdown = shutdown_tx.subscribe();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(1));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            ticker.tick().await;
            let mut panic_count = 0u64;
            loop {
                tokio::select! {
                    _ = ticker.tick() => {}
                    _ = tick_shutdown.changed() => {
                        if *tick_shutdown.borrow() {
                            break;
                        }
                        continue;
                    }
                }
                let res = eve_server::sp6_lifecycle::catch_unwind_future(async {
                    let prev_offset = tick_ctx.clock().offset_secs();
                    let advanced = tick_ctx.clock().tick_wall_second();
                    if advanced > 0 {
                        let cur_offset = tick_ctx.clock().offset_secs();
                        if cur_offset != prev_offset {
                            let _ = tick_ctx.db.tx("clock_offset_sync", |wtx| {
                                wtx.set_clock_offset_secs(cur_offset);
                                Ok::<_, eve_db::TxError>(())
                            });
                        }
                        eve_server::sp6_lifecycle::tick_background_cluster_with_step(
                            &tick_ctx, advanced,
                        )
                        .await;
                    }
                })
                .await;
                if let Err(panic_msg) = res {
                    panic_count += 1;
                    error!(
                        panic_count,
                        panic = %panic_msg,
                        "cluster_tick caught panic during tick; continuing loop"
                    );
                }
            }
        })
    };

    info!(
        loaded_types = ctx.sde.types.len(),
        loaded_systems = ctx.sde.solar_systems.len(),
        sde_path = %cfg.sde_path,
        world_db_path = %cfg.world_db_path,
        esi_port = cfg.esi_port,
        xmpp_port = cfg.xmpp_port,
        quasar_grpc_port = cfg.quasar_grpc_port,
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
            "Listening on ESI / SSO OAuth2 HTTP TCP"
        );
        Some(l)
    } else {
        None
    };
    let quasar_grpc_listener = if cfg.feature_flags.enable_quasar_gateway && cfg.quasar_grpc_port > 0 {
        let l = TcpListener::bind((cfg.bind_address.as_str(), cfg.quasar_grpc_port)).await?;
        info!(
            bind_address = %cfg.bind_address,
            quasar_grpc_port = cfg.quasar_grpc_port,
            "Listening on Quasar Public Gateway HTTP/2 gRPC TCP (set EVE_PUBLIC_GATEWAY_ADDRESS={}:{} on client)",
            quasar_advertise_host(&cfg.bind_address),
            cfg.quasar_grpc_port,
        );
        Some(l)
    } else {
        None
    };

    // XMPP client sink on `cfg.xmpp_port` (default TCP port 5222, `ejabberd_client_address` / `ejabberd_client_port`)
    // so V24.01 `XmppChatService.Login` (`xmpp/xmppConnection.py::_connectToXmppServer`) does not hit
    // `ECONNREFUSED` (`socket.error`) and pop up `RetryChatServerConnection` when local chat
    // is handled over Quasar / machoNet (`LSC`).
    info!(
        bind_address = %cfg.bind_address,
        xmpp_port = bound_xmpp_port,
        "Listening on XMPP Chat TCP"
    );
    let xmpp_conn_semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let xmpp_conn_semaphore_task = Arc::clone(&xmpp_conn_semaphore);
    let xmpp_ctx = Arc::clone(&ctx);
    let mut xmpp_shutdown = shutdown_tx.subscribe();
    let xmpp_handle = tokio::spawn(async move {
        loop {
            let accept_res = tokio::select! {
                res = xmpp_listener.accept() => res,
                _ = xmpp_shutdown.changed() => {
                    if *xmpp_shutdown.borrow() {
                        break;
                    }
                    continue;
                }
            };
            let Ok((stream, _addr)) = accept_res else {
                break;
            };
            let Ok(permit) = Arc::clone(&xmpp_conn_semaphore_task).try_acquire_owned() else {
                continue;
            };
            let client_ctx = Arc::clone(&xmpp_ctx);
            tokio::spawn(async move {
                let _permit = permit;
                handle_xmpp_stream_with_timeouts(
                    stream,
                    Some(&client_ctx),
                    XMPP_IDLE_READ_TIMEOUT,
                    XMPP_WRITE_TIMEOUT,
                    XMPP_STANZA_TIMEOUT,
                )
                .await;
            });
        }
    });

    info!(
        bind_address = %cfg.bind_address,
        bridge_port = cfg.bridge_port,
        "Listening on Python Client Bridge (JSON/MessagePack)"
    );
    info!(
        bind_address = %cfg.bind_address,
        macho_port = cfg.macho_port,
        "Listening on Native machoNet / GPSTransport TCP"
    );

    loop {
        tokio::select! {
            _ = wait_for_shutdown_signal() => {
                info!("Shutdown signal received; stopping listeners");
                break;
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("Programmatic shutdown requested; stopping listeners");
                    break;
                }
            }
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
                    info!(%addr, "Accepted native machoNet connection");
                    if let Err(e) = handle_macho_client(stream, ctx_clone).await {
                        error!(%addr, error = %e, "machoNet connection closed with error");
                    }
                });
            }
            Ok((stream, addr)) = async {
                match &esi_listener {
                    Some(l) => l.accept().await,
                    None => std::future::pending().await,
                }
            } => {
                let Ok(permit) = Arc::clone(&http_conn_limiter).try_acquire_owned() else {
                    warn!(%addr, "Connection limit reached; rejecting SSO/ESI HTTP connection");
                    continue;
                };
                let ctx_clone = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_http_connection(stream, ctx_clone).await {
                        error!(%addr, error = %e, "SSO/ESI HTTP connection closed with error");
                    }
                });
            }
            Ok((stream, addr)) = async {
                match &quasar_grpc_listener {
                    Some(l) => l.accept().await,
                    None => std::future::pending().await,
                }
            } => {
                let Ok(permit) = Arc::clone(&http_conn_limiter).try_acquire_owned() else {
                    warn!(%addr, "Connection limit reached; rejecting Quasar gRPC HTTP/2 connection");
                    continue;
                };
                let ctx_clone = Arc::clone(&ctx);
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(e) = handle_quasar_grpc_connection(stream, ctx_clone).await {
                        warn!(%addr, error = %e, "Quasar gRPC HTTP/2 connection closed with error");
                    }
                });
            }
        }
    }

    drop(bridge_listener);
    drop(macho_listener);
    drop(esi_listener);
    drop(quasar_grpc_listener);

    run_clean_shutdown(
        &ctx,
        &shutdown_tx,
        Some(physics_handle),
        Some(cluster_tick_handle),
        Some(xmpp_handle),
        &conn_limiter,
        &http_conn_limiter,
        &xmpp_conn_semaphore,
        SHUTDOWN_RPC_DRAIN_TIMEOUT,
    )
    .await;
    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = async {
                match sigterm {
                    Ok(mut s) => {
                        s.recv().await;
                    }
                    Err(_) => std::future::pending::<()>().await,
                }
            } => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn drain_semaphore_permits(sem: &Semaphore, total_permits: usize, deadline: Instant) -> bool {
    let want = u32::try_from(total_permits).unwrap_or(u32::MAX);
    match timeout_at(deadline, sem.acquire_many(want)).await {
        Ok(Ok(permit)) => {
            drop(permit);
            true
        }
        _ => false,
    }
}

/// Executes the graceful server shutdown sequence (`#920`, `#899` §3.6):
/// 1. Signal background tickers and acceptors to stop accepting new work.
/// 2. Wait up to `drain_timeout` (10s in production) for in-flight RPC/connection permits to drain.
/// 3. Flush final checkpoints from every active solar system actor, sync runtime state, and persist
///    `world_meta.clock_offset_secs`.
/// 4. Record `world_meta.clean_shutdown = 1` and run `PRAGMA wal_checkpoint(TRUNCATE)`.
#[allow(clippy::too_many_arguments)]
async fn run_clean_shutdown(
    ctx: &ServerContext,
    shutdown_tx: &watch::Sender<bool>,
    physics_handle: Option<tokio::task::JoinHandle<()>>,
    cluster_tick_handle: Option<tokio::task::JoinHandle<()>>,
    xmpp_handle: Option<tokio::task::JoinHandle<()>>,
    conn_limiter: &Arc<Semaphore>,
    http_conn_limiter: &Arc<Semaphore>,
    xmpp_conn_semaphore: &Arc<Semaphore>,
    drain_timeout: Duration,
) {
    let _ = shutdown_tx.send(true);
    {
        let mut cs = ctx.cluster_state.write().unwrap_or_else(|e| e.into_inner());
        cs.shutdown_in_progress = true;
        cs.shutdown_graceful = Some(true);
        cs.shutdown_reason = Some("Cluster shutting down gracefully".to_string());
        cs.shutdown_timeout_seconds = Some(drain_timeout.as_secs() as i64);
    }

    for handle in [xmpp_handle, physics_handle, cluster_tick_handle]
        .into_iter()
        .flatten()
    {
        let _ = timeout(Duration::from_secs(2), handle).await;
    }

    let drain_deadline = Instant::now() + drain_timeout;
    let (drained_tcp, drained_http, drained_xmpp) = tokio::join!(
        drain_semaphore_permits(conn_limiter, MAX_CONCURRENT_CONNECTIONS, drain_deadline),
        drain_semaphore_permits(http_conn_limiter, MAX_CONCURRENT_CONNECTIONS, drain_deadline),
        drain_semaphore_permits(xmpp_conn_semaphore, MAX_CONCURRENT_CONNECTIONS, drain_deadline),
    );
    if !(drained_tcp && drained_http && drained_xmpp) {
        warn!(
            drained_tcp,
            drained_http,
            drained_xmpp,
            timeout_secs = drain_timeout.as_secs(),
            "Shutdown drain deadline elapsed with active connections still open; proceeding to final checkpoint"
        );
    }

    let closers: Vec<_> = {
        let mut guard = ctx
            .session_closers
            .write()
            .unwrap_or_else(|e| e.into_inner());
        guard.drain().map(|(_, tx)| tx).collect()
    };
    for tx in closers {
        let _ = tx.send((
            "ServerShuttingDown".to_string(),
            "Cluster shutting down".to_string(),
        ));
    }

    let flushed = eve_server::sp6_lifecycle::flush_all_systems_final_checkpoints(ctx).await;

    let wal_res = if !ctx.db.is_write_locked_by_current_thread() {
        Some(ctx.db.with_sqlite_conn(|conn| -> Result<(i64, i64, i64), String> {
            eve_db::sqlite_store::system_checkpoint::set_clean_shutdown_marker(conn, true)?;
            eve_db::sqlite_store::system_checkpoint::wal_checkpoint_truncate(conn)
        }))
    } else {
        None
    };
    match wal_res {
        Some(Ok((busy, log_frames, checkpointed_frames))) => {
            info!(
                flushed,
                busy,
                log_frames,
                checkpointed_frames,
                "Clean shutdown complete after WAL truncate checkpoint"
            );
        }
        Some(Err(err)) => {
            warn!(flushed, error = %err, "Clean shutdown WAL truncate failed");
        }
        None => {
            info!(flushed, "Clean shutdown complete");
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
    let peer_addr = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "127.0.0.1:0".to_string());
    let session_id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    // New sessions start strictly unauthenticated (`user_id: None`, `role: 0`)
    let mut session = SessionState::new(session_id);
    session.address = peer_addr;
    session.live_notify_listener = true;
    let mut sys_rx = ctx.systems.get_or_spawn(30000142).subscribe();
    let mut notify_rx = ctx.notify_tx.subscribe();

    let (close_tx, mut close_rx) = tokio::sync::mpsc::unbounded_channel::<(String, String)>();
    ctx.register_session_closer(session_id, close_tx);
    let mut session_guard = eve_server::router::SessionCleanupGuard::new(Arc::clone(&ctx), session_id);

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
    session_guard.set_reader_abort(reader_task.abort_handle());

    let res: Result<()> = async {
        loop {
            tokio::select! {
                Some((reason_code, reason)) = close_rx.recv() => {
                    let close_evt = BridgeEnvelope::ServerEvent {
                        tick: 0,
                        event: "GPSTransportClosed".to_string(),
                        args: vec![PyRep::String(reason_code), PyRep::String(reason)],
                    };
                    if let Ok(bytes) = serde_json::to_vec(&close_evt) {
                        let _ = write_bounded_frame(&mut writer, &bytes).await;
                    }
                    break;
                }
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
                        let mut pre_rpc_notifs = session
                            .character_id
                            .map(|cid| ctx.drain_notifications_for_char(cid))
                            .unwrap_or_default();
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
                        let sess_diff_sent = !diff.is_empty();
                        if sess_diff_sent {
                            let session_event = build_session_changed_envelope(&session, diff);
                            let sess_bytes = serde_json::to_vec(&session_event)?;
                            write_bounded_frame(&mut writer, &sess_bytes).await?;
                        }

                        if let Some(cid) = session.character_id {
                            pre_rpc_notifs.extend(ctx.drain_notifications_for_char(cid));
                        }
                        for notif in pre_rpc_notifs {
                            if sess_diff_sent && notif.event == "OnSessionChanged" {
                                continue;
                            }
                            let server_event = BridgeEnvelope::ServerEvent {
                                tick: 0,
                                event: notif.event,
                                args: notif.args,
                            };
                            if let Ok(out_bytes) = serde_json::to_vec(&server_event) {
                                let _ = write_bounded_frame(&mut writer, &out_bytes).await;
                            }
                        }
                        for (event, args) in session.drain_notifications() {
                            if sess_diff_sent && event == "OnSessionChanged" {
                                continue;
                            }
                            let server_event = BridgeEnvelope::ServerEvent {
                                tick: 0,
                                event,
                                args,
                            };
                            if let Ok(out_bytes) = serde_json::to_vec(&server_event) {
                                let _ = write_bounded_frame(&mut writer, &out_bytes).await;
                            }
                        }

                        let was_in_space = prev_session.character_id.is_some()
                            && prev_session.station_id.is_none()
                            && prev_session.structure_id.is_none();
                        let is_in_space = session.character_id.is_some()
                            && session.station_id.is_none()
                            && session.structure_id.is_none();
                        if was_in_space
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
                        if was_in_space || is_in_space {
                            if let (Some(sys_id), Some(ship_id)) = (
                                session.solar_system_id.or(prev_session.solar_system_id),
                                session.ship_id.or(prev_session.ship_id),
                            ) {
                                let handle = ctx.systems.get_or_spawn(sys_id);
                                if let Ok((tick, events, pyrep)) = handle.drain_observer_updates(ship_id).await {
                                    if !events.is_empty() {
                                        let ev = BridgeEnvelope::ServerEvent {
                                            tick,
                                            event: "DoDestinyUpdate".to_string(),
                                            args: vec![pyrep],
                                        };
                                        if let Ok(bytes) = serde_json::to_vec(&ev) {
                                            let _ = write_bounded_frame(&mut writer, &bytes).await;
                                        }
                                    }
                                }
                                let _ = handle
                                    .flush_peer_observer_updates(Some(ship_id))
                                    .await;
                            }
                        }
                    }
                }
                notify_res = notify_rx.recv(), if session.character_id.is_some() => {
                    if let Some(cid) = session.character_id {
                        let should_drain = match notify_res {
                            Ok((target_cid, _)) => target_cid == cid,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => true,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => false,
                        };
                        if should_drain {
                            for notif in ctx.drain_notifications_for_char(cid) {
                                let server_event = BridgeEnvelope::ServerEvent {
                                    tick: 0,
                                    event: notif.event,
                                    args: notif.args,
                                };
                                if let Ok(out_bytes) = serde_json::to_vec(&server_event) {
                                    let _ = write_bounded_frame(&mut writer, &out_bytes).await;
                                }
                            }
                        }
                    }
                }
                Ok(tick_msg) = sys_rx.recv() => {
                    if session.character_id.is_some()
                        && session.station_id.is_none()
                        && session.solar_system_id == Some(tick_msg.solar_system_id)
                    {
                        if let Some(ship_id) = session.ship_id {
                            if let Some(updates_pyrep) = tick_msg.observer_updates_pyrep.get(&ship_id) {
                                let server_event = BridgeEnvelope::ServerEvent {
                                    tick: tick_msg.tick,
                                    event: "DoDestinyUpdate".to_string(),
                                    args: vec![updates_pyrep.clone()],
                                };
                                let out_bytes = serde_json::to_vec(&server_event)?;
                                write_bounded_frame(&mut writer, &out_bytes).await?;
                            } else if !tick_msg.events.is_empty() {
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
            }
        }
        Ok(())
    }
    .await;

    reader_task.abort();
    ctx.cleanup_disconnected_client_session(&mut session).await;
    res
}

fn parse_admin_token(raw: Option<&str>) -> Option<&str> {
    raw.map(str::trim).filter(|t| !t.is_empty())
}

fn quasar_advertise_host(bind_address: &str) -> &str {
    if bind_address == "0.0.0.0" {
        "127.0.0.1"
    } else {
        bind_address
    }
}

enum XmppFrameOutcome {
    Stanza(String),
    CloseStream,
    NeedMore { has_partial_stanza: bool },
    Invalid(&'static str),
}

fn extract_next_xmpp_stanza(buf: &mut Vec<u8>) -> XmppFrameOutcome {
    loop {
        let ws = buf.iter().take_while(|b| b.is_ascii_whitespace()).count();
        if ws > 0 {
            buf.drain(..ws);
        }
        if buf.is_empty() {
            return XmppFrameOutcome::NeedMore {
                has_partial_stanza: false,
            };
        }

        let valid_str = match std::str::from_utf8(buf) {
            Ok(s) => s,
            Err(e) => {
                if e.error_len().is_some() {
                    return XmppFrameOutcome::Invalid("invalid UTF-8 in XMPP stream");
                }
                let valid_up_to = e.valid_up_to();
                if valid_up_to == 0 {
                    return XmppFrameOutcome::NeedMore {
                        has_partial_stanza: true,
                    };
                }
                std::str::from_utf8(&buf[..valid_up_to]).unwrap_or("")
            }
        };

        if !valid_str.starts_with('<') {
            return XmppFrameOutcome::Invalid("expected '<' at start of XMPP stanza");
        }
        if valid_str.starts_with("<?") {
            let Some(end_pi) = valid_str.find("?>") else {
                return XmppFrameOutcome::NeedMore {
                    has_partial_stanza: true,
                };
            };
            buf.drain(..end_pi + 2);
            continue;
        }
        if valid_str.starts_with("</stream:stream") {
            let Some(gt) = valid_str.find('>') else {
                return XmppFrameOutcome::NeedMore {
                    has_partial_stanza: true,
                };
            };
            buf.drain(..=gt);
            return XmppFrameOutcome::CloseStream;
        }
        if valid_str.starts_with("<stream:stream") {
            let after = "<stream:stream".len();
            let bytes = valid_str.as_bytes();
            if bytes.len() == after {
                return XmppFrameOutcome::NeedMore {
                    has_partial_stanza: true,
                };
            }
            if matches!(
                bytes[after],
                b'>' | b'/' | b' ' | b'\t' | b'\r' | b'\n'
            ) {
                let mut in_quote: Option<u8> = None;
                for (idx, &b) in bytes.iter().enumerate().skip(after) {
                    if let Some(q) = in_quote {
                        if b == q {
                            in_quote = None;
                        }
                    } else if b == b'\'' || b == b'"' {
                        in_quote = Some(b);
                    } else if b == b'>' {
                        let stanza = valid_str[..=idx].to_string();
                        buf.drain(..=idx);
                        return XmppFrameOutcome::Stanza(stanza);
                    }
                }
                return XmppFrameOutcome::NeedMore {
                    has_partial_stanza: true,
                };
            }
        }

        let bytes = valid_str.as_bytes();
        let mut i = 0usize;
        let mut depth = 0usize;
        let mut in_tag = false;
        let mut closing_tag = false;
        let mut in_quote: Option<u8> = None;
        let mut last_non_ws_in_tag = 0u8;

        while i < bytes.len() {
            if !in_tag {
                if bytes[i] == b'<' {
                    if bytes[i..].starts_with(b"<!--") {
                        let Some(rel_end) = valid_str[i + 4..].find("-->") else {
                            return XmppFrameOutcome::NeedMore {
                                has_partial_stanza: true,
                            };
                        };
                        i += 4 + rel_end + 3;
                        continue;
                    }
                    if bytes[i..].starts_with(b"<![CDATA[") {
                        let Some(rel_end) = valid_str[i + 9..].find("]]>") else {
                            return XmppFrameOutcome::NeedMore {
                                has_partial_stanza: true,
                            };
                        };
                        i += 9 + rel_end + 3;
                        continue;
                    }
                    if bytes.get(i + 1) == Some(&b'/') {
                        in_tag = true;
                        closing_tag = true;
                        last_non_ws_in_tag = 0;
                        i += 2;
                    } else if bytes.get(i + 1) == Some(&b'?') {
                        let Some(rel_end) = valid_str[i + 2..].find("?>") else {
                            return XmppFrameOutcome::NeedMore {
                                has_partial_stanza: true,
                            };
                        };
                        i += 2 + rel_end + 2;
                    } else {
                        in_tag = true;
                        closing_tag = false;
                        last_non_ws_in_tag = 0;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            } else if let Some(q) = in_quote {
                if bytes[i] == q {
                    in_quote = None;
                }
                last_non_ws_in_tag = bytes[i];
                i += 1;
            } else if bytes[i] == b'\'' || bytes[i] == b'"' {
                in_quote = Some(bytes[i]);
                last_non_ws_in_tag = bytes[i];
                i += 1;
            } else if bytes[i] == b'>' {
                in_tag = false;
                if closing_tag {
                    if depth == 0 {
                        return XmppFrameOutcome::Invalid("unmatched XMPP closing tag");
                    }
                    depth -= 1;
                } else if last_non_ws_in_tag != b'/' {
                    depth += 1;
                }
                if depth == 0 {
                    let stanza = valid_str[..=i].to_string();
                    buf.drain(..=i);
                    return XmppFrameOutcome::Stanza(stanza);
                }
                i += 1;
            } else {
                if !bytes[i].is_ascii_whitespace() {
                    last_non_ws_in_tag = bytes[i];
                }
                i += 1;
            }
        }

        return XmppFrameOutcome::NeedMore {
            has_partial_stanza: true,
        };
    }
}

async fn handle_xmpp_stream_with_timeouts<S>(
    mut stream: S,
    ctx: Option<&ServerContext>,
    idle_read_timeout: Duration,
    write_timeout: Duration,
    stanza_timeout: Duration,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut read_buf = [0u8; 2048];
    let mut stanza_buf: Vec<u8> = Vec::with_capacity(2048);
    let mut stream_count = 0u32;
    let mut auth = eve_server::services::fleet_social::XmppStreamAuth::default();
    let mut stanza_deadline: Option<Instant> = None;

    'conn: loop {
        loop {
            if stanza_buf.len() > MAX_XMPP_STANZA_BYTES {
                break 'conn;
            }
            match extract_next_xmpp_stanza(&mut stanza_buf) {
                XmppFrameOutcome::Stanza(stanza) => {
                    stanza_deadline = None;
                    if let Some(reply) = xmpp_stanza_reply_with_ctx_and_auth(
                        ctx,
                        Some(&mut auth),
                        &stanza,
                        &mut stream_count,
                    ) {
                        if timeout(write_timeout, stream.write_all(reply.as_bytes()))
                            .await
                            .map_or(true, |r| r.is_err())
                        {
                            break 'conn;
                        }
                    }
                }
                XmppFrameOutcome::CloseStream => {
                    let _ = timeout(write_timeout, stream.write_all(b"</stream:stream>")).await;
                    break 'conn;
                }
                XmppFrameOutcome::NeedMore { has_partial_stanza } => {
                    if has_partial_stanza {
                        if stanza_deadline.is_none() {
                            stanza_deadline = Some(Instant::now() + stanza_timeout);
                        }
                    } else {
                        stanza_deadline = None;
                    }
                    break;
                }
                XmppFrameOutcome::Invalid(reason) => {
                    tracing::debug!(reason, "Closing invalid XMPP stream");
                    break 'conn;
                }
            }
        }

        if stanza_buf.len() > MAX_XMPP_STANZA_BYTES {
            break 'conn;
        }

        let read_res = if let Some(deadline) = stanza_deadline {
            timeout_at(deadline, stream.read(&mut read_buf)).await
        } else {
            timeout(idle_read_timeout, stream.read(&mut read_buf)).await
        };
        let Ok(Ok(n)) = read_res else {
            break 'conn;
        };
        if n == 0 {
            break 'conn;
        }
        if stanza_buf.len().saturating_add(n) > MAX_XMPP_STANZA_BYTES {
            break 'conn;
        }
        stanza_buf.extend_from_slice(&read_buf[..n]);
    }
}

#[cfg(test)]
fn xmpp_stanza_reply(chunk: &str, stream_count: &mut u32) -> Option<String> {
    xmpp_stanza_reply_with_ctx_and_auth(None, None, chunk, stream_count)
}

fn xmpp_root_tag(stanza: &str) -> Option<&str> {
    let mut s = stanza.trim_start();
    while s.starts_with("<?") {
        let end = s.find("?>")?;
        s = s[end + 2..].trim_start();
    }
    if !s.starts_with('<') || s.starts_with("</") || s.starts_with("<!") {
        return None;
    }
    let after_lt = &s[1..];
    let end = after_lt
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(after_lt.len());
    let tag = &after_lt[..end];
    (!tag.is_empty()).then_some(tag)
}

fn xmpp_extract_attr<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    for q in ['\'', '"'] {
        let needle = format!("{name}={q}");
        let mut offset = 0usize;
        while let Some(rel) = xml[offset..].find(&needle) {
            let start = offset + rel;
            let prev_ok = start == 0
                || xml[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_whitespace());
            let val_start = start + needle.len();
            if prev_ok {
                if let Some(end_rel) = xml[val_start..].find(q) {
                    return Some(&xml[val_start..val_start + end_rel]);
                }
            }
            offset = val_start;
        }
    }
    None
}

fn xmpp_stanza_reply_with_ctx_and_auth(
    ctx: Option<&ServerContext>,
    mut auth: Option<&mut eve_server::services::fleet_social::XmppStreamAuth>,
    chunk: &str,
    stream_count: &mut u32,
) -> Option<String> {
    match xmpp_root_tag(chunk) {
        Some("stream:stream") => {
            *stream_count += 1;
            let reply = if *stream_count == 1 {
                if ctx.is_some() {
                    "<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' from='chat.evomu.local' id='evomu1' version='1.0'><stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism></mechanisms></stream:features>"
                } else {
                    "<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' from='chat.evomu.local' id='evomu1' version='1.0'><stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism><mechanism>ANONYMOUS</mechanism></mechanisms></stream:features>"
                }
            } else if ctx.is_none() || auth.as_deref().is_some_and(|a| a.sasl_authenticated) {
                "<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' from='chat.evomu.local' id='evomu2' version='1.0'><stream:features><bind xmlns='urn:ietf:params:xml:ns:xmpp-bind'/><session xmlns='urn:ietf:params:xml:ns:xmpp-session'/></stream:features>"
            } else {
                "<?xml version='1.0'?><stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams' from='chat.evomu.local' id='evomu1' version='1.0'><stream:features><mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><mechanism>PLAIN</mechanism></mechanisms></stream:features>"
            };
            Some(reply.to_string())
        }
        Some("auth") => {
            if let Some(c) = ctx {
                let Some(a) = auth.as_deref_mut() else {
                    return Some(
                        "<failure xmlns='urn:ietf:params:xml:ns:xmpp-sasl'><not-authorized/></failure>"
                            .to_string(),
                    );
                };
                return match eve_server::services::fleet_social::xmpp_authenticate_sasl_plain(
                    c, chunk, a,
                ) {
                    Ok(()) => Some("<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>".to_string()),
                    Err(failure) => Some(failure.to_string()),
                };
            }
            Some("<success xmlns='urn:ietf:params:xml:ns:xmpp-sasl'/>".to_string())
        }
        Some("presence") => {
            if let Some(room_jid) = xmpp_extract_attr(chunk, "to") {
                let stanza_id = xmpp_extract_attr(chunk, "id");
                let id_attr = stanza_id
                    .as_deref()
                    .map(|id| format!(" id='{id}'"))
                    .unwrap_or_default();
                let to_jid = auth
                    .as_deref()
                    .and_then(|a| a.bound_jid.as_deref())
                    .unwrap_or("user@chat.evomu.local/eve");
                if let Some(c) = ctx {
                    if !eve_server::services::fleet_social::xmpp_can_join_room(
                        c,
                        auth.as_deref(),
                        room_jid,
                    ) {
                        return Some(format!(
                            "<presence from='{room_jid}' to='{to_jid}' type='error'{id_attr}><error code='403' type='auth'><not-authorized xmlns='urn:ietf:params:xml:ns:xmpp-stanzas'/></error></presence>"
                        ));
                    }
                }
                let is_unavailable = xmpp_extract_attr(chunk, "type")
                    .is_some_and(|t| t.eq_ignore_ascii_case("unavailable"));
                let p_reply = if is_unavailable {
                    format!("<presence from='{room_jid}' to='{to_jid}' type='unavailable'{id_attr}><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='member' role='none'/><status code='110'/></x></presence>")
                } else {
                    format!("<presence from='{room_jid}' to='{to_jid}'{id_attr}><x xmlns='http://jabber.org/protocol/muc#user'><item affiliation='member' role='participant'/><status code='110'/></x></presence>")
                };
                return Some(p_reply);
            }
            None
        }
        Some("iq") => {
            let stanza_id = xmpp_extract_attr(chunk, "id")?;
            Some(
                eve_server::services::fleet_social::xmpp_iq_reply_with_auth_mut(
                    ctx,
                    auth.as_deref_mut(),
                    chunk,
                    stanza_id,
                ),
            )
        }
        _ => None,
    }
}

fn build_session_changed_envelope(
    session: &SessionState,
    diff: std::collections::HashMap<String, (PyRep, PyRep)>,
) -> BridgeEnvelope {
    let changes_dict = PyRep::Dict(
        diff.into_iter()
            .map(|(k, (old_v, new_v))| (PyRep::String(k), PyRep::Tuple(vec![old_v, new_v])))
            .collect(),
    );
    let has_solarsystem = session.station_id.is_none();
    let sess_rep = PyRep::keyval([
        ("sessionID", PyRep::Integer(session.session_id as i64)),
        (
            "charid",
            session.character_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "userid",
            session
                .user_id
                .map(|u| PyRep::Integer(u as i64))
                .unwrap_or(PyRep::None),
        ),
        (
            "corpid",
            session
                .corporation_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        (
            "allianceid",
            session.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "stationid",
            session.station_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "structureid",
            session.structure_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        (
            "solarsystemid2",
            session
                .solar_system_id
                .map(|s| PyRep::Integer(s as i64))
                .unwrap_or(PyRep::None),
        ),
        (
            "solarsystemid",
            if has_solarsystem {
                session
                    .solar_system_id
                    .map(|s| PyRep::Integer(s as i64))
                    .unwrap_or(PyRep::None)
            } else {
                PyRep::None
            },
        ),
        (
            "shipid",
            session.ship_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
        ("corprole", PyRep::Integer(session.corp_role as i64)),
    ]);
    BridgeEnvelope::ServerEvent {
        tick: 0,
        event: "OnSessionChanged".to_string(),
        args: vec![PyRep::Bool(false), sess_rep, changes_dict],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eve_machonet::{
        placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
        CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
        VersionExchange,
    };
    use eve_proto::marshal;
    use std::collections::HashMap;

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

        // 1. Mode A: Python Client Bridge TCP round-trip (Auth -> OnSessionChanged)
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

        let sess_frame = read_bounded_frame(&mut b_stream)
            .await?
            .expect("session event");
        let BridgeEnvelope::ServerEvent { event, .. } = serde_json::from_slice(&sess_frame)?
        else {
            panic!("Expected ServerEvent");
        };
        assert_eq!(event, "OnSessionChanged");
        drop(b_stream);

        // 2. Mode B: Native machoNet GPSTransport TCP handshake -> CallReq
        let mut m_stream = TcpStream::connect(macho_addr).await?;
        let ver_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("version exchange");
        let ver_pyrep = marshal::decode(&ver_frame)?;
        let expected_ver = VersionExchange::default();
        assert_eq!(ver_pyrep, expected_ver.to_server_hello_pyrep());

        // Step 1: Client sends VersionExchangeClient
        write_bounded_frame(&mut m_stream, &marshal::encode(&expected_ver.to_pyrep())).await?;
        // Step 2: Client sends CryptoRequestPacket ("placebo", {}) -> server replies "OK CC"
        let crypto_req = CryptoRequestPacket::placebo().to_pyrep();
        write_bounded_frame(&mut m_stream, &marshal::encode(&crypto_req)).await?;
        let ok_cc_frame = read_bounded_frame(&mut m_stream).await?.expect("OK CC");
        assert_eq!(marshal::decode(&ok_cc_frame)?.as_str(), Some("OK CC"));

        // Step 3: Client sends CryptoChallengePacket -> server replies passwordVersion (2) + CryptoServerHandshake
        let challenge =
            CryptoChallengePacket::new_password_login("demo", "demo-secret").to_pyrep();
        write_bounded_frame(&mut m_stream, &marshal::encode(&challenge)).await?;
        let pw_ver_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("passwordVersion");
        assert_eq!(marshal::decode(&pw_ver_frame)?.as_i64(), Some(2));
        let srv_hs_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("CryptoServerHandshake");
        let srv_hs = CryptoServerHandshake::from_pyrep(&marshal::decode(&srv_hs_frame)?)
            .expect("decode CryptoServerHandshake");

        // Step 4: Client sends CryptoHandshakeResult -> server replies CryptoHandshakeAck + SessionInitialStateNotification + SessionChangeNotification
        let hs_res = CryptoHandshakeResult {
            challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
            func_output: Vec::new(),
            func_result: PyRep::Integer(0),
        };
        write_bounded_frame(&mut m_stream, &marshal::encode(&hs_res.to_pyrep())).await?;
        let ack_frame = read_bounded_frame(&mut m_stream).await?.expect("ack");
        let ack =
            CryptoHandshakeAck::from_pyrep(&marshal::decode(&ack_frame)?).expect("decode ack");
        assert_eq!(ack.user_id, 1);

        let init_notif_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("SessionInitialStateNotification");
        let init_notif =
            MachoPacket::from_pyrep(&marshal::decode(&init_notif_frame)?).expect("init_notif");
        assert_eq!(
            init_notif.packet_type,
            MachoPacketType::SessionInitialStateNotification
        );

        let scn_frame = read_bounded_frame(&mut m_stream)
            .await?
            .expect("SessionChangeNotification");
        let scn = MachoPacket::from_pyrep(&marshal::decode(&scn_frame)?).expect("scn");
        assert_eq!(scn.packet_type, MachoPacketType::SessionChangeNotification);

        // Authenticated RPC: CallReq (charUnboundMgr.SelectCharacterID)
        let call_pkt = MachoPacket {
            packet_type: MachoPacketType::CallReq,
            source: MachoAddress::Client {
                client_id: ack.client_id,
                call_id: 42,
                service: None,
            },
            destination: MachoAddress::Node {
                node_id: 888444,
                service: Some("charUnboundMgr".into()),
                call_id: 42,
            },
            user_id: Some(ack.user_id),
            payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::String("SelectCharacterID".into()),
                PyRep::Tuple(vec![PyRep::Integer(90_000_001)]),
                PyRep::Dict(vec![]),
            ])))]),
            named_payload: HashMap::new(),
        };
        write_bounded_frame(&mut m_stream, &marshal::encode(&call_pkt.to_pyrep())).await?;
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

        let call_rsp_frame = read_bounded_frame(&mut m_stream).await?.expect("call rsp");
        let call_rsp_pyrep = marshal::decode(&call_rsp_frame)?;
        let parsed_rsp = MachoPacket::from_pyrep(&call_rsp_pyrep).expect("MachoPacket CallRsp");
        assert_eq!(parsed_rsp.packet_type, MachoPacketType::CallRsp);
        drop(m_stream);

        let _ = bridge_task.await;
        let _ = macho_task.await;
        Ok(())
    }

    #[tokio::test]
    async fn bridge_client_disconnect_and_abort_clean_up_quasar_subscribers_and_online_state() -> Result<()> {
        async fn bridge_call(
            stream: &mut TcpStream,
            req_id: u64,
            service: &str,
            method: &str,
            args: Vec<PyRep>,
        ) -> Result<std::result::Result<PyRep, eve_proto::BridgeRpcError>> {
            let req = BridgeEnvelope::RpcRequest {
                req_id,
                service: service.into(),
                bound_id: None,
                method: method.into(),
                args,
                kwargs: HashMap::new(),
                caller_trace: Some("bridge-disconnect-test".into()),
            };
            write_bounded_frame(stream, &serde_json::to_vec(&req)?).await?;
            loop {
                let raw = read_bounded_frame(stream).await?.expect("bridge frame");
                if let BridgeEnvelope::RpcResponse {
                    req_id: got_id,
                    result,
                } = serde_json::from_slice(&raw)?
                {
                    assert_eq!(got_id, req_id);
                    return Ok(result);
                }
            }
        }

        let ctx = Arc::new(ServerContext::new());
        let bridge_listener = Arc::new(TcpListener::bind("127.0.0.1:0").await?);
        let bridge_addr = bridge_listener.local_addr()?;

        // 1. Normal TCP disconnect evicts Quasar subscriber + stream_receiver and releases online lock.
        let ctx_c1 = Arc::clone(&ctx);
        let listener_1 = Arc::clone(&bridge_listener);
        let bridge_task_1 = tokio::spawn(async move {
            let (sock, _) = listener_1.accept().await.expect("accept 1");
            handle_bridge_client(sock, ctx_c1).await
        });

        let mut b_stream = TcpStream::connect(bridge_addr).await?;
        assert!(
            bridge_call(
                &mut b_stream,
                1,
                "authentication",
                "Login",
                vec![PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            )
            .await?
            .is_ok()
        );
        assert!(
            bridge_call(
                &mut b_stream,
                2,
                "charUnboundMgr",
                "SelectCharacterID",
                vec![PyRep::Integer(90_000_001)],
            )
            .await?
            .is_ok()
        );
        assert!(
            bridge_call(&mut b_stream, 3, "publicGatewaySvc", "Subscribe", Vec::new())
                .await?
                .is_ok()
        );
        assert_eq!(
            ctx.quasar_gateway.subscribers.read().unwrap().len(),
            1,
            "active Bridge session must be registered in PublicGatewayRouter::subscribers"
        );
        assert_eq!(
            ctx.quasar_gateway.stream_receivers.read().unwrap().len(),
            1,
            "active Bridge session must retain its notice stream_receiver while connected"
        );
        assert!(ctx.db.get_online_session(1).is_some());

        drop(b_stream);
        let _ = bridge_task_1.await?;

        assert!(
            ctx.quasar_gateway.subscribers.read().unwrap().is_empty(),
            "Bridge disconnect must remove GatewaySubscriber"
        );
        assert!(
            ctx.quasar_gateway.stream_receivers.read().unwrap().is_empty(),
            "Bridge disconnect must drop retained mpsc::Receiver"
        );
        assert!(ctx.db.get_online_session(1).is_none());

        // 2. Aborting `handle_bridge_client` mid-flight still triggers RAII `SessionCleanupGuard::drop`.
        let ctx_c2 = Arc::clone(&ctx);
        let bridge_task_2 = tokio::spawn(async move {
            let (sock, _) = bridge_listener.accept().await.expect("accept 2");
            handle_bridge_client(sock, ctx_c2).await
        });

        let mut b_stream_2 = TcpStream::connect(bridge_addr).await?;
        assert!(
            bridge_call(
                &mut b_stream_2,
                10,
                "authentication",
                "Login",
                vec![PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            )
            .await?
            .is_ok()
        );
        assert!(
            bridge_call(&mut b_stream_2, 11, "publicGatewaySvc", "Subscribe", Vec::new())
                .await?
                .is_ok()
        );
        assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 1);
        assert_eq!(ctx.quasar_gateway.stream_receivers.read().unwrap().len(), 1);

        bridge_task_2.abort();
        let _ = bridge_task_2.await;

        assert!(
            ctx.quasar_gateway.subscribers.read().unwrap().is_empty(),
            "RAII SessionCleanupGuard must evict subscriber on task abort/drop"
        );
        assert!(
            ctx.quasar_gateway.stream_receivers.read().unwrap().is_empty(),
            "RAII SessionCleanupGuard must drop stream_receiver on task abort/drop"
        );
        drop(b_stream_2);
        Ok(())
    }
}

#[cfg(test)]
#[path = "main_unit_tests.rs"]
mod main_unit_tests;
