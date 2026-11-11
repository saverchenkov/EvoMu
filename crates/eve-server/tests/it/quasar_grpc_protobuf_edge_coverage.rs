//! Deep edge-case, negative-path, malformed-input, role-rejection, idempotency,
//! and h2c transport coverage for Quasar gRPC, PublicGatewayRouter, and the core
//! Quasar protobuf services (career/daily goals, SKINR/cosmetics/market, campaigns/
//! progression/seasons, entitlements/fulcrum/store/corps/freelance, social/chat/mail/wars,
//! achievements, and sovereignty/FW/insurgency/mercenary dens).

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use bytes::{BufMut, Bytes, BytesMut};
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    EveInternalRequest, EvePublicCharacterId, EvePublicCorporationId, EvePublicEvent,
    EvePublicRequest, EvePublicResponse, EvePublicUserId, GoogleAny, InternalGatewayRequest,
    QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::quasar_gateway::{
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK,
    STATUS_PRECONDITION_FAILED, STATUS_SERVICE_UNAVAILABLE, STATUS_UNAUTHORIZED,
};
use eve_server::quasar_grpc::{frame_grpc_message, handle_quasar_grpc_connection};
use eve_server::router::client_compat::{normalize_macho_notification, normalize_macho_rpc_reply};
use eve_server::router::ServerContext;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use tokio::net::{TcpListener, TcpStream};

fn player_session(user_id: u64, char_id: i64, corp_id: i64) -> SessionState {
    let mut sess = SessionState::new(user_id);
    sess.user_id = Some(user_id);
    sess.character_id = Some(char_id);
    sess.corporation_id = Some(corp_id);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    sess
}

fn gm_session(user_id: u64, char_id: i64, corp_id: i64) -> SessionState {
    let mut sess = player_session(user_id, char_id, corp_id);
    sess.role |= service_roles::ROLE_GM_DEFAULT;
    sess
}

fn no_char_session(user_id: u64) -> SessionState {
    let mut sess = SessionState::new(user_id);
    sess.user_id = Some(user_id);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    sess
}

fn unauth_session() -> SessionState {
    SessionState::new(999)
}

fn make_pub_req(type_name: &str, payload: Vec<u8>) -> EvePublicRequest {
    let mut r = EvePublicRequest::default();
    r.correlation_uuid = vec![0xAA; 16];
    r.application_instance_uuid = vec![0xBB; 16];
    r.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value: payload,
    });
    r
}

fn send_quasar(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    payload: Vec<u8>,
) -> EvePublicResponse {
    let req = make_pub_req(type_name, payload);
    ctx.quasar_gateway
        .handle_public_request(sess, &req, None, false, ctx.db.clock().now_unix_secs())
}

fn send_quasar_idem(
    ctx: &ServerContext,
    sess: &SessionState,
    type_name: &str,
    payload: Vec<u8>,
    idem_key: &str,
) -> EvePublicResponse {
    let req = make_pub_req(type_name, payload);
    ctx.quasar_gateway.handle_public_request(
        sess,
        &req,
        Some(idem_key.as_bytes().to_vec()),
        false,
        ctx.db.clock().now_unix_secs(),
    )
}

fn uuid_ident(field_num: u32, uuid: &[u8]) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_bytes_field(1, uuid, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn seq_ident(field_num: u32, sequential: u64) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, sequential, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(field_num, &inner, &mut out);
    out
}

fn extract_uuid_list_first(rsp: &EvePublicResponse, list_field: u32) -> Option<[u8; 16]> {
    let val = &rsp.payload.as_ref()?.value;
    let wm = WireMessage::decode(val).ok()?;
    for f in wm.fields {
        if f.field_number == list_field {
            if let Some(entry_bytes) = f.value.as_bytes() {
                if let Ok(entry_wm) = WireMessage::decode(entry_bytes) {
                    // Check either field 1 directly being 16 bytes OR field 1 being nested identifier
                    for ef in entry_wm.fields {
                        if ef.field_number == 1 {
                            if let Some(b) = ef.value.as_bytes() {
                                if b.len() == 16 {
                                    let mut arr = [0u8; 16];
                                    arr.copy_from_slice(b);
                                    return Some(arr);
                                }
                                if let Ok(id_wm) = WireMessage::decode(b) {
                                    for idf in id_wm.fields {
                                        if idf.field_number == 1 {
                                            if let Some(ub) = idf.value.as_bytes() {
                                                if ub.len() == 16 {
                                                    let mut arr = [0u8; 16];
                                                    arr.copy_from_slice(ub);
                                                    return Some(arr);
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
        }
    }
    None
}

async fn try_send_h2_grpc_call(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    frame: Bytes,
) -> Result<(http::StatusCode, http::HeaderMap, Bytes, http::HeaderMap), h2::Error> {
    let stream = TcpStream::connect(addr).await.expect("connect h2");
    let (mut client, conn) = h2::client::handshake(stream).await?;
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut builder = http::Request::builder()
        .method(http::Method::POST)
        .uri(format!("http://{addr}{path}"))
        .header("content-type", "application/grpc");
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let request = builder.body(()).expect("build h2 req");
    let (response_fut, mut send_stream) = client.send_request(request, false)?;
    let send_res = if !frame.is_empty() {
        send_stream.send_data(frame, true)
    } else {
        send_stream.send_data(Bytes::new(), true)
    };

    let response = match response_fut.await {
        Ok(resp) => resp,
        Err(resp_err) => {
            send_res?;
            return Err(resp_err);
        }
    };
    let status = response.status();
    let resp_headers = response.headers().clone();
    // gRPC Trailers-Only response (server closes before reading request body; no data frames follow).
    if resp_headers.contains_key("grpc-status") {
        return Ok((status, resp_headers, Bytes::new(), http::HeaderMap::new()));
    }
    send_res?;
    let mut body = response.into_body();
    let mut data = BytesMut::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        let len = chunk.len();
        data.extend_from_slice(&chunk);
        let _ = body.flow_control().release_capacity(len);
    }
    let trailers = body.trailers().await?.unwrap_or_default();
    Ok((status, resp_headers, data.freeze(), trailers))
}

async fn send_h2_grpc_call(
    addr: std::net::SocketAddr,
    path: &str,
    headers: &[(&str, &str)],
    frame: Bytes,
) -> (http::StatusCode, http::HeaderMap, Bytes, http::HeaderMap) {
    try_send_h2_grpc_call(addr, path, headers, frame)
        .await
        .expect("h2 grpc call succeeded")
}

fn frame_compressed(flag: u8, payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.put_u8(flag);
    buf.put_u32(payload.len() as u32);
    buf.extend_from_slice(payload);
    buf.freeze()
}

// ============================================================================
// 1. quasar_grpc h2c transport, compression, auth & normalizer edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_grpc_h2c_transport_auth_compression_and_normalizer_edges() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let ctx_srv = Arc::clone(&ctx);
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let c = Arc::clone(&ctx_srv);
            tokio::spawn(async move {
                let _ = handle_quasar_grpc_connection(stream, c).await;
            });
        }
    });

    // 1) /Ping returns empty GoogleEmpty with grpc-status=0
    let (st, _hdr, body, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Requests/Ping",
        &[],
        frame_grpc_message(&[]),
    )
    .await;
    assert_eq!(st, http::StatusCode::OK);
    assert_eq!(tr.get("grpc-status").and_then(|v| v.to_str().ok()), Some("0"));
    assert!(body.len() >= 5);

    // 2) Unknown method -> grpc-status=12 (UNIMPLEMENTED)
    let (st, hdr, _body, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Unknown/Method",
        &[],
        frame_grpc_message(&[]),
    )
    .await;
    assert_eq!(st, http::StatusCode::OK);
    let grpc_st = tr
        .get("grpc-status")
        .or_else(|| hdr.get("grpc-status"))
        .and_then(|v| v.to_str().ok());
    assert_eq!(grpc_st, Some("12"));

    // 3) Unauthenticated calls on /Send, /Consume, /Publish -> grpc-status=16
    for path in [
        "/eve_public.gateway.Requests/Send",
        "/eve_public.gateway.Notices/Consume",
        "/eve_public.gateway.Events/Publish",
    ] {
        let (_, hdr, _, tr) = send_h2_grpc_call(addr, path, &[], frame_grpc_message(&[])).await;
        let code = tr
            .get("grpc-status")
            .or_else(|| hdr.get("grpc-status"))
            .and_then(|v| v.to_str().ok());
        assert_eq!(code, Some("16"), "expected UNAUTHENTICATED for {path}");

        let (_, hdr2, _, tr2) = send_h2_grpc_call(
            addr,
            path,
            &[("authorization", "Bearer not-a-valid-token")],
            frame_grpc_message(&[]),
        )
        .await;
        let code2 = tr2
            .get("grpc-status")
            .or_else(|| hdr2.get("grpc-status"))
            .and_then(|v| v.to_str().ok());
        assert_eq!(code2, Some("16"), "expected UNAUTHENTICATED on bad bearer for {path}");
    }

    // Mint valid gateway token for user=1, char=90_000_001
    let now = ctx.db.clock().now_unix_secs();
    let token = ctx
        .db
        .issue_gateway_dev_token(1, Some(90_000_001), "tranquility", now + 3600, now)
        .expect("mint dev token");
    let auth_header = format!("Bearer {token}");

    // 4) Invalid wire bytes on /Requests/Send -> grpc-status=3 (INVALID_ARGUMENT)
    let (_, hdr, _, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Requests/Send",
        &[("authorization", &auth_header)],
        frame_grpc_message(&[0xFF, 0xFF, 0xFF, 0x01]),
    )
    .await;
    assert_eq!(
        tr.get("grpc-status")
            .or_else(|| hdr.get("grpc-status"))
            .and_then(|v| v.to_str().ok()),
        Some("3")
    );

    // 5) Truncated gRPC frame resets stream cleanly after GrpcFrameReader error
    let truncated = Bytes::from_static(&[0u8, 0u8, 0u8, 0u8, 20u8, 1u8, 2u8]);
    assert!(
        try_send_h2_grpc_call(
            addr,
            "/eve_public.gateway.Requests/Send",
            &[("authorization", &auth_header)],
            truncated,
        )
        .await
        .is_err()
    );

    // 6) Oversized frame (>16 MiB length prefix) rejects frame in GrpcFrameReader
    let mut big_hdr = BytesMut::with_capacity(5);
    big_hdr.put_u8(0);
    big_hdr.put_u32(32 * 1024 * 1024);
    assert!(
        try_send_h2_grpc_call(
            addr,
            "/eve_public.gateway.Requests/Send",
            &[("authorization", &auth_header)],
            big_hdr.freeze(),
        )
        .await
        .is_err()
    );

    // 7) Unsupported grpc-encoding with compressed=1 rejects frame in decompress_grpc_payload
    assert!(
        try_send_h2_grpc_call(
            addr,
            "/eve_public.gateway.Requests/Send",
            &[("authorization", &auth_header), ("grpc-encoding", "br")],
            frame_compressed(1, b"not-brotli"),
        )
        .await
        .is_err()
    );

    // 8) gzip and deflate compressed /Requests/Send requests succeed with grpc-status=0
    let valid_pub = make_pub_req("eve_public.Unknown.DoesNotExist", vec![]);
    let raw_bytes = valid_pub.encode_proto();
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&raw_bytes).expect("gz write");
    let gz_bytes = gz.finish().expect("gz finish");
    let (st, _, resp_body, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Requests/Send",
        &[
            ("authorization", &auth_header),
            ("grpc-encoding", "gzip"),
            ("application_instance_uuid", "01020304-0506-0708-090a-0b0c0d0e0f10"),
        ],
        frame_compressed(1, &gz_bytes),
    )
    .await;
    assert_eq!(st, http::StatusCode::OK);
    assert_eq!(tr.get("grpc-status").and_then(|v| v.to_str().ok()), Some("0"));
    assert!(resp_body.len() > 5);
    let rsp_msg = EvePublicResponse::decode_proto(&resp_body[5..]).expect("decode resp");
    assert_eq!(rsp_msg.status_code, STATUS_NOT_FOUND);

    let mut defl = ZlibEncoder::new(Vec::new(), Compression::default());
    defl.write_all(&raw_bytes).expect("deflate write");
    let defl_bytes = defl.finish().expect("deflate finish");
    let (st, _, _, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Requests/Send",
        &[
            ("authorization", &auth_header),
            ("grpc-encoding", "deflate"),
            ("application_instance_uuid-bin", "AQIDBAUGBwgJCgsMDQ4PEA"),
        ],
        frame_compressed(1, &defl_bytes),
    )
    .await;
    assert_eq!(st, http::StatusCode::OK);
    assert_eq!(tr.get("grpc-status").and_then(|v| v.to_str().ok()), Some("0"));

    // 9) Events/Publish with invalid bytes -> grpc-status=3
    let (_, hdr, _, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Events/Publish",
        &[("authorization", &auth_header)],
        frame_grpc_message(&[0xFF, 0xFF, 0xFF, 0x02]),
    )
    .await;
    assert_eq!(
        tr.get("grpc-status")
            .or_else(|| hdr.get("grpc-status"))
            .and_then(|v| v.to_str().ok()),
        Some("3")
    );

    // 10) Events/Publish with spoofed AuthoritativeContext -> grpc-status=7 (PERMISSION_DENIED)
    let mut spoof_ev = EvePublicEvent::default();
    spoof_ev.uuid = vec![0x11; 16];
    spoof_ev.authoritative_context = Some(AuthoritativeContext {
        authenticated_user: Some(EvePublicUserId { sequential: 2 }),
        active_character: Some(EvePublicCharacterId { sequential: 90_000_002 }),
        service_role_mask: service_roles::ROLE_ADMIN,
        ..Default::default()
    });
    let (_, hdr, _, tr) = send_h2_grpc_call(
        addr,
        "/eve_public.gateway.Events/Publish",
        &[("authorization", &auth_header)],
        frame_grpc_message(&spoof_ev.encode_proto()),
    )
    .await;
    assert_eq!(
        tr.get("grpc-status")
            .or_else(|| hdr.get("grpc-status"))
            .and_then(|v| v.to_str().ok()),
        Some("7")
    );

    // 11) Normalizer edges: short args & non-matching types
    assert_eq!(
        normalize_macho_notification("OnDockingAccepted", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );
    assert_eq!(
        normalize_macho_notification(
            "OnDockingAccepted",
            vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]
        ),
        vec![PyRep::Integer(60_003_760)]
    );
    assert_eq!(
        normalize_macho_notification("OnTargetClear", vec![PyRep::Bool(true)]),
        vec![PyRep::Bool(true)]
    );
    assert!(normalize_macho_notification("DoDestinyUpdate", vec![]).is_empty());
    assert!(normalize_macho_notification("OnBeanPrime", vec![PyRep::None]).len() >= 1);
    assert!(normalize_macho_notification("OnOrganizationContactsUpdated", vec![PyRep::Integer(5)]).len() >= 1);
    assert!(normalize_macho_notification("OnSharedBookmarksFolderUpdated", vec![PyRep::Integer(5)]).len() >= 1);

    let err_res = Err(BridgeRpcError::user_error("InvalidArgument"));
    assert!(normalize_macho_rpc_reply("structureDirectory", "GetJumpBridgesWithMyAccess", err_res).is_err());
    let jb = normalize_macho_rpc_reply(
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        Ok(PyRep::List(vec![])),
    )
    .expect("jb ok");
    assert!(matches!(jb, PyRep::Tuple(_)));
    let jt = normalize_macho_rpc_reply("jumpTimers", "GetTimers", Ok(PyRep::Tuple(vec![])))
        .expect("jt ok");
    assert!(matches!(jt, PyRep::Tuple(_)));
    let notes = normalize_macho_rpc_reply("charMgr", "GetOwnerNoteLabels", Ok(PyRep::None))
        .expect("notes ok");
    assert_eq!(notes, PyRep::None);
}

// ============================================================================
// 2. quasar_gateway routing, spoofing, idempotency, hijack & internal edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_gateway_routing_spoof_idempotency_hijack_and_internal_edges() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let unauth = unauth_session();
    let mut s1 = player_session(1, 90_000_001, 1_000_125);
    let no_char = no_char_session(1);

    // Unauth request -> 401
    let rsp = send_quasar(&ctx, &unauth, "eve_public.goal.api.GetCapacityRequest", vec![]);
    assert_eq!(rsp.status_code, STATUS_UNAUTHORIZED);

    // No active character -> 403 on character-scoped request
    let rsp = send_quasar(&ctx, &no_char, "eve_public.goal.api.GetCapacityRequest", vec![]);
    assert_eq!(rsp.status_code, STATUS_FORBIDDEN);

    // Spoofed AuthoritativeContext: user mismatch, char mismatch, corp mismatch -> 403
    let mut bad_user = make_pub_req("eve_public.goal.api.GetCapacityRequest", vec![]);
    bad_user.authoritative_context = Some(AuthoritativeContext {
        authenticated_user: Some(EvePublicUserId { sequential: 999 }),
        ..Default::default()
    });
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&s1, &bad_user, None, false, ctx.db.clock().now_unix_secs())
            .status_code,
        STATUS_FORBIDDEN
    );

    let mut bad_char = make_pub_req("eve_public.goal.api.GetCapacityRequest", vec![]);
    bad_char.authoritative_context = Some(AuthoritativeContext {
        active_character: Some(EvePublicCharacterId { sequential: 90_000_002 }),
        ..Default::default()
    });
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&s1, &bad_char, None, false, ctx.db.clock().now_unix_secs())
            .status_code,
        STATUS_FORBIDDEN
    );

    let mut bad_corp = make_pub_req("eve_public.goal.api.GetCapacityRequest", vec![]);
    bad_corp.authoritative_context = Some(AuthoritativeContext {
        identity: Some(eve_proto::quasar::ActiveIdentity {
            character: Some(EvePublicCharacterId { sequential: 90_000_001 }),
            corporation: Some(EvePublicCorporationId { sequential: 9_999_999 }),
            roles: None,
        }),
        ..Default::default()
    });
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&s1, &bad_corp, None, false, ctx.db.clock().now_unix_secs())
            .status_code,
        STATUS_FORBIDDEN
    );

    // Missing Any payload -> 400
    let empty_req = EvePublicRequest::default();
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&s1, &empty_req, None, false, ctx.db.clock().now_unix_secs())
            .status_code,
        STATUS_BAD_REQUEST
    );

    // Unknown type URL -> 404
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.nonexistent.api.FakeRequest", vec![]).status_code,
        STATUS_NOT_FOUND
    );

    // Truncated wire bytes on a real handler -> 400
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.ClaimRewardRequest",
            vec![0xFF, 0xFF, 0xFF],
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    // Non-GM calling admin request -> 403
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.admin.ProgressRequest",
            vec![],
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // Idempotency deduplication returns exact cached response
    let r1 = send_quasar_idem(
        &ctx,
        &s1,
        "eve_public.goal.api.GetCapacityRequest",
        vec![],
        "idem-edge-key-1",
    );
    let r2 = send_quasar_idem(
        &ctx,
        &s1,
        "eve_public.goal.api.GetCapacityRequest",
        vec![],
        "idem-edge-key-1",
    );
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(r1.payload, r2.payload);

    // serviceGatewayMgr hijack validation & active hijack behavior
    let mut gm = gm_session(1, 90_000_001, 1_000_125);
    let kw_empty = HashMap::new();
    let mut kw_bad_err = HashMap::new();
    kw_bad_err.insert("error".to_string(), PyRep::String("BogusCodeName".to_string()));
    assert!(ctx
        .dispatch_rpc(&mut gm, "serviceGatewayMgr", None, "qa_enable_hijack_mode", &[], &kw_bad_err, None)
        .await
        .is_err());

    let mut kw_bad_code = HashMap::new();
    kw_bad_code.insert("error".to_string(), PyRep::Integer(999));
    assert!(ctx
        .dispatch_rpc(&mut gm, "serviceGatewayMgr", None, "qa_enable_hijack_mode", &[], &kw_bad_code, None)
        .await
        .is_err());

    let mut kw_neg_lat = HashMap::new();
    kw_neg_lat.insert("latency".to_string(), PyRep::Real(-1.0));
    assert!(ctx
        .dispatch_rpc(&mut gm, "serviceGatewayMgr", None, "qa_enable_hijack_mode", &[], &kw_neg_lat, None)
        .await
        .is_err());

    let mut kw_big_lat = HashMap::new();
    kw_big_lat.insert("latency".to_string(), PyRep::Real(500.0));
    assert!(ctx
        .dispatch_rpc(&mut gm, "serviceGatewayMgr", None, "qa_enable_hijack_mode", &[], &kw_big_lat, None)
        .await
        .is_err());

    let mut kw_hijack = HashMap::new();
    kw_hijack.insert("error".to_string(), PyRep::Integer(503));
    kw_hijack.insert("latency".to_string(), PyRep::Real(2.0));
    ctx.dispatch_rpc(&mut gm, "serviceGatewayMgr", None, "qa_enable_hijack_mode", &[], &kw_hijack, None)
        .await
        .expect("enable hijack");
    let hijacked = send_quasar(&ctx, &s1, "eve_public.goal.api.GetCapacityRequest", vec![]);
    assert_eq!(hijacked.status_code, STATUS_SERVICE_UNAVAILABLE);
    ctx.dispatch_rpc(&mut gm, "serviceGatewayMgr", None, "qa_disable_hijack_mode", &[], &kw_empty, None)
        .await
        .expect("disable hijack");

    // publicGatewaySvc Subscribe / PublishEvent / SendRequest error edges
    let mut unauth_mut = unauth_session();
    assert!(ctx
        .dispatch_rpc(&mut unauth_mut, "publicGatewaySvc", None, "Subscribe", &[], &kw_empty, None)
        .await
        .is_err());
    assert!(ctx
        .dispatch_rpc(
            &mut unauth_mut,
            "publicGatewaySvc",
            None,
            "PublishEvent",
            &[PyRep::Bytes(vec![0xFF, 0xFF, 0xFF])],
            &kw_empty,
            None,
        )
        .await
        .is_err());
    let send_err_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(vec![0xFF, 0xFF, 0xFF])],
            &kw_empty,
            None,
        )
        .await
        .expect("SendRequest wraps malformed proto in EvePublicResponse");
    match send_err_rep {
        PyRep::KeyVal(kv) => {
            assert_eq!(
                kv.get("status_code"),
                Some(&PyRep::Integer(i64::from(STATUS_BAD_REQUEST)))
            );
        }
        other => panic!("unexpected SendRequest reply: {other:?}"),
    }

    // handle_internal_gateway_request & handle_eve_internal_request
    let int_empty = InternalGatewayRequest {
        ack_id: 77,
        request: None,
    };
    let int_rsp = ctx.quasar_gateway.handle_internal_gateway_request(
        &s1,
        &int_empty,
        ctx.db.clock().now_unix_secs(),
    );
    assert_eq!(int_rsp.ack_id, 77);
    assert_eq!(int_rsp.response.expect("inner rsp").status_code, STATUS_BAD_REQUEST);

    let eve_int_none = EveInternalRequest::default();
    let eve_int_rsp = ctx.quasar_gateway.handle_eve_internal_request(
        &s1,
        &eve_int_none,
        ctx.db.clock().now_unix_secs(),
    );
    assert_eq!(eve_int_rsp.status_code, STATUS_BAD_REQUEST);
}

// ============================================================================
// 3. Career Goals & Daily Goals validation, claim & admin edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_career_and_daily_goals_validation_and_claim_edges() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let no_char = no_char_session(1);
    let s1 = player_session(1, 90_000_001, 1_000_125);
    let gm = gm_session(1, 90_000_001, 1_000_125);
    let unknown_uuid = [0x9E; 16];

    // Fetch existing career goal UUID for char 90_000_001
    let all_cg = send_quasar(&ctx, &s1, "eve_public.career.goal.api.GetAllRequest", vec![]);
    assert_eq!(all_cg.status_code, STATUS_OK);
    let first_cg_uuid = extract_uuid_list_first(&all_cg, 1).expect("at least one career goal");

    // Career Goal GetProgress / HasClaimed / ClaimReward negative paths
    for ty in [
        "eve_public.career.goal.api.ClaimRewardRequest",
        "eve_public.app.eveonline.career.goal.ClaimRewardRequest",
        "eve.character.career.goal.GetProgressRequest",
        "eve.character.career.goal.HasClaimedRewardRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_NOT_FOUND
        );
    }

    // Claim before completion -> 403
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_ident(1, &first_cg_uuid),
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // Career milestone UUID edge cases: unknown milestone -> 404, valid unreached milestone -> 403
    let mut bad_ms_uuid = [0u8; 16];
    bad_ms_uuid[0] = 0x43;
    bad_ms_uuid[1] = 0x4D;
    bad_ms_uuid[12..16].copy_from_slice(&999_999u32.to_be_bytes());
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_ident(1, &bad_ms_uuid),
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Complete first career goal as GM then double-claim -> OK then 403
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.admin.ProgressRequest",
            uuid_ident(1, &first_cg_uuid),
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    let mut prog_zero = uuid_ident(1, &first_cg_uuid);
    encode_varint_field(2, 0, &mut prog_zero);
    assert_eq!(
        send_quasar(
            &ctx,
            &gm,
            "eve_public.career.goal.api.admin.ProgressRequest",
            prog_zero,
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &gm,
            "eve_public.career.goal.api.admin.CompleteRequest",
            uuid_ident(1, &unknown_uuid),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &gm,
            "eve_public.career.goal.api.admin.CompleteRequest",
            uuid_ident(1, &first_cg_uuid),
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_ident(1, &first_cg_uuid),
        )
        .status_code,
        STATUS_OK
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.career.goal.api.ClaimRewardRequest",
            uuid_ident(1, &first_cg_uuid),
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // Admin GetAllByCharacter negative paths: non-GM -> 403, GM without character -> 403
    let mut gm_no_char = no_char_session(1);
    gm_no_char.role |= service_roles::ROLE_GM_DEFAULT;
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.career.goal.api.admin.GetAllByCharacterRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &gm_no_char, "eve.career.goal.api.admin.GetAllByCharacterRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );

    // Daily Goals negative paths
    let all_dg = send_quasar(&ctx, &s1, "eve_public.dailygoal.api.GetAllCurrentRequest", vec![]);
    assert_eq!(all_dg.status_code, STATUS_OK);
    let first_dg_uuid = extract_uuid_list_first(&all_dg, 1).expect("at least one daily goal");

    for ty in [
        "eve_public.dailygoal.api.GetRequest",
        "eve_public.dailygoal.api.RedeemRequest",
        "eve_public.dailygoal.api.PayForCompletionRequest",
        "eve.dailygoal.api.GetRequest",
        "eve.dailygoal.api.GetProgressRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_NOT_FOUND
        );
    }

    // Redeem uncompleted daily goal -> 403
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.dailygoal.api.RedeemRequest",
            uuid_ident(1, &first_dg_uuid),
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // Daily goal admin non-GM rejected 403, missing UUID -> 400
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.dailygoal.api.admin.ProgressRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &gm, "eve_public.dailygoal.api.admin.ProgressRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.dailygoal.api.admin.AutoRedeemRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
}

// ============================================================================
// 4. Cosmetics Drafts, SKINR Licenses/Sequencing & Paragon Market edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_cosmetics_drafts_skinr_and_market_edge_paths() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let s1 = player_session(1, 90_000_001, 1_000_125);
    let s2 = player_session(2, 90_000_002, 1_000_125);
    let no_char = no_char_session(1);
    let unknown_uuid = [0xD4; 16];

    // 1) Drafts API: missing ID -> 400, unknown ID -> 404, cross-char -> 403, save capacity -> 400
    for ty in [
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_NOT_FOUND
        );
    }

    // Save a draft for s1, verify s2 cannot Get/Update/Delete it (403)
    let mut save_req = Vec::new();
    let mut design = Vec::new();
    encode_varint_field(1, 587, &mut design); // Rifter hull
    encode_bytes_field(1, &design, &mut save_req);
    encode_string_field(2, "EdgeDraft", &mut save_req);
    let saved = send_quasar(
        &ctx,
        &s1,
        "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
        save_req,
    );
    assert_eq!(saved.status_code, STATUS_OK);
    let saved_id = extract_uuid_list_first(&saved, 1)
        .or_else(|| {
            // SaveResponse might have identifier directly at field 1
            let wm = WireMessage::decode(&saved.payload.as_ref()?.value).ok()?;
            for f in wm.fields {
                if f.field_number == 1 {
                    if let Some(b) = f.value.as_bytes() {
                        if let Ok(iwm) = WireMessage::decode(b) {
                            for iif in iwm.fields {
                                if iif.field_number == 1 {
                                    if let Some(ub) = iif.value.as_bytes() {
                                        if ub.len() == 16 {
                                            let mut arr = [0u8; 16];
                                            arr.copy_from_slice(ub);
                                            return Some(arr);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            None
        })
        .expect("saved draft id");

    assert_eq!(
        send_quasar(
            &ctx,
            &s2,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
            uuid_ident(1, &saved_id),
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s2,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
            uuid_ident(1, &saved_id),
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s2,
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
            uuid_ident(1, &saved_id),
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // 2) Cosmetics market edges
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            vec![],
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let mut bad_skin_inner = Vec::new();
    encode_string_field(1, "deadbeefcafebabe0000000000000000", &mut bad_skin_inner);
    let mut bad_skin_hex = Vec::new();
    encode_bytes_field(2, &bad_skin_inner, &mut bad_skin_hex);
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.cosmetic.market.api.GetBuyerFeeRequest",
            bad_skin_hex,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    for ty in [
        "eve_public.cosmetic.market.skin.listing.api.GetRequest",
        "eve_public.cosmetic.market.skin.listing.api.CancelListingRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_NOT_FOUND
        );
    }
    for ty in [
        "eve_public.cosmetic.market.component.listing.api.InitiatePurchaseListingRequest",
        "eve_public.cosmetic.market.skin.listing.api.InitiatePurchaseListingRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_BAD_REQUEST
        );
        let mut pur_unknown = uuid_ident(1, &unknown_uuid);
        encode_varint_field(2, 1, &mut pur_unknown);
        assert_eq!(send_quasar(&ctx, &s1, ty, pur_unknown).status_code, STATUS_NOT_FOUND);
    }

    // Create skin listing invalid args (0 price / 0 qty / non-GM admin SetExpiration)
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.cosmetic.market.skin.listing.api.InitiateCreateListingRequest",
            vec![],
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.cosmetic.market.skin.listing.api.admin.SetExpirationRequest",
            uuid_ident(1, &unknown_uuid),
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // 3) SKINR Component Licenses, 3rd-party SKIN Licenses & Sequencing Jobs
    for ty in [
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.item.api.ConsumeRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
    }

    // Non-GM rejected on license admin Grant/Revoke
    for admin_ty in [
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.GrantRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.api.admin.RevokeRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.GrantRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.infinite.api.admin.RevokeRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, admin_ty, vec![]).status_code, STATUS_FORBIDDEN);
    }

    // Unknown skin hex on thirdparty.api.GetRequest -> 404
    let mut bad_skin_get = Vec::new();
    encode_string_field(1, "ffffffffffffffffffffffffffffffff", &mut bad_skin_get);
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
            bad_skin_get,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Sequencing Initiate with missing design -> 400
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
            vec![],
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
}

// ============================================================================
// 5. Campaigns, Generic Goals, Seasonal Challenges & Progression edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_campaigns_goals_seasons_and_progression_edge_paths() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let s1 = player_session(1, 90_000_001, 1_000_125);
    let no_char = no_char_session(1);
    let unknown_uuid = [0xA7; 16];

    let mut unknown_camp = [0u8; 16];
    unknown_camp[0..8].copy_from_slice(b"CAMPAIGN");
    unknown_camp[8..16].copy_from_slice(&999_999u64.to_be_bytes());
    let mut unknown_obj = [0u8; 16];
    unknown_obj[0..8].copy_from_slice(b"CAMPOBJ_");
    unknown_obj[8..16].copy_from_slice(&999_999u64.to_be_bytes());
    let mut unknown_trk = [0u8; 16];
    unknown_trk[0..8].copy_from_slice(b"CAMPTRK_");
    unknown_trk[8..16].copy_from_slice(&999_999u64.to_be_bytes());

    // Campaign API: missing/malformed ID -> 400, unknown ID with valid prefix -> 404
    for (ty, valid_unknown) in [
        ("eve_public.campaign.api.GetRequest", &unknown_camp),
        ("eve_public.campaign.api.GetStatusRequest", &unknown_camp),
        ("eve_public.campaign.api.GetStatsRequest", &unknown_camp),
        ("eve_public.campaign.objective.api.GetRequest", &unknown_obj),
        ("eve_public.campaign.objective.api.GetStatusRequest", &unknown_obj),
        ("eve_public.campaign.objective.api.GetStatsRequest", &unknown_obj),
        ("eve_public.campaign.track.api.GetTickingContributionsPredictionRequest", &unknown_trk),
    ] {
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_BAD_REQUEST
        );
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, valid_unknown)).status_code,
            STATUS_NOT_FOUND
        );
    }

    // Char-scoped objective APIs
    for (ty, valid_unknown) in [
        ("eve_public.campaign.objective.api.GetParticipantStatusRequest", &unknown_obj),
        ("eve_public.campaign.objective.api.GetParticipantStatsRequest", &unknown_obj),
        ("eve_public.campaign.objective.api.CommitRequest", &unknown_obj),
        ("eve_public.campaign.objective.api.ResignRequest", &unknown_obj),
        ("eve_public.campaign.objective.api.RedeemRewardsRequest", &unknown_obj),
        ("eve_public.campaign.track.api.RedeemParticipantRewardsRequest", &unknown_trk),
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, valid_unknown)).status_code,
            STATUS_NOT_FOUND
        );
    }

    // Generic Goal API edges: Close / SetCurrentProgress (admin 403 / 400 / 404) / Delete / GetMyContributorSummary
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.goal.api.SetCurrentProgressRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.goal.api.SetNameRequest", vec![]).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.goal.api.SetDescriptionRequest", vec![]).status_code,
        STATUS_FORBIDDEN
    );

    let gm = gm_session(1, 90_000_001, 1_000_125);
    assert_eq!(
        send_quasar(&ctx, &gm, "eve_public.goal.api.SetCurrentProgressRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &gm,
            "eve_public.goal.api.SetCurrentProgressRequest",
            uuid_ident(1, &unknown_uuid),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &gm,
            "eve.goal.api.SetNameRequest",
            uuid_ident(1, &unknown_uuid), // empty name -> 400 before lookup
        )
        .status_code,
        STATUS_BAD_REQUEST
    );

    assert_eq!(
        send_quasar(&ctx, &no_char, "eve_public.goal.api.DeleteRequest", vec![]).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.goal.api.DeleteRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.goal.api.DeleteRequest",
            uuid_ident(1, &unknown_uuid),
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
            vec![],
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
            uuid_ident(1, &unknown_uuid),
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Seasonal challenges cross-character rejection -> 403
    let other_char_payload = seq_ident(1, 90_000_002);
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
            other_char_payload,
        )
        .status_code,
        STATUS_FORBIDDEN
    );
}

// ============================================================================
// 6. Entitlements, AssetHolding, Store/Payment, Corps & Freelance edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_entitlements_fulcrum_corps_and_freelance_edge_paths() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let unauth = unauth_session();
    let s1 = player_session(1, 90_000_001, 1_000_125);
    let no_char = no_char_session(1);
    let gm = gm_session(1, 90_000_001, 1_000_125);
    let unknown_uuid = [0xE3; 16];

    // 1) Asset holding & Entitlement Ledger
    for ty in [
        "eve_public.assetholding.asset.api.GetRequest",
        "eve.assetholding.asset.api.GetRequest",
        "eve_public.assetholding.entitlement.api.GetRequest",
        "eve.assetholding.entitlement.api.GetRequest",
        "eve_public.assetholding.entitlement.api.RedeemRequest",
        "eve.assetholding.entitlement.api.RedeemRequest",
        "eve.assetholding.entitlement.api.DeleteRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &unauth, ty, vec![]).status_code, STATUS_UNAUTHORIZED);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
    }

    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.assetholding.asset.api.GetRequest",
            uuid_ident(1, &unknown_uuid),
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Non-GM rejected on admin asset/entitlement ops
    for admin_ty in [
        "eve.assetholding.entitlement.api.admin.RedeemRequest",
        "eve.assetholding.entitlement.api.admin.ExpireRequest",
        "eve_public.assetholding.entitlement.api.admin.ExpireRequest",
        "eve.assetholding.entitlement.api.admin.AutoRedeemRequest",
        "eve_public.assetholding.entitlement.api.admin.AutoRedeemRequest",
        "eve.assetholding.entitlement.api.admin.RedeliverStaleEntitlementsRequest",
        "eve.assetholding.entitlement.api.DirectDeliveryRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, admin_ty, vec![]).status_code, STATUS_FORBIDDEN);
    }

    // 2) Entitlement memberships & admin grants
    assert_eq!(
        send_quasar(&ctx, &unauth, "eve.entitlement.user.api.GetRequest", vec![]).status_code,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.entitlement.character.api.IsEntitledRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    for admin_ty in [
        "eve.entitlement.user.api.GrantRequest",
        "eve.entitlement.user.subscription.api.GrantRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, admin_ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &gm, admin_ty, vec![]).status_code, STATUS_BAD_REQUEST);
    }

    // 3) Payment / Store edges
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.store.targetaudience.offer.api.GetRequest",
            vec![],
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    let mut bad_ta_entry = Vec::new();
    encode_string_field(1, "999999", &mut bad_ta_entry);
    let mut bad_ta_id = Vec::new();
    encode_bytes_field(2, &bad_ta_entry, &mut bad_ta_id);
    let mut bad_ta = Vec::new();
    encode_bytes_field(1, &bad_ta_id, &mut bad_ta);
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.store.targetaudience.offer.api.GetRequest",
            bad_ta,
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.payment.code.api.RedeemRequest", vec![])
            .status_code,
        STATUS_NOT_IMPLEMENTED
    );
    assert_eq!(
        send_quasar(&ctx, &unauth, "eve_public.payment.schedule.api.GetRequest", vec![])
            .status_code,
        STATUS_UNAUTHORIZED
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.payment.schedule.api.GetRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    let mut bad_sched = Vec::new();
    encode_varint_field(1, 999_999, &mut bad_sched);
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.payment.schedule.api.GetRequest", bad_sched)
            .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.payment.token.api.GetRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.payment.purchase.api.CostRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );

    // 4) Corporation Goals & Freelance Projects
    assert_eq!(
        send_quasar(
            &ctx,
            &no_char,
            "eve_public.corporationgoal.api.GetAllRequest",
            vec![],
        )
        .status_code,
        STATUS_FORBIDDEN
    );
    let mut unknown_corp_goal = [0u8; 16];
    unknown_corp_goal[0..8].copy_from_slice(b"CORPGOAL");
    unknown_corp_goal[8..16].copy_from_slice(&999_999u64.to_be_bytes());
    for ty in [
        "eve_public.corporationgoal.api.GetContributorSummariesForGoalRequest",
        "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest",
        "eve_public.corporationgoal.api.RedeemMyRewardsRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_uuid)).status_code,
            STATUS_BAD_REQUEST
        );
        assert_eq!(
            send_quasar(&ctx, &s1, ty, uuid_ident(1, &unknown_corp_goal)).status_code,
            STATUS_NOT_FOUND
        );
    }

    assert_eq!(
        send_quasar(&ctx, &no_char, "eve_public.freelance.project.api.CreateRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.freelance.project.api.CreateRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
}

// ============================================================================
// 7. Player Social Comms: Local Chat, Presence, Mail, Contact Labels, Wars
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_player_social_comms_chat_mail_contacts_and_war_edges() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let s1 = player_session(1, 90_000_001, 1_000_125);
    let mut s_abyss = player_session(1, 90_000_001, 1_000_125);
    s_abyss.solar_system_id = Some(32_000_001);
    let no_char = no_char_session(1);
    let gm = gm_session(1, 90_000_001, 1_000_125);

    // Chat BroadcastMessage: empty text -> 400, Abyssal -> 403, no char -> 403
    assert_eq!(
        send_quasar(&ctx, &no_char, "eve_public.chat.local.api.BroadcastMessageRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.chat.local.api.BroadcastMessageRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    let mut msg_bytes = Vec::new();
    encode_string_field(1, "hello local", &mut msg_bytes);
    assert_eq!(
        send_quasar(
            &ctx,
            &s_abyss,
            "eve_public.chat.local.api.BroadcastMessageRequest",
            msg_bytes,
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // Chat admin.Mute: non-GM -> 403, missing char/duration -> 400, unknown char -> 404
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.chat.local.api.admin.MuteRequest", vec![])
            .status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &gm, "eve_public.chat.local.api.admin.MuteRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    let mut unknown_mute = seq_ident(1, 999_999_999);
    let mut dur_msg = Vec::new();
    encode_varint_field(1, 60, &mut dur_msg);
    encode_bytes_field(2, &dur_msg, &mut unknown_mute);
    assert_eq!(
        send_quasar(
            &ctx,
            &gm,
            "eve_public.chat.local.api.admin.MuteRequest",
            unknown_mute,
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Presence queries: no char -> 403, unknown char -> 404
    for ty in [
        "eve_public.presence.api.GetCorporationRequest",
        "eve_public.presence.api.GetAllianceRequest",
        "eve_public.presence.api.GetFactionRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(
            send_quasar(&ctx, &s1, ty, seq_ident(1, 999_999_999)).status_code,
            STATUS_NOT_FOUND
        );
    }

    // Mail CCPSendMail: non-GM -> 403, GM empty body/recipients -> 400
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.mail.CCPSendMailRequest", vec![]).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        send_quasar(&ctx, &gm, "eve.mail.CCPSendMailRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );

    // Contact label cross-char -> 403, missing -> 400
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve.contact.label.GetForCharacterRequest",
            seq_ident(1, 90_000_002),
        )
        .status_code,
        STATUS_FORBIDDEN
    );

    // War GetRequest missing -> 400, unknown -> 404
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.war.GetRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.war.GetRequest", seq_ident(1, 9_999_999)).status_code,
        STATUS_NOT_FOUND
    );
}

// ============================================================================
// 8. Achievements, Sovereignty Hubs/Skyhooks/MercDens, FW & Insurgency edges
// ============================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_achievements_sovereignty_fw_and_insurgency_edge_paths() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    let s1 = player_session(1, 90_000_001, 1_000_125);
    let no_char = no_char_session(1);
    let gm = gm_session(1, 90_000_001, 1_000_125);

    // 1) Achievements: missing -> 400, unknown -> 404 (achievement) / 200 (category), Claim -> 412, admin -> 403
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.achievement.api.GetRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.achievement.api.GetRequest",
            seq_ident(1, 9_999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.achievement.category.api.GetRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.achievement.category.api.GetRequest",
            seq_ident(1, 9_999_999),
        )
        .status_code,
        STATUS_OK
    );

    assert_eq!(
        send_quasar(&ctx, &s1, "eve_public.achievement.reward.api.ClaimRequest", vec![]).status_code,
        STATUS_PRECONDITION_FAILED
    );
    let claim_rsp = send_quasar(
        &ctx,
        &s1,
        "eve_public.achievement.reward.api.ClaimRequest",
        seq_ident(1, 1),
    );
    assert_eq!(claim_rsp.status_code, STATUS_PRECONDITION_FAILED);

    for admin_ty in [
        "eve_public.achievement.api.admin.AddProgressRequest",
        "eve_public.achievement.api.admin.CompleteRequest",
        "eve_public.achievement.api.admin.ResetRequest",
        "eve_public.achievement.category.api.admin.ResetRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, admin_ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &gm, admin_ty, vec![]).status_code, STATUS_BAD_REQUEST);
    }

    // 2) Sovereignty Hubs, Skyhooks & Mercenary Dens
    for ty in [
        "eve_public.sovereignty.hub.api.GetResourcesRequest",
        "eve_public.sovereignty.hub.fuel.api.GetRequest",
        "eve_public.sovereignty.hub.fuel.api.AddRequest",
        "eve_public.sovereignty.hub.upgrade.api.GetRequest",
        "eve_public.sovereignty.hub.upgrade.api.UninstallRequest",
        "eve_public.sovereignty.skyhook.api.ActivateRequest",
        "eve_public.sovereignty.skyhook.api.DeactivateRequest",
        "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
        "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &no_char, ty, vec![]).status_code, STATUS_FORBIDDEN);
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_BAD_REQUEST);
    }

    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.sovereignty.hub.api.GetResourcesRequest",
            seq_ident(1, 9_999_999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.sovereignty.skyhook.api.ActivateRequest",
            seq_ident(1, 9_999_999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            seq_ident(1, 9_999_999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    // Non-GM denied on mercenaryden admin
    for ty in [
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest",
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest",
    ] {
        assert_eq!(send_quasar(&ctx, &s1, ty, vec![]).status_code, STATUS_FORBIDDEN);
        let mut bad_level = seq_ident(1, 1);
        encode_varint_field(2, 500, &mut bad_level);
        assert_eq!(send_quasar(&ctx, &gm, ty, bad_level).status_code, STATUS_BAD_REQUEST);
    }

    // 3) Factions, FW Warzone, Pirate Insurgency & Space/Navigation
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.faction.api.GetRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve.faction.api.GetRequest",
            seq_ident(1, 999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(&ctx, &s1, "eve.faction.SearchRequest", vec![]).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.faction.activity.GetSolarSystemScoresRequest",
            seq_ident(1, 30_000_142), // Jita: non-FW solar system
        )
        .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve.factionwarfare.warzone.api.GetParticipantsRequest",
            seq_ident(1, 999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );

    assert_eq!(
        send_quasar(&ctx, &s1, "eve.pirate.insurgency.api.GetRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve.pirate.insurgency.api.GetRequest",
            seq_ident(1, 999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.pirate.corruption.api.GetSystemInfoRequest",
            seq_ident(1, 39_999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(
            &ctx,
            &s1,
            "eve_public.pirate.suppression.api.GetSystemInfoRequest",
            seq_ident(1, 39_999_999),
        )
        .status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        send_quasar(&ctx, &no_char, "eve.universe.navigation.GetShortestGateJumpCountRequest", vec![])
            .status_code,
        STATUS_BAD_REQUEST
    );
}
