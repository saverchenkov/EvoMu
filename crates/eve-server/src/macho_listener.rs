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

use crate::router::ServerContext;
use anyhow::Result;
use eve_machonet::{
    GPSTransportClosed, HandshakeIdentity, HandshakeStepOutput, MachoAddress,
    MachoHandshakeStateMachine, MachoPacket, MachoPacketType, SessionState, VersionExchange,
    MACHO_PROXY_NODE_ID,
};
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, warn};

/// Maximum frame length accepted on TCP listeners (4 MiB, matching `eve_proto::marshal::MAX_FRAME_SIZE`) to prevent memory exhaustion.
pub const MAX_MACHO_FRAME_LEN: usize = eve_proto::marshal::MAX_FRAME_SIZE;
/// Idle timeout waiting for the next machoNet frame header on an open connection.
pub const IDLE_READ_TIMEOUT_SECS: u64 = 300;
/// Maximum timeout to finish reading a frame body after its 4-byte length prefix arrives.
pub const FRAME_BODY_TIMEOUT_SECS: u64 = 30;

static NEXT_MACHO_SESSION_ID: AtomicU64 = AtomicU64::new(10_000);

/// Writes a length-prefixed (`u32` little-endian) marshaled `PyRep` frame to an async writer.
pub async fn write_framed_pyrep<W: AsyncWriteExt + Unpin>(
    stream: &mut W,
    rep: &PyRep,
) -> Result<()> {
    let bytes = marshal::encode(rep);
    stream.write_u32_le(bytes.len() as u32).await?;
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

/// Reads and decodes a single length-prefixed (`u32` little-endian) marshaled `PyRep` frame from an async reader.
pub async fn read_framed_pyrep<R: AsyncReadExt + Unpin>(stream: &mut R) -> Result<Option<PyRep>> {
    let frame_len = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_u32_le(),
    )
    .await
    {
        Ok(Ok(len)) => len as usize,
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => anyhow::bail!("Timed out waiting for machoNet frame header after 5s"),
    };
    if frame_len == 0 || frame_len > MAX_MACHO_FRAME_LEN {
        anyhow::bail!("Invalid machoNet frame length: {frame_len} bytes");
    }
    let mut payload = vec![0u8; frame_len];
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read_exact(&mut payload),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Timed out waiting for machoNet frame body after 5s"))??;
    let rep = marshal::decode(&payload)?;
    Ok(Some(rep))
}

/// Handles a single native `machoNet` TCP connection through the 4-step V24.01 `GPSTransport`
/// handshake state machine and subsequent authenticated `MachoPacket` RPC dispatch (`P-6`).
pub async fn handle_macho_client(mut stream: TcpStream, ctx: Arc<ServerContext>) -> Result<()> {
    let session_id = NEXT_MACHO_SESSION_ID.fetch_add(1, Ordering::Relaxed);
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

    let mut session = SessionState::new(session_id);
    session.address = peer_addr.clone();

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

    // Step 1: Server immediately sends `LowLevelVersionExchange` upon TCP connection accept.
    if let Err(e) = write_framed_pyrep(&mut stream, &handshake.server_hello()).await {
        ctx.unregister_session_closer(session_id);
        return Err(e);
    }

    let mut sys_rx = ctx.systems.get_or_spawn(30000142).subscribe();
    let mut notify_rx = ctx.notify_tx.subscribe();
    let mut ballpark_ready = false;

    let (mut reader, mut writer) = stream.into_split();
    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::channel::<std::result::Result<Vec<u8>, std::io::Error>>(32);
    let reader_task = tokio::spawn(async move {
        loop {
            let frame_len = match tokio::time::timeout(
                std::time::Duration::from_secs(IDLE_READ_TIMEOUT_SECS),
                reader.read_u32_le(),
            )
            .await
            {
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
                                "timed out waiting for machoNet frame header after {IDLE_READ_TIMEOUT_SECS}s"
                            ),
                        )))
                        .await;
                    break;
                }
            };
            if frame_len == 0 || frame_len > MAX_MACHO_FRAME_LEN {
                let _ = frame_tx
                    .send(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("invalid frame length {frame_len} bytes"),
                    )))
                    .await;
                break;
            }
            let mut payload = vec![0u8; frame_len];
            match tokio::time::timeout(
                std::time::Duration::from_secs(FRAME_BODY_TIMEOUT_SECS),
                reader.read_exact(&mut payload),
            )
            .await
            {
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

    loop {
        tokio::select! {
            Some((reason_code, reason)) = close_rx.recv() => {
                let closed = GPSTransportClosed::new(reason, reason_code);
                let _ = write_framed_pyrep(&mut writer, &closed.to_pyrep()).await;
                break;
            }
            Ok(tick_msg) = sys_rx.recv(), if ballpark_ready => {
                if session.character_id.is_some()
                    && session.station_id.is_none()
                    && session.structure_id.is_none()
                    && session.solar_system_id == Some(tick_msg.solar_system_id)
                {
                    if let Some(ship_id) = session.ship_id {
                        if let Some(updates_pyrep) = tick_msg.observer_updates_pyrep.get(&ship_id) {
                            let notif_pkt = MachoPacket::new_broadcast_notification(
                                MACHO_PROXY_NODE_ID,
                                client_id,
                                session.user_id,
                                "DoDestinyUpdate",
                                vec![updates_pyrep.clone(), PyRep::Bool(false)],
                            );
                            if write_framed_pyrep(&mut writer, &notif_pkt.to_pyrep()).await.is_err() {
                                break;
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
                        let mut write_failed = false;
                        for notif in ctx.drain_notifications_for_char(cid) {
                            let notif_pkt = MachoPacket::new_broadcast_notification(
                                MACHO_PROXY_NODE_ID,
                                client_id,
                                session.user_id,
                                &notif.event,
                                notif.args,
                            );
                            if write_framed_pyrep(&mut writer, &notif_pkt.to_pyrep()).await.is_err() {
                                write_failed = true;
                                break;
                            }
                        }
                        if write_failed {
                            break;
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
                let pyrep = match marshal::decode(&payload) {
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
                                    if let Some(d) = displaced {
                                        if d.session_id != session_id {
                                            if let Some(disp_cid) = d.character_id {
                                                let _ = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(
                                                    &ctx, disp_cid,
                                                )
                                                .await;
                                                crate::services::character_skills::notify_watchlist_presence(
                                                    &ctx, disp_cid, false,
                                                );
                                                crate::services::fleet_social::fleet_social_notify::on_character_logout(
                                                    &ctx, disp_cid,
                                                );
                                                crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(
                                                    &ctx, disp_cid,
                                                );
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

                match pkt.packet_type {
                    MachoPacketType::PingReq => {
                        let mut payload = pkt.payload.clone();
                        let now_ft = ctx.now_blueos_filetime_ticks();
                        if let PyRep::Tuple(items) = &mut payload {
                            if let Some(PyRep::List(times)) = items.first_mut() {
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
                            }
                        }
                        let rsp = MachoPacket {
                            packet_type: MachoPacketType::PingRsp,
                            source: pkt.destination.clone(),
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

                        if session.solar_system_id != prev_session.solar_system_id {
                            ballpark_ready = false;
                            if let Some(new_sys) = session.solar_system_id {
                                sys_rx = ctx.systems.get_or_spawn(new_sys).subscribe();
                            }
                        }
                        if (method == "UpdateStateRequest"
                            || (service == "beyonce" && method == "MachoBindObject"))
                            && session.station_id.is_none()
                            && session.structure_id.is_none()
                        {
                            ballpark_ready = true;
                            if service == "beyonce" && method == "MachoBindObject" {
                                let _ = crate::services::space_destiny::handle_update_state_request(
                                    &ctx,
                                    &session,
                                    None,
                                )
                                .await;
                            }
                        }

                        let rsp_pkt = match rpc_res {
                            Ok(val) => {
                                let mut named_payload = HashMap::new();
                                if let PyRep::Tuple(items) = &val {
                                    if let Some(PyRep::SubStruct(inner)) = items.first() {
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
                                }
                                MachoPacket {
                                    packet_type: MachoPacketType::CallRsp,
                                    source: pkt.destination.clone(),
                                    destination: pkt.source.clone(),
                                    user_id: session.user_id,
                                    payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(val))]),
                                    named_payload,
                                }
                            }
                            Err(err) => MachoPacket {
                                packet_type: MachoPacketType::ErrorResponse,
                                source: pkt.destination.clone(),
                                destination: pkt.source.clone(),
                                user_id: session.user_id,
                                payload: eve_machonet::wrapped_runtime_error_body(
                                    MachoPacketType::CallReq as i64,
                                    &format!("{}: {}", err.code, err.message),
                                ),
                                named_payload: HashMap::new(),
                            },
                        };
                        if write_framed_pyrep(&mut writer, &rsp_pkt.to_pyrep()).await.is_err() {
                            break;
                        }

                        let mut sess_change_sent = false;
                        if let Some(sess_change_payload) = prev_session
                            .to_session_change_pyrep(&session, vec![MACHO_PROXY_NODE_ID as i64])
                        {
                            sess_change_sent = true;
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
                            if write_framed_pyrep(&mut writer, &sess_pkt.to_pyrep()).await.is_err()
                            {
                                break;
                            }
                        }

                        let was_in_space = prev_session.station_id.is_none()
                            && prev_session.structure_id.is_none();
                        let is_in_space =
                            session.station_id.is_none() && session.structure_id.is_none();
                        if ballpark_ready && (was_in_space || is_in_space) {
                            if let (Some(sys_id), Some(ship_id)) = (
                                session.solar_system_id.or(prev_session.solar_system_id),
                                session.ship_id.or(prev_session.ship_id),
                            ) {
                                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                                if let Ok((_tick, obs_evs, updates_pyrep)) =
                                    sys_handle.drain_observer_updates(ship_id).await
                                {
                                    if !obs_evs.is_empty() {
                                        let notif_pkt = MachoPacket::new_broadcast_notification(
                                            MACHO_PROXY_NODE_ID,
                                            client_id,
                                            session.user_id,
                                            "DoDestinyUpdate",
                                            vec![updates_pyrep, PyRep::Bool(false)],
                                        );
                                        if write_framed_pyrep(&mut writer, &notif_pkt.to_pyrep())
                                            .await
                                            .is_err()
                                        {
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                        if !is_in_space {
                            ballpark_ready = false;
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
                        let mut notif_write_failed = false;
                        for notif in pre_rpc_notifs {
                            if sess_change_sent && notif.event == "OnSessionChanged" {
                                continue;
                            }
                            let notif_pkt = MachoPacket::new_broadcast_notification(
                                MACHO_PROXY_NODE_ID,
                                client_id,
                                session.user_id,
                                &notif.event,
                                notif.args,
                            );
                            if write_framed_pyrep(&mut writer, &notif_pkt.to_pyrep())
                                .await
                                .is_err()
                            {
                                notif_write_failed = true;
                                break;
                            }
                        }
                        if !notif_write_failed {
                            for (event, args) in session.drain_notifications() {
                                if sess_change_sent && event == "OnSessionChanged" {
                                    continue;
                                }
                                let notif_pkt = MachoPacket::new_broadcast_notification(
                                    MACHO_PROXY_NODE_ID,
                                    client_id,
                                    session.user_id,
                                    &event,
                                    args,
                                );
                                if write_framed_pyrep(&mut writer, &notif_pkt.to_pyrep())
                                    .await
                                    .is_err()
                                {
                                    notif_write_failed = true;
                                    break;
                                }
                            }
                        }
                        if notif_write_failed {
                            break;
                        }
                    }
                    MachoPacketType::AuthenticationReq => {
                        let err_pkt = MachoPacket {
                            packet_type: MachoPacketType::ErrorResponse,
                            source: pkt.destination.clone(),
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
    ctx.unregister_session_closer(session_id);
    ctx.release_session_bindings(session_id);
    if let Some(uid) = session.user_id {
        // A dropped connection that still held the character online is a log-off for watchers and triggers SP-6 in-space logoff handling.
        if ctx.db.release_online_lock(uid, session_id) {
            if let (Some(sys_id), Some(ship_id)) = (session.solar_system_id, session.ship_id) {
                if session.station_id.is_none() && session.structure_id.is_none() {
                    let handle = ctx.systems.get_or_spawn(sys_id);
                    if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
                        ctx.db.with_mut_snapshot(|snap| {
                            snap.item_positions.insert(
                                ship_id,
                                [ball.position.x, ball.position.y, ball.position.z],
                            );
                        });
                    }
                }
            }
            if let Some(cid) = session.character_id {
                let _ = crate::sp6_lifecycle::handle_character_logoff_or_disconnect(&ctx, cid).await;
                crate::services::character_skills::notify_watchlist_presence(&ctx, cid, false);
                crate::services::fleet_social::fleet_social_notify::on_character_logout(&ctx, cid);
                crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(&ctx, cid);
            }
        }
    }

    Ok(())
}

pub use eve_machonet::extract_macho_call_req;
