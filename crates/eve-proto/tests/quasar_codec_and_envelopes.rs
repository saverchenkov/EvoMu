#![forbid(unsafe_code)]

use eve_proto::quasar::*;
use eve_proto::quasar_wire::{
    decode_envelope, decode_fields, encode_envelope, encode_fields, read_varint, write_varint,
    ProtoWireValue, QuasarGatewayEnvelope,
};
use eve_proto::{GatewayFrame, GatewayFrameKind, ProtoError, PyRep};

#[test]
fn test_quasar_wire_primitives_hashes_and_errors() {
    // Type URL prefix helpers
    assert!(has_quasar_type_url_prefix("type.googleapis.com/foo.Bar"));
    assert!(has_quasar_type_url_prefix("type.evetech.net/foo.Bar"));
    assert!(!has_quasar_type_url_prefix("eve_public.Request"));
    assert_eq!(
        strip_quasar_type_url_prefix("type.googleapis.com/.foo.Bar"),
        "foo.Bar"
    );
    assert_eq!(
        strip_quasar_type_url_prefix("type.evetech.net/.foo.Bar"),
        "foo.Bar"
    );
    assert_eq!(strip_quasar_type_url_prefix(".foo.Bar"), "foo.Bar");

    // SHA-256 and MD5 RFC test vectors
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");

    // Varint & ZigZag 32/64 roundtrip + overflow + EOF
    for v in [0u64, 1, 127, 128, 16_384, u64::from(u32::MAX), u64::MAX] {
        let mut buf = Vec::new();
        encode_varint(v, &mut buf);
        let mut pos = 0usize;
        assert_eq!(decode_varint(&buf, &mut pos).unwrap(), v);
        assert_eq!(pos, buf.len());
    }
    assert_eq!(decode_varint(&[], &mut 0), Err(ProtoError::UnexpectedEof).map_err(|_| QuasarError::UnexpectedEof));
    assert_eq!(
        decode_varint(&[0xFF; 10], &mut 0),
        Err(QuasarError::VarintOverflow)
    );
    assert_eq!(
        decode_varint(&[0x80; 11], &mut 0),
        Err(QuasarError::VarintOverflow)
    );

    for s32 in [0i32, 1, -1, 42, -42, i32::MIN, i32::MAX] {
        assert_eq!(decode_zigzag32(encode_zigzag32(s32)), s32);
    }
    for s64 in [0i64, 1, -1, 1_000_000, -1_000_000, i64::MIN, i64::MAX] {
        assert_eq!(decode_zigzag64(encode_zigzag64(s64)), s64);
    }

    // WireType and WireValue accessors
    assert_eq!(WireType::from_u8(0), Ok(WireType::Varint));
    assert_eq!(WireType::from_u8(1), Ok(WireType::Fixed64));
    assert_eq!(WireType::from_u8(2), Ok(WireType::LengthDelimited));
    assert_eq!(WireType::from_u8(5), Ok(WireType::Fixed32));
    assert_eq!(WireType::from_u8(3), Err(QuasarError::InvalidWireType(3)));

    let v_var = WireValue::Varint(99);
    let v_f64 = WireValue::Fixed64(0x1122_3344_5566_7788);
    let v_str = WireValue::LengthDelimited(b"hello".to_vec());
    let v_bad_utf8 = WireValue::LengthDelimited(vec![0xFF, 0xFE]);
    let v_f32 = WireValue::Fixed32(0x1234_5678);

    assert_eq!(v_var.wire_type(), WireType::Varint);
    assert_eq!(v_var.as_varint(), Some(99));
    assert_eq!(v_var.as_bytes(), None);
    assert_eq!(v_var.as_str(), Ok(None));
    assert_eq!(v_var.as_fixed32(), None);
    assert_eq!(v_var.as_fixed64(), None);

    assert_eq!(v_f64.wire_type(), WireType::Fixed64);
    assert_eq!(v_f64.as_fixed64(), Some(0x1122_3344_5566_7788));
    assert_eq!(v_f64.as_varint(), None);

    assert_eq!(v_str.wire_type(), WireType::LengthDelimited);
    assert_eq!(v_str.as_bytes(), Some(b"hello".as_slice()));
    assert_eq!(v_str.as_str(), Ok(Some("hello")));
    assert!(matches!(v_bad_utf8.as_str(), Err(QuasarError::InvalidUtf8(_))));

    assert_eq!(v_f32.wire_type(), WireType::Fixed32);
    assert_eq!(v_f32.as_fixed32(), Some(0x1234_5678));

    // WireMessage encode/decode and malformed frames
    let mut raw = Vec::new();
    encode_varint_field(1, 777, &mut raw);
    encode_fixed64_field(2, 0xDEAD_BEEF_CAFE_BABE, &mut raw);
    encode_string_field(3, "quasar", &mut raw);
    encode_fixed32_field(4, 0xCAFE_0001, &mut raw);
    let decoded = WireMessage::decode(&raw).unwrap();
    assert_eq!(decoded.fields.len(), 4);
    assert_eq!(decoded.encode(), raw);

    assert_eq!(WireMessage::decode(&[0x00]), Err(QuasarError::InvalidWireType(0xFF)));
    assert_eq!(WireMessage::decode(&[0x09, 0x01, 0x02]), Err(QuasarError::UnexpectedEof));
    assert_eq!(WireMessage::decode(&[0x0D, 0x01, 0x02]), Err(QuasarError::UnexpectedEof));
    assert_eq!(WireMessage::decode(&[0x0A, 0x05, 0x01]), Err(QuasarError::UnexpectedEof));

    // FieldType & FieldLabel coverage
    for i in 1..=20 {
        let ft = FieldType::from_i32(i);
        let _ = ft.expected_wire_type();
        let _ = ft.is_packable_scalar();
    }
    assert_eq!(FieldLabel::from_i32(2), FieldLabel::Required);
    assert_eq!(FieldLabel::from_i32(3), FieldLabel::Repeated);
    assert_eq!(FieldLabel::from_i32(99), FieldLabel::Optional);
}

#[test]
fn test_quasar_well_known_and_core_types_roundtrip() {
    let ts = GoogleTimestamp {
        seconds: 1_700_000_000,
        nanos: 123_456_789,
    };
    let ts_rt = GoogleTimestamp::decode_proto(&ts.encode_proto()).unwrap();
    assert_eq!(ts_rt, ts);
    assert_eq!(GoogleTimestamp::decode_proto(&[]).unwrap(), GoogleTimestamp::default());
    assert_eq!(ts.encode_proto().len() > 0, true);
    assert_eq!(GoogleTimestamp::default().encode_proto(), Vec::<u8>::new());

    let dur = GoogleDuration {
        seconds: 60,
        nanos: 500_000,
    };
    assert_eq!(GoogleDuration::decode_proto(&dur.encode_proto()).unwrap(), dur);
    assert_eq!(GoogleDuration::decode_proto(&[]).unwrap(), GoogleDuration::default());
    assert_eq!(GoogleDuration::default().encode_proto(), Vec::<u8>::new());

    let any = dur.to_any();
    assert_eq!(any.type_url, "type.googleapis.com/google.protobuf.Duration");
    assert_eq!(any.normalized_type_name(), "google.protobuf.Duration");
    assert_eq!(GoogleDuration::from_any(&any).unwrap(), dur);
    assert_eq!(
        GoogleAny::decode_proto(&any.encode_proto()).unwrap(),
        any
    );
    assert!(matches!(
        GoogleTimestamp::from_any(&any),
        Err(QuasarError::TypeUrlMismatch { .. })
    ));

    assert_eq!(GoogleEmpty.encode_proto(), Vec::<u8>::new());
    assert_eq!(GoogleEmpty::decode_proto(&[]).unwrap(), GoogleEmpty);

    let uid = EvePublicUserId { sequential: -987_654 };
    assert_eq!(EvePublicUserId::decode_proto(&uid.encode_proto()).unwrap(), uid);
    assert_eq!(EvePublicUserId::decode_proto(&[]).unwrap().sequential, 0);

    let cid = EvePublicCharacterId { sequential: 2_112_345_678 };
    assert_eq!(EvePublicCharacterId::decode_proto(&cid.encode_proto()).unwrap(), cid);

    let corp = EvePublicCorporationId { sequential: 98_000_001 };
    assert_eq!(EvePublicCorporationId::decode_proto(&corp.encode_proto()).unwrap(), corp);

    let roles = EvePublicCorpRoleCollection {
        bitmask: 0x0000_FFFF_0000_0001,
    };
    assert_eq!(
        EvePublicCorpRoleCollection::decode_proto(&roles.encode_proto()).unwrap(),
        roles
    );

    let tok = EveIdempotencyToken {
        uuid: vec![1, 2, 3, 4, 5, 6, 7, 8],
    };
    assert_eq!(EveIdempotencyToken::decode_proto(&tok.encode_proto()).unwrap(), tok);

    let ip4 = EveIpAddress::default();
    assert_eq!(EveIpAddress::decode_proto(&ip4.encode_proto()).unwrap(), ip4);
    let ip6 = EveIpAddress::V6(vec![0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(EveIpAddress::decode_proto(&ip6.encode_proto()).unwrap(), ip6);

    let tc = TraceContext {
        trace_id: vec![0xAA; 16],
        parent_id: vec![0xBB; 8],
        flags: Some(TraceFlags { sampled: true }),
        state: Some(TraceState { sample_rate: 100 }),
    };
    assert_eq!(TraceContext::decode_proto(&tc.encode_proto()).unwrap(), tc);
    let tc_unsampled = TraceContext {
        trace_id: vec![],
        parent_id: vec![],
        flags: Some(TraceFlags { sampled: false }),
        state: Some(TraceState { sample_rate: 0 }),
    };
    assert_eq!(
        TraceContext::decode_proto(&tc_unsampled.encode_proto()).unwrap(),
        tc_unsampled
    );

    let ver = SemanticVersionSpec {
        major: 24,
        minor: 1,
        patch: 3619879,
        prerelease_tags: vec!["rc1".into(), "hotfix".into()],
        build_tags: vec!["mac".into()],
    };
    assert_eq!(
        SemanticVersionSpec::decode_proto(&ver.encode_proto()).unwrap(),
        ver
    );

    let rgb = ColorRgb {
        red: 12,
        green: 34,
        blue: 56,
    };
    assert_eq!(ColorRgb::decode_proto(&rgb.encode_proto()).unwrap(), rgb);

    let frac = MathFraction {
        numerator: 3,
        denominator: 4,
    };
    assert_eq!(
        MathFraction::decode_proto(&frac.encode_proto()).unwrap(),
        frac
    );

    let v3 = MathVector3 {
        x: 1.5,
        y: -2.25,
        z: 3.75,
    };
    assert_eq!(MathVector3::decode_proto(&v3.encode_proto()).unwrap(), v3);

    let v4 = MathVector4 {
        x: 0.5,
        y: 0.5,
        z: -0.5,
        w: 0.5,
    };
    assert_eq!(MathVector4::decode_proto(&v4.encode_proto()).unwrap(), v4);

    let dev = DeviceUserIdentifier {
        device: vec![9, 8, 7],
        user: Some(uid),
    };
    assert_eq!(
        DeviceUserIdentifier::decode_proto(&dev.encode_proto()).unwrap(),
        dev
    );

    let rtt = NetworkRttSample {
        protocol: 2,
        rtt: Some(dur),
    };
    assert_eq!(
        NetworkRttSample::decode_proto(&rtt.encode_proto()).unwrap(),
        rtt
    );
}

#[test]
fn test_quasar_gateway_envelopes_and_wire_frames() {
    // AuthoritativeContext authenticated + unauthenticated branches
    let ctx = AuthoritativeContext::for_session_at(
        Some(-42),
        Some(90001),
        Some(98001),
        0x55AA,
        "tranquility",
        vec![1, 2, 3, 4],
        1_700_000_123,
    )
    .with_service_roles(0x8000_0000);
    assert_eq!(ctx.effective_user_id(), Some(-42));
    assert_eq!(ctx.effective_character_id(), Some(90001));
    assert_eq!(ctx.effective_corporation_id(), Some(98001));
    assert_eq!(ctx.now_unix(), 1_700_000_123);
    assert_eq!(ctx.service_role_mask, 0x8000_0000);

    let ctx_wire = AuthoritativeContext::decode_proto(&ctx.encode_proto()).unwrap();
    assert_eq!(ctx_wire.effective_user_id(), Some(-42));
    assert_eq!(ctx_wire.effective_character_id(), Some(90001));
    assert_eq!(ctx_wire.effective_corporation_id(), Some(98001));
    assert_eq!(ctx_wire.corp_role_mask, 0x55AA);
    assert_eq!(ctx_wire.now_unix(), 1_700_000_123);

    let anon = AuthoritativeContext::for_session(None, None, None, 0xFF, "singularity", vec![]);
    assert_eq!(anon.effective_user_id(), None);
    assert_eq!(anon.effective_character_id(), None);
    assert_eq!(anon.effective_corporation_id(), None);
    assert_eq!(anon.corp_role_mask, 0);
    let anon_rt = AuthoritativeContext::decode_proto(&anon.encode_proto()).unwrap();
    assert!(anon_rt.no_authenticated_user);
    assert!(anon_rt.no_active_character);
    assert!(anon_rt.no_active_identity);
    assert_eq!(anon_rt.effective_user_id(), None);
    assert_eq!(anon_rt.effective_character_id(), None);

    // Public Request & Response
    let pub_req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: 100, nanos: 1 }),
        correlation_uuid: vec![0xAA; 16],
        payload: Some(GoogleEmpty.to_any()),
        external_origin: "client".into(),
        application_instance_uuid: vec![0xBB; 16],
        authoritative_context: Some(ctx_wire.clone()),
        trace_context: Some(TraceContext {
            trace_id: vec![1, 2],
            ..Default::default()
        }),
    };
    let pub_req_rt = EvePublicRequest::decode_proto(&pub_req.encode_proto()).unwrap();
    assert_eq!(pub_req_rt.correlation_uuid, pub_req.correlation_uuid);
    assert_eq!(pub_req_rt.external_origin, "client");
    assert_eq!(pub_req_rt.application_instance_uuid, vec![0xBB; 16]);
    assert!(pub_req_rt.authoritative_context.is_some());
    assert!(pub_req_rt.trace_context.is_some());

    let pub_rsp = EvePublicResponse {
        dispatched: Some(GoogleTimestamp { seconds: 101, nanos: 2 }),
        correlation_uuid: vec![0xCC; 16],
        status_code: 200,
        status_message: "OK".into(),
        payload: Some(GoogleEmpty.to_any()),
        internal_origin: "backend".into(),
        application_instance_uuid: vec![0xDD; 4],
        gateway_instance_uuid: vec![0xEE; 4],
        trace_context: Some(TraceContext::default()),
    };
    assert_eq!(
        EvePublicResponse::decode_proto(&pub_rsp.encode_proto()).unwrap(),
        pub_rsp
    );

    // All 7 NoticeTargetGroup variants
    let targets = [
        NoticeTargetGroup::ApplicationInstanceUuid(vec![1, 2]),
        NoticeTargetGroup::SolarSystem(30000142),
        NoticeTargetGroup::User(-7),
        NoticeTargetGroup::Character(2110000001),
        NoticeTargetGroup::Corporation(98000001),
        NoticeTargetGroup::Alliance(99000001),
        NoticeTargetGroup::BubbleInstanceUuid(vec![9, 10]),
    ];
    let expected_names = [
        "application_instance_uuid",
        "solar_system",
        "user",
        "character",
        "corporation",
        "alliance",
        "bubble_instance_uuid",
    ];
    for (tg, ename) in targets.iter().zip(expected_names.iter()) {
        assert_eq!(tg.target_group_name(), *ename);
        assert_eq!(
            NoticeTargetGroup::decode_proto(&tg.encode_proto()).unwrap(),
            *tg
        );
    }

    let notice = EvePublicNotice {
        dispatched: Some(GoogleTimestamp { seconds: 50, nanos: 0 }),
        uuid: vec![7; 16],
        internal_origin: "svc".into(),
        tenant: "tq".into(),
        payload: Some(GoogleEmpty.to_any()),
        target_group: Some(NoticeTargetGroup::SolarSystem(30000142)),
        trace_context: Some(TraceContext::default()),
    };
    assert_eq!(
        EvePublicNotice::decode_proto(&notice.encode_proto()).unwrap(),
        notice
    );

    let event = EvePublicEvent {
        occurred: Some(GoogleTimestamp { seconds: 60, nanos: 0 }),
        uuid: vec![8; 16],
        payload: Some(GoogleEmpty.to_any()),
        journey: vec![1, 3],
        external_origin: "gw".into(),
        authoritative_context: None,
        application_instance_uuid: vec![4, 5],
        trace_context: Some(TraceContext::default()),
    };
    assert_eq!(
        EvePublicEvent::decode_proto(&event.encode_proto()).unwrap(),
        event
    );

    // Internal request / response / subscription envelopes
    let ireq = EveInternalRequest {
        correlation_uuid: "corr-1".into(),
        payload: Some(GoogleEmpty.to_any()),
        language: 1033,
        tenant: "tq".into(),
        publisher: "pub".into(),
        forwarded_for: "127.0.0.1".into(),
        trace_context: Some(TraceContext::default()),
        ignore_cache: true,
        created: Some(GoogleTimestamp { seconds: 70, nanos: 0 }),
    };
    assert_eq!(
        EveInternalRequest::decode_proto(&ireq.encode_proto()).unwrap(),
        ireq
    );

    let irsp = EveInternalResponse {
        correlation_uuid: "corr-1".into(),
        status_code: 201,
        status_message: "Created".into(),
        payload: Some(GoogleEmpty.to_any()),
        tenant: "tq".into(),
        publisher: "svc".into(),
        payload_not_cached: true,
        payload_cache_info: Some(EveResponseCacheInfo {
            cached_on: Some(GoogleTimestamp { seconds: 10, nanos: 0 }),
            cached_until: Some(GoogleTimestamp { seconds: 20, nanos: 0 }),
        }),
        payload_not_paged: true,
        payload_paging_info: Some(EveResponsePagingInfo {
            total_pages_count: 5,
        }),
        recipient: "client".into(),
        created: Some(GoogleTimestamp { seconds: 15, nanos: 0 }),
        trace_context: Some(TraceContext::default()),
    };
    assert_eq!(
        EveInternalResponse::decode_proto(&irsp.encode_proto()).unwrap(),
        irsp
    );

    let igw_req = InternalGatewayRequest {
        ack_id: 99,
        request: Some(pub_req_rt.clone()),
    };
    let igw_req_rt = InternalGatewayRequest::decode_proto(&igw_req.encode_proto()).unwrap();
    assert_eq!(igw_req_rt.ack_id, 99);
    assert!(igw_req_rt.request.is_some());

    let sub_info = InternalGatewayRequestsSubscriptionInfo {
        bind_requests: vec![EvePublicRequest {
            external_origin: "bind".into(),
            ..Default::default()
        }],
        consumer: Some(InternalGatewayConsumerInfo {
            name: "market".into(),
            context: "live".into(),
            tag: "v1".into(),
        }),
        in_flight_max: 64,
        unbind_requests: vec![EvePublicRequest {
            external_origin: "unbind".into(),
            ..Default::default()
        }],
    };
    let igw_rsp = InternalGatewayResponse {
        response: Some(pub_rsp.clone()),
        ack_id: 100,
        subscription: Some(sub_info),
    };
    let igw_rsp_rt = InternalGatewayResponse::decode_proto(&igw_rsp.encode_proto()).unwrap();
    assert_eq!(igw_rsp_rt, igw_rsp);

    // quasar_wire helpers
    let mut vbuf = Vec::new();
    write_varint(300, &mut vbuf);
    assert_eq!(read_varint(&mut vbuf.as_slice()), Ok(300));
    assert_eq!(read_varint(&mut [].as_slice()), Err(ProtoError::UnexpectedEof));
    assert_eq!(read_varint(&mut [0x80; 10].as_slice()), Err(ProtoError::UnexpectedEof));

    let fields = vec![
        (1, ProtoWireValue::Varint(42)),
        (2, ProtoWireValue::Fixed64(99)),
        (3, ProtoWireValue::Bytes(b"xyz".to_vec())),
        (4, ProtoWireValue::Fixed32(12)),
    ];
    let enc_f = encode_fields(&fields);
    assert_eq!(decode_fields(&enc_f).unwrap(), fields);
    assert_eq!(decode_fields(&[0x00]), Err(ProtoError::UnsupportedOpcode(0)));
    assert_eq!(decode_fields(&[0x0B]), Err(ProtoError::UnsupportedOpcode(3)));
    assert_eq!(decode_fields(&[0x09, 0x01]), Err(ProtoError::UnexpectedEof));
    assert_eq!(decode_fields(&[0x0D, 0x01]), Err(ProtoError::UnexpectedEof));
    assert_eq!(decode_fields(&[0x0A, 0x05, 0x01]), Err(ProtoError::UnexpectedEof));

    let qenv = QuasarGatewayEnvelope {
        type_url: "type.googleapis.com/test.Msg".into(),
        correlation_id: 12345,
        status_code: 200,
        payload: vec![1, 2, 3],
    };
    assert_eq!(decode_envelope(&encode_envelope(&qenv)).unwrap(), qenv);

    // GatewayFrame binary & PyRep roundtrips + errors
    assert_eq!(GatewayFrameKind::from_u64(1), Some(GatewayFrameKind::Request));
    assert_eq!(GatewayFrameKind::from_u64(2), Some(GatewayFrameKind::Response));
    assert_eq!(GatewayFrameKind::from_u64(3), Some(GatewayFrameKind::Notice));
    assert_eq!(GatewayFrameKind::from_u64(9), None);

    let gframe = GatewayFrame {
        kind: GatewayFrameKind::Request,
        correlation_id: 77,
        type_name: "eve_public.character.GetRequest".into(),
        status_code: 0,
        actor_character_id: Some(-55),
        idempotency_key: Some("idem-1".into()),
        payload: vec![0xDE, 0xAD],
    };
    let gw_bytes = gframe.encode_protobuf_wire();
    assert_eq!(GatewayFrame::decode_protobuf_wire(&gw_bytes).unwrap(), gframe);
    let gw_rep = gframe.to_pyrep();
    assert_eq!(GatewayFrame::from_pyrep(&gw_rep).unwrap(), gframe);
    assert!(GatewayFrame::decode_protobuf_wire(&[]).is_err());
    assert!(GatewayFrame::decode_protobuf_wire(&[0x0F]).is_err());
    assert!(GatewayFrame::from_pyrep(&PyRep::None).is_err());
    assert!(GatewayFrame::from_pyrep(&PyRep::Tuple(vec![])).is_err());
}
