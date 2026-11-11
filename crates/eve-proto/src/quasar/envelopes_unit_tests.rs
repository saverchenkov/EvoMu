//! Unit and mutation-killing tests for `eve_proto::quasar::envelopes`.

use super::*;

#[test]
fn active_identity_and_authoritative_context_branches_roundtrip() {
    let id = ActiveIdentity {
        character: Some(EvePublicCharacterId {
            sequential: 90_000_001,
        }),
        corporation: Some(EvePublicCorporationId {
            sequential: 98_000_001,
        }),
        roles: Some(EvePublicCorpRoleCollection { bitmask: 0x1000 }),
    };
    assert_eq!(
        ActiveIdentity::decode_proto(&id.encode_proto()).unwrap(),
        id
    );
    assert_eq!(
        ActiveIdentity::decode_proto(&ActiveIdentity::default().encode_proto()).unwrap(),
        ActiveIdentity::default()
    );

    // Authenticated session context
    let ctx = AuthoritativeContext::for_session_at(
        Some(1001),
        Some(90_000_001),
        Some(98_000_001),
        0x4000,
        "tranquility",
        vec![1, 2, 3, 4],
        1_700_000_000,
    )
    .with_service_roles(0x8000);
    assert_eq!(ctx.effective_user_id(), Some(1001));
    assert_eq!(ctx.effective_character_id(), Some(90_000_001));
    assert_eq!(ctx.effective_corporation_id(), Some(98_000_001));
    assert_eq!(ctx.now_unix(), 1_700_000_000);
    assert_eq!(ctx.service_role_mask, 0x8000);
    assert_eq!(ctx.corp_role_mask, 0x4000);

    let dec_ctx = AuthoritativeContext::decode_proto(&ctx.encode_proto()).unwrap();
    assert_eq!(dec_ctx.effective_user_id(), Some(1001));
    assert_eq!(dec_ctx.effective_character_id(), Some(90_000_001));
    assert_eq!(dec_ctx.effective_corporation_id(), Some(98_000_001));
    assert_eq!(dec_ctx.corp_role_mask, 0x4000);

    // Fallback character ID from identity when active_character is None
    let mut id_only_ctx = ctx.clone();
    id_only_ctx.active_character = None;
    id_only_ctx.no_active_character = true;
    assert_eq!(id_only_ctx.effective_character_id(), Some(90_000_001));

    // Unauthenticated session context (no_authenticated_user, no_active_character, no_active_identity)
    let unauth = AuthoritativeContext::for_session(None, None, None, 0xFFFF, "singularity", vec![]);
    assert!(unauth.no_authenticated_user);
    assert!(unauth.no_active_character);
    assert!(unauth.no_active_identity);
    assert_eq!(unauth.corp_role_mask, 0);
    assert_eq!(unauth.effective_user_id(), None);
    assert_eq!(unauth.effective_character_id(), None);
    assert_eq!(unauth.effective_corporation_id(), None);
    assert_eq!(unauth.now_unix(), 0);
    let dec_unauth = AuthoritativeContext::decode_proto(&unauth.encode_proto()).unwrap();
    assert_eq!(dec_unauth, unauth);
}

#[test]
fn public_request_response_notice_target_groups_and_event_roundtrip() {
    let any_payload = GoogleAny {
        type_url: "type.googleapis.com/eve_public.ping.Request".into(),
        value: vec![0x08, 0x01],
    };
    let tc = TraceContext {
        trace_id: vec![0x11; 16],
        parent_id: vec![0x22; 8],
        flags: Some(TraceFlags { sampled: true }),
        state: Some(TraceState { sample_rate: 10 }),
    };
    let auth_ctx = AuthoritativeContext::for_session(
        Some(10),
        Some(20),
        Some(30),
        1,
        "tranquility",
        vec![9, 9],
    );

    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 100,
            nanos: 200,
        }),
        correlation_uuid: vec![1, 2, 3],
        payload: Some(any_payload.clone()),
        external_origin: "client".into(),
        application_instance_uuid: vec![4, 5, 6],
        authoritative_context: Some(auth_ctx.clone()),
        trace_context: Some(tc.clone()),
    };
    let mut dec_req = EvePublicRequest::decode_proto(&req.encode_proto()).unwrap();
    // AuthoritativeContext.corp_role_mask is restored from identity.roles.bitmask
    assert_eq!(dec_req, req);
    assert_eq!(
        EvePublicRequest::decode_proto(&EvePublicRequest::default().encode_proto()).unwrap(),
        EvePublicRequest::default()
    );
    let _ = &mut dec_req;

    let rsp = EvePublicResponse {
        dispatched: Some(GoogleTimestamp {
            seconds: 101,
            nanos: 300,
        }),
        correlation_uuid: vec![1, 2, 3],
        status_code: 200,
        status_message: "OK".into(),
        payload: Some(any_payload.clone()),
        internal_origin: "evomu-gateway".into(),
        application_instance_uuid: vec![4, 5, 6],
        gateway_instance_uuid: vec![7, 8, 9],
        trace_context: Some(tc.clone()),
    };
    assert_eq!(
        EvePublicResponse::decode_proto(&rsp.encode_proto()).unwrap(),
        rsp
    );
    assert_eq!(
        EvePublicResponse::decode_proto(&EvePublicResponse::default().encode_proto()).unwrap(),
        EvePublicResponse::default()
    );

    // All 7 NoticeTargetGroup variants and target_group_name
    let groups = [
        (
            NoticeTargetGroup::ApplicationInstanceUuid(vec![0xAA, 0xBB]),
            "application_instance_uuid",
        ),
        (
            NoticeTargetGroup::SolarSystem(30_000_142),
            "solar_system",
        ),
        (NoticeTargetGroup::User(-42), "user"),
        (NoticeTargetGroup::Character(90_000_001), "character"),
        (NoticeTargetGroup::Corporation(98_000_001), "corporation"),
        (NoticeTargetGroup::Alliance(-99_000_001), "alliance"),
        (
            NoticeTargetGroup::BubbleInstanceUuid(vec![0xCC, 0xDD]),
            "bubble_instance_uuid",
        ),
    ];
    for (tg, expected_name) in groups {
        assert_eq!(tg.target_group_name(), expected_name);
        let dec = NoticeTargetGroup::decode_proto(&tg.encode_proto()).unwrap();
        assert_eq!(dec, tg);
    }

    let notice = EvePublicNotice {
        dispatched: Some(GoogleTimestamp {
            seconds: 500,
            nanos: 0,
        }),
        uuid: vec![0xDE, 0xAD],
        internal_origin: "market-svc".into(),
        tenant: "tranquility".into(),
        payload: Some(any_payload.clone()),
        target_group: Some(NoticeTargetGroup::SolarSystem(30_000_142)),
        trace_context: Some(tc.clone()),
    };
    assert_eq!(
        EvePublicNotice::decode_proto(&notice.encode_proto()).unwrap(),
        notice
    );

    let event = EvePublicEvent {
        occurred: Some(GoogleTimestamp {
            seconds: 600,
            nanos: 1,
        }),
        uuid: vec![0xBE, 0xEF],
        payload: Some(any_payload),
        journey: vec![1, 2, 3, 4],
        external_origin: "eve-client".into(),
        authoritative_context: Some(auth_ctx),
        application_instance_uuid: vec![5, 6],
        trace_context: Some(tc),
    };
    assert_eq!(
        EvePublicEvent::decode_proto(&event.encode_proto()).unwrap(),
        event
    );
}

#[test]
fn internal_request_response_and_gateway_subscription_envelopes_roundtrip() {
    let any_payload = GoogleAny {
        type_url: "type.googleapis.com/eve.internal.Ping".into(),
        value: vec![0x10, 0x02],
    };
    let tc = TraceContext {
        trace_id: vec![1, 2],
        parent_id: vec![3, 4],
        flags: None,
        state: None,
    };

    let ireq = EveInternalRequest {
        correlation_uuid: "corr-123".into(),
        payload: Some(any_payload.clone()),
        language: -1,
        tenant: "tranquility".into(),
        publisher: "gateway".into(),
        forwarded_for: "127.0.0.1".into(),
        trace_context: Some(tc.clone()),
        ignore_cache: true,
        created: Some(GoogleTimestamp {
            seconds: 777,
            nanos: 888,
        }),
    };
    assert_eq!(
        EveInternalRequest::decode_proto(&ireq.encode_proto()).unwrap(),
        ireq
    );
    assert_eq!(
        EveInternalRequest::decode_proto(&EveInternalRequest::default().encode_proto()).unwrap(),
        EveInternalRequest::default()
    );

    let irsp = EveInternalResponse {
        correlation_uuid: "corr-123".into(),
        status_code: 201,
        status_message: "Created".into(),
        payload: Some(any_payload),
        tenant: "tranquility".into(),
        publisher: "service".into(),
        payload_not_cached: true,
        payload_cache_info: Some(EveResponseCacheInfo {
            cached_on: Some(GoogleTimestamp {
                seconds: 10,
                nanos: 0,
            }),
            cached_until: Some(GoogleTimestamp {
                seconds: 20,
                nanos: 0,
            }),
        }),
        payload_not_paged: true,
        payload_paging_info: Some(EveResponsePagingInfo {
            total_pages_count: 5,
        }),
        recipient: "client-1".into(),
        created: Some(GoogleTimestamp {
            seconds: 30,
            nanos: 0,
        }),
        trace_context: Some(tc),
    };
    assert_eq!(
        EveInternalResponse::decode_proto(&irsp.encode_proto()).unwrap(),
        irsp
    );
    assert_eq!(
        EveInternalResponse::decode_proto(&EveInternalResponse::default().encode_proto()).unwrap(),
        EveInternalResponse::default()
    );

    let gw_req = InternalGatewayRequest {
        ack_id: 42,
        request: Some(EvePublicRequest {
            external_origin: "gw".into(),
            ..Default::default()
        }),
    };
    assert_eq!(
        InternalGatewayRequest::decode_proto(&gw_req.encode_proto()).unwrap(),
        gw_req
    );

    let sub_info = InternalGatewayRequestsSubscriptionInfo {
        bind_requests: vec![EvePublicRequest {
            external_origin: "bind".into(),
            ..Default::default()
        }],
        consumer: Some(InternalGatewayConsumerInfo {
            name: "worker-1".into(),
            context: "prod".into(),
            tag: "v24".into(),
        }),
        in_flight_max: 64,
        unbind_requests: vec![EvePublicRequest {
            external_origin: "unbind".into(),
            ..Default::default()
        }],
    };
    let gw_rsp = InternalGatewayResponse {
        response: Some(EvePublicResponse {
            status_code: 200,
            status_message: "OK".into(),
            ..Default::default()
        }),
        ack_id: 99,
        subscription: Some(sub_info),
    };
    assert_eq!(
        InternalGatewayResponse::decode_proto(&gw_rsp.encode_proto()).unwrap(),
        gw_rsp
    );
}

#[test]
fn envelopes_decode_unknown_fields_wrong_wire_types_and_corrupt_submessages() {
    // Unknown field number + wrong wire types for all field numbers 1..=13 (both varint and bytes)
    let mut all_varints = Vec::new();
    for fnum in 1..=13 {
        encode_varint_field(fnum, 1, &mut all_varints);
    }
    encode_varint_field(99, 7, &mut all_varints);

    assert!(ActiveIdentity::decode_proto(&all_varints).is_ok());
    assert!(AuthoritativeContext::decode_proto(&all_varints).is_ok());
    assert!(EvePublicRequest::decode_proto(&all_varints).is_ok());
    assert!(EvePublicResponse::decode_proto(&all_varints).is_ok());
    assert!(NoticeTargetGroup::decode_proto(&all_varints).is_ok());
    assert!(EvePublicNotice::decode_proto(&all_varints).is_ok());
    assert!(EvePublicEvent::decode_proto(&all_varints).is_ok());
    assert!(EveInternalRequest::decode_proto(&all_varints).is_ok());
    assert!(EveInternalResponse::decode_proto(&all_varints).is_ok());
    assert!(InternalGatewayRequest::decode_proto(&all_varints).is_ok());
    assert!(InternalGatewayConsumerInfo::decode_proto(&all_varints).is_ok());
    assert!(InternalGatewayRequestsSubscriptionInfo::decode_proto(&all_varints).is_ok());
    assert!(InternalGatewayResponse::decode_proto(&all_varints).is_ok());

    // Wrong wire type (bytes) for varint fields & empty bytes / unknown field 99
    let mut all_empty_bytes = Vec::new();
    for fnum in 1..=13 {
        encode_bytes_field(fnum, &[], &mut all_empty_bytes);
    }
    encode_bytes_field(99, b"unknown", &mut all_empty_bytes);
    assert!(ActiveIdentity::decode_proto(&all_empty_bytes).is_ok());
    assert!(AuthoritativeContext::decode_proto(&all_empty_bytes).is_ok());
    assert!(EvePublicRequest::decode_proto(&all_empty_bytes).is_ok());
    assert!(EvePublicResponse::decode_proto(&all_empty_bytes).is_ok());
    assert!(NoticeTargetGroup::decode_proto(&all_empty_bytes).is_ok());
    assert!(EvePublicNotice::decode_proto(&all_empty_bytes).is_ok());
    assert!(EvePublicEvent::decode_proto(&all_empty_bytes).is_ok());
    assert!(EveInternalRequest::decode_proto(&all_empty_bytes).is_ok());
    assert!(EveInternalResponse::decode_proto(&all_empty_bytes).is_ok());
    assert!(InternalGatewayRequest::decode_proto(&all_empty_bytes).is_ok());
    assert!(InternalGatewayConsumerInfo::decode_proto(&all_empty_bytes).is_ok());
    assert!(InternalGatewayRequestsSubscriptionInfo::decode_proto(&all_empty_bytes).is_ok());
    assert!(InternalGatewayResponse::decode_proto(&all_empty_bytes).is_ok());

    // Invalid protobuf bytes inside each nested message / invalid UTF-8 inside string fields
    let bad_proto = [0x08u8]; // truncated varint
    let bad_utf8 = [0xFFu8, 0xFEu8];

    let check_bad_bytes = |fnum: u32, payload: &[u8], decode_fn: fn(&[u8]) -> Result<(), QuasarError>| {
        let mut buf = Vec::new();
        encode_bytes_field(fnum, payload, &mut buf);
        assert!(decode_fn(&buf).is_err(), "expected err for field {fnum}");
    };

    for f in [1, 2, 3] {
        check_bad_bytes(f, &bad_proto, |b| ActiveIdentity::decode_proto(b).map(|_| ()));
    }
    for f in [1, 2, 5, 7, 10] {
        check_bad_bytes(f, &bad_proto, |b| AuthoritativeContext::decode_proto(b).map(|_| ()));
    }
    check_bad_bytes(3, &bad_utf8, |b| AuthoritativeContext::decode_proto(b).map(|_| ()));

    for f in [1, 3, 6, 7] {
        check_bad_bytes(f, &bad_proto, |b| EvePublicRequest::decode_proto(b).map(|_| ()));
    }
    check_bad_bytes(4, &bad_utf8, |b| EvePublicRequest::decode_proto(b).map(|_| ()));

    for f in [1, 5, 9] {
        check_bad_bytes(f, &bad_proto, |b| EvePublicResponse::decode_proto(b).map(|_| ()));
    }
    for f in [4, 6] {
        check_bad_bytes(f, &bad_utf8, |b| EvePublicResponse::decode_proto(b).map(|_| ()));
    }

    for f in [1, 5, 6, 7] {
        check_bad_bytes(f, &bad_proto, |b| EvePublicNotice::decode_proto(b).map(|_| ()));
    }
    for f in [3, 4] {
        check_bad_bytes(f, &bad_utf8, |b| EvePublicNotice::decode_proto(b).map(|_| ()));
    }

    for f in [1, 3, 6, 8] {
        check_bad_bytes(f, &bad_proto, |b| EvePublicEvent::decode_proto(b).map(|_| ()));
    }
    check_bad_bytes(5, &bad_utf8, |b| EvePublicEvent::decode_proto(b).map(|_| ()));

    for f in [2, 7, 9] {
        check_bad_bytes(f, &bad_proto, |b| EveInternalRequest::decode_proto(b).map(|_| ()));
    }
    for f in [1, 4, 5, 6] {
        check_bad_bytes(f, &bad_utf8, |b| EveInternalRequest::decode_proto(b).map(|_| ()));
    }

    for f in [4, 8, 10, 12, 13] {
        check_bad_bytes(f, &bad_proto, |b| EveInternalResponse::decode_proto(b).map(|_| ()));
    }
    for f in [1, 3, 5, 6, 11] {
        check_bad_bytes(f, &bad_utf8, |b| EveInternalResponse::decode_proto(b).map(|_| ()));
    }

    check_bad_bytes(2, &bad_proto, |b| InternalGatewayRequest::decode_proto(b).map(|_| ()));
    for f in [1, 2, 3] {
        check_bad_bytes(f, &bad_utf8, |b| InternalGatewayConsumerInfo::decode_proto(b).map(|_| ()));
    }
    for f in [1, 2, 4] {
        check_bad_bytes(f, &bad_proto, |b| {
            InternalGatewayRequestsSubscriptionInfo::decode_proto(b).map(|_| ())
        });
    }
    for f in [1, 3] {
        check_bad_bytes(f, &bad_proto, |b| InternalGatewayResponse::decode_proto(b).map(|_| ()));
    }

    // NoticeTargetGroup invalid UTF-8 / truncated wire
    assert!(NoticeTargetGroup::decode_proto(&bad_proto).is_err());
    // Truncated envelope bytes
    assert!(ActiveIdentity::decode_proto(&bad_proto).is_err());
    assert!(AuthoritativeContext::decode_proto(&bad_proto).is_err());
    assert!(EvePublicRequest::decode_proto(&bad_proto).is_err());
    assert!(EvePublicResponse::decode_proto(&bad_proto).is_err());
    assert!(EvePublicNotice::decode_proto(&bad_proto).is_err());
    assert!(EvePublicEvent::decode_proto(&bad_proto).is_err());
    assert!(EveInternalRequest::decode_proto(&bad_proto).is_err());
    assert!(EveInternalResponse::decode_proto(&bad_proto).is_err());
    assert!(InternalGatewayRequest::decode_proto(&bad_proto).is_err());
    assert!(InternalGatewayConsumerInfo::decode_proto(&bad_proto).is_err());
    assert!(InternalGatewayRequestsSubscriptionInfo::decode_proto(&bad_proto).is_err());
    assert!(InternalGatewayResponse::decode_proto(&bad_proto).is_err());
}
