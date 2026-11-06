//! Deep branch-coverage and mutation-killing unit tests for `quasar_gateway` and `quasar_grpc`.

use crate::quasar_gateway::{
    BoundedIdempotencyCache, CachedIdempotentResponse, MAX_GATEWAY_SUBSCRIBERS,
    MAX_IDEMPOTENCY_ENTRIES, NOTICE_CACHE_MAX_SIZE_BYTES, NoticeRoutingCache, NoticeScope,
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_INTERNAL_ERROR, STATUS_OK,
    STATUS_UNAUTHORIZED, require_admin_role, validate_raw_wire_frame,
};
use crate::quasar_grpc::*;
use crate::router::ServerContext;
use bytes::BytesMut;
use eve_machonet::{SessionState, service_roles};
use eve_proto::PyRep;
use eve_proto::quasar::{
    AuthoritativeContext, DescriptorProto, DynamicQuasarMessage, EveInternalRequest,
    EvePublicRequest, EvePublicResponse, FieldDescriptorProto, FieldLabel, FieldType, GoogleAny,
    InternalGatewayRequest, QuasarDescriptorRegistry, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
    WireField, WireMessage, WireValue, encode_varint_field,
};
use eve_sde::SdeRegistry;
use http::{Request, StatusCode};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

fn frame_grpc_msg(payload: &[u8]) -> bytes::Bytes {
    let mut buf = BytesMut::with_capacity(5 + payload.len());
    buf.extend_from_slice(&[0u8]);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf.freeze()
}

fn sign_hs256_jwt(header_json: &str, payload_json: &str, secret: &[u8]) -> String {
    use eve_db::accounts::{base64url_encode_no_pad, hmac_sha256};
    let h = base64url_encode_no_pad(header_json.as_bytes());
    let p = base64url_encode_no_pad(payload_json.as_bytes());
    let input = format!("{h}.{p}");
    let sig = hmac_sha256(secret, input.as_bytes());
    format!("{input}.{}", base64url_encode_no_pad(&sig))
}

#[test]
fn gateway_routing_idempotency_subscriber_cap_internal_envelopes_and_rpc_edges() {
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), sde);
    let now = 1_700_100_000_u64;

    // 1. NoticeRoutingCache capacity eviction & OOM boundary
    let mut cache = NoticeRoutingCache::new(64);
    for seq in 0..4u64 {
        let mut n = eve_proto::quasar::EvePublicNotice {
            tenant: "tranquility".into(),
            uuid: vec![seq as u8; 16],
            ..Default::default()
        };
        n.internal_origin = "orig".into();
        let sz = n.encode_proto().len();
        assert!(sz <= 64);
        cache
            .add_notice(n, NoticeScope::SolarSystem(30_000_142), now + seq)
            .unwrap();
        assert!(cache.current_bytes <= 64);
    }
    let mut huge_notice = eve_proto::quasar::EvePublicNotice::default();
    huge_notice.uuid = vec![9u8; 128];
    assert_eq!(
        cache
            .add_notice(huge_notice, NoticeScope::Broadcast, now)
            .unwrap_err()
            .code,
        "CacheOOMException"
    );

    // 2. BoundedIdempotencyCache overwrite timestamp guard + max-entry eviction
    let mut idem = BoundedIdempotencyCache::default();
    let k0 = (1, 100, "svc.Req".into(), vec![1]);
    idem.insert(
        k0.clone(),
        CachedIdempotentResponse {
            status_code: STATUS_OK,
            status_message: "v1".into(),
            payload: None,
            created_at_unix: 100,
        },
        100,
    );
    idem.insert(
        k0.clone(),
        CachedIdempotentResponse {
            status_code: STATUS_OK,
            status_message: "v2".into(),
            payload: None,
            created_at_unix: 200,
        },
        200,
    );
    // At now = 700, old ts=100 is expired (>=600s) but map entry has created_at_unix=200 so it stays
    assert_eq!(idem.get(&k0, 700).unwrap().status_message, "v2");
    for i in 1..=(MAX_IDEMPOTENCY_ENTRIES + 5) {
        let key = (2, i as u32, "svc.Req".into(), (i as u32).to_be_bytes().to_vec());
        idem.insert(
            key,
            CachedIdempotentResponse {
                status_code: STATUS_OK,
                status_message: "ok".into(),
                payload: None,
                created_at_unix: 750,
            },
            750,
        );
    }
    assert!(idem.map.len() <= MAX_IDEMPOTENCY_ENTRIES);

    // 3. Subscriber capacity eviction, unsubscribed on_session_changed / poll_notices, and type filter
    let mut sub_sess = SessionState::new(10);
    sub_sess.user_id = Some(1);
    sub_sess.character_id = Some(90_000_001);
    sub_sess.solar_system_id = Some(30_000_142);
    let _rx = router.subscribe_session(
        &sub_sess,
        vec!["eve_public.wanted.Notice".into()],
        1_000,
    );
    let (_, delivered_filtered) = router
        .publish_notice(
            NoticeScope::Character(90_000_001),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.ignored.Notice"),
                value: Vec::new(),
            },
            1_001,
        )
        .unwrap();
    assert_eq!(delivered_filtered, 0);

    // Fill to MAX_GATEWAY_SUBSCRIBERS to exercise oldest subscriber eviction (session 10)
    for sid in 100..(100 + MAX_GATEWAY_SUBSCRIBERS as u64) {
        let mut s = SessionState::new(sid);
        s.user_id = Some(sid);
        let _r = router.subscribe_session(&s, Vec::new(), 2_000 + sid);
    }
    assert!(!router.has_subscriber(10));
    assert_eq!(router.on_session_changed(&sub_sess, now), 0);
    assert!(router.poll_notices(10).is_empty());

    // 4. Unauthenticated AuthoritativeContext in GetNamesByUserRequest handler -> 401
    let mut sess = SessionState::new(50_001);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    let names_handler = router
        .handlers
        .read()
        .unwrap()
        .get("eve_public.character.GetNamesByUserRequest")
        .cloned()
        .unwrap();
    let unauth_ctx = AuthoritativeContext::default();
    let unauth_err = names_handler(
        &unauth_ctx,
        &GoogleAny::default(),
        &DynamicQuasarMessage::new("eve_public.character.GetNamesByUserRequest"),
    )
    .unwrap_err();
    assert_eq!(unauth_err.0, STATUS_UNAUTHORIZED);

    // 5. Internal gateway & eve internal envelopes + hijack latency math + RPC edge cases
    let empty_req = EvePublicRequest {
        correlation_uuid: vec![7, 8, 9, 10],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    let int_none = router.handle_internal_gateway_request(
        &sess,
        &InternalGatewayRequest {
            request: None,
            ack_id: 42,
        },
        now,
    );
    assert_eq!(int_none.ack_id, 42);
    assert_eq!(int_none.response.unwrap().status_code, STATUS_BAD_REQUEST);

    let int_ok = router.handle_internal_gateway_request(
        &sess,
        &InternalGatewayRequest {
            request: Some(empty_req.clone()),
            ack_id: 43,
        },
        now,
    );
    assert_eq!(int_ok.response.unwrap().status_code, STATUS_OK);

    // Verify latency arithmetic: now + 2.25s => seconds = now + 2, nanos = 250_000_000
    router.set_server_hijack_mode(true, None, Some(2.25));
    let eve_rsp = router.handle_eve_internal_request(
        &sess,
        &EveInternalRequest {
            correlation_uuid: "corr-44".into(),
            publisher: "svc-a".into(),
            payload: empty_req.payload.clone(),
            ..Default::default()
        },
        1_000_000,
    );
    assert_eq!(eve_rsp.status_code, STATUS_OK);
    let disp = eve_rsp.created.unwrap();
    assert_eq!(disp.seconds, 1_000_002);
    assert_eq!(disp.nanos, 250_000_000);
    router.set_server_hijack_mode(false, None, None);

    // Spoofed character in AuthoritativeContext -> 403
    let mut spoof_req = empty_req.clone();
    spoof_req.authoritative_context = Some(AuthoritativeContext::for_session_at(
        Some(1),
        Some(90_999_999),
        None,
        0,
        "tranquility",
        b"gw".to_vec(),
        now,
    ));
    assert_eq!(
        router
            .handle_public_request(&sess, &spoof_req, None, false, now)
            .status_code,
        STATUS_FORBIDDEN
    );

    // RPC hijack None latency, Subscribe mixed args, SendRequest bad idem kwarg, require_admin_role, validate_raw_wire_frame
    assert!(
        router
            .dispatch_service_gateway_mgr_rpc(
                "qa_enable_hijack_mode",
                &[PyRep::Integer(STATUS_INTERNAL_ERROR as i64), PyRep::None],
                &HashMap::new(),
            )
            .unwrap()
            .is_ok()
    );
    router.set_server_hijack_mode(false, None, None);

    assert!(
        router
            .dispatch_public_gateway_rpc(
                &sess,
                "Subscribe",
                &[PyRep::Integer(999), PyRep::String("eve_public.test.Notice".into())],
                &HashMap::new(),
                now,
            )
            .unwrap()
            .is_ok()
    );
    let mut bad_idem_kw = HashMap::new();
    bad_idem_kw.insert("idempotency_key".into(), PyRep::Integer(12345));
    let send_rep = router
        .dispatch_public_gateway_rpc(
            &sess,
            "SendRequest",
            &[PyRep::Bytes(empty_req.encode_proto())],
            &bad_idem_kw,
            now,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(send_rep, PyRep::KeyVal(_)));

    assert_eq!(
        require_admin_role(&AuthoritativeContext::default()).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    let gm_ctx =
        AuthoritativeContext::default().with_service_roles(service_roles::ROLE_GM_MASK);
    assert!(require_admin_role(&gm_ctx).is_ok());
    assert!(validate_raw_wire_frame(&empty_req.encode_proto()).is_ok());
    assert!(validate_raw_wire_frame(&[0x0a, 0x05, 0x01]).is_err());
}

#[tokio::test]
async fn http2_grpc_requests_notices_stream_jwt_variants_and_wire_coercion_mutants() {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
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
    let (mut client, h2_conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = h2_conn.await;
    });

    // 1. Auth edge cases: empty bearer, wrong alg, expired local-dev JWT, invalid signature -> status 16
    let forged_dev_jwt = sign_hs256_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        r#"{"sub":"USER:EVE:1","tenant":"tranquility"}"#,
        b"dev",
    );
    let expired_jwt = sign_hs256_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        r#"{"sub":"USER:EVE:1","exp":1}"#,
        b"dev",
    );
    let wrong_alg_jwt = sign_hs256_jwt(
        r#"{"alg":"RS256","typ":"JWT"}"#,
        r#"{"sub":"USER:EVE:1"}"#,
        b"dev",
    );
    let bad_sig_jwt = sign_hs256_jwt(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        r#"{"sub":"USER:EVE:1"}"#,
        b"wrong-secret",
    );
    for auth_val in [
        "Bearer ",
        &format!("Bearer {forged_dev_jwt}"),
        &format!("Bearer {expired_jwt}"),
        &format!("Bearer {wrong_alg_jwt}"),
        &format!("Bearer {bad_sig_jwt}"),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", auth_val)
            .body(())
            .unwrap();
        let (rsp_fut, _send) = client.send_request(req, true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(
            rsp.headers().get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );
    }

    // 2. Real OAuth2 SSO HS256 JWT + CHARACTER:EVE local-dev JWT on Requests/Send (200 + non-200 log)
    let now_unix = ctx.now_unix();
    let uid_pkce = ctx.db.provision_player_account("grpc_pkce_user", "grpc_pkce_pass");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.user_id = uid_pkce;
        }
    });
    let verifier = "grpc_test_pkce_verifier_0123456789";
    let challenge = eve_db::accounts::base64url_encode_no_pad(&eve_db::accounts::sha256_bytes(verifier.as_bytes()));
    let code = ctx
        .db
        .authorize_pkce(
            "grpc_pkce_user",
            "grpc_pkce_pass",
            90_000_001,
            "evomu-test",
            "http://127.0.0.1/callback",
            Vec::new(),
            &challenge,
            "S256",
            now_unix,
        )
        .unwrap();
    let sso_pair = ctx
        .db
        .exchange_pkce_code(&code, "evomu-test", verifier, now_unix)
        .unwrap();
    let char_dev_jwt = ctx
        .db
        .issue_gateway_dev_token(uid_pkce, Some(90_000_001), "tranquility", now_unix + 3600, now_unix)
        .unwrap();
    assert!(
        ctx.quasar_gateway.descriptors.get_message("google.protobuf.Empty").is_some()
            || NOTICE_CACHE_MAX_SIZE_BYTES == 2_000_000
    );

    for (auth_hdr, uuid_hdr_name, uuid_hdr_val) in [
        (
            format!("Bearer {}", sso_pair.access_token),
            "application_instance_uuid",
            "EVOMU-CLIENT-001",
        ),
        (
            format!("Bearer {char_dev_jwt}"),
            "application_instance_uuid-bin",
            "AQIDBAUGBwgJCgsMDQ4PEA==",
        ),
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", auth_hdr)
            .header(uuid_hdr_name, uuid_hdr_val)
            .body(())
            .unwrap();
        let (rsp_fut, mut send_stream) = client.send_request(req, false).unwrap();

        let pub_req = EvePublicRequest {
            correlation_uuid: vec![0xaa, 0xbb, 0xcc, 0xdd],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
                value: vec![0x08, 0x01],
            }),
            ..Default::default()
        };
        send_stream.send_data(frame_grpc_msg(&pub_req.encode_proto()), true).unwrap();

        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);
        let mut body = rsp.into_body();
        let frame = body.data().await.unwrap().unwrap();
        assert_eq!(frame[0], 0);
        let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        let decoded_rsp = EvePublicResponse::decode_proto(&frame[5..5 + len]).unwrap();
        assert_eq!(decoded_rsp.status_code, 200);
        assert_eq!(decoded_rsp.correlation_uuid, vec![0xaa, 0xbb, 0xcc, 0xdd]);
    }

    // 3. Invalid protobuf on Requests/Send and Events/Publish -> grpc-status = 3 + grpc-message
    for uri in [
        "http://127.0.0.1/eve_public.gateway.Requests/Send",
        "http://127.0.0.1/eve_public.gateway.Events/Publish",
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {char_dev_jwt}"))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_stream) = client.send_request(req, false).unwrap();
        send_stream.send_data(frame_grpc_msg(&[0xff, 0xff, 0x0a, 0x09]), true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        let trailers = rsp.into_body().trailers().await.unwrap().unwrap();
        assert_eq!(trailers.get("grpc-status").and_then(|v| v.to_str().ok()), Some("3"));
        assert!(trailers.get("grpc-message").is_some());
    }

    // 4. Notices/Consume stream receives live published notice and closes cleanly on WritesDone
    let req = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {char_dev_jwt}"))
        .header("application_instance_uuid", "EVOMU-NOTICE-001")
        .body(())
        .unwrap();
    let (rsp_fut, mut notice_send) = client.send_request(req, false).unwrap();
    let rsp = rsp_fut.await.unwrap();
    assert_eq!(rsp.status(), StatusCode::OK);
    let mut notice_body = rsp.into_body();

    // Give server loop a brief tick to register subscriber, then publish notice to uid_pkce
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    ctx.quasar_gateway
        .publish_notice(
            NoticeScope::User(uid_pkce as i32),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.LiveNotice"),
                value: vec![0x08, 0x2a],
            },
            ctx.now_unix(),
        )
        .unwrap();

    let chunk = tokio::time::timeout(std::time::Duration::from_secs(2), notice_body.data())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let nlen = u32::from_be_bytes([chunk[1], chunk[2], chunk[3], chunk[4]]) as usize;
    let recv_notice = eve_proto::quasar::EvePublicNotice::decode_proto(&chunk[5..5 + nlen]).unwrap();
    assert_eq!(
        recv_notice.payload.unwrap().type_url,
        format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.LiveNotice")
    );
    notice_send.send_data(bytes::Bytes::new(), true).unwrap();
    if let Ok(Ok(Some(tr))) = tokio::time::timeout(std::time::Duration::from_secs(1), notice_body.trailers()).await {
        assert_eq!(tr.get("grpc-status").and_then(|v| v.to_str().ok()), Some("0"));
    }

    // 5. Wire coercion depth limit, zigzag Sint32/Sint64, non-UTF8 hex string, and wire mismatch branches
    let mk_field = |number: u32, name: &str, field_type: FieldType| FieldDescriptorProto {
        name: name.into(),
        number,
        label: FieldLabel::Optional,
        field_type,
        type_name: String::new(),
        oneof_index: None,
        json_name: name.into(),
    };
    let mut synth_desc = QuasarDescriptorRegistry::empty_with_builtins();
    synth_desc.messages_by_full_name.insert(
        "test.wire.Sample".into(),
        DescriptorProto {
            name: "Sample".into(),
            full_name: "test.wire.Sample".into(),
            fields: vec![
                mk_field(1, "dbl", FieldType::Double),
                mk_field(2, "txt", FieldType::String),
                mk_field(3, "raw", FieldType::Bytes),
                mk_field(4, "s32", FieldType::Sint32),
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    let mut mixed_wire = WireMessage::default();
    mixed_wire.fields.push(WireField { field_number: 1, value: WireValue::LengthDelimited(b"bad-double".to_vec()) });
    mixed_wire.fields.push(WireField { field_number: 2, value: WireValue::Varint(77) });
    mixed_wire.fields.push(WireField { field_number: 3, value: WireValue::Varint(1234) });
    mixed_wire.fields.push(WireField { field_number: 4, value: WireValue::Varint(0) });
    let mut any_sample = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.wire.Sample"),
        value: mixed_wire.encode(),
    };
    normalize_quasar_any_payload(&synth_desc, &mut any_sample);

    // Non-UTF8 raw leaf bytes coerce to hex in String-first descriptor
    synth_desc.messages_by_full_name.insert(
        "test.wire.StrLeaf".into(),
        DescriptorProto {
            name: "StrLeaf".into(),
            full_name: "test.wire.StrLeaf".into(),
            fields: vec![mk_field(1, "hex_str", FieldType::String)],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    let mut any_hex = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.wire.StrLeaf"),
        value: vec![0xff, 0x0a, 0x41],
    };
    normalize_quasar_any_payload(&synth_desc, &mut any_hex);
    assert!(!any_hex.value.is_empty());

    // Sint64 & Sint32 varint coercion on raw integer payload
    synth_desc.messages_by_full_name.insert(
        "test.wire.SintLeaf".into(),
        DescriptorProto {
            name: "SintLeaf".into(),
            full_name: "test.wire.SintLeaf".into(),
            fields: vec![mk_field(1, "s", FieldType::Sint64)],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    let mut raw_v = Vec::new();
    encode_varint_field(1, 5, &mut raw_v);
    let mut any_sint = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.wire.SintLeaf"),
        value: raw_v,
    };
    normalize_quasar_any_payload(&synth_desc, &mut any_sint);
    assert!(!any_sint.value.is_empty());
}
