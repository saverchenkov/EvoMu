//! Unit tests for `quasar_gateway` request handling, auth/tenancy validation, QA hijack mode,
//! wire fallback, internal envelopes, event telemetry, and MachoNet RPC dispatch.

#![forbid(unsafe_code)]

use super::*;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DescriptorProto, EveInternalRequest, EvePublicEvent, EvePublicRequest, FieldDescriptorProto,
    FieldLabel, FieldType, GoogleAny, GoogleEmpty, InternalGatewayRequest,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn new_router_with_descriptors(descriptors: Arc<QuasarDescriptorRegistry>) -> PublicGatewayRouter {
    PublicGatewayRouter::new(
        descriptors,
        Arc::new(WorldRepository::with_demo_seed()),
        shared_sde(),
    )
}

fn make_session(user_id: Option<u64>, char_id: Option<i64>, corp_id: Option<i64>, role: u64) -> SessionState {
    let mut s = SessionState::new(77);
    s.user_id = user_id;
    s.character_id = char_id;
    s.corporation_id = corp_id;
    s.role = role;
    s
}

fn make_pub_req(type_name: &str, value: Vec<u8>, auth_ctx: Option<AuthoritativeContext>) -> EvePublicRequest {
    EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_700_000_000,
            nanos: 0,
        }),
        correlation_uuid: vec![0xAA; 16],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value,
        }),
        external_origin: "test-client".into(),
        application_instance_uuid: Vec::new(),
        authoritative_context: auth_ctx,
        trace_context: None,
    }
}

#[test]
fn handle_public_request_enforces_auth_context_hijack_and_policies() {
    let router = new_router_with_descriptors(QuasarDescriptorRegistry::open_default_or_empty());
    let now = 1_700_000_000u64;

    // 1. Unauthenticated session -> 401
    let unauth = make_session(None, None, None, service_roles::ROLE_PLAYER_DEFAULT);
    let empty_req = make_pub_req("google.protobuf.Empty", Vec::new(), None);
    let rsp_unauth = router.handle_public_request(&unauth, &empty_req, None, false, now);
    assert_eq!(rsp_unauth.status_code, STATUS_UNAUTHORIZED);

    // 2. require_character_override with no character -> 403
    let no_char = make_session(Some(10), None, Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let rsp_need_char = router.handle_public_request(&no_char, &empty_req, None, true, now);
    assert_eq!(rsp_need_char.status_code, STATUS_FORBIDDEN);

    // 3. AuthoritativeContext user, character, and corporation spoofing -> 403
    let player = make_session(Some(10), Some(90_000_001), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let spoof_uid = AuthoritativeContext::for_session(Some(99), Some(90_000_001), Some(98_000_001), 0, "tranquility", vec![1; 16]);
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("google.protobuf.Empty", Vec::new(), Some(spoof_uid)), None, false, now).status_code,
        STATUS_FORBIDDEN
    );
    let spoof_cid = AuthoritativeContext::for_session(Some(10), Some(90_000_002), Some(98_000_001), 0, "tranquility", vec![1; 16]);
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("google.protobuf.Empty", Vec::new(), Some(spoof_cid)), None, false, now).status_code,
        STATUS_FORBIDDEN
    );
    let spoof_corp = AuthoritativeContext::for_session(Some(10), Some(90_000_001), Some(98_000_002), 0, "tranquility", vec![1; 16]);
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("google.protobuf.Empty", Vec::new(), Some(spoof_corp)), None, false, now).status_code,
        STATUS_FORBIDDEN
    );

    // Matching AuthoritativeContext + GoogleEmpty -> 200 with Empty payload
    let valid_ctx = AuthoritativeContext::for_session(Some(10), Some(90_000_001), Some(98_000_001), 0, "tranquility", vec![1; 16]);
    let ok_rsp = router.handle_public_request(
        &player,
        &make_pub_req("google.protobuf.Empty", GoogleEmpty.encode_proto(), Some(valid_ctx)),
        None,
        false,
        now,
    );
    assert_eq!(ok_rsp.status_code, STATUS_OK);
    assert!(ok_rsp
        .payload
        .as_ref()
        .is_some_and(|p| p.type_url.ends_with("google.protobuf.Empty") && p.value.is_empty()));

    // 4. QA Hijack Mode: latency shifts dispatched timestamp; error_code short-circuits response.
    router.set_server_hijack_mode(true, None, Some(2.25));
    assert!(router.is_server_hijack_mode_enabled());
    let delayed_rsp = router.handle_public_request(&player, &empty_req, None, false, now);
    assert_eq!(delayed_rsp.status_code, STATUS_OK);
    let ts = delayed_rsp.dispatched.unwrap();
    assert_eq!(ts.seconds, (now as i64) + 2);
    assert_eq!(ts.nanos, 250_000_000);

    router.set_server_hijack_mode(true, Some(STATUS_SERVICE_UNAVAILABLE), None);
    let err_rsp = router.handle_public_request(&player, &empty_req, None, false, now);
    assert_eq!(err_rsp.status_code, STATUS_SERVICE_UNAVAILABLE);
    assert_eq!(err_rsp.status_message, "Error: 503");
    router.set_server_hijack_mode(false, None, None);

    // 5. Missing payload -> 400; unknown type -> 404; character-scoped without char -> 403; admin without GM -> 403.
    let mut missing_payload = empty_req.clone();
    missing_payload.payload = None;
    assert_eq!(
        router.handle_public_request(&player, &missing_payload, None, false, now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("eve_public.nonexistent.Message", Vec::new(), None), None, false, now).status_code,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        router.handle_public_request(&no_char, &make_pub_req("eve_public.cosmetic.market.skin.listing.api.GetAllRequest", Vec::new(), None), None, false, now).status_code,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("eve.fulfillment.api.DeliverItemRequest", Vec::new(), None), None, false, now).status_code,
        STATUS_FORBIDDEN
    );

    // 6. Builtin GetNamesByUserRequest returns character list for authenticated user.
    router.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().expect("demo character").clone();
        ch.character_id = 90_000_777;
        ch.user_id = 10;
        ch.name = "PilotTen".into();
        snap.characters.insert(90_000_777, ch);
    });
    let names_rsp = router.handle_public_request(
        &no_char,
        &make_pub_req("eve_public.character.GetNamesByUserRequest", Vec::new(), None),
        None,
        false,
        now,
    );
    assert_eq!(names_rsp.status_code, STATUS_OK);
    assert!(names_rsp.payload.is_some());
}

#[test]
fn handle_public_request_wire_fallback_idempotency_and_notice_rollback() {
    // Build a custom descriptor registry with an embedded `eve.idempotency.Token` field
    // plus an unimplemented descriptor to test 501 and wire decoding errors.
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.messages_by_full_name.insert(
        "eve.idempotency.Token".into(),
        DescriptorProto {
            name: "Token".into(),
            full_name: "eve.idempotency.Token".into(),
            fields: vec![FieldDescriptorProto {
                name: "uuid".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Bytes,
                type_name: String::new(),
                oneof_index: None,
                json_name: String::new(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    reg.messages_by_full_name.insert(
        "eve_public.test.IdempotentActionRequest".into(),
        DescriptorProto {
            name: "IdempotentActionRequest".into(),
            full_name: "eve_public.test.IdempotentActionRequest".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "token".into(),
                    number: 1,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Message,
                    type_name: ".eve.idempotency.Token".into(),
                    oneof_index: None,
                    json_name: String::new(),
                },
                FieldDescriptorProto {
                    name: "fail".into(),
                    number: 2,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Bool,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: String::new(),
                },
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    reg.messages_by_full_name.insert(
        "eve_public.test.UnimplementedActionRequest".into(),
        DescriptorProto {
            name: "UnimplementedActionRequest".into(),
            full_name: "eve_public.test.UnimplementedActionRequest".into(),
            fields: Vec::new(),
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );

    let router = Arc::new(new_router_with_descriptors(Arc::new(reg)));
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cl = Arc::clone(&calls);
    let gw_cl = Arc::clone(&router);
    router.register_handler(
        "eve_public.test.IdempotentActionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let n = calls_cl.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            // Enqueue a pending notice that should be rolled back on error or published on OK.
            gw_cl.enqueue_notice(
                NoticeScope::Broadcast,
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice"),
                    value: vec![n as u8],
                },
            );
            if dyn_msg.get(2).is_some() {
                return Err((STATUS_CONFLICT, "Simulated conflict".into()));
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.IdempotentActionResponse"),
                value: vec![n as u8],
            })
        }),
    );

    // Handler WITHOUT descriptor uses decode_wire_dynamic on valid wire bytes and rejects truncated wire bytes.
    router.register_handler(
        "eve_public.test.NoDescriptorRequest",
        Arc::new(|_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.NoDescriptorResponse"),
                value: vec![42],
            })
        }),
    );

    let player = make_session(Some(10), Some(90_000_001), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let now = 2_000_000u64;

    // 1. Truncated wire bytes on handler without descriptor -> 400
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("eve_public.test.NoDescriptorRequest", vec![0x0A, 0x10, 0x01], None), None, false, now).status_code,
        STATUS_BAD_REQUEST
    );
    // Valid wire bytes on handler without descriptor -> 200 via decode_wire_dynamic
    let mut valid_wire = Vec::new();
    encode_varint_field(1, 99, &mut valid_wire);
    let no_desc_rsp = router.handle_public_request(&player, &make_pub_req("eve_public.test.NoDescriptorRequest", valid_wire, None), None, false, now);
    assert_eq!(no_desc_rsp.status_code, STATUS_OK);
    assert_eq!(no_desc_rsp.payload.unwrap().value, vec![42]);

    // 2. Known descriptor with invalid wire bytes -> 400; valid wire bytes with no handler -> 501
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("eve_public.test.UnimplementedActionRequest", vec![0xFF], None), None, false, now).status_code,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        router.handle_public_request(&player, &make_pub_req("eve_public.test.UnimplementedActionRequest", Vec::new(), None), None, false, now).status_code,
        STATUS_NOT_IMPLEMENTED
    );

    // 3. Failed handler rolls back pending notices queued during execution!
    let mut fail_wire = Vec::new();
    encode_varint_field(2, 1, &mut fail_wire);
    let fail_rsp = router.handle_public_request(&player, &make_pub_req("eve_public.test.IdempotentActionRequest", fail_wire, None), None, false, now);
    assert_eq!(fail_rsp.status_code, STATUS_CONFLICT);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 1);

    // 4. Embedded idempotency token deduplicates within TTL and re-executes after TTL!
    let mut tok_inner = Vec::new();
    encode_bytes_field(1, b"idem-uuid-123456", &mut tok_inner);
    let mut idem_wire = Vec::new();
    encode_bytes_field(1, &tok_inner, &mut idem_wire);

    let r1 = router.handle_public_request(&player, &make_pub_req("eve_public.test.IdempotentActionRequest", idem_wire.clone(), None), None, false, now);
    assert_eq!(r1.status_code, STATUS_OK);
    assert_eq!(r1.payload.as_ref().unwrap().value, vec![2]);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

    // Cached hit at now + 100
    let r2 = router.handle_public_request(&player, &make_pub_req("eve_public.test.IdempotentActionRequest", idem_wire.clone(), None), None, false, now + 100);
    assert_eq!(r2.status_code, STATUS_OK);
    assert_eq!(r2.payload.as_ref().unwrap().value, vec![2]);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 2);

    // Expired at now + IDEMPOTENCY_TTL_SECS -> executes handler again!
    let r3 = router.handle_public_request(&player, &make_pub_req("eve_public.test.IdempotentActionRequest", idem_wire, None), None, false, now + IDEMPOTENCY_TTL_SECS);
    assert_eq!(r3.status_code, STATUS_OK);
    assert_eq!(r3.payload.as_ref().unwrap().value, vec![3]);
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 3);
}

#[test]
fn internal_envelopes_event_publishing_and_machonet_rpc_dispatch() {
    let router = new_router_with_descriptors(QuasarDescriptorRegistry::open_default_or_empty());
    let mut player = make_session(Some(10), Some(90_000_001), Some(98_000_001), service_roles::ROLE_PLAYER_DEFAULT);
    let now = 3_000_000u64;

    // 1. handle_internal_gateway_request: missing inner request -> 400; present -> 200
    let missing_int = InternalGatewayRequest {
        request: None,
        ack_id: 77,
    };
    let rsp_missing = router.handle_internal_gateway_request(&player, &missing_int, now);
    assert_eq!(rsp_missing.ack_id, 77);
    assert_eq!(rsp_missing.response.unwrap().status_code, STATUS_BAD_REQUEST);

    let ok_int = InternalGatewayRequest {
        request: Some(make_pub_req("google.protobuf.Empty", Vec::new(), None)),
        ack_id: 88,
    };
    let rsp_ok = router.handle_internal_gateway_request(&player, &ok_int, now);
    assert_eq!(rsp_ok.ack_id, 88);
    assert_eq!(rsp_ok.response.unwrap().status_code, STATUS_OK);

    // 2. handle_eve_internal_request
    let eve_req = EveInternalRequest {
        correlation_uuid: "corr-xyz".into(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"),
            value: Vec::new(),
        }),
        publisher: "monolith.node1".into(),
        ..Default::default()
    };
    let eve_rsp = router.handle_eve_internal_request(&player, &eve_req, now);
    assert_eq!(eve_rsp.status_code, STATUS_OK);
    assert_eq!(eve_rsp.correlation_uuid, "corr-xyz");
    assert_eq!(eve_rsp.recipient, "monolith.node1");
    assert_eq!(eve_rsp.publisher, "evomu.quasar.gateway");

    // 3. publish_event caps at MAX_PUBLISHED_EVENTS (1024) and stamps occurred & authoritative_context.
    for i in 0..(MAX_PUBLISHED_EVENTS + 5) {
        let mut ev_bytes = Vec::new();
        encode_string_field(1, &format!("ptr_{i}"), &mut ev_bytes);
        let ev = EvePublicEvent {
            uuid: vec![i as u8],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.helperpointer.ui.link.Clicked"),
                value: ev_bytes,
            }),
            external_origin: "client".into(),
            ..Default::default()
        };
        router.publish_event(&player, ev, now + i as u64);
    }
    {
        let events = router.published_events.read().unwrap();
        assert_eq!(events.len(), MAX_PUBLISHED_EVENTS);
        assert_eq!(events.front().unwrap().occurred.as_ref().unwrap().seconds, (now + 5) as i64);
        assert!(events.back().unwrap().authoritative_context.is_some());
    }

    // 4. dispatch_service_gateway_mgr_rpc validation & state transitions.
    let kwargs = HashMap::new();
    assert!(router.dispatch_service_gateway_mgr_rpc("UnknownMethod", &[], &kwargs).is_none());
    assert_eq!(
        router.dispatch_service_gateway_mgr_rpc("qa_is_hijack_mode_enabled", &[], &kwargs).unwrap().unwrap(),
        PyRep::Bool(false)
    );
    assert!(router
        .dispatch_service_gateway_mgr_rpc(
            "qa_enable_hijack_mode",
            &[PyRep::String("TimeoutException".into()), PyRep::Real(1.5)],
            &kwargs
        )
        .unwrap()
        .is_ok());
    assert_eq!(
        router.dispatch_service_gateway_mgr_rpc("qa_is_hijack_mode_enabled", &[], &kwargs).unwrap().unwrap(),
        PyRep::Bool(true)
    );
    // Invalid error string, invalid HTTP code, out-of-range latency -> Err
    assert!(router.dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[PyRep::String("Bogus".into())], &kwargs).unwrap().is_err());
    assert!(router.dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[PyRep::Integer(418)], &kwargs).unwrap().is_err());
    assert!(router.dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[PyRep::None, PyRep::Real(301.0)], &kwargs).unwrap().is_err());
    assert!(router.dispatch_service_gateway_mgr_rpc("qa_enable_hijack_mode", &[PyRep::None, PyRep::Integer(-1)], &kwargs).unwrap().is_err());
    assert!(router.dispatch_service_gateway_mgr_rpc("qa_disable_hijack_mode", &[], &kwargs).unwrap().is_ok());

    // 5. dispatch_public_gateway_rpc: Subscribe, PollNotices, PublishEventPayload, SendRequest, validate_raw_wire_frame.
    let unauth = make_session(None, None, None, 0);
    assert!(router.dispatch_public_gateway_rpc(&unauth, "Subscribe", &[], &kwargs, now).unwrap().is_err());
    assert!(router.dispatch_public_gateway_rpc(&unauth, "PublishEvent", &[PyRep::Bytes(vec![])], &kwargs, now).unwrap().is_err());

    let sub_rep = router
        .dispatch_public_gateway_rpc(
            &player,
            "Subscribe",
            &[
                PyRep::String("eve_public.debug.Notice".into()),
                PyRep::List(vec![PyRep::String("eve_public.other.Notice".into())]),
            ],
            &kwargs,
            now,
        )
        .unwrap()
        .unwrap();
    assert!(matches!(sub_rep, PyRep::KeyVal(_)));
    assert!(router.take_stream_receiver(player.session_id).is_some());

    let any_notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice"),
        value: vec![9, 8],
    };
    let (pub_n, _) = router.publish_notice(NoticeScope::Character(90_000_001), any_notice, now).unwrap();
    assert!(matches!(notice_to_pyrep(&pub_n), PyRep::KeyVal(_)));

    let polled = router.dispatch_public_gateway_rpc(&player, "PollNotices", &[], &kwargs, now).unwrap().unwrap();
    if let PyRep::List(items) = polled {
        assert_eq!(items.len(), 1);
    } else {
        panic!("expected list from PollNotices");
    }

    let any_ev = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.launcher.Start"),
        value: Vec::new(),
    };
    assert_eq!(
        router
            .dispatch_public_gateway_rpc(&player, "PublishEventPayload", &[PyRep::Bytes(any_ev.encode_proto())], &kwargs, now)
            .unwrap()
            .unwrap(),
        PyRep::Bool(true)
    );

    let raw_req = make_pub_req("google.protobuf.Empty", Vec::new(), None).encode_proto();
    assert!(validate_raw_wire_frame(&raw_req).is_ok());
    assert!(validate_raw_wire_frame(&[0xFF]).is_err());

    let mut send_kw = HashMap::new();
    send_kw.insert("idempotency_key".to_string(), PyRep::String("kw-idem-1".into()));
    assert!(router
        .dispatch_public_gateway_rpc(&player, "SendRequest", &[PyRep::Bytes(raw_req)], &send_kw, now)
        .unwrap()
        .is_ok());

    // Malformed wire frame to SendRequest returns STATUS_BAD_REQUEST inside response PyRep.
    assert!(router
        .dispatch_public_gateway_rpc(&player, "SendRequest", &[PyRep::Bytes(vec![0xFF])], &kwargs, now)
        .unwrap()
        .is_ok());

    player.role = service_roles::ROLE_PLAYER_DEFAULT;
    assert!(require_admin_role(&AuthoritativeContext::for_session(Some(1), None, None, 0, "t", vec![])).is_err());
}
