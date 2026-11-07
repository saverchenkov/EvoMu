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
pub(crate) mod normalize_replies;

pub use normalize_notices::{normalize_macho_notification, normalize_quasar_client_response};
pub(crate) use normalize_notices::normalize_quasar_any_payload;
pub use normalize_replies::normalize_macho_rpc_reply;

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
const GRPC_STATUS_UNIMPLEMENTED: &str = "12";
const GRPC_STATUS_UNAUTHENTICATED: &str = "16";

/// Pre-encoded 5-byte gRPC frame header for an empty `google.protobuf.Empty` response (`[0, 0, 0, 0, 0]`).
const EMPTY_GRPC_FRAME: [u8; 5] = [0, 0, 0, 0, 0];

/// Maximum uncompressed gRPC request payload size (16 MiB) matching client and gateway limits.
const MAX_GRPC_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

static NEXT_GRPC_SESSION_ID: AtomicU64 = AtomicU64::new(9_000_000);

pub async fn handle_quasar_grpc_connection(
    stream: TcpStream,
    ctx: Arc<ServerContext>,
) -> anyhow::Result<()> {
    let _ = stream.set_nodelay(true);
    let peer_ip = stream
        .peer_addr()
        .ok()
        .map(|sa| eve_db::gateway_tokens::normalize_peer_ip(sa.ip()));
    let mut h2_conn = h2::server::Builder::new()
        .max_concurrent_streams(256)
        .initial_window_size(1_048_576)
        .initial_connection_window_size(4_194_304)
        .max_frame_size(1_048_576)
        .handshake(stream)
        .await?;

    while let Some(accept_res) = h2_conn.accept().await {
        let (request, respond) = match accept_res {
            Ok(pair) => pair,
            Err(e) => {
                if e.is_io() {
                    break;
                }
                tracing::warn!(error = %e, "Quasar gRPC stream accept error");
                continue;
            }
        };
        let stream_ctx = Arc::clone(&ctx);
        tokio::spawn(async move {
            if let Err(e) = handle_grpc_stream(request, respond, stream_ctx, peer_ip).await {
                tracing::warn!(error = %e, "Quasar gRPC stream closed with error");
            }
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct VerifiedGatewayIdentity {
    user_id: u64,
    token_character_id: Option<i64>,
    account_role: u64,
}

async fn handle_grpc_stream(
    request: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    ctx: Arc<ServerContext>,
    peer_ip: Option<std::net::IpAddr>,
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
            send.send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)?;
            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        "/eve_public.gateway.Requests/Send" => {
            let Some(identity) = authenticate_grpc_headers(&ctx, &headers, peer_ip) else {
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
                let session =
                    build_session_for_identity(&ctx, grpc_session_id, identity, claimed_cid);
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
                send.send_data(encoded, false)?;
            }

            send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
            Ok(())
        }
        "/eve_public.gateway.Notices/Consume" => {
            let Some(identity) = authenticate_grpc_headers(&ctx, &headers, peer_ip) else {
                return send_trailers_only(
                    &mut respond,
                    GRPC_STATUS_UNAUTHENTICATED,
                    "Missing or invalid Quasar authorization token",
                );
            };
            let grpc_session_id = NEXT_GRPC_SESSION_ID.fetch_add(1, Ordering::Relaxed);
            let mut session = build_session_for_identity(&ctx, grpc_session_id, identity, None);
            let now_unix = ctx.now_unix();
            let mut rx =
                ctx.quasar_gateway
                    .subscribe_session(&session, Vec::new(), now_unix);
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

            let res: anyhow::Result<()> = async {
                loop {
                    tokio::select! {
                        maybe_notice = rx.recv() => {
                            let Some(mut notice) = maybe_notice else {
                                send_grpc_trailers(&mut send, GRPC_STATUS_OK, None)?;
                                break;
                            };
                            if let Some(ref mut any_payload) = notice.payload {
                                normalize_quasar_any_payload(&ctx.quasar_gateway.descriptors, any_payload);
                            }
                            let encoded = frame_grpc_message(&notice.encode_proto());
                            send.send_data(encoded, false)?;
                        }
                        msg_res = reader.next_message() => {
                            match msg_res {
                                Ok(Some(_empty_ack_or_sub)) => {
                                    // Refresh session context when client sends Subscribe/Acknowledge.
                                    let updated = build_session_for_identity(&ctx, grpc_session_id, identity, None);
                                    if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                        session = updated;
                                        ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
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
                            let updated = build_session_for_identity(&ctx, grpc_session_id, identity, None);
                            if session_scope_tuple(&updated) != session_scope_tuple(&session) {
                                session = updated;
                                ctx.quasar_gateway.on_session_changed(&session, ctx.now_unix());
                            }
                        }
                        _ = std::future::poll_fn(|cx| send.poll_reset(cx)) => {
                            break;
                        }
                    }
                }
                Ok(())
            }
            .await;

            ctx.quasar_gateway.unsubscribe_session(grpc_session_id);
            res
        }
        "/eve_public.gateway.Events/Publish" => {
            let Some(identity) = authenticate_grpc_headers(&ctx, &headers, peer_ip) else {
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
                let session =
                    build_session_for_identity(&ctx, grpc_session_id, identity, claimed_cid);
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
                send.send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)?;
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

fn build_session_for_identity(
    ctx: &ServerContext,
    grpc_session_id: u64,
    identity: VerifiedGatewayIdentity,
    claimed_char_id: Option<i64>,
) -> SessionState {
    let mut session = SessionState::new(grpc_session_id);
    session.user_id = Some(identity.user_id);
    session.role = identity.account_role;

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
    session
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
    let raw_hdr = headers
        .get("authorization")
        .or_else(|| headers.get("authorization_token"))?
        .to_str()
        .ok()?
        .trim();
    if raw_hdr.is_empty() {
        return None;
    }
    let token = raw_hdr
        .strip_prefix("Bearer ")
        .or_else(|| raw_hdr.strip_prefix("bearer "))
        .unwrap_or(raw_hdr)
        .trim();
    if token.is_empty() {
        return None;
    }

    let now_unix = ctx.now_unix();
    let verified = match ctx.db.verify_quasar_bearer_jwt(token, now_unix) {
        Ok(v) => v,
        Err(_) if ctx.feature_flags.allow_local_dev_gateway_jwt => {
            let ip = peer_ip?;
            ctx.db.verify_local_dev_client_jwt(token, ip, now_unix).ok()?
        }
        Err(_) => return None,
    };
    let mut account_role = lookup_active_account_role(ctx, verified.user_id, now_unix)?;
    if !verified.is_full_sso {
        account_role &= !service_roles::ROLE_GM_MASK;
    }
    Some(VerifiedGatewayIdentity {
        user_id: verified.user_id,
        token_character_id: verified.character_id,
        account_role,
    })
}

fn lookup_active_account_role(ctx: &ServerContext, user_id: u64, now_unix: u64) -> Option<u64> {
    ctx.db.with_snapshot(|snap| {
        let acc = snap.accounts.values().find(|a| a.user_id == user_id)?;
        acc.check_active_restriction(now_unix).ok()?;
        Some(acc.role)
    })
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

    async fn next_message(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        loop {
            if self.buf.len() >= 5 {
                let compressed = self.buf[0];
                let msg_len =
                    u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]])
                        as usize;
                if msg_len > MAX_GRPC_MESSAGE_BYTES {
                    anyhow::bail!(
                        "gRPC frame length {msg_len} exceeds MAX_GRPC_MESSAGE_BYTES ({MAX_GRPC_MESSAGE_BYTES})"
                    );
                }
                if self.buf.len() >= 5 + msg_len {
                    self.buf.advance(5);
                    let raw = self.buf.split_to(msg_len);
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
                anyhow::bail!("Truncated gRPC frame at stream EOF ({} trailing bytes)", self.buf.len());
            }
            match self.body.data().await {
                Some(Ok(chunk)) => {
                    let len = chunk.len();
                    if len > 0 {
                        let _ = self.body.flow_control().release_capacity(len);
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
}
