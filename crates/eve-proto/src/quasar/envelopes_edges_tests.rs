use super::*;

fn wrong_bytes(fields: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &f in fields {
        encode_bytes_field(f, b"wrong", &mut out);
    }
    encode_varint_field(99, 1, &mut out);
    out
}

fn wrong_varints(fields: &[u32]) -> Vec<u8> {
    let mut out = Vec::new();
    for &f in fields {
        encode_varint_field(f, 1, &mut out);
    }
    encode_bytes_field(99, b"unknown", &mut out);
    out
}

#[test]
fn active_identity_and_authoritative_context_edges() {
    // ActiveIdentity: empty, corrupt nested f1/f2/f3, wrong wire types, truncated.
    assert!(ActiveIdentity::default().encode_proto().is_empty());
    for f in [1u32, 2, 3] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(ActiveIdentity::decode_proto(&buf).is_err());
    }
    assert_eq!(
        ActiveIdentity::decode_proto(&wrong_varints(&[1, 2, 3])).unwrap(),
        ActiveIdentity::default()
    );
    assert!(ActiveIdentity::decode_proto(&[0x0A, 0x05]).is_err());

    // AuthoritativeContext helpers and edge combinations.
    let anon = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1, 2]);
    assert_eq!(anon.effective_user_id(), None);
    assert_eq!(anon.effective_character_id(), None);
    assert_eq!(anon.effective_corporation_id(), None);
    assert_eq!(anon.now_unix(), 0);
    assert!(anon.no_authenticated_user);
    assert!(anon.no_active_character);
    assert!(anon.no_active_identity);

    let full = AuthoritativeContext::for_session_at(
        Some(42),
        Some(9001),
        Some(9800_0001),
        0xAA,
        "tranquility",
        vec![9, 8],
        1_700_000_123,
    )
    .with_service_roles(0x55);
    assert_eq!(full.effective_user_id(), Some(42));
    assert_eq!(full.effective_character_id(), Some(9001));
    assert_eq!(full.effective_corporation_id(), Some(9800_0001));
    assert_eq!(full.now_unix(), 1_700_000_123);
    assert_eq!(full.service_role_mask, 0x55);
    let round = AuthoritativeContext::decode_proto(&full.encode_proto()).unwrap();
    assert_eq!(round.corp_role_mask, 0xAA);

    // effective_character_id falls back to identity.character when active_character is None
    // as long as no_active_identity == false (even if no_active_character == true).
    let mut fallback_ctx = full.clone();
    fallback_ctx.no_active_character = true;
    fallback_ctx.active_character = None;
    assert_eq!(fallback_ctx.effective_character_id(), Some(9001));

    // Toggle ordering on wire: setting authenticated_user after no_authenticated_user clears the flag,
    // and setting no_authenticated_user after clears authenticated_user (same for char & identity).
    let mut ordered = Vec::new();
    encode_varint_field(4, 1, &mut ordered); // no_authenticated_user = true
    encode_bytes_field(5, &EvePublicUserId { sequential: 7 }.encode_proto(), &mut ordered);
    encode_varint_field(6, 1, &mut ordered); // no_active_character = true
    encode_bytes_field(7, &EvePublicCharacterId { sequential: 8 }.encode_proto(), &mut ordered);
    encode_varint_field(9, 1, &mut ordered); // no_active_identity = true
    encode_bytes_field(
        10,
        &ActiveIdentity {
            character: Some(EvePublicCharacterId { sequential: 8 }),
            corporation: None,
            roles: None,
        }
        .encode_proto(),
        &mut ordered,
    );
    let dec_cleared = AuthoritativeContext::decode_proto(&ordered).unwrap();
    assert!(!dec_cleared.no_authenticated_user);
    assert_eq!(dec_cleared.effective_user_id(), Some(7));
    assert!(!dec_cleared.no_active_character);
    assert_eq!(dec_cleared.effective_character_id(), Some(8));
    assert!(!dec_cleared.no_active_identity);
    assert_eq!(dec_cleared.corp_role_mask, 0);

    let mut reverse_ordered = Vec::new();
    encode_bytes_field(5, &EvePublicUserId { sequential: 7 }.encode_proto(), &mut reverse_ordered);
    encode_varint_field(4, 1, &mut reverse_ordered);
    encode_bytes_field(7, &EvePublicCharacterId { sequential: 8 }.encode_proto(), &mut reverse_ordered);
    encode_varint_field(6, 1, &mut reverse_ordered);
    encode_bytes_field(
        10,
        &ActiveIdentity {
            character: Some(EvePublicCharacterId { sequential: 8 }),
            corporation: None,
            roles: Some(EvePublicCorpRoleCollection { bitmask: 9 }),
        }
        .encode_proto(),
        &mut reverse_ordered,
    );
    encode_varint_field(9, 1, &mut reverse_ordered);
    let dec_nulled = AuthoritativeContext::decode_proto(&reverse_ordered).unwrap();
    assert!(dec_nulled.no_authenticated_user);
    assert!(dec_nulled.authenticated_user.is_none());
    assert!(dec_nulled.no_active_character);
    assert!(dec_nulled.active_character.is_none());
    assert!(dec_nulled.no_active_identity);
    assert!(dec_nulled.identity.is_none());
    assert_eq!(dec_nulled.corp_role_mask, 9);

    // Corrupt nested submessages (f1, f2, f5, f7, f10) & invalid UTF-8 on tenant (f3).
    for f in [1u32, 2, 5, 7, 10] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(AuthoritativeContext::decode_proto(&buf).is_err());
    }
    let mut bad_utf8 = Vec::new();
    encode_bytes_field(3, &[0xFF, 0xFE], &mut bad_utf8);
    assert!(matches!(
        AuthoritativeContext::decode_proto(&bad_utf8),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // Wrong wire types ignored cleanly; truncated outer returns Err.
    let mut wrong_ac = wrong_varints(&[1, 2, 3, 5, 7, 8, 10, 11]);
    wrong_ac.extend_from_slice(&wrong_bytes(&[4, 6, 9]));
    assert_eq!(
        AuthoritativeContext::decode_proto(&wrong_ac).unwrap(),
        AuthoritativeContext::default()
    );
    assert!(AuthoritativeContext::decode_proto(&[0xFF]).is_err());
}

#[test]
fn public_request_response_notice_and_event_edges() {
    // EvePublicRequest
    assert!(EvePublicRequest::default().encode_proto().is_empty());
    for f in [1u32, 3, 6, 7] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EvePublicRequest::decode_proto(&buf).is_err());
    }
    let mut bad_utf8_req = Vec::new();
    encode_bytes_field(4, &[0xFF], &mut bad_utf8_req);
    assert!(matches!(
        EvePublicRequest::decode_proto(&bad_utf8_req),
        Err(QuasarError::InvalidUtf8(_))
    ));
    assert_eq!(
        EvePublicRequest::decode_proto(&wrong_varints(&[1, 2, 3, 4, 5, 6, 7])).unwrap(),
        EvePublicRequest::default()
    );
    assert!(EvePublicRequest::decode_proto(&[0x0A, 0x05]).is_err());

    // EvePublicResponse
    assert!(EvePublicResponse::default().encode_proto().is_empty());
    for f in [1u32, 5, 9] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EvePublicResponse::decode_proto(&buf).is_err());
    }
    for f in [4u32, 6] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0xFF], &mut buf);
        assert!(matches!(
            EvePublicResponse::decode_proto(&buf),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }
    let mut wrong_rsp = wrong_varints(&[1, 2, 4, 5, 6, 7, 8, 9]);
    wrong_rsp.extend_from_slice(&wrong_bytes(&[3]));
    assert_eq!(
        EvePublicResponse::decode_proto(&wrong_rsp).unwrap(),
        EvePublicResponse::default()
    );
    assert!(EvePublicResponse::decode_proto(&[0x0A, 0x05]).is_err());

    // NoticeTargetGroup: all 7 variants & wrong wire types / empty.
    let targets = [
        (NoticeTargetGroup::ApplicationInstanceUuid(vec![1, 2, 3]), "application_instance_uuid"),
        (NoticeTargetGroup::SolarSystem(30_000_142), "solar_system"),
        (NoticeTargetGroup::User(-5), "user"),
        (NoticeTargetGroup::Character(90_001), "character"),
        (NoticeTargetGroup::Corporation(98_000_001), "corporation"),
        (NoticeTargetGroup::Alliance(-12), "alliance"),
        (NoticeTargetGroup::BubbleInstanceUuid(vec![4, 5]), "bubble_instance_uuid"),
    ];
    for (tg, name) in targets {
        assert_eq!(tg.target_group_name(), name);
        let notice = EvePublicNotice {
            target_group: Some(tg.clone()),
            ..Default::default()
        };
        let dec = EvePublicNotice::decode_proto(&notice.encode_proto()).unwrap();
        assert_eq!(dec.target_group, Some(tg));
    }
    assert_eq!(
        NoticeTargetGroup::decode_proto(&[]).unwrap(),
        NoticeTargetGroup::Character(0)
    );
    let mut wrong_tg = wrong_varints(&[1, 7]);
    wrong_tg.extend_from_slice(&wrong_bytes(&[2, 3, 4, 5, 6]));
    assert_eq!(
        NoticeTargetGroup::decode_proto(&wrong_tg).unwrap(),
        NoticeTargetGroup::Character(0)
    );
    assert!(NoticeTargetGroup::decode_proto(&[0x08]).is_err());

    // EvePublicNotice
    assert!(EvePublicNotice::default().encode_proto().is_empty());
    for f in [1u32, 5, 6, 7] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EvePublicNotice::decode_proto(&buf).is_err());
    }
    for f in [3u32, 4] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0xFF], &mut buf);
        assert!(matches!(
            EvePublicNotice::decode_proto(&buf),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }
    assert_eq!(
        EvePublicNotice::decode_proto(&wrong_varints(&[1, 2, 3, 4, 5, 6, 7])).unwrap(),
        EvePublicNotice::default()
    );
    assert!(EvePublicNotice::decode_proto(&[0x0A, 0x05]).is_err());

    // EvePublicEvent
    assert!(EvePublicEvent::default().encode_proto().is_empty());
    let ev = EvePublicEvent {
        occurred: Some(GoogleTimestamp { seconds: 50, nanos: 1 }),
        uuid: vec![1, 2],
        payload: Some(GoogleEmpty.to_any()),
        journey: vec![3, 4],
        external_origin: "origin".to_string(),
        authoritative_context: Some(AuthoritativeContext::default()),
        application_instance_uuid: vec![5],
        trace_context: Some(TraceContext::default()),
    };
    assert_eq!(EvePublicEvent::decode_proto(&ev.encode_proto()).unwrap(), ev);
    for f in [1u32, 3, 6, 8] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EvePublicEvent::decode_proto(&buf).is_err());
    }
    let mut bad_utf8_ev = Vec::new();
    encode_bytes_field(5, &[0xFF], &mut bad_utf8_ev);
    assert!(matches!(
        EvePublicEvent::decode_proto(&bad_utf8_ev),
        Err(QuasarError::InvalidUtf8(_))
    ));
    assert_eq!(
        EvePublicEvent::decode_proto(&wrong_varints(&[1, 2, 3, 4, 5, 6, 7, 8])).unwrap(),
        EvePublicEvent::default()
    );
    assert!(EvePublicEvent::decode_proto(&[0x0A, 0x05]).is_err());
}

#[test]
fn internal_envelopes_and_gateway_edges() {
    // EveInternalRequest
    assert!(EveInternalRequest::default().encode_proto().is_empty());
    let ireq = EveInternalRequest {
        correlation_uuid: "corr-1".to_string(),
        payload: Some(GoogleEmpty.to_any()),
        language: 1033,
        tenant: "tranquility".to_string(),
        publisher: "svc".to_string(),
        forwarded_for: "127.0.0.1".to_string(),
        trace_context: Some(TraceContext::default()),
        ignore_cache: true,
        created: Some(GoogleTimestamp { seconds: 10, nanos: 2 }),
    };
    assert_eq!(
        EveInternalRequest::decode_proto(&ireq.encode_proto()).unwrap(),
        ireq
    );
    for f in [2u32, 7, 9] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EveInternalRequest::decode_proto(&buf).is_err());
    }
    for f in [1u32, 4, 5, 6] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0xFF], &mut buf);
        assert!(matches!(
            EveInternalRequest::decode_proto(&buf),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }
    let mut wrong_ireq = wrong_varints(&[1, 2, 4, 5, 6, 7, 9]);
    wrong_ireq.extend_from_slice(&wrong_bytes(&[3, 8]));
    assert_eq!(
        EveInternalRequest::decode_proto(&wrong_ireq).unwrap(),
        EveInternalRequest::default()
    );
    assert!(EveInternalRequest::decode_proto(&[0x0A, 0x05]).is_err());

    // EveResponseCacheInfo & EveResponsePagingInfo
    assert!(EveResponseCacheInfo::default().encode_proto().is_empty());
    for f in [1u32, 2] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EveResponseCacheInfo::decode_proto(&buf).is_err());
    }
    assert_eq!(
        EveResponseCacheInfo::decode_proto(&wrong_varints(&[1, 2])).unwrap(),
        EveResponseCacheInfo::default()
    );
    assert!(EveResponseCacheInfo::decode_proto(&[0x0A, 0x05]).is_err());

    assert!(EveResponsePagingInfo::default().encode_proto().is_empty());
    assert_eq!(
        EveResponsePagingInfo::decode_proto(&wrong_bytes(&[1])).unwrap(),
        EveResponsePagingInfo::default()
    );
    assert!(EveResponsePagingInfo::decode_proto(&[0x08]).is_err());

    // EveInternalResponse
    assert!(EveInternalResponse::default().encode_proto().is_empty());
    let irsp = EveInternalResponse {
        correlation_uuid: "corr-2".to_string(),
        status_code: 200,
        status_message: "ok".to_string(),
        payload: Some(GoogleEmpty.to_any()),
        tenant: "tranquility".to_string(),
        publisher: "svc".to_string(),
        payload_not_cached: true,
        payload_cache_info: Some(EveResponseCacheInfo {
            cached_on: Some(GoogleTimestamp { seconds: 1, nanos: 0 }),
            cached_until: Some(GoogleTimestamp { seconds: 2, nanos: 0 }),
        }),
        payload_not_paged: true,
        payload_paging_info: Some(EveResponsePagingInfo { total_pages_count: 5 }),
        recipient: "gateway".to_string(),
        created: Some(GoogleTimestamp { seconds: 3, nanos: 0 }),
        trace_context: Some(TraceContext::default()),
    };
    assert_eq!(
        EveInternalResponse::decode_proto(&irsp.encode_proto()).unwrap(),
        irsp
    );
    for f in [4u32, 8, 10, 12, 13] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(EveInternalResponse::decode_proto(&buf).is_err());
    }
    for f in [1u32, 3, 5, 6, 11] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0xFF], &mut buf);
        assert!(matches!(
            EveInternalResponse::decode_proto(&buf),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }
    let mut wrong_irsp = wrong_varints(&[1, 3, 4, 5, 6, 8, 10, 11, 12, 13]);
    wrong_irsp.extend_from_slice(&wrong_bytes(&[2, 7, 9]));
    assert_eq!(
        EveInternalResponse::decode_proto(&wrong_irsp).unwrap(),
        EveInternalResponse::default()
    );
    assert!(EveInternalResponse::decode_proto(&[0x0A, 0x05]).is_err());

    // InternalGatewayRequest, ConsumerInfo, RequestsSubscriptionInfo, Response
    assert!(InternalGatewayRequest::default().encode_proto().is_empty());
    let mut corrupt_gw_req = Vec::new();
    encode_bytes_field(2, &[0x08], &mut corrupt_gw_req);
    assert!(InternalGatewayRequest::decode_proto(&corrupt_gw_req).is_err());
    let mut wrong_gw_req = wrong_bytes(&[1]);
    wrong_gw_req.extend_from_slice(&wrong_varints(&[2]));
    assert_eq!(
        InternalGatewayRequest::decode_proto(&wrong_gw_req).unwrap(),
        InternalGatewayRequest::default()
    );
    assert!(InternalGatewayRequest::decode_proto(&[0x08]).is_err());

    assert!(InternalGatewayConsumerInfo::default().encode_proto().is_empty());
    for f in [1u32, 2, 3] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0xFF], &mut buf);
        assert!(matches!(
            InternalGatewayConsumerInfo::decode_proto(&buf),
            Err(QuasarError::InvalidUtf8(_))
        ));
    }
    assert_eq!(
        InternalGatewayConsumerInfo::decode_proto(&wrong_varints(&[1, 2, 3])).unwrap(),
        InternalGatewayConsumerInfo::default()
    );
    assert!(InternalGatewayConsumerInfo::decode_proto(&[0x0A, 0x05]).is_err());

    assert!(InternalGatewayRequestsSubscriptionInfo::default()
        .encode_proto()
        .is_empty());
    for f in [1u32, 2, 4] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(InternalGatewayRequestsSubscriptionInfo::decode_proto(&buf).is_err());
    }
    let mut wrong_sub = wrong_varints(&[1, 2, 4]);
    wrong_sub.extend_from_slice(&wrong_bytes(&[3]));
    assert_eq!(
        InternalGatewayRequestsSubscriptionInfo::decode_proto(&wrong_sub).unwrap(),
        InternalGatewayRequestsSubscriptionInfo::default()
    );
    assert!(InternalGatewayRequestsSubscriptionInfo::decode_proto(&[0x0A, 0x05]).is_err());

    assert!(InternalGatewayResponse::default().encode_proto().is_empty());
    for f in [1u32, 3] {
        let mut buf = Vec::new();
        encode_bytes_field(f, &[0x08], &mut buf);
        assert!(InternalGatewayResponse::decode_proto(&buf).is_err());
    }
    let mut wrong_gw_rsp = wrong_varints(&[1, 3]);
    wrong_gw_rsp.extend_from_slice(&wrong_bytes(&[2]));
    assert_eq!(
        InternalGatewayResponse::decode_proto(&wrong_gw_rsp).unwrap(),
        InternalGatewayResponse::default()
    );
    assert!(InternalGatewayResponse::decode_proto(&[0x0A, 0x05]).is_err());
}
