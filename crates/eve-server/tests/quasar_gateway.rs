//! Comprehensive integration tests for `Q-2` (V24.01 Quasar `publicGatewaySvc` / `serviceGatewayMgr`
//! request/response framing, `correlation_uuid`, `authoritative_context`, `idempotency_key` deduplication,
//! HTTP/Quasar status codes `200`/`400`/`401`/`403`/`404`/`501`, `NoticeRoutingCache`, and multi-client
//! `Notice` fan-out across `character`, `corporation`, `alliance`, `solar_system`, `fleet`, `user`,
//! `application_instance_uuid`, `bubble_instance_uuid`, and `broadcast`/`all` scopes).

use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicValue, EveIdempotencyToken, EveInternalRequest,
    EvePublicCharacterId, EvePublicEvent, EvePublicRequest, EvePublicResponse, EvePublicUserId,
    GoogleAny, GoogleEmpty, GoogleTimestamp, InternalGatewayRequest, NoticeTargetGroup,
    QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::quasar_gateway::{
    NoticeScope, MAX_PUBLISHED_EVENTS, MAX_SUBSCRIBER_INBOX_LEN, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn extract_response_proto(rep: &PyRep) -> EvePublicResponse {
    if let PyRep::KeyVal(entries) = rep {
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    return EvePublicResponse::decode_proto(b)
                        .expect("decode EvePublicResponse from response_proto");
                }
            }
        }
    }
    panic!("Expected KeyVal containing response_proto, got {rep:?}");
}

// parity: plan:Q-2
// parity: feat:quasar.gateway-envelope
#[tokio::test]
async fn q2_gateway_envelope_framing_status_codes_tenancy_and_idempotency() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // 1. Unauthenticated session -> 401 STATUS_UNAUTHORIZED
    let mut unauth_sess = SessionState::new(1_001);
    let ping_req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_000,
            nanos: 0,
        }),
        correlation_uuid: b"corr-unauth-0001".to_vec(),
        payload: Some(GoogleEmpty.to_any()),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-unauth-00001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let unauth_rep = ctx
        .dispatch_rpc(
            &mut unauth_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ping_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest returns framed EvePublicResponse");
    let unauth_rsp = extract_response_proto(&unauth_rep);
    assert_eq!(unauth_rsp.status_code, STATUS_UNAUTHORIZED);
    assert_eq!(unauth_rsp.correlation_uuid, b"corr-unauth-0001");

    // 2. Authenticated user WITHOUT character:
    //    - Character-scoped request (`eve_public.goal.api.GetRequest`) is rejected with 403 STATUS_FORBIDDEN
    //      even if client attempts to pass `require_character=false` in kwargs (server-enforced policy).
    //    - User-scoped pre-character request (`eve_public.character.GetNamesByUserRequest`) is allowed (200 OK).
    let mut user_only_sess = SessionState::new(1_002);
    user_only_sess.user_id = Some(1);
    let mut bypass_attempt_kw = HashMap::new();
    bypass_attempt_kw.insert("require_character".into(), PyRep::Bool(false));
    let mut char_scoped_req = ping_req.clone();
    char_scoped_req.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetRequest"),
        value: Vec::new(),
    });
    let no_char_rep = ctx
        .dispatch_rpc(
            &mut user_only_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(char_scoped_req.encode_proto())],
            &bypass_attempt_kw,
            None,
        )
        .await
        .expect("SendRequest no-char");
    let no_char_rsp = extract_response_proto(&no_char_rep);
    assert_eq!(no_char_rsp.status_code, STATUS_FORBIDDEN);

    let mut prechar_names_req = ping_req.clone();
    prechar_names_req.correlation_uuid = b"corr-prechar-001".to_vec();
    prechar_names_req.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
        value: Vec::new(),
    });
    let prechar_rep = ctx
        .dispatch_rpc(
            &mut user_only_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(prechar_names_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest prechar GetNamesByUserRequest");
    let prechar_rsp = extract_response_proto(&prechar_rep);
    assert_eq!(prechar_rsp.status_code, STATUS_OK);

    // 3. Authenticated user with character spoofing another user/character/corporation in AuthoritativeContext -> 403 STATUS_FORBIDDEN
    let mut alice_sess = SessionState::new(1_003);
    alice_sess.user_id = Some(1);
    alice_sess.character_id = Some(90_000_001);
    alice_sess.corporation_id = Some(98_000_001);
    alice_sess.solar_system_id = Some(30_000_142);

    let mut spoof_user_req = ping_req.clone();
    spoof_user_req.authoritative_context = Some(AuthoritativeContext {
        authenticated_user: Some(EvePublicUserId { sequential: 999 }),
        ..Default::default()
    });
    let spoof_user_rsp = ctx.quasar_gateway.handle_public_request(
        &alice_sess,
        &spoof_user_req,
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(spoof_user_rsp.status_code, STATUS_FORBIDDEN);

    let mut spoof_char_req = ping_req.clone();
    spoof_char_req.authoritative_context = Some(AuthoritativeContext {
        active_character: Some(EvePublicCharacterId {
            sequential: 90_000_002,
        }),
        ..Default::default()
    });
    let spoof_char_rsp = ctx.quasar_gateway.handle_public_request(
        &alice_sess,
        &spoof_char_req,
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(spoof_char_rsp.status_code, STATUS_FORBIDDEN);

    let mut spoof_corp_req = ping_req.clone();
    spoof_corp_req.authoritative_context = Some(AuthoritativeContext::for_session_at(
        Some(1),
        Some(90_000_001),
        Some(98_999_999),
        0,
        "tranquility",
        b"gw".to_vec(),
        ctx.now_unix(),
    ));
    let spoof_corp_rsp = ctx.quasar_gateway.handle_public_request(
        &alice_sess,
        &spoof_corp_req,
        None,
        true,
        ctx.now_unix(),
    );
    assert_eq!(spoof_corp_rsp.status_code, STATUS_FORBIDDEN);

    // 4. Missing payload -> 400 STATUS_BAD_REQUEST; unknown message type -> 404 STATUS_NOT_FOUND;
    //    malformed payload wire -> 400 STATUS_BAD_REQUEST; valid type with no handler -> 501 STATUS_NOT_IMPLEMENTED
    let mut missing_payload_req = ping_req.clone();
    missing_payload_req.payload = None;
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&alice_sess, &missing_payload_req, None, true, ctx.now_unix())
            .status_code,
        STATUS_BAD_REQUEST
    );

    let mut unknown_type_req = ping_req.clone();
    unknown_type_req.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.nonexistent.BogusRequest"),
        value: Vec::new(),
    });
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&alice_sess, &unknown_type_req, None, true, ctx.now_unix())
            .status_code,
        STATUS_NOT_FOUND
    );

    let mut bad_wire_req = ping_req.clone();
    bad_wire_req.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
        value: vec![0x0A, 0xFF, 0x01], // truncated length-delimited field
    });
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&alice_sess, &bad_wire_req, None, true, ctx.now_unix())
            .status_code,
        STATUS_BAD_REQUEST
    );

    let mut unimpl_type_req = ping_req.clone();
    unimpl_type_req.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetRequest"),
        value: Vec::new(),
    });
    assert_eq!(
        ctx.quasar_gateway
            .handle_public_request(&alice_sess, &unimpl_type_req, None, true, ctx.now_unix())
            .status_code,
        STATUS_NOT_IMPLEMENTED
    );

    // 5. Valid 200 OK handler + correlation_uuid preservation + live character DB query
    let mut names_req = ping_req.clone();
    names_req.correlation_uuid = b"corr-names-00001".to_vec();
    names_req.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
        value: Vec::new(),
    });
    let names_rep = ctx
        .dispatch_rpc(
            &mut alice_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(names_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("SendRequest GetNamesByUserRequest");
    let names_rsp = extract_response_proto(&names_rep);
    assert_eq!(names_rsp.status_code, STATUS_OK);
    assert_eq!(names_rsp.correlation_uuid, b"corr-names-00001");
    let rsp_any = names_rsp.payload.expect("response payload");
    assert_eq!(
        rsp_any.normalized_type_name(),
        "eve_public.character.GetNamesByUserResponse"
    );
    let dyn_names = ctx
        .quasar_gateway
        .descriptors
        .decode_dynamic("eve_public.character.GetNamesByUserResponse", &rsp_any.value)
        .expect("decode GetNamesByUserResponse");
    assert!(matches!(dyn_names.get(1), Some(DynamicValue::Repeated(v)) if !v.is_empty()));

    // 6. Idempotency key deduplication: handler executes once when re-sent with identical idempotency_key
    let call_counter = Arc::new(AtomicUsize::new(0));
    let counter_clone = Arc::clone(&call_counter);
    ctx.quasar_gateway.register_handler(
        "eve.accounting.GetIskInWalletRequest",
        Arc::new(move |_auth_ctx, _any, _dyn_msg| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst) + 1;
            let mut val = Vec::new();
            eve_proto::quasar::encode_varint_field(2, n as u64, &mut val);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.GetIskInWalletResponse"),
                value: val,
            })
        }),
    );

    let reserve_dyn = ctx
        .quasar_gateway
        .descriptors
        .instantiate_populated_sample("eve.accounting.GetIskInWalletRequest", 7)
        .unwrap();
    let idem_token = EveIdempotencyToken {
        uuid: b"idem-key-reserve1".to_vec(),
    };
    assert_eq!(
        EveIdempotencyToken::decode_proto(&idem_token.encode_proto()).unwrap(),
        idem_token
    );

    let mut idem_kw = HashMap::new();
    idem_kw.insert(
        "idempotency_key".into(),
        PyRep::Bytes(b"idem-key-reserve1".to_vec()),
    );
    let mut req1 = ping_req.clone();
    req1.correlation_uuid = b"corr-idem-call-1".to_vec();
    req1.payload = Some(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.accounting.GetIskInWalletRequest"),
        value: ctx
            .quasar_gateway
            .descriptors
            .encode_dynamic(&reserve_dyn)
            .unwrap(),
    });
    let mut req2 = req1.clone();
    req2.correlation_uuid = b"corr-idem-call-2".to_vec();

    let rep1 = ctx
        .dispatch_rpc(
            &mut alice_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req1.encode_proto())],
            &idem_kw,
            None,
        )
        .await
        .unwrap();
    let rep2 = ctx
        .dispatch_rpc(
            &mut alice_sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(req2.encode_proto())],
            &idem_kw,
            None,
        )
        .await
        .unwrap();
    let rsp1 = extract_response_proto(&rep1);
    let rsp2 = extract_response_proto(&rep2);
    assert_eq!(call_counter.load(Ordering::SeqCst), 1);
    assert_eq!(rsp1.status_code, STATUS_OK);
    assert_eq!(rsp2.status_code, STATUS_OK);
    assert_eq!(rsp1.payload, rsp2.payload);
    assert_eq!(rsp1.correlation_uuid, b"corr-idem-call-1");
    assert_eq!(rsp2.correlation_uuid, b"corr-idem-call-2");
}

#[tokio::test]
async fn q2_service_gateway_mgr_hijack_mode_and_internal_gateway_requests() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // Non-GM/QA session cannot invoke serviceGatewayMgr.qa_enable_hijack_mode
    let mut player_sess = SessionState::new(2_001);
    player_sess.user_id = Some(1);
    player_sess.character_id = Some(90_000_001);
    let denied = ctx
        .dispatch_rpc(
            &mut player_sess,
            "serviceGatewayMgr",
            None,
            "qa_enable_hijack_mode",
            &[PyRep::Integer(503)],
            &empty_kw,
            None,
        )
        .await;
    assert_eq!(denied.unwrap_err().code, "InsufficientRole");

    // QA/GM session enables hijack mode with error 503
    let mut qa_sess = SessionState::new(2_002);
    qa_sess.user_id = Some(1);
    qa_sess.character_id = Some(90_000_001);
    qa_sess.role = service_roles::ROLE_QA | service_roles::ROLE_GML;

    // Invalid error code or negative latency is rejected with InvalidArgument
    let bad_err = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "serviceGatewayMgr",
            None,
            "qa_enable_hijack_mode",
            &[PyRep::Integer(999), PyRep::Real(0.1)],
            &empty_kw,
            None,
        )
        .await;
    assert_eq!(bad_err.unwrap_err().code, "InvalidArgument");

    let bad_lat = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "serviceGatewayMgr",
            None,
            "qa_enable_hijack_mode",
            &[PyRep::Integer(500), PyRep::Real(-1.0)],
            &empty_kw,
            None,
        )
        .await;
    assert_eq!(bad_lat.unwrap_err().code, "InvalidArgument");

    let enabled = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "serviceGatewayMgr",
            None,
            "qa_enable_hijack_mode",
            &[PyRep::Integer(503), PyRep::Real(0.25)],
            &empty_kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(enabled, PyRep::None);

    let is_enabled = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "serviceGatewayMgr",
            None,
            "qa_is_hijack_mode_enabled",
            &[],
            &empty_kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(is_enabled, PyRep::Bool(true));

    // While server hijack mode is enabled with error=503 and latency=0.25, gateway requests return status_code 503
    // with dispatched timestamp offset by 0.25s (250_000_000 nanos).
    let ping_req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_010,
            nanos: 0,
        }),
        correlation_uuid: b"corr-hijack-503".to_vec(),
        payload: Some(GoogleEmpty.to_any()),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-hijack-0001".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    let hijacked_rsp =
        ctx.quasar_gateway
            .handle_public_request(&player_sess, &ping_req, None, false, ctx.now_unix());
    assert_eq!(hijacked_rsp.status_code, 503);
    assert_eq!(hijacked_rsp.status_message, "Error: 503");
    assert_eq!(hijacked_rsp.dispatched.unwrap().nanos, 250_000_000);

    // V24 string error option "TimeoutException" maps to STATUS_GATEWAY_TIMEOUT (504)
    ctx.dispatch_rpc(
        &mut qa_sess,
        "serviceGatewayMgr",
        None,
        "qa_enable_hijack_mode",
        &[PyRep::String("TimeoutException".into()), PyRep::None],
        &empty_kw,
        None,
    )
    .await
    .unwrap();
    let timeout_rsp =
        ctx.quasar_gateway
            .handle_public_request(&player_sess, &ping_req, None, false, ctx.now_unix());
    assert_eq!(timeout_rsp.status_code, 504);

    // Disable server hijack mode
    let disabled = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "serviceGatewayMgr",
            None,
            "qa_disable_hijack_mode",
            &[],
            &empty_kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(disabled, PyRep::None);

    let is_enabled_after = ctx
        .dispatch_rpc(
            &mut qa_sess,
            "serviceGatewayMgr",
            None,
            "qa_is_hijack_mode_enabled",
            &[],
            &empty_kw,
            None,
        )
        .await
        .unwrap();
    assert_eq!(is_enabled_after, PyRep::Bool(false));

    let ok_rsp =
        ctx.quasar_gateway
            .handle_public_request(&player_sess, &ping_req, None, false, ctx.now_unix());
    assert_eq!(ok_rsp.status_code, STATUS_OK);

    // Client sessions MUST NOT be able to invoke internal gateway RPCs (`HandleInternalRequest`)
    // or server-only notice publication (`PublishNotice`) over `publicGatewaySvc`.
    let int_req = InternalGatewayRequest {
        ack_id: 777,
        request: Some(ping_req),
    };
    let client_int_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "publicGatewaySvc",
            None,
            "HandleInternalRequest",
            &[PyRep::Bytes(int_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(client_int_err.code, "UnimplementedRPC");

    let client_pub_notice_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "publicGatewaySvc",
            None,
            "PublishNotice",
            &[PyRep::String("broadcast".into())],
            &empty_kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(client_pub_notice_err.code, "UnimplementedRPC");

    // Server-side internal gateway methods (`handle_internal_gateway_request` / `handle_eve_internal_request`)
    // remain available to trusted server subsystems.
    let int_rsp = ctx
        .quasar_gateway
        .handle_internal_gateway_request(&player_sess, &int_req, ctx.now_unix());
    assert_eq!(int_rsp.ack_id, 777);
    assert_eq!(int_rsp.response.unwrap().status_code, STATUS_OK);

    let eve_req = EveInternalRequest {
        correlation_uuid: "eve-int-corr-1".into(),
        payload: Some(GoogleEmpty.to_any()),
        tenant: "tranquility".into(),
        publisher: "internal.svc".into(),
        ..Default::default()
    };
    let eve_rsp =
        ctx.quasar_gateway
            .handle_eve_internal_request(&player_sess, &eve_req, ctx.now_unix());
    assert_eq!(eve_rsp.status_code, STATUS_OK);
    assert_eq!(eve_rsp.correlation_uuid, "eve-int-corr-1");

    // Backend-only `ConsumeEventsRequest` is NOT exposed on the client-facing publicGatewaySvc path (returns 501).
    for req_type in [
        "eve_public.internal_gateway.ConsumeEventsRequest",
        "eve.gateway.ConsumeEventsRequest",
    ] {
        let env = EvePublicRequest {
            correlation_uuid: b"corr-consume-ev1".to_vec(),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{req_type}"),
                value: Vec::new(),
            }),
            ..Default::default()
        };
        let rsp =
            ctx.quasar_gateway
                .handle_public_request(&player_sess, &env, None, true, ctx.now_unix());
        assert_eq!(
            rsp.status_code, STATUS_NOT_IMPLEMENTED,
            "Client-facing publicGatewaySvc must not expose backend consumer {req_type}"
        );
    }
}

#[tokio::test]
async fn q2_scoped_notice_fan_out_and_notice_routing_cache_flush_on_session_change() {
    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // Subscribe 3 distinct client sessions with different character / corp / alliance / solar_system / fleet scopes
    let mut sess_a = SessionState::new(3_001);
    sess_a.user_id = Some(10);
    sess_a.character_id = Some(90_000_001);
    sess_a.corporation_id = Some(98_000_001);
    sess_a.alliance_id = Some(99_000_001);
    sess_a.solar_system_id = Some(30_000_142); // Jita
    sess_a.fleet_id = Some(700_001);

    let mut sess_b = SessionState::new(3_002);
    sess_b.user_id = Some(20);
    sess_b.character_id = Some(90_000_002);
    sess_b.corporation_id = Some(98_000_001); // Same corp as A
    sess_b.alliance_id = Some(99_000_001); // Same alliance as A
    sess_b.solar_system_id = Some(30_002_187); // Amarr (different system)
    sess_b.fleet_id = Some(700_001); // Same fleet as A

    let mut sess_c = SessionState::new(3_003);
    sess_c.user_id = Some(30);
    sess_c.character_id = Some(90_000_003);
    sess_c.corporation_id = Some(98_000_002); // Different corp
    sess_c.alliance_id = Some(99_000_002); // Different alliance
    sess_c.solar_system_id = Some(30_000_142); // Jita (same system as A)
    sess_c.fleet_id = None;

    for s in [&mut sess_a, &mut sess_b, &mut sess_c] {
        ctx.dispatch_rpc(s, "publicGatewaySvc", None, "Subscribe", &[], &empty_kw, None)
            .await
            .expect("Subscribe session to publicGatewaySvc");
    }
    sess_b.solar_system_id = Some(30_002_187);
    ctx.quasar_gateway
        .on_session_changed(&sess_b, ctx.now_unix());

    let mut stream_rx_a = ctx
        .quasar_gateway
        .take_stream_receiver(sess_a.session_id)
        .expect("Subscribe must retain live mpsc::Receiver stream");

    let sample_notice_payload = EvePublicCharacterId {
        sequential: 90_000_001,
    }
    .to_any();

    // 1. Character-scoped notice to 90_000_001 -> only sess_a receives it (on both stream_rx and inbox)
    let (_, delivered_char) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Character(90_000_001),
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(delivered_char, 1);
    let pushed_a = stream_rx_a.try_recv().expect("notice pushed over live mpsc stream");
    assert_eq!(
        pushed_a.target_group,
        Some(NoticeTargetGroup::Character(90_000_001))
    );
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_a.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_b.session_id).len(), 0);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_c.session_id).len(), 0);

    // 2. Corporation-scoped notice to 98_000_001 -> sess_a and sess_b receive it, sess_c does not
    let (_, delivered_corp) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Corporation(98_000_001),
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(delivered_corp, 2);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_a.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_b.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_c.session_id).len(), 0);

    // 2b. Alliance-scoped notice to 99_000_001 -> sess_a and sess_b receive it, sess_c does not
    let (alliance_notice, delivered_all) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Alliance(99_000_001),
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(delivered_all, 2);
    assert_eq!(
        alliance_notice.target_group,
        Some(NoticeTargetGroup::Alliance(99_000_001))
    );
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_a.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_b.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_c.session_id).len(), 0);

    // 3. SolarSystem-scoped notice to Jita (30_000_142) -> sess_a and sess_c receive it, sess_b does not
    let (_, delivered_sys) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::SolarSystem(30_000_142),
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(delivered_sys, 2);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_a.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_b.session_id).len(), 0);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_c.session_id).len(), 1);

    // 4. Fleet-scoped notice to 700_001 -> sess_a and sess_b receive it, sess_c does not;
    //    target_group is None because eve_public.Notice.target_group has no fleet variant.
    let (fleet_notice, delivered_fleet) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Fleet(700_001),
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(delivered_fleet, 2);
    assert_eq!(fleet_notice.target_group, None);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_a.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_b.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_c.session_id).len(), 0);

    // 5. Server-side Broadcast notice -> all 3 sessions receive it, target_group is None
    let (bcast_notice, delivered_bcast) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::Broadcast,
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(delivered_bcast, 3);
    assert_eq!(bcast_notice.tenant, "tranquility");
    assert_eq!(bcast_notice.target_group, None);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_a.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_b.session_id).len(), 1);
    assert_eq!(ctx.quasar_gateway.poll_notices(sess_c.session_id).len(), 1);

    // 6. NoticeRoutingCache: publish a notice to solar system 30_002_510 (Rens) before any subscriber is in Rens,
    //    then move sess_c into 30_002_510 and verify NoticeRoutingCache flushes the cached notice on session change!
    let (cached_notice, initial_delivered) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::SolarSystem(30_002_510),
            sample_notice_payload.clone(),
            ctx.now_unix(),
        )
        .unwrap();
    assert_eq!(initial_delivered, 0);
    assert_eq!(
        cached_notice.target_group,
        Some(NoticeTargetGroup::SolarSystem(30_002_510))
    );

    sess_c.solar_system_id = Some(30_002_510);
    let flushed = ctx
        .quasar_gateway
        .on_session_changed(&sess_c, ctx.now_unix());
    assert_eq!(flushed, 1);
    let polled_c = ctx.quasar_gateway.poll_notices(sess_c.session_id);
    assert_eq!(polled_c.len(), 1);
    assert_eq!(
        polled_c[0].target_group,
        Some(NoticeTargetGroup::SolarSystem(30_002_510))
    );

    // 7. Bounded per-subscriber inbox and bounded published_events ring buffer prevent memory exhaustion
    for _ in 0..(MAX_SUBSCRIBER_INBOX_LEN + 32) {
        ctx.quasar_gateway
            .publish_notice(
                NoticeScope::Character(90_000_001),
                sample_notice_payload.clone(),
                ctx.now_unix(),
            )
            .unwrap();
    }
    let bounded_polled = ctx.quasar_gateway.poll_notices(sess_a.session_id);
    assert_eq!(bounded_polled.len(), MAX_SUBSCRIBER_INBOX_LEN);

    for idx in 0..(MAX_PUBLISHED_EVENTS + 16) {
        let ev = EvePublicEvent {
            uuid: format!("client-ev-{idx:06}").into_bytes(),
            payload: Some(GoogleEmpty.to_any()),
            external_origin: "eve-client".into(),
            ..Default::default()
        };
        let ev_rep = ctx
            .dispatch_rpc(
                &mut sess_a,
                "publicGatewaySvc",
                None,
                "PublishEvent",
                &[PyRep::Bytes(ev.encode_proto())],
                &empty_kw,
                None,
            )
            .await
            .unwrap();
        assert_eq!(ev_rep, PyRep::Bool(true));
    }
    let events = ctx.quasar_gateway.published_events.read().unwrap();
    assert_eq!(events.len(), MAX_PUBLISHED_EVENTS);
    assert_eq!(
        events
            .back()
            .and_then(|e| e.authoritative_context.as_ref())
            .and_then(|c| c.effective_character_id()),
        Some(90_000_001)
    );
}
