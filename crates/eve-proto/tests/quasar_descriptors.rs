//! Comprehensive integration tests for `Q-1` (V24.01 Quasar Protobuf extraction, 1,455-file
//! `FileDescriptorProto` registry, dynamic wire round-tripping across all 6,741 message types,
//! static core primitive serialization, and deterministic Rust codegen).

use eve_proto::quasar::{
    ActiveIdentity, AuthoritativeContext, ColorRgb, DeviceUserIdentifier, DynamicValue,
    EveIdempotencyToken, EveInternalRequest, EveInternalResponse, EveIpAddress,
    EvePublicCharacterId, EvePublicCorpRoleCollection, EvePublicCorporationId, EvePublicEvent,
    EvePublicNotice, EvePublicRequest, EvePublicResponse, EvePublicUserId, FileDescriptorProto,
    GoogleAny, GoogleDuration, GoogleEmpty, GoogleTimestamp, InternalGatewayRequest,
    InternalGatewayResponse, MathFraction, MathVector3, MathVector4, NetworkRttSample,
    NoticeTargetGroup, QuasarDescriptorRegistry, QuasarError, QuasarMessage, SemanticVersionSpec,
    TraceContext, TraceFlags, TraceState, WireMessage, EXPECTED_QUASAR_DOMAIN_COUNT,
    EXPECTED_QUASAR_FILE_COUNT,
};
use std::collections::BTreeMap;

const EMBEDDED_QUASAR_CATALOG_JSON: &str =
    include_str!("../../../docs/index/quasar_protobuf_catalog.json");

#[test]
fn q1_all_1455_descriptors_across_135_domains_load_and_match_manifest() {
    let reg = QuasarDescriptorRegistry::open_default()
        .expect("load 1,455 Quasar FileDescriptorProto schemas from data/quasar_descriptors.sqlite");

    assert_eq!(reg.files.len(), EXPECTED_QUASAR_FILE_COUNT);
    assert_eq!(reg.manifest.descriptor_file_count, 1_455);
    assert_eq!(reg.files_by_domain.len(), EXPECTED_QUASAR_DOMAIN_COUNT);
    assert_eq!(reg.manifest.domain_count, 135);
    assert_eq!(reg.manifest.unique_package_count, 1_270);
    assert_eq!(reg.manifest.top_level_message_count, 5_786);
    assert_eq!(reg.manifest.total_message_count, 6_741);
    assert_eq!(reg.manifest.total_enum_count, 197);
    assert_eq!(reg.manifest.total_service_count, 10);
    assert_eq!(reg.manifest.total_field_count, 15_453);
    assert_eq!(reg.manifest.total_serialized_pb_bytes, 1_260_412);
    assert_eq!(
        reg.manifest.combined_serialized_pb_sha256,
        "06a2d8bde8222e444dcb740932cc3ef5e208bb853693d33a6a48ec8664f068be"
    );
    assert_eq!(
        reg.manifest.combined_serialized_pb_md5,
        "318048feebdaee6d35565c0e2d5b284c"
    );

    // Every single entry in docs/index/quasar_protobuf_catalog.json must be present in the registry
    let catalog: BTreeMap<String, Vec<String>> =
        serde_json::from_str(EMBEDDED_QUASAR_CATALOG_JSON).expect("parse quasar catalog");
    assert_eq!(catalog.len(), 135);
    let mut total_catalog_files = 0usize;
    for (domain, paths) in &catalog {
        let domain_indices = reg
            .files_by_domain
            .get(domain)
            .unwrap_or_else(|| panic!("Domain {domain} missing from QuasarDescriptorRegistry"));
        assert_eq!(
            domain_indices.len(),
            paths.len(),
            "Domain {domain} file count mismatch"
        );
        for pyj_path in paths {
            total_catalog_files += 1;
            let fd = reg.get_file_by_pyj(pyj_path).unwrap_or_else(|| {
                panic!("Cataloged pyj_path {pyj_path} missing from QuasarDescriptorRegistry")
            });
            assert_eq!(&fd.domain, domain);
            assert!(!fd.name.is_empty());
            assert!(!fd.raw_bytes.is_empty());

            // Verify FileDescriptorProto wire re-encoding round-trips cleanly
            let reencoded_wire = fd.encode_wire();
            let reparsed =
                FileDescriptorProto::decode_wire(&fd.pyj_path, &fd.domain, reencoded_wire)
                    .expect("re-parse encoded FileDescriptorProto");
            assert_eq!(reparsed.name, fd.name);
            assert_eq!(reparsed.package, fd.package);
            assert_eq!(reparsed.syntax, fd.syntax);
            assert_eq!(reparsed.dependencies, fd.dependencies);
            assert_eq!(reparsed.messages.len(), fd.messages.len());
            assert_eq!(reparsed.enums.len(), fd.enums.len());
            assert_eq!(reparsed.services.len(), fd.services.len());
        }
    }
    assert_eq!(total_catalog_files, 1_455);
}

#[test]
fn q1_all_6741_quasar_messages_instantiate_encode_and_decode_roundtrip() {
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    let mut message_count = 0usize;
    let mut packed_repeated_field_count = 0usize;
    for (file_idx, file) in reg.files.iter().enumerate() {
        fn check_msg_tree(
            reg: &QuasarDescriptorRegistry,
            m: &eve_proto::quasar::DescriptorProto,
            seed: u64,
            counter: &mut usize,
            packed_counter: &mut usize,
        ) {
            *counter += 1;
            let sample = reg
                .instantiate_populated_sample(&m.full_name, seed)
                .unwrap_or_else(|e| panic!("Failed to instantiate {}: {e}", m.full_name));
            let wire = reg
                .encode_dynamic(&sample)
                .unwrap_or_else(|e| panic!("Failed to encode {}: {e}", m.full_name));
            let wm = WireMessage::decode(&wire).expect("decode WireMessage");
            for fdesc in &m.fields {
                if fdesc.label == eve_proto::quasar::FieldLabel::Repeated
                    && fdesc.field_type.is_packable_scalar()
                    && sample.fields.contains_key(&fdesc.number)
                {
                    *packed_counter += 1;
                    let matching: Vec<_> = wm
                        .fields
                        .iter()
                        .filter(|wf| wf.field_number == fdesc.number)
                        .collect();
                    assert_eq!(
                        matching.len(),
                        1,
                        "Proto3 packable repeated scalar {}.{} must emit a single packed wire field",
                        m.full_name,
                        fdesc.name
                    );
                    assert_eq!(
                        matching[0].value.wire_type(),
                        eve_proto::quasar::WireType::LengthDelimited,
                        "Proto3 packable repeated scalar {}.{} must use WireType::LengthDelimited",
                        m.full_name,
                        fdesc.name
                    );
                }
            }
            let decoded = reg
                .decode_dynamic(&m.full_name, &wire)
                .unwrap_or_else(|e| panic!("Failed to decode {}: {e}", m.full_name));
            assert_eq!(
                decoded, sample,
                "Round-trip mismatch for message {}",
                m.full_name
            );

            // Also verify GoogleAny packing and unpacking for every message
            let any = GoogleAny {
                type_url: sample.type_url(),
                value: wire,
            };
            let any_wire = any.encode_proto();
            let any_decoded = GoogleAny::decode_proto(&any_wire).expect("decode GoogleAny");
            assert_eq!(any_decoded.normalized_type_name(), m.full_name);

            for (sub_idx, nested) in m.nested_types.iter().enumerate() {
                check_msg_tree(
                    reg,
                    nested,
                    seed.wrapping_add(sub_idx as u64 + 17),
                    counter,
                    packed_counter,
                );
            }
        }

        for (msg_idx, m) in file.messages.iter().enumerate() {
            let seed = ((file_idx as u64) << 8) ^ (msg_idx as u64 + 1);
            check_msg_tree(&reg, m, seed, &mut message_count, &mut packed_repeated_field_count);
        }

        // Verify deterministic Rust codegen for every single file
        let rust_src = QuasarDescriptorRegistry::generate_rust_module_for_file(file);
        assert!(rust_src.contains(&file.name));
        for m in &file.messages {
            assert!(
                rust_src.contains(&format!("pub struct {}", m.name)),
                "Generated Rust module for {} missing struct {}",
                file.pyj_path,
                m.name
            );
        }
    }

    assert_eq!(
        message_count, 6_741,
        "All 6,741 Quasar message descriptors across 1,455 files must be round-trip verified"
    );
    assert!(
        packed_repeated_field_count > 0,
        "Expected proto3 packed repeated scalar fields across 6,741 messages"
    );
}

#[test]
fn q1_static_core_types_and_gateway_envelopes_wire_compat_with_v24_descriptors() {
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    // 1. User ID uses sint32 (ZigZag varint) in eve_public.user.Identifier
    let user_id = EvePublicUserId {
        sequential: 5_000_001,
    };
    let user_wire = user_id.encode_proto();
    assert_eq!(
        EvePublicUserId::decode_proto(&user_wire).unwrap(),
        user_id
    );
    let dyn_user = reg
        .decode_dynamic("eve_public.user.Identifier", &user_wire)
        .expect("dynamic decode eve_public.user.Identifier");
    assert_eq!(dyn_user.get(1), Some(&DynamicValue::Sint32(5_000_001)));

    // 2. Character, Corporation, DeviceUser identifiers
    let char_id = EvePublicCharacterId {
        sequential: 90_000_001,
    };
    let corp_id = EvePublicCorporationId {
        sequential: 98_000_001,
    };
    let dev_user = DeviceUserIdentifier {
        device: b"device-uuid-0001".to_vec(),
        user: Some(user_id),
    };
    assert_eq!(
        EvePublicCharacterId::decode_proto(&char_id.encode_proto()).unwrap(),
        char_id
    );
    assert_eq!(
        EvePublicCorporationId::decode_proto(&corp_id.encode_proto()).unwrap(),
        corp_id
    );
    assert_eq!(
        DeviceUserIdentifier::decode_proto(&dev_user.encode_proto()).unwrap(),
        dev_user
    );

    // 3. Shared primitives: MathFraction, MathVector3, MathVector4, ColorRgb, SemanticVersionSpec, EveIpAddress, TraceContext, EveIdempotencyToken, NetworkRttSample
    let frac = MathFraction {
        numerator: 1_250_000,
        denominator: 100,
    };
    let frac_wire = frac.encode_proto();
    assert_eq!(MathFraction::decode_proto(&frac_wire).unwrap(), frac);
    let dyn_frac = reg
        .decode_dynamic("eve_public.math.Fraction", &frac_wire)
        .unwrap();
    assert_eq!(dyn_frac.get(1), Some(&DynamicValue::Uint64(1_250_000)));
    assert_eq!(dyn_frac.get(2), Some(&DynamicValue::Uint64(100)));

    let vec3 = MathVector3 {
        x: 125_000.5,
        y: -42.25,
        z: 9_876.125,
    };
    let vec3_wire = vec3.encode_proto();
    assert_eq!(MathVector3::decode_proto(&vec3_wire).unwrap(), vec3);
    let dyn_vec3 = reg
        .decode_dynamic("eve_public.math.Vector3", &vec3_wire)
        .unwrap();
    assert_eq!(dyn_vec3.get(1), Some(&DynamicValue::Double(125_000.5)));

    let vec4 = MathVector4 {
        x: 1.0,
        y: 2.0,
        z: 3.0,
        w: 4.0,
    };
    assert_eq!(
        MathVector4::decode_proto(&vec4.encode_proto()).unwrap(),
        vec4
    );

    let color = ColorRgb {
        red: 64,
        green: 128,
        blue: 255,
    };
    let color_wire = color.encode_proto();
    assert_eq!(ColorRgb::decode_proto(&color_wire).unwrap(), color);
    let dyn_color = reg
        .decode_dynamic("eve_public.color.RGB", &color_wire)
        .unwrap();
    assert_eq!(dyn_color.get(2), Some(&DynamicValue::Uint32(128)));

    let semver = SemanticVersionSpec {
        major: 24,
        minor: 1,
        patch: 0,
        prerelease_tags: vec!["tq".into()],
        build_tags: vec!["2548602".into()],
    };
    let semver_wire = semver.encode_proto();
    assert_eq!(
        SemanticVersionSpec::decode_proto(&semver_wire).unwrap(),
        semver
    );

    let ipv4 = EveIpAddress::V4(0x7F00_0001);
    let ipv4_wire = ipv4.encode_proto();
    assert_eq!(EveIpAddress::decode_proto(&ipv4_wire).unwrap(), ipv4);
    let dyn_ip = reg.decode_dynamic("eve_public.IPAddress", &ipv4_wire).unwrap();
    assert_eq!(dyn_ip.get(1), Some(&DynamicValue::Fixed32(0x7F00_0001)));

    let trace = TraceContext {
        trace_id: b"0123456789abcdef".to_vec(),
        parent_id: b"fedcba9876543210".to_vec(),
        flags: Some(TraceFlags { sampled: true }),
        state: Some(TraceState { sample_rate: 100 }),
    };
    assert_eq!(
        TraceContext::decode_proto(&trace.encode_proto()).unwrap(),
        trace
    );

    let idem = EveIdempotencyToken {
        uuid: b"idem-uuid-123456".to_vec(),
    };
    assert_eq!(
        EveIdempotencyToken::decode_proto(&idem.encode_proto()).unwrap(),
        idem
    );

    let dur = GoogleDuration {
        seconds: 120,
        nanos: 500_000_000,
    };
    assert_eq!(
        GoogleDuration::decode_proto(&dur.encode_proto()).unwrap(),
        dur
    );
    let rtt = NetworkRttSample {
        protocol: 1,
        rtt: Some(dur),
    };
    assert_eq!(
        NetworkRttSample::decode_proto(&rtt.encode_proto()).unwrap(),
        rtt
    );
    assert_eq!(
        GoogleEmpty::decode_proto(&GoogleEmpty.encode_proto()).unwrap(),
        GoogleEmpty
    );

    // 4. Gateway envelopes: AuthoritativeContext, EvePublicRequest, EvePublicResponse, EvePublicNotice, EvePublicEvent
    let auth_ctx = AuthoritativeContext::for_session(
        Some(5_000_001),
        Some(90_000_001),
        Some(98_000_001),
        128,
        "tranquility",
        b"gw-instance-0001".to_vec(),
    );
    let auth_wire = auth_ctx.encode_proto();
    let auth_dec = AuthoritativeContext::decode_proto(&auth_wire).unwrap();
    assert_eq!(auth_dec, auth_ctx);
    let dyn_auth = reg
        .decode_dynamic("eve_public.AuthoritativeContext", &auth_wire)
        .expect("dynamic decode AuthoritativeContext");
    assert_eq!(
        dyn_auth.get(3),
        Some(&DynamicValue::String("tranquility".into()))
    );

    let pub_req = EvePublicRequest {
        issued: Some(GoogleTimestamp {
            seconds: 1_760_000_000,
            nanos: 123,
        }),
        correlation_uuid: b"corr-uuid-000001".to_vec(),
        payload: Some(char_id.to_any()),
        external_origin: "eve-client".into(),
        application_instance_uuid: b"app-inst-0000001".to_vec(),
        authoritative_context: Some(auth_ctx.clone()),
        trace_context: Some(trace.clone()),
    };
    let req_wire = pub_req.encode_proto();
    assert_eq!(EvePublicRequest::decode_proto(&req_wire).unwrap(), pub_req);
    let dyn_req = reg
        .decode_dynamic("eve_public.Request", &req_wire)
        .expect("dynamic decode eve_public.Request");
    assert_eq!(
        dyn_req.get(2),
        Some(&DynamicValue::Bytes(b"corr-uuid-000001".to_vec()))
    );

    let pub_rsp = EvePublicResponse {
        dispatched: Some(GoogleTimestamp {
            seconds: 1_760_000_001,
            nanos: 456,
        }),
        correlation_uuid: b"corr-uuid-000001".to_vec(),
        status_code: 200,
        status_message: "OK".into(),
        payload: Some(char_id.to_any()),
        internal_origin: "evomu.quasar".into(),
        application_instance_uuid: b"app-inst-0000001".to_vec(),
        gateway_instance_uuid: b"gw-instance-0001".to_vec(),
        trace_context: Some(trace.clone()),
    };
    let rsp_wire = pub_rsp.encode_proto();
    assert_eq!(EvePublicResponse::decode_proto(&rsp_wire).unwrap(), pub_rsp);
    let dyn_rsp = reg
        .decode_dynamic("eve_public.Response", &rsp_wire)
        .expect("dynamic decode eve_public.Response");
    assert_eq!(dyn_rsp.get(3), Some(&DynamicValue::Uint32(200)));

    for tg in [
        NoticeTargetGroup::ApplicationInstanceUuid(b"app-uuid-1234567".to_vec()),
        NoticeTargetGroup::SolarSystem(30_000_142),
        NoticeTargetGroup::User(5_000_001),
        NoticeTargetGroup::Character(90_000_001),
        NoticeTargetGroup::Corporation(98_000_001),
        NoticeTargetGroup::Alliance(99_000_001),
        NoticeTargetGroup::BubbleInstanceUuid(b"bubble-uuid-1234".to_vec()),
    ] {
        let notice = EvePublicNotice {
            dispatched: Some(GoogleTimestamp {
                seconds: 1_760_000_002,
                nanos: 0,
            }),
            uuid: b"notice-uuid-0001".to_vec(),
            internal_origin: "evomu.notice".into(),
            tenant: "tranquility".into(),
            payload: Some(char_id.to_any()),
            target_group: Some(tg),
            trace_context: Some(trace.clone()),
        };
        let notice_wire = notice.encode_proto();
        assert_eq!(EvePublicNotice::decode_proto(&notice_wire).unwrap(), notice);
        let _ = reg
            .decode_dynamic("eve_public.Notice", &notice_wire)
            .expect("dynamic decode eve_public.Notice");
    }

    let ev = EvePublicEvent {
        occurred: Some(GoogleTimestamp {
            seconds: 1_760_000_003,
            nanos: 0,
        }),
        uuid: b"event-uuid-00001".to_vec(),
        payload: Some(GoogleEmpty.to_any()),
        journey: b"journey-uuid-001".to_vec(),
        external_origin: "client".into(),
        authoritative_context: Some(auth_ctx),
        application_instance_uuid: b"app-inst-0000001".to_vec(),
        trace_context: Some(trace.clone()),
    };
    let ev_wire = ev.encode_proto();
    assert_eq!(EvePublicEvent::decode_proto(&ev_wire).unwrap(), ev);
    let _ = reg
        .decode_dynamic("eve_public.Event", &ev_wire)
        .expect("dynamic decode eve_public.Event");

    let int_req = InternalGatewayRequest {
        ack_id: 42,
        request: Some(pub_req),
    };
    let int_req_wire = int_req.encode_proto();
    assert_eq!(
        InternalGatewayRequest::decode_proto(&int_req_wire).unwrap(),
        int_req
    );
    let _ = reg
        .decode_dynamic("eve_public.internal_gateway.Request", &int_req_wire)
        .unwrap();

    let int_rsp = InternalGatewayResponse {
        response: Some(pub_rsp),
        ack_id: 42,
    };
    let int_rsp_wire = int_rsp.encode_proto();
    assert_eq!(
        InternalGatewayResponse::decode_proto(&int_rsp_wire).unwrap(),
        int_rsp
    );
    let _ = reg
        .decode_dynamic("eve_public.internal_gateway.Response", &int_rsp_wire)
        .unwrap();

    let eve_req = EveInternalRequest {
        correlation_uuid: "corr-str-1".into(),
        payload: Some(GoogleEmpty.to_any()),
        language: 1,
        tenant: "tranquility".into(),
        publisher: "evomu".into(),
        forwarded_for: "127.0.0.1".into(),
        trace_context: Some(trace.clone()),
        ignore_cache: true,
        created: Some(GoogleTimestamp {
            seconds: 1_760_000_004,
            nanos: 0,
        }),
    };
    let eve_req_wire = eve_req.encode_proto();
    assert_eq!(
        EveInternalRequest::decode_proto(&eve_req_wire).unwrap(),
        eve_req
    );
    let _ = reg.decode_dynamic("eve.Request", &eve_req_wire).unwrap();

    let eve_rsp = EveInternalResponse {
        correlation_uuid: "corr-str-1".into(),
        status_code: 200,
        status_message: "OK".into(),
        payload: Some(GoogleEmpty.to_any()),
        tenant: "tranquility".into(),
        publisher: "evomu".into(),
        payload_not_cached: true,
        payload_not_paged: true,
        recipient: "client".into(),
        created: Some(GoogleTimestamp {
            seconds: 1_760_000_005,
            nanos: 0,
        }),
        trace_context: Some(trace),
    };
    let eve_rsp_wire = eve_rsp.encode_proto();
    assert_eq!(
        EveInternalResponse::decode_proto(&eve_rsp_wire).unwrap(),
        eve_rsp
    );
    let _ = reg.decode_dynamic("eve.Response", &eve_rsp_wire).unwrap();
}

#[test]
fn q1_rejects_malformed_wire_frames_and_wire_type_mismatches() {
    let reg = QuasarDescriptorRegistry::open_default().expect("open QuasarDescriptorRegistry");

    // Truncated length-delimited field
    let truncated = [0x0A, 0x10, 0x01, 0x02];
    assert_eq!(
        WireMessage::decode(&truncated),
        Err(QuasarError::UnexpectedEof)
    );

    // Invalid wire type 3 (StartGroup)
    let bad_wt = [0x0B];
    assert_eq!(
        WireMessage::decode(&bad_wt),
        Err(QuasarError::InvalidWireType(3))
    );

    // Overflowing varint (>10 bytes with MSB set)
    let bad_varint = [0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01];
    assert_eq!(
        WireMessage::decode(&bad_varint),
        Err(QuasarError::VarintOverflow)
    );

    // Wire type mismatch against schema: field 1 of eve_public.character.Identifier is uint32 (Varint), send LengthDelimited instead
    let wrong_schema_wt = [0x0A, 0x02, b'a', b'b'];
    let err = reg
        .decode_dynamic("eve_public.character.Identifier", &wrong_schema_wt)
        .unwrap_err();
    assert!(matches!(err, QuasarError::WireTypeMismatch { number: 1, .. }));

    // ActiveIdentity round-trip
    let active_id = ActiveIdentity {
        character: Some(EvePublicCharacterId {
            sequential: 90_000_001,
        }),
        corporation: Some(EvePublicCorporationId {
            sequential: 98_000_001,
        }),
        roles: Some(EvePublicCorpRoleCollection { bitmask: 7 }),
    };
    assert_eq!(
        ActiveIdentity::decode_proto(&active_id.encode_proto()).unwrap(),
        active_id
    );
}
