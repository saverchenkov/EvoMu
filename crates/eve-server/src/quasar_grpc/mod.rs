//! `quasar_grpc` — Native V24.01 Quasar Public Gateway HTTP/2 (`h2c`) gRPC transport.
//!
//! Serves V24.01 EVE client gateway RPCs over HTTP/2 (`h2c`, port `26005`):
//! - `/eve_public.gateway.Requests/Send` (request/response unary-over-h2c)
//! - `/eve_public.gateway.Notices/Consume` (server-streaming push notices)
//! - `/eve_public.gateway.Events/Publish` (client telemetry & event publishing)
//! - `/eve_public.gateway.Events/Ping`
//! - `/eve_public.gateway.Notices/Ping`
//! - `/eve_public.gateway.Requests/Ping`

#![forbid(unsafe_code)]

pub(crate) mod normalize_notices;

pub use normalize_notices::normalize_quasar_client_response;
pub(crate) use normalize_notices::normalize_quasar_any_payload;

use crate::router::ServerContext;
use bytes::{Buf, Bytes, BytesMut};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{EvePublicEvent, EvePublicRequest, QuasarMessage};
use flate2::read::{GzDecoder, ZlibDecoder};
use h2::server::SendResponse;
use h2::RecvStream;
use http::{HeaderMap, HeaderValue, Request, Response, StatusCode};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

const GRPC_STATUS_OK: &str = "0";
const GRPC_STATUS_INVALID_ARGUMENT: &str = "3";
const GRPC_STATUS_PERMISSION_DENIED: &str = "7";
const GRPC_STATUS_RESOURCE_EXHAUSTED: &str = "8";
const GRPC_STATUS_UNIMPLEMENTED: &str = "12";
const GRPC_STATUS_UNAUTHENTICATED: &str = "16";

/// Pre-encoded 5-byte gRPC frame header for an empty `google.protobuf.Empty` response (`[0, 0, 0, 0, 0]`).
const EMPTY_GRPC_FRAME: [u8; 5] = [0, 0, 0, 0, 0];

/// Maximum concurrent active HTTP/2 gRPC streams per TCP connection.
pub const H2_MAX_CONCURRENT_STREAMS: u32 = 256;
/// Per-stream HTTP/2 receive window size (`1 MiB`).
pub const H2_INITIAL_STREAM_WINDOW_BYTES: u32 = 1_048_576;
/// Connection-wide HTTP/2 receive window size (`4 MiB`).
pub const H2_INITIAL_CONNECTION_WINDOW_BYTES: u32 = 4_194_304;
/// Maximum HTTP/2 frame payload size (`256 KiB`, bounded within RFC 7540 `16 KiB..=16 MiB`).
pub const H2_MAX_FRAME_SIZE_BYTES: u32 = 262_144;
/// Maximum decoded HTTP/2 HPACK header list size (`64 KiB`).
pub const H2_MAX_HEADER_LIST_BYTES: u32 = 65_536;
/// Maximum `h2` per-stream outbound send buffer size (`1 MiB`).
pub const H2_MAX_SEND_BUFFER_BYTES: usize = 1_048_576;
/// Maximum locally-reset stream state entries retained concurrently per HTTP/2 connection.
pub const H2_MAX_CONCURRENT_RESET_STREAMS: usize = 256;
/// Maximum pending-accept remotely-reset streams tolerated before `ENHANCE_YOUR_CALM`.
pub const H2_MAX_PENDING_ACCEPT_RESET_STREAMS: usize = 128;
/// Maximum wall-clock time allowed for the HTTP/2 connection preface and `SETTINGS` handshake.
pub const H2_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum time to wait for outbound HTTP/2 flow-control capacity (`WINDOW_UPDATE`) from a stalled peer.
pub const H2_SEND_CAPACITY_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum wire bytes buffered inside `GrpcFrameReader::buf` (`1 MiB`), matching `H2_INITIAL_STREAM_WINDOW_BYTES`.
pub const MAX_GRPC_FRAME_BUFFER_BYTES: usize = H2_INITIAL_STREAM_WINDOW_BYTES as usize;
/// Maximum uncompressed or wire gRPC message payload size (`1 MiB - 5` bytes so a complete frame fits in one stream window).
pub const MAX_GRPC_MESSAGE_BYTES: usize = MAX_GRPC_FRAME_BUFFER_BYTES - 5;

static NEXT_GRPC_SESSION_ID: AtomicU64 = AtomicU64::new(9_000_000);
static LAST_REJECTED_BEARER_WARN_UNIX: AtomicU64 = AtomicU64::new(0);

fn warn_rejected_bearer_token(now_unix: u64, peer_ip: Option<std::net::IpAddr>, reason: &str) {
    let prev = LAST_REJECTED_BEARER_WARN_UNIX.load(Ordering::Relaxed);
    if (prev == 0 || now_unix >= prev.saturating_add(10))
        && LAST_REJECTED_BEARER_WARN_UNIX
            .compare_exchange(prev, now_unix.max(1), Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        tracing::warn!(
            peer_ip = ?peer_ip,
            reason = reason,
            "Rejected Quasar gRPC bearer token"
        );
    }
}

/// RAII guard ensuring `Notices/Consume` always unsubscribes `session_id` from `PublicGatewayRouter`
/// on normal stream close, early protocol/send error, auth revocation, or task cancellation/drop.
struct GrpcSubscriberGuard {
    gateway: Arc<crate::quasar_gateway::PublicGatewayRouter>,
    session_id: u64,
}

impl Drop for GrpcSubscriberGuard {
    fn drop(&mut self) {
        self.gateway.unsubscribe_session(self.session_id);
    }
}

pub async fn handle_quasar_grpc_connection(
    stream: TcpStream,
    ctx: Arc<ServerContext>,
) -> anyhow::Result<()> {
    handle_quasar_grpc_connection_with_limits(
        stream,
        ctx,
        H2_MAX_CONCURRENT_STREAMS,
        H2_SEND_CAPACITY_TIMEOUT,
    )
    .await
}

pub async fn handle_quasar_grpc_connection_with_limits(
    stream: TcpStream,
    ctx: Arc<ServerContext>,
    max_concurrent_streams: u32,
    send_capacity_timeout: Duration,
) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let peer_ip = stream
        .peer_addr()
        .ok()
        .map(|sa| eve_db::gateway_tokens::normalize_peer_ip(sa.ip()));
    let effective_max_streams = max_concurrent_streams.max(1);
    let mut builder = h2::server::Builder::new();
    builder
        .max_concurrent_streams(effective_max_streams)
        .max_local_error_reset_streams(None)
        .max_concurrent_reset_streams(H2_MAX_CONCURRENT_RESET_STREAMS)
        .max_pending_accept_reset_streams(H2_MAX_PENDING_ACCEPT_RESET_STREAMS)
        .initial_window_size(H2_INITIAL_STREAM_WINDOW_BYTES)
        .initial_connection_window_size(H2_INITIAL_CONNECTION_WINDOW_BYTES)
        .max_frame_size(H2_MAX_FRAME_SIZE_BYTES)
        .max_header_list_size(H2_MAX_HEADER_LIST_BYTES)
        .max_send_buffer_size(H2_MAX_SEND_BUFFER_BYTES);
    let mut h2_conn =
        tokio::time::timeout(H2_HANDSHAKE_TIMEOUT, builder.handshake::<_, Bytes>(stream))
            .await
            .map_err(|_| anyhow::anyhow!("Quasar gRPC HTTP/2 handshake timed out"))??;

    let stream_semaphore = Arc::new(tokio::sync::Semaphore::new(effective_max_streams as usize));

    while let Some(accept_res) = h2_conn.accept().await {
        let (request, mut respond) = match accept_res {
            Ok(pair) => pair,
            Err(e) => {
                if !e.is_io() {
                    tracing::warn!(error = %e, "Quasar gRPC stream accept error");
                }
                break;
            }
        };
        let Ok(stream_permit) = Arc::clone(&stream_semaphore).try_acquire_owned() else {
            let _ = send_trailers_only(
                &mut respond,
                GRPC_STATUS_RESOURCE_EXHAUSTED,
                "Maximum concurrent Quasar gRPC streams exceeded",
            );
            continue;
        };
        let stream_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            let _permit = stream_permit;
            if let Err(e) = handle_grpc_stream(
                request,
                respond,
                stream_ctx,
                peer_ip,
                send_capacity_timeout,
            )
            .await
            {
                tracing::warn!(error = %e, "Quasar gRPC stream closed with error");
            }
        });
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct VerifiedGatewayIdentity {
    raw_token: String,
    peer_ip: Option<std::net::IpAddr>,
    user_id: u64,
    token_character_id: Option<i64>,
    is_full_sso: bool,
    bound_online_session_id: Option<u64>,
}

async fn handle_grpc_stream(
    request: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    ctx: Arc<ServerContext>,
    peer_ip: Option<std::net::IpAddr>,
    send_capacity_timeout: Duration,
) -> anyhow::Result<()> {
    let path = request.uri().path().to_string();
    let headers = request.headers().clone();
    let grpc_encoding = headers
        .get("grpc-encoding")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("identity")
        .to_ascii_lowercase();
    let app_uuid_override = parse_app_instance_uuid(&headers);

    match path.as_str() {
        "/eve_public.gateway.Events/Ping"
        | "/eve_public.gateway.Notices/Ping"
        | "/eve_public.gateway.Requests/Ping" => {
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);
            let _ = reader.next_message().await?;
            let mut send = send_grpc_headers(&mut respond, false)?;
            send_grpc_data_bounded(
                &mut send,
                Bytes::from_static(&EMPTY_GRPC_FRAME),
                false,
                send_capacity_timeout,
            )
            .await?;
            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        "/eve_public.gateway.Requests/Send" => {
            let Some(mut identity) = authenticate_grpc_headers(&ctx, &headers, peer_ip) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut send = send_grpc_headers(&mut respond, false)?;
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);

            while let Some(frame_bytes) = reader.next_message().await? {
                let mut pub_req = match EvePublicRequest::decode_proto(&frame_bytes) {
                    Ok(r) => r,
                    Err(e) => {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_INVALID_ARGUMENT,
                            Some(&format!("Invalid EvePublicRequest: {e}")),
                        )?;
                        return Ok(());
                    }
                };
                if pub_req.application_instance_uuid.is_empty() {
                    if let Some(ref hdr_uuid) = app_uuid_override {
                        pub_req.application_instance_uuid = hdr_uuid.clone();
                    }
                }
                let claimed_cid = pub_req
                    .authoritative_context
                    .as_ref()
                    .and_then(|ac| ac.effective_character_id())
                    .map(i64::from);
                let session = match build_session_for_identity(
                    &ctx,
                    grpc_session_id,
                    &mut identity,
                    claimed_cid,
                ) {
                    Ok(s) => s,
                    Err(reason) => {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_UNAUTHENTICATED,
                            Some(&reason),
                        )?;
                        return Ok(());
                    }
                };
                let mut rsp = ctx.quasar_gateway.handle_public_request(
                    &session,
                    &pub_req,
                    None,
                    false,
                    ctx.now_unix(),
                );
                normalize_quasar_client_response(&ctx.quasar_gateway.descriptors, &mut rsp);
                if rsp.status_code != 200 {
                    let req_type = pub_req
                        .payload
                        .as_ref()
                        .map(|p| p.normalized_type_name())
                        .unwrap_or("<none>");
                    tracing::warn!(
                        req_type = %req_type,
                        status_code = rsp.status_code,
                        status_message = %rsp.status_message,
                        user_id = identity.user_id,
                        char_id = ?session.character_id,
                        "Quasar gRPC public request returned non-200 status"
                    );
                }
                let encoded = frame_grpc_message(&rsp.encode_proto());
                send_grpc_data_bounded(&mut send, encoded, false, send_capacity_timeout).await?;
            }

            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        "/eve_public.gateway.Notices/Consume" => {
            let Some(mut identity) = authenticate_grpc_headers(&ctx, &headers, peer_ip) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut session =
                match build_session_for_identity(&ctx, grpc_session_id, &mut identity, None) {
                    Ok(s) => s,
                    Err(reason) => {
                        return send_trailers_only(
                            &mut respond,
                            GRPC_STATUS_UNAUTHENTICATED,
                            &reason,
                        );
                    }
                };
            let now_unix = ctx.now_unix();
            let mut rx =
                ctx.quasar_gateway
                    .subscribe_session(&session, Vec::new(), now_unix);
            let _subscriber_guard = GrpcSubscriberGuard {
                gateway: Arc::clone(&ctx.quasar_gateway),
                session_id: grpc_session_id,
            };
            if let Some(ref app_uuid) = app_uuid_override {
                if let Ok(mut subs) = ctx.quasar_gateway.subscribers.write() {
                    if let Some(sub) = subs.get_mut(&grpc_session_id) {
                        sub.application_instance_uuid = app_uuid.clone();
                    }
                }
                ctx.quasar_gateway.on_session_changed(&session, now_unix);
            }

            let mut send = send_grpc_headers(&mut respond, false)?;
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);
            let mut sync_tick = tokio::time::interval(Duration::from_millis(500));
            sync_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    maybe_notice = rx.recv() => {
                        let Some(mut notice) = maybe_notice else {
                            match build_session_for_identity(&ctx, grpc_session_id, &mut identity, None) {
                                Ok(_) => send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?,
                                Err(reason) => {
                                    send_grpc_trailers(&mut send, GRPC_STATUS_UNAUTHENTICATED, Some(&reason))?
                                }
                            }
                            break;
                        };
                        match build_session_for_identity(&ctx, grpc_session_id, &mut identity, None) {
                            Ok(updated) => {
                                if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                    session = updated;
                                    ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
                                }
                            }
                            Err(reason) => {
                                send_grpc_trailers(&mut send, GRPC_STATUS_UNAUTHENTICATED, Some(&reason))?;
                                break;
                            }
                        }
                        if let Some(ref mut any_payload) = notice.payload {
                            normalize_quasar_any_payload(&ctx.quasar_gateway.descriptors, any_payload);
                        }
                        let encoded = frame_grpc_message(&notice.encode_proto());
                        send_grpc_data_bounded(&mut send, encoded, false, send_capacity_timeout).await?;
                    }
                    msg_res = reader.next_message() => {
                        match msg_res {
                            Ok(Some(_empty_ack_or_sub)) => {
                                // Refresh session context and re-verify auth/session/token revocation when client sends Subscribe/Acknowledge.
                                match build_session_for_identity(&ctx, grpc_session_id, &mut identity, None) {
                                    Ok(updated) => {
                                        if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                            session = updated;
                                            ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
                                        }
                                    }
                                    Err(reason) => {
                                        send_grpc_trailers(&mut send, GRPC_STATUS_UNAUTHENTICATED, Some(&reason))?;
                                        break;
                                    }
                                }
                            }
                            Ok(None) => {
                                // Client called WritesDone() (`FinishStream`).
                                send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
                                break;
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    _ = sync_tick.tick() => {
                        match build_session_for_identity(&ctx, grpc_session_id, &mut identity, None) {
                            Ok(updated) => {
                                if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                    session = updated;
                                    ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
                                }
                            }
                            Err(reason) => {
                                send_grpc_trailers(&mut send, GRPC_STATUS_UNAUTHENTICATED, Some(&reason))?;
                                break;
                            }
                        }
                    }
                    _ = std::future::poll_fn(|cx| send.poll_reset(cx)) => {
                        break;
                    }
                }
            }
            Ok(())
        }
        "/eve_public.gateway.Events/Publish" => {
            let Some(mut identity) = authenticate_grpc_headers(&ctx, &headers, peer_ip) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut send = send_grpc_headers(&mut respond, false)?;
            let mut reader = GrpcFrameReader::new(request.into_body(), grpc_encoding);

            while let Some(frame_bytes) = reader.next_message().await? {
                let mut ev = match EvePublicEvent::decode_proto(&frame_bytes) {
                    Ok(ev) => ev,
                    Err(e) => {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_INVALID_ARGUMENT,
                            Some(&format!("Invalid EvePublicEvent: {e}")),
                        )?;
                        return Ok(());
                    }
                };
                if ev.application_instance_uuid.is_empty() {
                    if let Some(ref hdr_uuid) = app_uuid_override {
                        ev.application_instance_uuid = hdr_uuid.clone();
                    }
                }
                let claimed_cid = ev
                    .authoritative_context
                    .as_ref()
                    .and_then(|ac| ac.effective_character_id())
                    .map(i64::from);
                let session = match build_session_for_identity(
                    &ctx,
                    grpc_session_id,
                    &mut identity,
                    claimed_cid,
                ) {
                    Ok(s) => s,
                    Err(reason) => {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_UNAUTHENTICATED,
                            Some(&reason),
                        )?;
                        return Ok(());
                    }
                };
                if let Some(ref ac) = ev.authoritative_context {
                    if ac
                        .effective_user_id()
                        .is_some_and(|u| session.user_id != Some(u64::from(u as u32)))
                        || ac
                            .effective_character_id()
                            .is_some_and(|c| session.character_id != Some(i64::from(c)))
                        || ac
                            .effective_corporation_id()
                            .is_some_and(|corp| session.corporation_id != Some(i64::from(corp)))
                        || (ac.service_role_mask & !session.role) != 0
                    {
                        send_grpc_trailers(
                            &mut send,
                            GRPC_STATUS_PERMISSION_DENIED,
                            Some("AuthoritativeContext does not match authenticated session"),
                        )?;
                        return Ok(());
                    }
                }
                ctx.quasar_gateway
                    .publish_event(&session, ev, ctx.now_unix());
                // `EventPublisher::ReadConfirm` / `ReadEmpty` expects one `google.protobuf.Empty` per Event.
                send_grpc_data_bounded(
                    &mut send,
                    Bytes::from_static(&EMPTY_GRPC_FRAME),
                    false,
                    send_capacity_timeout,
                )
                .await?;
            }

            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        _ => send_trailers_only(
            &mut respond,
            GRPC_STATUS_UNIMPLEMENTED,
            &format!("Unknown gRPC method {path}"),
        ),
    }
}

fn session_scope_tuple(s: &SessionState) -> (Option<u64>, Option<i64>, Option<i64>, Option<i64>, Option<u32>, Option<i64>) {
    (
        s.user_id,
        s.character_id,
        s.corporation_id,
        s.alliance_id,
        s.solar_system_id,
        s.fleet_id,
    )
}

fn verify_gateway_bearer_token(
    ctx: &ServerContext,
    token: &str,
    peer_ip: Option<std::net::IpAddr>,
    now_unix: u64,
) -> Result<eve_db::QuasarVerifiedToken, String> {
    match ctx.db.verify_quasar_bearer_jwt(token, now_unix) {
        Ok(v) => Ok(v),
        Err(bearer_err) if ctx.feature_flags.allow_local_dev_gateway_jwt => {
            let Some(ip) = peer_ip else {
                return Err("missing_peer_ip_for_local_dev_jwt".to_string());
            };
            ctx.db
                .verify_local_dev_client_jwt(token, ip, now_unix)
                .map_err(|local_err| {
                    format!("bearer_and_local_dev_jwt_rejected ({bearer_err:?} / {local_err:?})")
                })
        }
        Err(bearer_err) => Err(format!("bearer_jwt_rejected ({bearer_err:?})")),
    }
}

fn build_session_for_identity(
    ctx: &ServerContext,
    grpc_session_id: u64,
    identity: &mut VerifiedGatewayIdentity,
    claimed_char_id: Option<i64>,
) -> Result<SessionState, String> {
    let now_unix = ctx.now_unix();
    ctx.db.clear_expired_ban_by_id(identity.user_id, now_unix);

    // 1. Re-verify the bearer JWT against `now_unix`, `revoked_jtis`, `kicked_users`,
    //    `online_sessions` (for local-dev JWTs), and `acc.check_active_restriction(now_unix)`.
    let reverified =
        verify_gateway_bearer_token(ctx, &identity.raw_token, identity.peer_ip, now_unix).map_err(
            |reason| format!("Quasar authorization token revoked or expired: {reason}"),
        )?;
    if reverified.user_id != identity.user_id {
        return Err("Quasar authorization token subject changed mid-stream".to_string());
    }
    identity.token_character_id = reverified.character_id;
    identity.is_full_sso = reverified.is_full_sso;

    // 2. Enforce session revocation, displacement, and account restrictions via `check_session_revoked`.
    if let Some(bound_sid) = identity.bound_online_session_id {
        if let Some((code, msg)) =
            ctx.db
                .check_session_revoked(identity.user_id, bound_sid, now_unix)
        {
            return Err(format!("Quasar session revoked ({code}): {msg}"));
        }
        if bound_sid != grpc_session_id {
            if let Some((code, msg)) =
                ctx.db
                    .check_session_revoked(identity.user_id, grpc_session_id, now_unix)
            {
                if code != "MultiLoginBlocked" {
                    return Err(format!("Quasar stream revoked ({code}): {msg}"));
                }
            }
        }
    } else {
        if let Some((code, msg)) =
            ctx.db
                .check_session_revoked(identity.user_id, grpc_session_id, now_unix)
        {
            if code != "MultiLoginBlocked" {
                return Err(format!("Quasar session revoked ({code}): {msg}"));
            }
        }
        if let Some(os) = ctx.db.get_online_session(identity.user_id) {
            identity.bound_online_session_id = Some(os.session_id);
        }
    }

    // 3. Re-read live `acc.role` and active account restriction status so mid-stream GM/admin
    //    role revocations (`set_account_role`) take effect on the very next frame.
    let Some(mut live_role) = lookup_active_account_role(ctx, identity.user_id, now_unix) else {
        return Err("Quasar account missing or restricted".to_string());
    };
    if !identity.is_full_sso {
        live_role &= !service_roles::ROLE_GM_MASK;
    }

    let mut session = SessionState::new(grpc_session_id);
    session.user_id = Some(identity.user_id);
    session.role = live_role;

    let resolved_char = ctx.db.with_snapshot(|snap| {
        if let Some(cid) = claimed_char_id {
            if let Some(ch) = snap.characters.get(&cid) {
                if ch.user_id == identity.user_id {
                    return Some((cid, ch.corporation_id, ch.alliance_id));
                }
            }
            // Do not populate `claimed_char_id` if it belongs to another user;
            // fall through to caller's own online/token character (if any) so `handle_public_request`
            // returns `403 AuthoritativeContext character mismatch`.
        }
        if let Some(online_cid) = snap
            .sessions
            .get(&identity.user_id)
            .and_then(|os| os.character_id)
        {
            if let Some(ch) = snap.characters.get(&online_cid) {
                if ch.user_id == identity.user_id {
                    return Some((online_cid, ch.corporation_id, ch.alliance_id));
                }
            }
        }
        identity.token_character_id.and_then(|cid| {
            snap.characters
                .get(&cid)
                .filter(|c| c.user_id == identity.user_id)
                .map(|c| (cid, c.corporation_id, c.alliance_id))
        })
    });

    if let Some((cid, corp_id, alliance_id)) = resolved_char {
        session.character_id = Some(cid);
        session.corporation_id = Some(corp_id);
        session.alliance_id = alliance_id;
        ctx.sync_session(&mut session);
    }
    Ok(session)
}

fn parse_app_instance_uuid(headers: &HeaderMap) -> Option<Vec<u8>> {
    if let Some(val) = headers.get("application_instance_uuid") {
        if let Ok(s) = val.to_str() {
            let trimmed = s.trim();
            if let Ok(u) = uuid::Uuid::parse_str(trimmed) {
                return Some(u.as_bytes().to_vec());
            }
            if trimmed.len() == 16 {
                return Some(trimmed.as_bytes().to_vec());
            }
        }
    }
    if let Some(val) = headers.get("application_instance_uuid-bin") {
        if let Ok(s) = val.to_str() {
            if let Ok(decoded) = eve_db::base64url_decode_no_pad(s.trim()) {
                if decoded.len() == 16 {
                    return Some(decoded);
                }
            }
        }
    }
    None
}

fn authenticate_grpc_headers(
    ctx: &ServerContext,
    headers: &HeaderMap,
    peer_ip: Option<std::net::IpAddr>,
) -> Option<VerifiedGatewayIdentity> {
    let now_unix = ctx.now_unix();
    let raw_hdr = headers
        .get("authorization")
        .or_else(|| headers.get("authorization_token"))?
        .to_str()
        .ok()?
        .trim();
    if raw_hdr.is_empty() {
        warn_rejected_bearer_token(now_unix, peer_ip, "empty_authorization_header");
        return None;
    }
    let token = raw_hdr
        .strip_prefix("Bearer ")
        .or_else(|| raw_hdr.strip_prefix("bearer "))
        .unwrap_or(raw_hdr)
        .trim();
    if token.is_empty() {
        warn_rejected_bearer_token(now_unix, peer_ip, "empty_bearer_token");
        return None;
    }

    let verified = match verify_gateway_bearer_token(ctx, token, peer_ip, now_unix) {
        Ok(v) => v,
        Err(reason) => {
            warn_rejected_bearer_token(now_unix, peer_ip, &reason);
            return None;
        }
    };
    ctx.db.clear_expired_ban_by_id(verified.user_id, now_unix);
    if lookup_active_account_role(ctx, verified.user_id, now_unix).is_none() {
        warn_rejected_bearer_token(
            now_unix,
            peer_ip,
            "account_missing_or_restricted_for_token",
        );
        return None;
    }
    let bound_online_session_id = ctx
        .db
        .get_online_session(verified.user_id)
        .map(|os| os.session_id);
    if let Some(bound_sid) = bound_online_session_id {
        if let Some((code, msg)) =
            ctx.db
                .check_session_revoked(verified.user_id, bound_sid, now_unix)
        {
            warn_rejected_bearer_token(
                now_unix,
                peer_ip,
                &format!("session_revoked ({code}: {msg})"),
            );
            return None;
        }
    }
    Some(VerifiedGatewayIdentity {
        raw_token: token.to_string(),
        peer_ip,
        user_id: verified.user_id,
        token_character_id: verified.character_id,
        is_full_sso: verified.is_full_sso,
        bound_online_session_id,
    })
}

fn lookup_active_account_role(ctx: &ServerContext, user_id: u64, now_unix: u64) -> Option<u64> {
    ctx.db.with_snapshot(|snap| {
        let acc = snap.accounts.values().find(|a| a.user_id == user_id)?;
        acc.check_active_restriction(now_unix).ok()?;
        Some(acc.role)
    })
}

/// Reserves outbound HTTP/2 flow-control capacity (`SendStream::reserve_capacity`) and sends
/// `data` in window-bounded slices so slow or zero-window peers cannot trigger unbounded
/// per-stream memory growth inside `h2`.
async fn send_grpc_data_bounded(
    send: &mut h2::SendStream<Bytes>,
    mut data: Bytes,
    end_of_stream: bool,
    capacity_timeout: Duration,
) -> anyhow::Result<()> {
    if data.is_empty() {
        send.send_data(Bytes::new(), end_of_stream)?;
        return Ok(());
    }
    while !data.is_empty() {
        send.reserve_capacity(data.len());
        let mut avail = send.capacity();
        if avail == 0 {
            let wait_res = tokio::time::timeout(
                capacity_timeout,
                std::future::poll_fn(|cx| send.poll_capacity(cx)),
            )
            .await;
            match wait_res {
                Ok(Some(Ok(granted))) => {
                    avail = granted;
                }
                Ok(Some(Err(e))) => {
                    send.reserve_capacity(0);
                    return Err(e.into());
                }
                Ok(None) => {
                    send.reserve_capacity(0);
                    anyhow::bail!(
                        "Quasar gRPC stream closed or reset while waiting for HTTP/2 send capacity"
                    );
                }
                Err(_) => {
                    send.reserve_capacity(0);
                    send.send_reset(h2::Reason::FLOW_CONTROL_ERROR);
                    anyhow::bail!(
                        "Timed out after {:?} waiting for HTTP/2 outbound flow-control capacity",
                        capacity_timeout
                    );
                }
            }
        }
        let chunk_len = data.len().min(avail);
        let chunk = data.split_to(chunk_len);
        let is_final = end_of_stream && data.is_empty();
        if let Err(e) = send.send_data(chunk, is_final) {
            send.reserve_capacity(0);
            return Err(e.into());
        }
    }
    Ok(())
}

fn send_grpc_headers(
    respond: &mut SendResponse<Bytes>,
    end_of_stream: bool,
) -> Result<h2::SendStream<Bytes>, h2::Error> {
    let rsp = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .header("grpc-accept-encoding", "identity,deflate,gzip")
        .body(())
        .expect("static gRPC response headers");
    respond.send_response(rsp, end_of_stream)
}

fn send_trailers_only(
    respond: &mut SendResponse<Bytes>,
    grpc_status: &str,
    grpc_message: &str,
) -> anyhow::Result<()> {
    let mut rsp = Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/grpc")
        .header("grpc-status", grpc_status)
        .body(())?;
    if !grpc_message.is_empty() {
        let sanitized: String = grpc_message
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(200)
            .collect();
        if let Ok(val) = HeaderValue::from_str(&sanitized) {
            rsp.headers_mut().insert("grpc-message", val);
        }
    }
    respond.send_response(rsp, true)?;
    Ok(())
}

fn send_grpc_trailers(
    send: &mut h2::SendStream<Bytes>,
    grpc_status: &str,
    grpc_message: Option<&str>,
) -> Result<(), h2::Error> {
    let mut trailers = HeaderMap::new();
    trailers.insert(
        "grpc-status",
        HeaderValue::from_str(grpc_status).unwrap_or_else(|_| HeaderValue::from_static("2")),
    );
    if let Some(msg) = grpc_message {
        let sanitized: String = msg
            .chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(200)
            .collect();
        if !sanitized.is_empty() {
            if let Ok(val) = HeaderValue::from_str(&sanitized) {
                trailers.insert("grpc-message", val);
            }
        }
    }
    send.send_trailers(trailers)
}

pub fn frame_grpc_message(payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.extend_from_slice(&[0u8]);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf.freeze()
}

struct GrpcFrameReader {
    body: RecvStream,
    buf: BytesMut,
    encoding: String,
    eof: bool,
}

impl GrpcFrameReader {
    fn new(body: RecvStream, encoding: String) -> Self {
        Self {
            body,
            buf: BytesMut::with_capacity(4096),
            encoding,
            eof: false,
        }
    }

    #[cfg(test)]
    fn buffered_bytes(&self) -> usize {
        self.buf.len()
    }

    #[cfg(test)]
    fn used_flow_control_capacity(&mut self) -> usize {
        self.body.flow_control().used_capacity()
    }

    async fn next_message(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        loop {
            if self.buf.len() >= 5 {
                let compressed = self.buf[0];
                if compressed > 1 {
                    anyhow::bail!(
                        "Invalid gRPC frame compression flag {compressed} (expected 0 or 1)"
                    );
                }
                let msg_len =
                    u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]])
                        as usize;
                if msg_len > MAX_GRPC_MESSAGE_BYTES
                    || 5usize.saturating_add(msg_len) > MAX_GRPC_FRAME_BUFFER_BYTES
                {
                    anyhow::bail!(
                        "gRPC frame length {msg_len} exceeds MAX_GRPC_MESSAGE_BYTES ({MAX_GRPC_MESSAGE_BYTES})"
                    );
                }
                let frame_wire_len = 5 + msg_len;
                if self.buf.len() >= frame_wire_len {
                    self.buf.advance(5);
                    let raw = self.buf.split_to(msg_len);
                    // Release HTTP/2 RecvStream flow-control capacity ONLY as complete gRPC
                    // frames are drained from `self.buf`, keeping partial or pipelined frames
                    // accounted against the stream and connection windows.
                    self.body.flow_control().release_capacity(frame_wire_len)?;
                    if compressed == 0 {
                        return Ok(Some(raw.to_vec()));
                    }
                    let decoded = decompress_grpc_payload(&raw, &self.encoding)?;
                    return Ok(Some(decoded));
                }
            }
            if self.eof {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                anyhow::bail!(
                    "Truncated gRPC frame at stream EOF ({} trailing bytes)",
                    self.buf.len()
                );
            }
            match self.body.data().await {
                Some(Ok(chunk)) => {
                    let len = chunk.len();
                    if len > 0 {
                        if self.buf.len().saturating_add(len) > MAX_GRPC_FRAME_BUFFER_BYTES {
                            anyhow::bail!(
                                "gRPC frame buffer ({} + {len} bytes) exceeds MAX_GRPC_FRAME_BUFFER_BYTES ({MAX_GRPC_FRAME_BUFFER_BYTES})",
                                self.buf.len()
                            );
                        }
                        self.buf.extend_from_slice(&chunk);
                    }
                }
                Some(Err(e)) => return Err(e.into()),
                None => {
                    self.eof = true;
                }
            }
        }
    }
}

fn decompress_grpc_payload(raw: &[u8], encoding: &str) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    match encoding {
        "gzip" => {
            let mut dec = GzDecoder::new(raw);
            dec.by_ref()
                .take((MAX_GRPC_MESSAGE_BYTES + 1) as u64)
                .read_to_end(&mut out)?;
        }
        "deflate" => {
            let mut dec = ZlibDecoder::new(raw);
            dec.by_ref()
                .take((MAX_GRPC_MESSAGE_BYTES + 1) as u64)
                .read_to_end(&mut out)?;
        }
        other => anyhow::bail!("Unsupported compressed gRPC encoding '{other}'"),
    }
    if out.len() > MAX_GRPC_MESSAGE_BYTES {
        anyhow::bail!("Decompressed gRPC payload exceeds MAX_GRPC_MESSAGE_BYTES");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quasar_gateway::{NoticeScope, STATUS_FORBIDDEN, STATUS_OK};
    use crate::router::client_compat::normalize_macho_rpc_reply;
    use eve_proto::quasar::{
        AuthoritativeContext, EvePublicNotice, EvePublicResponse, GoogleAny, GoogleTimestamp,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use eve_proto::PyRep;
    use std::collections::HashMap;
    use tokio::net::TcpListener;

    #[test]
    fn test_normalize_jump_bridges_macho_reply_wraps_list_as_3_tuple() {
        let empty = normalize_macho_rpc_reply(
            "structureDirectory",
            "GetJumpBridgesWithMyAccess",
            Ok(PyRep::List(Vec::new())),
        )
        .expect("empty list normalized");
        assert_eq!(
            empty,
            PyRep::Tuple(vec![
                PyRep::List(Vec::new()),
                PyRep::List(Vec::new()),
                PyRep::Dict(Vec::new()),
            ])
        );

        let populated = normalize_macho_rpc_reply(
            "structureDirectory",
            "GetJumpBridgesWithMyAccess",
            Ok(PyRep::List(vec![PyRep::keyval([
                ("structureID", PyRep::Integer(1_000_000_000_101)),
                ("solarSystemID", PyRep::Integer(30_000_142)),
                ("linkedStructureID", PyRep::Integer(1_000_000_000_102)),
                ("destinationSolarSystemID", PyRep::Integer(30_000_144)),
            ])])),
        )
        .expect("populated list normalized");
        let PyRep::Tuple(parts) = populated else {
            panic!("expected 3-tuple");
        };
        assert_eq!(parts.len(), 3);
        let PyRep::List(pairs) = &parts[0] else {
            panic!("expected pairs list");
        };
        assert_eq!(pairs.len(), 1);
        assert_eq!(
            parts[1],
            PyRep::List(vec![PyRep::Integer(1_000_000_000_101)])
        );
    }

    #[tokio::test]
    async fn test_skill_mgr2_get_skill_changes_for_isis_bound_and_unbound() {
        let ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
        let mut sess = SessionState::new(88_101);
        sess.user_id = Some(1);
        let kw = HashMap::new();
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(90_000_001)],
            &kw,
            None,
        )
        .await
        .expect("SelectCharacterID");

        let bound_id = ctx
            .bind_object(&sess, "skillMgr2", 90_000_001)
            .expect("bind skillMgr2");
        let res = ctx
            .dispatch_rpc(
                &mut sess,
                "skillMgr2",
                Some(bound_id),
                "GetSkillChangesForISIS",
                &[],
                &kw,
                Some("machoNet:26000"),
            )
            .await
            .expect("GetSkillChangesForISIS");
        assert_eq!(res, PyRep::List(Vec::new()));
    }

    #[tokio::test]
    async fn test_quasar_grpc_h2c_e2e_requests_notices_events_and_pings() {
        let ctx = Arc::new(ServerContext::with_repository(Arc::new(
            eve_db::WorldRepository::with_demo_seed(),
        )));
        // Select character 90_000_001 for user 1 so online session exists too.
        let mut macho_sess = SessionState::new(88_200);
        macho_sess.user_id = Some(1);
        ctx.db
            .acquire_online_lock(1, 88_200, Some(90_000_001), true, ctx.now_unix())
            .expect("online lock");
        let _ = ctx
            .dispatch_rpc(
                &mut macho_sess,
                "charUnboundMgr",
                None,
                "SelectCharacterID",
                &[PyRep::Integer(90_000_001)],
                &HashMap::new(),
                None,
            )
            .await
            .expect("SelectCharacterID");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let srv_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let c = Arc::clone(&srv_ctx);
                tokio::spawn(async move {
                    let _ = handle_quasar_grpc_connection(sock, c).await;
                });
            }
        });

        let tcp = TcpStream::connect(addr).await.expect("connect");
        let (mut h2_client, h2_conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let now_u = ctx.now_unix();
        let dev_jwt = ctx
            .db
            .issue_gateway_dev_token(1, None, "tranquility", now_u + 3600, now_u)
            .expect("issue_gateway_dev_token");
        let app_uuid = uuid::Uuid::new_v4();

        // 1. Unary Ping (`/eve_public.gateway.Requests/Ping`)
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Ping")
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            send_body
                .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), true)
                .unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());
            let frame = reader.next_message().await.unwrap().expect("ping frame");
            assert!(frame.is_empty());
            assert!(reader.next_message().await.unwrap().is_none());
        }

        // 2. Bidi `Requests/Send` covering all 5 Windows playtest Quasar RPCs (#256, #257, #259, #260, #264)
        //    plus character-spoofing rejection (#403).
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {dev_jwt}"))
                .header("application_instance_uuid", app_uuid.to_string())
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

            let mut sys_inner = Vec::new();
            eve_proto::quasar::encode_varint_field(1, 30_000_139, &mut sys_inner);
            let mut corruption_req = Vec::new();
            eve_proto::quasar::encode_bytes_field(1, &sys_inner, &mut corruption_req);

            let request_types: &[(&str, Option<u32>, Vec<u8>)] = &[
                ("eve_public.corporationgoal.api.GetMineWithRewardsRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.career.goal.api.GetDefinitionsRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.achievement.reward.api.GetUnclaimedRequest", None, Vec::new()),
                ("eve_public.dailygoal.api.GetAllWithRewardsRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.campaign.api.GetAllRequest", Some(90_000_001u32), Vec::new()),
                ("eve_public.pirate.corruption.api.GetSystemInfoRequest", Some(90_000_001u32), corruption_req),
                ("eve_public.achievement.definition.api.cdn.GetLatestRequest", None, vec![0x10, 0x01]),
                ("eve_public.space.api.cdn.GetUpdateRequest", None, vec![0x10, 0x01]),
                ("eve_public.cosmetic.character.title.definition.api.GetLatestRequest", None, vec![0x10, 0x01]),
                // V24.01 desktop client sends `type.evetech.net/` `Any.type_url` prefix (#301/#302)
                ("type.evetech.net/eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest", None, Vec::new()),
                ("type.evetech.net/eve_public.career.goal.api.GetDefinitionsRequest", Some(90_000_001u32), Vec::new()),
                ("type.evetech.net/eve_public.achievement.definition.api.cdn.GetLatestRequest", None, vec![0x10, 0x01]),
            ];

            for (idx, (type_name, char_opt, payload_bytes)) in request_types.iter().enumerate() {
                let mut corr = vec![0u8; 16];
                corr[15] = (idx + 1) as u8;
                let type_url = if eve_proto::quasar::has_quasar_type_url_prefix(type_name) {
                    (*type_name).to_string()
                } else {
                    format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}")
                };
                let pub_req = EvePublicRequest {
                    issued: Some(GoogleTimestamp {
                        seconds: ctx.now_unix() as i64,
                        nanos: 0,
                    }),
                    correlation_uuid: corr.clone(),
                    payload: Some(GoogleAny {
                        type_url,
                        value: payload_bytes.clone(),
                    }),
                    external_origin: "eve-desktop-client/3619879 tq".into(),
                    application_instance_uuid: app_uuid.as_bytes().to_vec(),
                    authoritative_context: Some(AuthoritativeContext::for_session_at(
                        Some(1),
                        *char_opt,
                        None,
                        0,
                        "tranquility",
                        Vec::new(),
                        ctx.now_unix(),
                    )),
                    trace_context: None,
                };
                send_body
                    .send_data(frame_grpc_message(&pub_req.encode_proto()), false)
                    .unwrap();
                let raw_rsp = reader
                    .next_message()
                    .await
                    .unwrap()
                    .expect("streamed EvePublicResponse");
                let decoded = EvePublicResponse::decode_proto(&raw_rsp).expect("decode response");
                assert_eq!(
                    decoded.status_code, STATUS_OK,
                    "{type_name} failed: {} ({})",
                    decoded.status_code, decoded.status_message
                );
                assert_eq!(decoded.correlation_uuid, corr);
            }

            // Spoofing another user's character (90_000_002 owned by user 2) must be rejected with 403!
            let spoof_req = EvePublicRequest {
                issued: Some(GoogleTimestamp {
                    seconds: ctx.now_unix() as i64,
                    nanos: 0,
                }),
                correlation_uuid: vec![9u8; 16],
                payload: Some(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetDefinitionsRequest"
                    ),
                    value: Vec::new(),
                }),
                external_origin: "eve-desktop-client/3619879 tq".into(),
                application_instance_uuid: app_uuid.as_bytes().to_vec(),
                authoritative_context: Some(AuthoritativeContext::for_session_at(
                    Some(1),
                    Some(90_000_002),
                    None,
                    0,
                    "tranquility",
                    Vec::new(),
                    ctx.now_unix(),
                )),
                trace_context: None,
            };
            send_body
                .send_data(frame_grpc_message(&spoof_req.encode_proto()), true)
                .unwrap();
            let raw_spoof = reader.next_message().await.unwrap().expect("spoof rsp");
            let decoded_spoof = EvePublicResponse::decode_proto(&raw_spoof).unwrap();
            assert_eq!(decoded_spoof.status_code, STATUS_FORBIDDEN);
            assert!(reader.next_message().await.unwrap().is_none());
        }

        // 3. Bidi `Notices/Consume` + server notice fan-out
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
                .header("content-type", "application/grpc")
                .header("authorization", dev_jwt.clone())
                .header("application_instance_uuid", app_uuid.to_string())
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            send_body
                .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)
                .unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

            tokio::time::sleep(Duration::from_millis(30)).await;
            let (_, delivered) = ctx
                .quasar_gateway
                .publish_notice(
                    NoticeScope::Character(90_000_001),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.ProgressChangedNotice"
                        ),
                        value: Vec::new(),
                    },
                    ctx.now_unix(),
                )
                .expect("publish_notice");
            assert_eq!(delivered, 1);

            let mut saw_progress_notice = false;
            for _ in 0..16 {
                let raw_notice = reader.next_message().await.unwrap().expect("notice frame");
                let notice = EvePublicNotice::decode_proto(&raw_notice).expect("decode notice");
                if notice
                    .payload
                    .as_ref()
                    .is_some_and(|p| p.type_url.ends_with("eve_public.dailygoal.api.ProgressChangedNotice"))
                {
                    saw_progress_notice = true;
                    break;
                }
            }
            assert!(saw_progress_notice, "Expected ProgressChangedNotice on Notices/Consume stream");
            send_body.send_data(Bytes::new(), true).unwrap();
            assert!(reader.next_message().await.unwrap().is_none());
        }

        // 4. Bidi `Events/Publish` returning `Empty` ack per published event
        {
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {dev_jwt}"))
                .header("application_instance_uuid", app_uuid.to_string())
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            let rsp = rsp_fut.await.unwrap();
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

            let ev = EvePublicEvent {
                occurred: Some(GoogleTimestamp {
                    seconds: ctx.now_unix() as i64,
                    nanos: 0,
                }),
                uuid: vec![7u8; 16],
                payload: Some(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.ui.OpenedEvent"),
                    value: Vec::new(),
                }),
                external_origin: "eve-desktop-client/3619879 tq".into(),
                application_instance_uuid: app_uuid.as_bytes().to_vec(),
                authoritative_context: None,
                ..Default::default()
            };
            send_body
                .send_data(frame_grpc_message(&ev.encode_proto()), true)
                .unwrap();
            let ack = reader.next_message().await.unwrap().expect("event ack");
            assert!(ack.is_empty());
            assert!(reader.next_message().await.unwrap().is_none());
        }
    }

    #[test]
    fn normalize_macho_rpc_reply_converts_jump_timers_keyval_to_three_tuple() {
        let raw = Ok(PyRep::keyval([
            ("jumpActivation", PyRep::Integer(133_500_000_000_000_000)),
            ("jumpFatigue", PyRep::Integer(133_500_010_000_000_000)),
            ("lastUpdated", PyRep::Integer(133_499_990_000_000_000)),
        ]));
        let normalized =
            normalize_macho_rpc_reply("jumpTimers", "GetTimers", raw).expect("normalized");
        assert_eq!(
            normalized,
            PyRep::Tuple(vec![
                PyRep::Integer(133_500_000_000_000_000),
                PyRep::Integer(133_500_010_000_000_000),
                PyRep::Integer(133_499_990_000_000_000),
            ])
        );
    }

    #[tokio::test]
    async fn test_quasar_grpc_h2c_e2e_client_crash_flows_wire_conformant() {
        use crate::services::quasar_campaign_state::{
            campaign_id_to_uuid_bytes, CAMPAIGN_STATE_ACTIVE, CAMPAIGN_STATE_COMPLETED,
            CampaignObjectiveRecord, CampaignRecord, CampaignTrackRecord, OBJECTIVE_STATE_ACTIVE,
            TRACK_STATE_STARTED,
        };
        use eve_proto::quasar::{
            encode_bytes_field, encode_string_field, DynamicValue, QuasarDescriptorRegistry,
        };

        let ctx = Arc::new(ServerContext::with_repository(Arc::new(
            eve_db::WorldRepository::with_demo_seed(),
        )));
        let now_u = ctx.now_unix();
        let char_id: u32 = 90_000_001;
        let corp_id: i64 = ctx.db.with_snapshot(|s| {
            s.characters
                .get(&(char_id as i64))
                .map(|c| c.corporation_id)
                .unwrap_or(98_000_001)
        });

        // Seed #256 (Corporation Goals): active corp project with unclaimed contribution reward.
        let corp_goal_id: u64 = 7_700_001;
        let mut corp_goal_uuid = vec![0u8; 16];
        corp_goal_uuid[0..8].copy_from_slice(b"CORPGOAL");
        corp_goal_uuid[8..16].copy_from_slice(&corp_goal_id.to_be_bytes());
        ctx.db.with_mut_snapshot(|snap| {
            snap.corp_projects.insert(
                corp_goal_id as i64,
                eve_db::CorpProjectRecord {
                    project_id: corp_goal_id as i64,
                    corporation_id: corp_id,
                    name: "Quasar 256 Test Goal".into(),
                    target_quantity: 100,
                    current_quantity: 40,
                    isk_per_unit: eve_db::IskCents::from_isk(1_000),
                    escrow_remaining: eve_db::IskCents::from_isk(50_000),
                },
            );
            let mut details = eve_db::PersistedProjectDetails {
                status: "Active".into(),
                ..Default::default()
            };
            details.char_contributions.insert(char_id as i64, 40);
            snap.alliance_war_state
                .project_details
                .insert(corp_goal_id as i64, details);
        });

        // Seed #257 (Career Goals / Job Board Relevance Profile): complete 2 career goals so claiming both APIs works.
        let career_goals = ctx.quasar_gateway.progression.career_goals.get_or_init_goals(char_id);
        let career_goal_uuid = career_goals.first().expect("seeded career goal 1").uuid;
        let career_goal_uuid_2 = career_goals.get(1).expect("seeded career goal 2").uuid;
        ctx.quasar_gateway
            .progression
            .career_goals
            .mark_completed(char_id, &career_goal_uuid, now_u)
            .expect("complete career goal 1");
        ctx.quasar_gateway
            .progression
            .career_goals
            .mark_completed(char_id, &career_goal_uuid_2, now_u)
            .expect("complete career goal 2");

        // Seed #259 (Achievements): complete 1 achievement so GetUnclaimedRequest returns a reward.
        let ach_list = ctx.quasar_gateway.progression.achievements.get_or_init_achievements(char_id);
        let ach_id = ach_list.first().expect("seeded achievement").def.id;
        let ach_cat_id = ach_list.first().expect("seeded achievement").def.category_id;
        ctx.quasar_gateway
            .progression
            .achievements
            .mark_completed(char_id, ach_id, now_u)
            .expect("complete achievement");

        // Seed #260 (Daily Goals): complete 1 daily goal (remaining unclaimed).
        let daily_goals = ctx
            .quasar_gateway
            .progression
            .daily_goals
            .get_or_init_goals_at(char_id, now_u);
        let first_daily = daily_goals.first().expect("seeded daily goal").clone();
        ctx.quasar_gateway
            .progression
            .daily_goals
            .add_progress(char_id, &first_daily.uuid, first_daily.template.target, now_u);

        // Seed #264 (Campaigns): active campaign in "event" namespace + upcoming + objective + track.
        let camp_id: u64 = 5501;
        let obj_id: u64 = 5502;
        let trk_id: u64 = 5503;
        let upcoming_camp_id: u64 = 5504;
        let mut contributors = HashMap::new();
        contributors.insert(char_id, (20i64, 2i64));
        let mut trk_participants = HashMap::new();
        trk_participants.insert(char_id, 2u64);
        ctx.quasar_gateway
            .progression
            .extra
            .register_campaign(
                CampaignRecord {
                    id: camp_id,
                    name: "Quasar 264 Campaign".into(),
                    description: "Reconciliation test".into(),
                    namespace: "eve".into(),
                    scheduled_start_unix: now_u.saturating_sub(60),
                    scheduled_expiry_unix: now_u + 86_400,
                    state: CAMPAIGN_STATE_ACTIVE,
                    transition_unix: now_u.saturating_sub(60),
                    etag: 1,
                    objectives: vec![obj_id],
                    ..Default::default()
                },
                vec![CampaignObjectiveRecord {
                    id: obj_id,
                    campaign_id: camp_id,
                    name: "Mine Ore Objective".into(),
                    target_progress: 100,
                    current_progress: 20,
                    committed_participants: vec![char_id],
                    contributors,
                    contribution_method: "MineAsteroid".into(),
                    isk_reward_per_interval: 10_000,
                    isk_reward_progress_interval: 10,
                    state: OBJECTIVE_STATE_ACTIVE,
                    transition_unix: now_u.saturating_sub(60),
                    ..Default::default()
                }],
                vec![CampaignTrackRecord {
                    id: trk_id,
                    campaign_id: camp_id,
                    key: "track_1".into(),
                    target_progress_value: 50,
                    current_progress: 2,
                    participants: trk_participants,
                    isk_reward_per_interval: 5_000,
                    isk_reward_progress_interval: 1,
                    state: TRACK_STATE_STARTED,
                    transition_unix: now_u.saturating_sub(60),
                    ..Default::default()
                }],
            )
            .expect("register active campaign");
        ctx.quasar_gateway
            .progression
            .extra
            .register_campaign(
                CampaignRecord {
                    id: upcoming_camp_id,
                    name: "Upcoming Campaign".into(),
                    namespace: "eve".into(),
                    scheduled_start_unix: now_u + 3600,
                    state: 0,
                    ..Default::default()
                },
                Vec::new(),
                Vec::new(),
            )
            .expect("register upcoming campaign");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let srv_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let c = Arc::clone(&srv_ctx);
                tokio::spawn(async move {
                    let _ = handle_quasar_grpc_connection(sock, c).await;
                });
            }
        });

        let tcp = TcpStream::connect(addr).await.expect("connect");
        let (mut h2_client, h2_conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
        tokio::spawn(async move {
            let _ = h2_conn.await;
        });

        let dev_jwt = ctx
            .db
            .issue_gateway_dev_token(1, None, "tranquility", now_u + 3600, now_u)
            .expect("issue_gateway_dev_token");
        let app_uuid = uuid::Uuid::new_v4();

        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .header("application_instance_uuid", app_uuid.to_string())
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);
        let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());

        // Helper encoded request payloads
        let mut goal_id_req = Vec::new();
        {
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &corp_goal_uuid, &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut goal_id_req);
        }
        let mut career_claim_req = Vec::new();
        {
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &career_goal_uuid, &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut career_claim_req);
        }
        let mut career_api_claim_req = Vec::new();
        {
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &career_goal_uuid_2, &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut career_api_claim_req);
        }
        let mut daily_get_req = Vec::new();
        {
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &first_daily.uuid, &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut daily_get_req);
        }
        let mut ach_get_req = Vec::new();
        {
            let mut u = [0u8; 16];
            u[12..16].copy_from_slice(&ach_id.to_be_bytes());
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &u, &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut ach_get_req);
        }
        let mut ach_cat_req = Vec::new();
        {
            let mut u = [0u8; 16];
            u[12..16].copy_from_slice(&ach_cat_id.to_be_bytes());
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &u, &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut ach_cat_req);
        }
        let mut camp_get_req = Vec::new();
        {
            let mut id_msg = Vec::new();
            encode_bytes_field(1, &campaign_id_to_uuid_bytes(camp_id), &mut id_msg);
            encode_bytes_field(1, &id_msg, &mut camp_get_req);
        }
        let mut camp_ns_req = Vec::new();
        {
            let mut ns_msg = Vec::new();
            encode_string_field(1, "eve", &mut ns_msg);
            encode_bytes_field(1, &ns_msg, &mut camp_ns_req);
        }

        let cases: Vec<(&str, Vec<u8>, &str, bool)> = vec![
            // #256 corporation/client/goals/goalsController.py -> get_unclaimed_goal_ids
            ("eve_public.corporationgoal.api.GetMineWithRewardsRequest", Vec::new(), "eve_public.corporationgoal.api.GetMineWithRewardsResponse", true),
            ("eve_public.corporationgoal.api.GetActiveRequest", Vec::new(), "eve_public.corporationgoal.api.GetActiveResponse", true),
            ("eve_public.corporationgoal.api.GetAllRequest", Vec::new(), "eve_public.corporationgoal.api.GetAllResponse", true),
            ("eve_public.corporationgoal.api.GetInactiveRequest", Vec::new(), "eve_public.corporationgoal.api.GetInactiveResponse", false),
            ("eve_public.corporationgoal.api.GetContributorSummariesForGoalRequest", goal_id_req.clone(), "eve_public.corporationgoal.api.GetContributorSummariesForGoalResponse", true),
            ("eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest", goal_id_req.clone(), "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalResponse", true),
            ("eve_public.corporationgoal.api.GetMyContributorSummariesRequest", Vec::new(), "eve_public.corporationgoal.api.GetMyContributorSummariesResponse", true),
            ("eve_public.goal.api.GetRequest", goal_id_req.clone(), "eve_public.goal.api.GetResponse", true),
            ("eve_public.goal.api.GetAllRequest", Vec::new(), "eve_public.goal.api.GetAllResponse", true),
            ("eve_public.goal.api.GetCapacityRequest", Vec::new(), "eve_public.goal.api.GetCapacityResponse", true),
            // #257 jobboard/client/relevance_profile.py -> _build_profile (career goals, skill plans, freelance)
            ("eve_public.career.goal.api.GetDefinitionsRequest", Vec::new(), "eve_public.career.goal.api.GetDefinitionsResponse", true),
            ("eve_public.career.goal.api.GetAllRequest", Vec::new(), "eve_public.career.goal.api.GetAllResponse", true),
            ("eve_public.app.eveonline.career.goal.GetAllRequest", Vec::new(), "eve_public.app.eveonline.career.goal.GetAllResponse", true),
            ("eve_public.app.eveonline.career.goal.ClaimRewardRequest", career_claim_req, "eve_public.app.eveonline.career.goal.ClaimRewardResponse", true),
            ("eve_public.career.goal.api.ClaimRewardRequest", career_api_claim_req, "eve_public.career.goal.api.ClaimRewardResponse", false),
            ("eve_public.skill.plan.GetAllRequest", Vec::new(), "eve_public.skill.plan.GetAllResponse", false),
            ("eve_public.skill.plan.GetActiveRequest", Vec::new(), "eve_public.skill.plan.GetActiveResponse", false),
            ("eve_public.freelance.project.api.GetAllActiveRequest", Vec::new(), "eve_public.freelance.project.api.GetAllActiveResponse", false),
            ("eve_public.freelance.project.api.GetAllCommittedRequest", Vec::new(), "eve_public.freelance.project.api.GetAllCommittedResponse", false),
            ("eve_public.freelance.project.api.GetAllUnredeemedRequest", Vec::new(), "eve_public.freelance.project.api.GetAllUnredeemedResponse", false),
            ("eve_public.freelance.project.api.GetCommitLimitsRequest", Vec::new(), "eve_public.freelance.project.api.GetCommitLimitsResponse", true),
            ("eve_public.freelance.project.api.GetCreationLimitsRequest", Vec::new(), "eve_public.freelance.project.api.GetCreationLimitsResponse", true),
            ("eve_public.freelance.project.api.GetCreationPricesRequest", Vec::new(), "eve_public.freelance.project.api.GetCreationPricesResponse", true),
            ("eve_public.freelance.contributionmethod.definition.api.GetAllLatestRequest", Vec::new(), "eve_public.freelance.contributionmethod.definition.api.GetAllLatestResponse", true),
            ("eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorRequest", Vec::new(), "eve_public.freelance.contributionmethod.definition.api.GetAllLatestWithinMajorResponse", true),
            // #259 btnDataAchievements.py -> update_badge
            ("eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new(), "eve_public.achievement.reward.api.GetUnclaimedResponse", true),
            ("eve_public.achievement.api.GetRequest", ach_get_req, "eve_public.achievement.api.GetResponse", true),
            ("eve_public.achievement.category.api.GetRequest", ach_cat_req, "eve_public.achievement.category.api.GetResponse", false),
            ("eve_public.achievement.showcase.api.GetCharacterScoreRequest", Vec::new(), "eve_public.achievement.showcase.api.GetCharacterScoreResponse", true),
            ("eve_public.achievement.definition.api.cdn.GetLatestRequest", vec![0x10, 0x01], "eve_public.achievement.definition.api.cdn.GetLatestResponse", true),
            // #260 dailygoals/client/goalsController.py -> get_unclaimed_goal_ids
            ("eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new(), "eve_public.dailygoal.api.GetAllCurrentResponse", true),
            ("eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest", Vec::new(), "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsResponse", true),
            ("eve_public.dailygoal.api.GetAllWithRewardsRequest", Vec::new(), "eve_public.dailygoal.api.GetAllWithRewardsResponse", true),
            ("eve_public.dailygoal.api.GetRequest", daily_get_req.clone(), "eve_public.dailygoal.api.GetResponse", true),
            ("eve_public.dailygoal.api.RedeemRequest", daily_get_req, "eve_public.dailygoal.api.RedeemResponse", false),
            // #264 campaigns/client/v1/repository.py -> _wait_to_reconcile
            ("eve_public.campaign.api.GetAllRequest", Vec::new(), "eve_public.campaign.api.GetAllResponse", true),
            ("eve_public.campaign.api.GetAllForNamespaceRequest", camp_ns_req.clone(), "eve_public.campaign.api.GetAllForNamespaceResponse", true),
            ("eve_public.campaign.api.GetRequest", camp_get_req.clone(), "eve_public.campaign.api.GetResponse", true),
            ("eve_public.campaign.api.GetStatusRequest", camp_get_req.clone(), "eve_public.campaign.api.GetStatusResponse", true),
            ("eve_public.campaign.api.GetParticipantStatsRequest", camp_get_req, "eve_public.campaign.api.GetParticipantStatsResponse", true),
            ("eve_public.campaign.api.GetLimitsRequest", Vec::new(), "eve_public.campaign.api.GetLimitsResponse", true),
            ("eve_public.campaign.api.GetAllUnredeemedRewardsRequest", Vec::new(), "eve_public.campaign.api.GetAllUnredeemedRewardsResponse", true),
            ("eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest", camp_ns_req, "eve_public.campaign.api.GetAllUnredeemedParticipantRewardsResponse", true),
            ("eve_public.campaign.contributionmethod.definition.api.GetAllRequest", Vec::new(), "eve_public.campaign.contributionmethod.definition.api.GetAllResponse", true),
            ("eve_public.campaign.trigger.eventmatch.definition.api.GetAllRequest", Vec::new(), "eve_public.campaign.trigger.eventmatch.definition.api.GetAllResponse", true),
            ("type.evetech.net/eve_public.corporationgoal.api.GetMineWithRewardsRequest", Vec::new(), "eve_public.corporationgoal.api.GetMineWithRewardsResponse", true),
            ("type.evetech.net/eve_public.dailygoal.api.GetAllCurrentRequest", Vec::new(), "eve_public.dailygoal.api.GetAllCurrentResponse", true),
            ("type.evetech.net/eve_public.campaign.api.GetAllRequest", Vec::new(), "eve_public.campaign.api.GetAllResponse", true),
            ("type.evetech.net/eve_public.achievement.reward.api.GetUnclaimedRequest", Vec::new(), "eve_public.achievement.reward.api.GetUnclaimedResponse", true),
        ];

        for (idx, (req_type, req_bytes, expected_rsp_type, require_non_empty)) in cases.iter().enumerate() {
            let mut corr = vec![0u8; 16];
            corr[14] = (idx >> 8) as u8;
            corr[15] = (idx & 0xFF) as u8;
            let type_url = if eve_proto::quasar::has_quasar_type_url_prefix(req_type) {
                (*req_type).to_string()
            } else {
                format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}")
            };
            let pub_req = EvePublicRequest {
                issued: Some(GoogleTimestamp {
                    seconds: now_u as i64,
                    nanos: 0,
                }),
                correlation_uuid: corr.clone(),
                payload: Some(GoogleAny {
                    type_url,
                    value: req_bytes.clone(),
                }),
                external_origin: "eve-desktop-client/3619879 tq".into(),
                application_instance_uuid: app_uuid.as_bytes().to_vec(),
                authoritative_context: Some(AuthoritativeContext::for_session_at(
                    Some(1),
                    Some(char_id),
                    Some(corp_id as u32),
                    0,
                    "tranquility",
                    Vec::new(),
                    now_u,
                )),
                trace_context: None,
            };
            let is_last = idx + 1 == cases.len();
            send_body
                .send_data(frame_grpc_message(&pub_req.encode_proto()), is_last)
                .unwrap();
            let raw_rsp = reader
                .next_message()
                .await
                .unwrap()
                .expect("streamed EvePublicResponse");
            let decoded = EvePublicResponse::decode_proto(&raw_rsp).expect("decode EvePublicResponse");
            assert_eq!(
                decoded.status_code, STATUS_OK,
                "{req_type} returned status {} ({})",
                decoded.status_code, decoded.status_message
            );
            let payload = decoded
                .payload
                .as_ref()
                .unwrap_or_else(|| panic!("{req_type} missing payload"));
            let norm = QuasarDescriptorRegistry::normalize_type_name(&payload.type_url);
            assert_eq!(
                norm, *expected_rsp_type,
                "{req_type} returned unexpected type_url {}",
                payload.type_url
            );
            if *require_non_empty {
                assert!(
                    !payload.value.is_empty(),
                    "{req_type} returned empty bytes for {expected_rsp_type}"
                );
            }
            let dyn_rsp = ctx
                .quasar_gateway
                .descriptors
                .decode_dynamic(norm, &payload.value)
                .unwrap_or_else(|e| panic!("{req_type} -> {norm} failed descriptor decode: {e}"));

            // Spot-check critical nested decoded structures
            if *req_type == "eve_public.dailygoal.api.GetAllCurrentRequest" {
                match dyn_rsp.get(1) {
                    Some(DynamicValue::Repeated(items)) => {
                        assert!(!items.is_empty(), "GetAllCurrentResponse.goals should be non-empty");
                        for it in items {
                            let DynamicValue::Message(goal) = it else {
                                panic!("GetAllCurrentResponse.goals entry must decode as Goal message");
                            };
                            assert!(goal.get(1).is_some(), "Goal.id must be populated");
                            assert!(goal.get(2).is_some(), "Goal.goal attributes must be populated");
                        }
                    }
                    other => panic!("Unexpected GetAllCurrentResponse field 1: {other:?}"),
                }
            } else if *req_type == "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest" {
                match dyn_rsp.get(1) {
                    Some(DynamicValue::Repeated(items)) => {
                        assert!(!items.is_empty(), "GetAllCompletedWithEntitlementsResponse.goals non-empty");
                        let DynamicValue::Message(goal) = &items[0] else {
                            panic!("GetAllCompletedWithEntitlementsResponse.goals entry must be Goal");
                        };
                        assert!(goal.get(1).is_some(), "Completed Goal.id must be populated");
                    }
                    other => panic!("Unexpected GetAllCompletedWithEntitlementsResponse field 1: {other:?}"),
                }
            } else if *req_type == "eve_public.app.eveonline.career.goal.ClaimRewardRequest" {
                assert!(dyn_rsp.get(1).is_some(), "ClaimRewardResponse.goal must be populated");
            }
        }
        assert!(reader.next_message().await.unwrap().is_none());
        let _ = CAMPAIGN_STATE_COMPLETED;
    }

    #[tokio::test]
    async fn test_quasar_grpc_h2c_unlimited_resets_and_non_io_accept_error_breaks() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let ctx = Arc::new(ServerContext::with_repository(Arc::new(
            eve_db::WorldRepository::with_demo_seed(),
        )));

        // Part 1: Over 1024 unary RPC calls and stream resets on a single persistent HTTP/2
        // connection must succeed without triggering `locally-reset streams reached limit (1024)`.
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("local_addr");
            let srv_ctx = Arc::clone(&ctx);
            let srv_task = tokio::spawn(async move {
                let (sock, _) = listener.accept().await.expect("accept");
                handle_quasar_grpc_connection(sock, srv_ctx).await
            });

            let tcp = TcpStream::connect(addr).await.expect("connect");
            let (mut h2_client, h2_conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
            let mut conn_task = tokio::spawn(async move {
                let _ = h2_conn.await;
            });

            for i in 0..2_200 {
                tokio::time::timeout(
                    Duration::from_secs(2),
                    std::future::poll_fn(|cx| h2_client.poll_ready(cx)),
                )
                .await
                .unwrap_or_else(|_| panic!("poll_ready timeout at i={i}"))
                .expect("h2 ready");
                let req = Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Requests/Ping")
                    .header("content-type", "application/grpc")
                    .body(())
                    .unwrap();
                if i % 2 == 0 {
                    // Normal unary Ping completion (>1024 unary calls total)
                    let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
                    send_body
                        .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), true)
                        .unwrap();
                    let rsp = tokio::time::timeout(Duration::from_secs(2), rsp_fut)
                        .await
                        .unwrap_or_else(|_| panic!("unary ping rsp timeout at i={i}"))
                        .expect("unary ping");
                    assert_eq!(rsp.status(), StatusCode::OK);
                    let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());
                    let frame = reader.next_message().await.unwrap().expect("ping frame");
                    assert!(frame.is_empty());
                    assert!(reader.next_message().await.unwrap().is_none());
                } else {
                    // Server-aborted stream via truncated frame (>1024 server stream resets total)
                    let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
                    send_body
                        .send_data(Bytes::from_static(&[0, 0, 0, 0, 5, 1, 2]), true)
                        .unwrap();
                    let _ = tokio::time::timeout(Duration::from_secs(2), rsp_fut)
                        .await
                        .unwrap_or_else(|_| panic!("reset stream rsp timeout at i={i}"));
                }
            }
            std::future::poll_fn(|cx| h2_client.poll_ready(cx))
                .await
                .expect("h2 ready final");
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Ping")
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
            send_body
                .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), true)
                .unwrap();
            let rsp = rsp_fut.await.expect("ping after >1024 calls/resets");
            assert_eq!(rsp.status(), StatusCode::OK);
            let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());
            let frame = reader.next_message().await.unwrap().expect("ping frame");
            assert!(frame.is_empty());
            assert!(reader.next_message().await.unwrap().is_none());

            drop(h2_client);
            let _ = tokio::time::timeout(Duration::from_secs(2), &mut conn_task)
                .await
                .unwrap_or_else(|_| {
                    conn_task.abort();
                    Ok(())
                });
            let _ = tokio::time::timeout(Duration::from_secs(2), srv_task)
                .await
                .expect("server exited cleanly after client close");
        }

        // Part 2: A connection-level non-IO HTTP/2 protocol error while the TCP socket remains open
        // must break out of `h2_conn.accept().await` cleanly instead of spinning forever.
        {
            let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
            let addr = listener.local_addr().expect("local_addr");
            let srv_ctx = Arc::clone(&ctx);
            let srv_task = tokio::spawn(async move {
                let (sock, _) = listener.accept().await.expect("accept");
                handle_quasar_grpc_connection(sock, srv_ctx).await
            });

            let mut raw = TcpStream::connect(addr).await.expect("connect");
            // HTTP/2 client connection preface + empty SETTINGS frame + SETTINGS ACK.
            raw.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
                .await
                .unwrap();
            raw.write_all(&[0, 0, 0, 0x04, 0x00, 0, 0, 0, 0])
                .await
                .unwrap();
            let mut buf = [0u8; 64];
            let _ = raw.read(&mut buf).await.unwrap();
            raw.write_all(&[0, 0, 0, 0x04, 0x01, 0, 0, 0, 0])
                .await
                .unwrap();
            // Send connection-level WINDOW_UPDATE (type 0x08) on stream 0 with increment 0 -> PROTOCOL_ERROR.
            raw.write_all(&[0, 0, 4, 0x08, 0x00, 0, 0, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
            raw.flush().await.unwrap();

            // Even with `raw` kept open (no EOF/IO error), the server accept loop must terminate promptly.
            let joined = tokio::time::timeout(Duration::from_secs(2), srv_task)
                .await
                .expect("accept loop must break on non-IO HTTP/2 protocol error instead of spinning")
                .expect("server task panicked");
            assert!(joined.is_ok(), "unexpected error: {joined:?}");
            drop(raw);
        }
    }

    #[tokio::test]
    async fn grpc_frame_reader_defers_flow_control_release_until_frame_drain_and_rejects_invalid_headers() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();

        let srv_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            let mut h2_srv = h2::server::Builder::new()
                .initial_window_size(H2_INITIAL_STREAM_WINDOW_BYTES)
                .handshake::<_, Bytes>(sock)
                .await
                .expect("h2 server handshake");

            // Stream 1: two pipelined frames in a single DATA chunk (25 bytes + 35 bytes = 60 bytes).
            let (req1, mut rsp1) = h2_srv.accept().await.unwrap().unwrap();
            let mut send1 = send_grpc_headers(&mut rsp1, false).unwrap();
            let mut reader1 = GrpcFrameReader::new(req1.into_body(), "identity".into());

            let msg_a = reader1
                .next_message()
                .await
                .expect("frame 1 ok")
                .expect("frame 1 present");
            assert_eq!(msg_a, vec![0xAA; 20]);
            // Only the 25 bytes of frame 1 were released; the 35 pipelined bytes of frame 2 remain
            // buffered and accounted against the HTTP/2 RecvStream flow-control window.
            assert_eq!(reader1.buffered_bytes(), 35);
            assert_eq!(reader1.used_flow_control_capacity(), 35);

            let msg_b = reader1
                .next_message()
                .await
                .expect("frame 2 ok")
                .expect("frame 2 present");
            assert_eq!(msg_b, vec![0xBB; 30]);
            assert_eq!(reader1.buffered_bytes(), 0);
            assert_eq!(reader1.used_flow_control_capacity(), 0);
            assert!(reader1.next_message().await.unwrap().is_none());
            send_grpc_trailers(&mut send1, GRPC_STATUS_OK, None).unwrap();

            // Stream 2: oversized frame header (> MAX_GRPC_MESSAGE_BYTES) fails immediately on 5-byte header.
            let (req2, mut rsp2) = h2_srv.accept().await.unwrap().unwrap();
            let mut reader2 = GrpcFrameReader::new(req2.into_body(), "identity".into());
            let err_oversize = reader2
                .next_message()
                .await
                .expect_err("oversized header must fail");
            assert!(
                err_oversize
                    .to_string()
                    .contains("exceeds MAX_GRPC_MESSAGE_BYTES"),
                "unexpected error: {err_oversize}"
            );
            let _ = send_trailers_only(&mut rsp2, GRPC_STATUS_INVALID_ARGUMENT, "oversized");

            // Stream 3: invalid compression flag (2) fails immediately.
            let (req3, mut rsp3) = h2_srv.accept().await.unwrap().unwrap();
            let mut reader3 = GrpcFrameReader::new(req3.into_body(), "identity".into());
            let err_comp = reader3
                .next_message()
                .await
                .expect_err("invalid compression flag must fail");
            assert!(
                err_comp
                    .to_string()
                    .contains("Invalid gRPC frame compression flag"),
                "unexpected error: {err_comp}"
            );
            let _ = send_trailers_only(&mut rsp3, GRPC_STATUS_INVALID_ARGUMENT, "bad_comp");
            let srv_drive = tokio::spawn(async move {
                while h2_srv.accept().await.is_some() {}
            });
            let _ = done_rx.await;
            srv_drive.abort();
        });

        let tcp = TcpStream::connect(addr).await.expect("connect");
        let (mut client, conn) = h2::client::handshake(tcp).await.expect("client h2");
        tokio::spawn(async move {
            let _ = conn.await;
        });

        // Stream 1: send both frames concatenated in one DATA write.
        let req1 = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/test/pipelined")
            .body(())
            .unwrap();
        let (rsp1_fut, mut send1) = client.send_request(req1, false).unwrap();
        let mut combined = BytesMut::new();
        combined.extend_from_slice(&frame_grpc_message(&vec![0xAA; 20]));
        combined.extend_from_slice(&frame_grpc_message(&vec![0xBB; 30]));
        send1.send_data(combined.freeze(), true).unwrap();
        let rsp1 = rsp1_fut.await.unwrap();
        assert_eq!(rsp1.status(), StatusCode::OK);

        // Stream 2: 5-byte frame header claiming MAX_GRPC_MESSAGE_BYTES + 1.
        let req2 = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/test/oversize")
            .body(())
            .unwrap();
        let (rsp2_fut, mut send2) = client.send_request(req2, false).unwrap();
        let mut bad_len_hdr = BytesMut::with_capacity(5);
        bad_len_hdr.extend_from_slice(&[0u8]);
        bad_len_hdr.extend_from_slice(&((MAX_GRPC_MESSAGE_BYTES as u32) + 1).to_be_bytes());
        send2.send_data(bad_len_hdr.freeze(), true).unwrap();
        let _ = rsp2_fut.await.unwrap();

        // Stream 3: compression byte = 2.
        let req3 = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/test/bad-compression")
            .body(())
            .unwrap();
        let (rsp3_fut, mut send3) = client.send_request(req3, false).unwrap();
        send3
            .send_data(Bytes::from_static(&[2, 0, 0, 0, 1, 0x00]), true)
            .unwrap();
        let _ = rsp3_fut.await.unwrap();
        let _ = done_tx.send(());
        drop(rsp1);
        drop(client);

        srv_task.await.expect("server assertions passed");
    }

    #[tokio::test]
    async fn send_grpc_data_bounded_slices_across_small_peer_window_and_times_out_zero_window() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let expected_payload: Vec<u8> = (0u16..300).map(|i| (i % 251) as u8).collect();
        let srv_payload = Bytes::from(expected_payload.clone());

        let srv_task = tokio::spawn(async move {
            // Connection 1: client advertises initial_window_size = 64 bytes -> server slices 300 bytes cleanly.
            let (sock1, _) = listener.accept().await.expect("accept 1");
            let mut h2_srv1 = h2::server::Builder::new()
                .handshake::<_, Bytes>(sock1)
                .await
                .expect("srv1 handshake");
            let (_req1, mut rsp1) = h2_srv1.accept().await.unwrap().unwrap();
            let srv1_drive = tokio::spawn(async move {
                while h2_srv1.accept().await.is_some() {}
            });
            let mut send1 = send_grpc_headers(&mut rsp1, false).unwrap();
            send_grpc_data_bounded(&mut send1, srv_payload, true, Duration::from_secs(2))
                .await
                .expect("window-sliced send must succeed");
            let _ = srv1_drive.await;

            // Connection 2: client advertises initial_window_size = 0 and never sends WINDOW_UPDATE -> timeout.
            let (sock2, _) = listener.accept().await.expect("accept 2");
            let mut h2_srv2 = h2::server::Builder::new()
                .handshake::<_, Bytes>(sock2)
                .await
                .expect("srv2 handshake");
            let (_req2, mut rsp2) = h2_srv2.accept().await.unwrap().unwrap();
            let srv2_drive = tokio::spawn(async move {
                while h2_srv2.accept().await.is_some() {}
            });
            let mut send2 = send_grpc_headers(&mut rsp2, false).unwrap();
            let stall_err = send_grpc_data_bounded(
                &mut send2,
                Bytes::from_static(&[0u8; 128]),
                true,
                Duration::from_millis(80),
            )
            .await
            .expect_err("stalled zero-window peer must time out cleanly");
            assert!(
                stall_err
                    .to_string()
                    .contains("outbound flow-control capacity"),
                "unexpected error: {stall_err}"
            );
            srv2_drive.abort();
        });

        // Client 1: 64-byte initial stream window, releasing capacity incrementally as chunks arrive.
        {
            let tcp1 = TcpStream::connect(addr).await.expect("connect 1");
            let (mut client1, conn1) = h2::client::Builder::new()
                .initial_window_size(64)
                .handshake::<_, Bytes>(tcp1)
                .await
                .expect("client1 handshake");
            tokio::spawn(async move {
                let _ = conn1.await;
            });
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/test/small-window")
                .body(())
                .unwrap();
            let (rsp_fut, _send) = client1.send_request(req, true).unwrap();
            let rsp = rsp_fut.await.unwrap();
            let mut body = rsp.into_body();
            let mut collected = Vec::new();
            while let Some(chunk_res) = body.data().await {
                let chunk = chunk_res.expect("chunk");
                assert!(chunk.len() <= 64, "server must respect 64-byte peer window");
                collected.extend_from_slice(&chunk);
                body.flow_control().release_capacity(chunk.len()).unwrap();
            }
            assert_eq!(collected, expected_payload);
        }

        // Client 2: 0-byte initial stream window, never releasing capacity.
        {
            let tcp2 = TcpStream::connect(addr).await.expect("connect 2");
            let (mut client2, conn2) = h2::client::Builder::new()
                .initial_window_size(0)
                .handshake::<_, Bytes>(tcp2)
                .await
                .expect("client2 handshake");
            tokio::spawn(async move {
                let _ = conn2.await;
            });
            let req = Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/test/zero-window")
                .body(())
                .unwrap();
            let (rsp_fut, _send) = client2.send_request(req, true).unwrap();
            let rsp = rsp_fut.await.unwrap();
            let mut body = rsp.into_body();
            // Server resets stream with FLOW_CONTROL_ERROR after 80ms timeout.
            let next = tokio::time::timeout(Duration::from_secs(2), body.data())
                .await
                .expect("stream reset arrives promptly");
            assert!(
                matches!(next, Some(Err(_)) | None),
                "expected stream reset on zero-window stall, got {next:?}"
            );
        }

        srv_task.await.expect("server flow-control test passed");
    }
}
