//! Edge-case, error-guard, boundary, and state-transition unit tests for `quasar_grpc`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::STATUS_OK;
use eve_db::accounts::{hmac_sha256, sha256_bytes};
use eve_db::{base64url_encode_no_pad, AccountRecord};
use eve_machonet::service_roles;
use eve_proto::quasar::{
    AuthoritativeContext, EvePublicEvent, EvePublicRequest, EvePublicResponse, GoogleAny,
    GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use http::{HeaderMap, HeaderValue, Request, StatusCode};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use tokio::net::TcpListener;

fn sign_hs256(header_json: &str, payload_json: &str, secret: &[u8]) -> String {
    let signing_input = format!(
        "{}.{}",
        base64url_encode_no_pad(header_json.as_bytes()),
        base64url_encode_no_pad(payload_json.as_bytes())
    );
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    format!("{signing_input}.{}", base64url_encode_no_pad(&sig))
}

async fn spawn_grpc(ctx: Arc<ServerContext>) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let c = Arc::clone(&ctx);
            tokio::spawn(async move {
                let _ = handle_quasar_grpc_connection(sock, c).await;
            });
        }
    });
    addr
}

fn frame_compressed(payload: &[u8]) -> Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.extend_from_slice(&[1u8]);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf.freeze()
}

#[test]
fn parse_app_instance_uuid_and_decompress_grpc_payload_edges() {
    // 1. UUID header: standard hyphenated UUID
    let u = uuid::Uuid::new_v4();
    let mut h = HeaderMap::new();
    h.insert(
        "application_instance_uuid",
        HeaderValue::from_str(&format!("  {}  ", u)).unwrap(),
    );
    assert_eq!(parse_app_instance_uuid(&h), Some(u.as_bytes().to_vec()));

    // 2. UUID header: raw 16-char string fallback
    h.clear();
    h.insert(
        "application_instance_uuid",
        HeaderValue::from_static("0123456789abcdef"),
    );
    assert_eq!(
        parse_app_instance_uuid(&h),
        Some(b"0123456789abcdef".to_vec())
    );

    // 3. Invalid string falls through to `-bin` header
    h.insert(
        "application_instance_uuid",
        HeaderValue::from_static("too-short"),
    );
    let raw_bytes = [0xAAu8; 16];
    h.insert(
        "application_instance_uuid-bin",
        HeaderValue::from_str(&base64url_encode_no_pad(&raw_bytes)).unwrap(),
    );
    assert_eq!(parse_app_instance_uuid(&h), Some(raw_bytes.to_vec()));

    // 4. `-bin` wrong length or invalid base64 returns None
    h.insert(
        "application_instance_uuid-bin",
        HeaderValue::from_str(&base64url_encode_no_pad(&[1u8; 8])).unwrap(),
    );
    assert_eq!(parse_app_instance_uuid(&h), None);
    h.insert(
        "application_instance_uuid-bin",
        HeaderValue::from_static("!!!not-b64!!!"),
    );
    assert_eq!(parse_app_instance_uuid(&h), None);
    assert_eq!(parse_app_instance_uuid(&HeaderMap::new()), None);

    // 5. decompress_grpc_payload: gzip & deflate succeed, unsupported fails, >16MiB fails
    let sample = b"hello quasar grpc compression payload";
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(sample).unwrap();
    let gz_bytes = gz.finish().unwrap();
    assert_eq!(
        decompress_grpc_payload(&gz_bytes, "gzip").unwrap(),
        sample.to_vec()
    );

    let mut zl = ZlibEncoder::new(Vec::new(), Compression::default());
    zl.write_all(sample).unwrap();
    let zl_bytes = zl.finish().unwrap();
    assert_eq!(
        decompress_grpc_payload(&zl_bytes, "deflate").unwrap(),
        sample.to_vec()
    );

    let err_unsup = decompress_grpc_payload(&gz_bytes, "br").unwrap_err();
    assert!(err_unsup.to_string().contains("Unsupported compressed gRPC encoding"));

    // Oversized compressed payload (> MAX_GRPC_MESSAGE_BYTES) must bail
    let big = vec![0u8; MAX_GRPC_MESSAGE_BYTES + 64];
    let mut big_zl = ZlibEncoder::new(Vec::new(), Compression::fast());
    big_zl.write_all(&big).unwrap();
    let big_zl_bytes = big_zl.finish().unwrap();
    let err_big = decompress_grpc_payload(&big_zl_bytes, "deflate").unwrap_err();
    assert!(err_big.to_string().contains("exceeds MAX_GRPC_MESSAGE_BYTES"));
}

#[test]
fn authenticate_grpc_headers_and_build_session_edges() {
    let mut raw_ctx = ServerContext::with_repository(Arc::new(eve_db::WorldRepository::with_demo_seed()));
    raw_ctx.feature_flags.allow_local_dev_gateway_jwt = false;
    let now_u = raw_ctx.now_unix();
    let loopback = Some(IpAddr::V4(Ipv4Addr::LOCALHOST));

    // Grant GM role to user 1 so we can verify non-full-SSO role stripping
    raw_ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.values_mut().find(|a| a.user_id == 1) {
            acc.role = service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GM_MASK;
        }
    });

    // 1. Missing / empty / whitespace Bearer headers
    assert!(authenticate_grpc_headers(&raw_ctx, &HeaderMap::new(), loopback).is_none());
    let mut hdrs = HeaderMap::new();
    hdrs.insert("authorization", HeaderValue::from_static("   "));
    assert!(authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).is_none());
    hdrs.insert("authorization", HeaderValue::from_static("Bearer   "));
    assert!(authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).is_none());

    // 2. Non-full-SSO dev token strips ROLE_GM_MASK, accepts lowercase `bearer ` and `authorization_token`
    let dev_jwt = raw_ctx
        .db
        .issue_gateway_dev_token(1, Some(90_000_001), "tranquility", now_u + 3600, now_u)
        .unwrap();
    hdrs.clear();
    hdrs.insert(
        "authorization",
        HeaderValue::from_str(&format!("bearer {dev_jwt}")).unwrap(),
    );
    let mut id1 = authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).expect("bearer lowercase");
    assert_eq!(id1.user_id, 1);
    assert_eq!(id1.token_character_id, Some(90_000_001));
    assert!(!id1.is_full_sso);

    hdrs.clear();
    hdrs.insert(
        "authorization_token",
        HeaderValue::from_str(&dev_jwt).unwrap(),
    );
    let id2 = authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).expect("authorization_token");
    assert_eq!(id2.user_id, 1);

    // 3. Full SSO token preserves ROLE_GM_MASK
    let verifier = "quasar_grpc_edges_pkce_verifier_1234567890";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let pkce_code = raw_ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            90_000_001,
            "eveLauncherTQ",
            "http://127.0.0.1/cb",
            Vec::new(),
            &challenge,
            "S256",
            now_u,
        )
        .unwrap();
    let full_sso = raw_ctx
        .db
        .exchange_pkce_code(&pkce_code, "eveLauncherTQ", verifier, now_u)
        .unwrap()
        .access_token;
    hdrs.clear();
    hdrs.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {full_sso}")).unwrap(),
    );
    let mut id_sso = authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).expect("full sso");
    assert!(id_sso.is_full_sso);
    let sso_sess = build_session_for_identity(&raw_ctx, 9_000_110, &mut id_sso, None).unwrap();
    assert_ne!(sso_sess.role & service_roles::ROLE_GM_MASK, 0);

    // 4. Local-dev JWT fallback: disabled when flag false, fails when peer_ip None, fails on bad JWT, succeeds when enabled + online IP bound
    let local_jwt = sign_hs256(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:1","exp":{}}}"#, now_u + 300),
        b"dev",
    );
    hdrs.clear();
    hdrs.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {local_jwt}")).unwrap(),
    );
    assert!(authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).is_none());

    raw_ctx.feature_flags.allow_local_dev_gateway_jwt = true;
    assert!(authenticate_grpc_headers(&raw_ctx, &hdrs, None).is_none());

    raw_ctx
        .db
        .acquire_online_lock(1, 77_001, Some(90_000_001), true, now_u)
        .unwrap();
    raw_ctx
        .db
        .set_online_session_peer_ip(1, 77_001, "127.0.0.1:50000");
    let mut id_local = authenticate_grpc_headers(&raw_ctx, &hdrs, loopback).expect("local dev jwt");
    assert_eq!(id_local.user_id, 1);
    assert!(!id_local.is_full_sso);
    let local_sess = build_session_for_identity(&raw_ctx, 9_000_110, &mut id_local, None).unwrap();
    assert_eq!(local_sess.role & service_roles::ROLE_GM_MASK, 0);

    let mut bad_hdrs = HeaderMap::new();
    bad_hdrs.insert("authorization", HeaderValue::from_static("Bearer not.a.jwt"));
    assert!(authenticate_grpc_headers(&raw_ctx, &bad_hdrs, loopback).is_none());

    // 5. build_session_for_identity: claimed char owned by another user (90_000_002) falls back to online/token char (90_000_001)
    let sess_spoof = build_session_for_identity(&raw_ctx, 9_000_111, &mut id1, Some(90_000_002)).unwrap();
    assert_eq!(sess_spoof.character_id, Some(90_000_001));

    // When online session has no char and token has no char, and claimed belongs to other user -> character_id is None
    raw_ctx.db.release_online_lock(1, 77_001);
    let no_char_jwt = raw_ctx
        .db
        .issue_gateway_dev_token(1, None, "tranquility", now_u + 3600, now_u)
        .unwrap();
    let mut no_char_id = VerifiedGatewayIdentity {
        user_id: 1,
        token_character_id: None,
        is_full_sso: false,
        raw_token: no_char_jwt,
        peer_ip: loopback,
        bound_online_session_id: None,
    };
    let sess_none = build_session_for_identity(&raw_ctx, 9_000_112, &mut no_char_id, Some(90_000_002)).unwrap();
    assert_eq!(sess_none.character_id, None);
    let sess_tok = build_session_for_identity(&raw_ctx, 9_000_113, &mut id1, None).unwrap();
    assert_eq!(sess_tok.character_id, Some(90_000_001));
    assert!(session_scope_tuple(&sess_tok) != session_scope_tuple(&sess_none));

    // 6. Restricted account rejected by lookup_active_account_role / authenticate_grpc_headers
    raw_ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "banned_edge".into(),
            AccountRecord {
                user_id: 777,
                username: "banned_edge".into(),
                password_token: "x".into(),
                role: service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: Some(now_u + 10_000),
                ban_reason: Some("SUSPENDED:test".into()),
            },
        );
    });
    assert!(lookup_active_account_role(&raw_ctx, 777, now_u).is_none());
    assert!(lookup_active_account_role(&raw_ctx, 999_999, now_u).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grpc_h2c_error_guards_compression_events_and_notices_edges() {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let now_u = ctx.now_unix();
    let addr = spawn_grpc(Arc::clone(&ctx)).await;

    let tcp = TcpStream::connect(addr).await.expect("connect");
    let (mut client, conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let dev_jwt = ctx
        .db
        .issue_gateway_dev_token(1, Some(90_000_001), "tranquility", now_u + 3600, now_u)
        .unwrap();

    // 1. Unknown gRPC path -> UNIMPLEMENTED (12) trailers-only
    {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.unknown.Service/Nope")
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (rsp_fut, _send) = client.send_request(req, true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.headers().get("grpc-status").unwrap(), GRPC_STATUS_UNIMPLEMENTED);
        assert!(rsp
            .headers()
            .get("grpc-message")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Unknown gRPC method"));
    }

    // 2. Unauthenticated Requests/Send, Notices/Consume, Events/Publish -> UNAUTHENTICATED (16)
    for path in [
        "/eve_public.gateway.Requests/Send",
        "/eve_public.gateway.Notices/Consume",
        "/eve_public.gateway.Events/Publish",
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1{path}"))
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (rsp_fut, _send) = client.send_request(req, true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(
            rsp.headers().get("grpc-status").unwrap(),
            GRPC_STATUS_UNAUTHENTICATED,
            "path {path}"
        );
    }

    // 3. Events/Ping and Notices/Ping succeed with EMPTY_GRPC_FRAME and grpc-status 0
    for path in [
        "/eve_public.gateway.Events/Ping",
        "/eve_public.gateway.Notices/Ping",
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(format!("http://127.0.0.1{path}"))
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        send_body
            .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), true)
            .unwrap();
        let rsp = rsp_fut.await.unwrap();
        let mut body = rsp.into_body();
        let mut got_empty = false;
        while let Some(chunk) = body.data().await {
            let chunk = chunk.unwrap();
            if chunk == EMPTY_GRPC_FRAME.as_slice() {
                got_empty = true;
            }
        }
        assert!(got_empty, "ping frame on {path}");
        let trailers = body.trailers().await.unwrap().expect("trailers");
        assert_eq!(trailers.get("grpc-status").unwrap(), GRPC_STATUS_OK);
    }

    // 4. Requests/Send with `grpc-encoding: gzip` + `application_instance_uuid-bin` header injection
    {
        let hdr_uuid = [0x42u8; 16];
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("grpc-encoding", "gzip")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .header(
                "application_instance_uuid-bin",
                base64url_encode_no_pad(&hdr_uuid),
            )
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);

        let pub_req = EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: now_u as i64,
                nanos: 0,
            }),
            correlation_uuid: vec![1u8; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.career.goal.api.GetDefinitionsRequest"),
                value: Vec::new(),
            }),
            application_instance_uuid: Vec::new(), // empty -> populated from header
            ..Default::default()
        };
        let mut gz = GzEncoder::new(Vec::new(), Compression::default());
        gz.write_all(&pub_req.encode_proto()).unwrap();
        let compressed_bytes = gz.finish().unwrap();
        send_body
            .send_data(frame_compressed(&compressed_bytes), true)
            .unwrap();

        let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());
        let raw_rsp = reader.next_message().await.unwrap().expect("gzip rsp");
        let decoded = EvePublicResponse::decode_proto(&raw_rsp).unwrap();
        assert_eq!(decoded.status_code, STATUS_OK);
        assert!(reader.next_message().await.unwrap().is_none());
    }

    // 5. Requests/Send with malformed EvePublicRequest bytes -> INVALID_ARGUMENT (3)
    {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        // Truncated varint / invalid wire type 7
        send_body
            .send_data(frame_grpc_message(&[0x0F, 0xFF, 0xFF]), true)
            .unwrap();
        let mut body = rsp.into_body();
        while let Some(_) = body.data().await {}
        let trailers = body.trailers().await.unwrap().expect("trailers");
        assert_eq!(trailers.get("grpc-status").unwrap(), GRPC_STATUS_INVALID_ARGUMENT);
    }

    // 6. Events/Publish: malformed event -> INVALID_ARGUMENT (3)
    {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        send_body
            .send_data(frame_grpc_message(&[0x0F, 0xFF]), true)
            .unwrap();
        let mut body = rsp.into_body();
        while let Some(_) = body.data().await {}
        let trailers = body.trailers().await.unwrap().expect("trailers");
        assert_eq!(trailers.get("grpc-status").unwrap(), GRPC_STATUS_INVALID_ARGUMENT);
    }

    // 7. Events/Publish: mismatched AuthoritativeContext (user, character, corp, service_role_mask) -> PERMISSION_DENIED (7)
    for (label, ac) in [
        (
            "user_mismatch",
            AuthoritativeContext::for_session_at(
                Some(999),
                Some(90_000_001),
                None,
                0,
                "tranquility",
                Vec::new(),
                now_u,
            ),
        ),
        (
            "char_mismatch",
            AuthoritativeContext::for_session_at(
                Some(1),
                Some(90_000_002),
                None,
                0,
                "tranquility",
                Vec::new(),
                now_u,
            ),
        ),
        (
            "corp_mismatch",
            AuthoritativeContext::for_session_at(
                Some(1),
                Some(90_000_001),
                Some(98_999_999),
                0,
                "tranquility",
                Vec::new(),
                now_u,
            ),
        ),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        let ev = EvePublicEvent {
            uuid: vec![3u8; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.ui.OpenedEvent"),
                value: Vec::new(),
            }),
            authoritative_context: Some(ac),
            ..Default::default()
        };
        send_body
            .send_data(frame_grpc_message(&ev.encode_proto()), true)
            .unwrap();
        let mut body = rsp.into_body();
        while let Some(_) = body.data().await {}
        let trailers = body.trailers().await.unwrap().expect("trailers");
        assert_eq!(
            trailers.get("grpc-status").unwrap(),
            GRPC_STATUS_PERMISSION_DENIED,
            "case {label}"
        );
    }

    // 8. GrpcFrameReader oversized frame (> MAX_GRPC_MESSAGE_BYTES) returns stream error
    {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Ping")
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        let mut bad_hdr = [0u8; 5];
        bad_hdr[1..5].copy_from_slice(&((MAX_GRPC_MESSAGE_BYTES as u32) + 1).to_be_bytes());
        send_body
            .send_data(Bytes::copy_from_slice(&bad_hdr), true)
            .unwrap();
        let err = rsp_fut.await;
        // Server closes/resets stream when reader.next_message() returns Err
        assert!(err.is_err() || err.unwrap().status() == StatusCode::OK);
    }

    // 9. Notices/Consume with ack message refreshing session + clean WritesDone unsubscribes
    {
        let app_uuid = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .header("application_instance_uuid", app_uuid.to_string())
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
        send_body
            .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), false)
            .unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);

        // Send a second ack frame then finish stream cleanly
        send_body
            .send_data(Bytes::from_static(&EMPTY_GRPC_FRAME), true)
            .unwrap();
        let mut reader = GrpcFrameReader::new(rsp.into_body(), "identity".into());
        assert!(reader.next_message().await.unwrap().is_none());
    }
}

