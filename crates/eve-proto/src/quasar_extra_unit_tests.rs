//! Deep branch-coverage and mutation-killing unit tests for `quasar` (`crates/eve-proto/src/quasar.rs`).

use crate::quasar::*;
use crate::{MAX_FRAME_SIZE, MAX_RECURSION_DEPTH};

#[test]
fn quasar_type_url_helpers_and_error_display_messages() {
    assert_eq!(TYPE_GOOGLEAPIS_PREFIX, "type.googleapis.com/");
    assert_eq!(TYPE_EVETECH_PREFIX, "type.evetech.net/");

    // Exact prefix boundaries
    assert!(has_quasar_type_url_prefix("type.googleapis.com/"));
    assert!(has_quasar_type_url_prefix("type.evetech.net/"));
    assert!(!has_quasar_type_url_prefix("type.googleapis.com"));
    assert!(!has_quasar_type_url_prefix("type.evetech.net"));
    assert!(!has_quasar_type_url_prefix("https://type.googleapis.com/eve.Req"));

    assert_eq!(
        strip_quasar_type_url_prefix("type.googleapis.com/...eve_public.Request"),
        "eve_public.Request"
    );
    assert_eq!(
        strip_quasar_type_url_prefix("type.evetech.net/..eve_public.Event"),
        "eve_public.Event"
    );
    assert_eq!(
        strip_quasar_type_url_prefix("...eve.Internal"),
        "eve.Internal"
    );
    assert_eq!(strip_quasar_type_url_prefix(""), "");

    // QuasarError Display formatting
    let errs = [
        (QuasarError::UnexpectedEof, "Unexpected EOF"),
        (QuasarError::VarintOverflow, "64-bit integer bounds"),
        (QuasarError::InvalidWireType(6), "Invalid protobuf wire type: 6"),
        (QuasarError::FrameTooLarge(9_999_999), "exceeds maximum allowed"),
        (QuasarError::RecursionLimitExceeded, "recursion depth"),
        (QuasarError::CollectionTooLarge(500_000), "collection length 500000"),
        (QuasarError::InvalidUtf8("bad byte".into()), "bad byte"),
        (
            QuasarError::WireTypeMismatch {
                field: "solar_system".into(),
                number: 7,
                expected: WireType::Varint,
                actual: WireType::Fixed64,
            },
            "solar_system",
        ),
        (QuasarError::UnknownMessageType("foo.Bar".into()), "foo.Bar"),
        (
            QuasarError::TypeUrlMismatch {
                expected: "e1".into(),
                actual: "a1".into(),
            },
            "expected 'e1', got 'a1'",
        ),
        (
            QuasarError::MissingDescriptorDatabase("/tmp/missing.sqlite".into()),
            "/tmp/missing.sqlite",
        ),
        (QuasarError::DatabaseError("locked".into()), "locked"),
        (QuasarError::ManifestMismatch("sha mismatch".into()), "sha mismatch"),
    ];
    for (err, needle) in errs {
        let rendered = err.to_string();
        assert!(
            rendered.contains(needle),
            "expected '{rendered}' to contain '{needle}'"
        );
    }
}

#[test]
fn sha256_and_md5_padding_block_boundaries() {
    // 55 bytes: 55 + 1 (0x80) + 8 (bit_len) = 64 bytes (single block exact fit)
    let msg_55 = vec![b'a'; 55];
    assert_eq!(
        sha256_hex(&msg_55),
        "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
    );
    assert_eq!(md5_hex(&msg_55), "ef1772b6dff9a122358552954ad0df65");
    assert_eq!(sha256_bytes(&msg_55).len(), 32);

    // 56 bytes: 56 + 1 = 57 != 56 -> spills into 2 blocks (128 bytes)
    let msg_56 = vec![b'a'; 56];
    assert_eq!(
        sha256_hex(&msg_56),
        "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
    );
    assert_eq!(md5_hex(&msg_56), "3b0c8ac703f828b04c6c197006d17218");

    // 64 bytes: exact 1 block of data + 1 full block of padding (128 bytes)
    let msg_64 = vec![b'a'; 64];
    assert_eq!(
        sha256_hex(&msg_64),
        "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
    );
    assert_eq!(md5_hex(&msg_64), "014842d480b571495a4a0363793f7367");
}

#[test]
fn varint_decode_64bit_boundaries_and_wire_message_edge_cases() {
    // 10-byte varint with 10th byte == 0x01 -> 1u64 << 63 (valid max high bit)
    let mut max_bit_buf = [0x80u8; 10];
    max_bit_buf[9] = 0x01;
    let mut pos = 0;
    assert_eq!(
        decode_varint(&max_bit_buf, &mut pos),
        Ok(1u64 << 63)
    );
    assert_eq!(pos, 10);

    // 10-byte varint with all 0xFF and 10th byte == 0x01 -> u64::MAX
    let mut u64_max_buf = [0xFFu8; 10];
    u64_max_buf[9] = 0x01;
    pos = 0;
    assert_eq!(decode_varint(&u64_max_buf, &mut pos), Ok(u64::MAX));
    assert_eq!(pos, 10);

    // 10-byte varint with continuation bit set on 10th byte (shift reaches 70) -> VarintOverflow
    let mut cont_10 = [0x80u8; 10];
    cont_10[9] = 0x81;
    pos = 0;
    assert_eq!(
        decode_varint(&cont_10, &mut pos),
        Err(QuasarError::VarintOverflow)
    );

    // Multi-varint stream starting at non-zero offset
    let mut stream = vec![0xAA];
    encode_varint(300, &mut stream);
    encode_varint(0, &mut stream);
    encode_varint(127, &mut stream);
    let mut cursor = 1;
    assert_eq!(decode_varint(&stream, &mut cursor), Ok(300));
    assert_eq!(decode_varint(&stream, &mut cursor), Ok(0));
    assert_eq!(decode_varint(&stream, &mut cursor), Ok(127));
    assert_eq!(cursor, stream.len());

    // WireType::from_u8 exhaustive invalid values
    for bad in [3u8, 4, 6, 7, 8, 255] {
        assert_eq!(
            WireType::from_u8(bad),
            Err(QuasarError::InvalidWireType(bad))
        );
    }

    // WireValue negative accessor checks across all variants
    let f64_val = WireValue::Fixed64(99);
    assert_eq!(f64_val.as_varint(), None);
    assert_eq!(f64_val.as_bytes(), None);
    assert_eq!(f64_val.as_str(), Ok(None));
    assert_eq!(f64_val.as_fixed32(), None);
    assert_eq!(f64_val.as_fixed64(), Some(99));

    let f32_val = WireValue::Fixed32(77);
    assert_eq!(f32_val.as_varint(), None);
    assert_eq!(f32_val.as_bytes(), None);
    assert_eq!(f32_val.as_str(), Ok(None));
    assert_eq!(f32_val.as_fixed32(), Some(77));
    assert_eq!(f32_val.as_fixed64(), None);

    let ld_val = WireValue::LengthDelimited(b"ok".to_vec());
    assert_eq!(ld_val.as_varint(), None);
    assert_eq!(ld_val.as_fixed32(), None);
    assert_eq!(ld_val.as_fixed64(), None);

    // Empty WireMessage
    let empty_wm = WireMessage::decode(&[]).unwrap();
    assert!(empty_wm.fields.is_empty());
    assert!(empty_wm.encode().is_empty());

    // Exact buffer length boundaries for Fixed64 (8 bytes), Fixed32 (4 bytes), LengthDelimited
    let mut exact_f64 = Vec::new();
    encode_fixed64_field(1, 0x0102_0304_0506_0708, &mut exact_f64);
    assert_eq!(
        WireMessage::decode(&exact_f64[..exact_f64.len() - 1]),
        Err(QuasarError::UnexpectedEof)
    );
    assert!(WireMessage::decode(&exact_f64).is_ok());

    let mut exact_f32 = Vec::new();
    encode_fixed32_field(2, 0x0102_0304, &mut exact_f32);
    assert_eq!(
        WireMessage::decode(&exact_f32[..exact_f32.len() - 1]),
        Err(QuasarError::UnexpectedEof)
    );
    assert!(WireMessage::decode(&exact_f32).is_ok());

    // LengthDelimited declaring length > MAX_FRAME_SIZE -> UnexpectedEof
    let mut huge_len_field = Vec::new();
    encode_tag(1, WireType::LengthDelimited, &mut huge_len_field);
    encode_varint((MAX_FRAME_SIZE + 1) as u64, &mut huge_len_field);
    assert_eq!(
        WireMessage::decode(&huge_len_field),
        Err(QuasarError::UnexpectedEof)
    );
}

#[test]
fn file_descriptor_proto_empty_package_streaming_variants_and_recursion_limit() {
    // 1. FileDescriptorProto with empty package and default proto2 syntax (when syntax is empty on encode)
    let fd_no_pkg = FileDescriptorProto {
        pyj_path: "root.pyj".into(),
        domain: "root".into(),
        name: "root.proto".into(),
        package: String::new(),
        syntax: String::new(),
        dependencies: vec![],
        messages: vec![DescriptorProto {
            name: "TopMsg".into(),
            full_name: "TopMsg".into(),
            fields: vec![FieldDescriptorProto {
                name: "raw".into(),
                number: 1,
                label: FieldLabel::Required,
                field_type: FieldType::Bytes,
                type_name: String::new(),
                oneof_index: None,
                json_name: String::new(),
            }],
            nested_types: vec![DescriptorProto {
                name: "Child".into(),
                full_name: "TopMsg.Child".into(),
                fields: vec![],
                nested_types: vec![],
                enum_types: vec![],
                oneofs: vec![],
                is_map_entry: false,
            }],
            enum_types: vec![],
            oneofs: vec![],
            is_map_entry: false,
        }],
        enums: vec![EnumDescriptorProto {
            name: "TopEnum".into(),
            full_name: "TopEnum".into(),
            values: vec![EnumValueDescriptorProto {
                name: "ZERO".into(),
                number: 0,
            }],
        }],
        services: vec![ServiceDescriptorProto {
            name: "TopSvc".into(),
            full_name: "TopSvc".into(),
            methods: vec![
                MethodDescriptorProto {
                    name: "Unary".into(),
                    input_type: "TopMsg".into(),
                    output_type: "TopMsg".into(),
                    client_streaming: false,
                    server_streaming: false,
                },
                MethodDescriptorProto {
                    name: "ClientStream".into(),
                    input_type: "TopMsg".into(),
                    output_type: "TopMsg".into(),
                    client_streaming: true,
                    server_streaming: false,
                },
                MethodDescriptorProto {
                    name: "ServerStream".into(),
                    input_type: "TopMsg".into(),
                    output_type: "TopMsg".into(),
                    client_streaming: false,
                    server_streaming: true,
                },
            ],
        }],
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: Vec::new(),
    };

    let wire = fd_no_pkg.encode_wire();
    let decoded = FileDescriptorProto::decode_wire("root.pyj", "root", wire).unwrap();
    assert_eq!(decoded.package, "");
    assert_eq!(decoded.syntax, "proto2");
    assert_eq!(decoded.messages[0].full_name, "TopMsg");
    assert_eq!(decoded.messages[0].nested_types[0].full_name, "TopMsg.Child");
    assert_eq!(decoded.enums[0].full_name, "TopEnum");
    assert_eq!(decoded.services[0].full_name, "TopSvc");
    assert_eq!(decoded.services[0].methods.len(), 3);
    assert!(!decoded.services[0].methods[0].client_streaming);
    assert!(!decoded.services[0].methods[0].server_streaming);
    assert!(decoded.services[0].methods[1].client_streaming);
    assert!(!decoded.services[0].methods[1].server_streaming);
    assert!(!decoded.services[0].methods[2].client_streaming);
    assert!(decoded.services[0].methods[2].server_streaming);

    // 2. Deeply nested DescriptorProto exceeding MAX_RECURSION_DEPTH (64) -> RecursionLimitExceeded
    let mut nested_bytes = Vec::new();
    encode_string_field(1, "Leaf", &mut nested_bytes);
    for depth in 0..=(MAX_RECURSION_DEPTH + 1) {
        let mut parent = Vec::new();
        encode_string_field(1, &format!("M{depth}"), &mut parent);
        encode_bytes_field(3, &nested_bytes, &mut parent);
        nested_bytes = parent;
    }
    let mut fd_deep = Vec::new();
    encode_string_field(1, "deep.proto", &mut fd_deep);
    encode_bytes_field(4, &nested_bytes, &mut fd_deep);
    assert_eq!(
        FileDescriptorProto::decode_wire("deep.pyj", "deep", fd_deep),
        Err(QuasarError::RecursionLimitExceeded)
    );
}

#[test]
fn quasar_registry_manifest_mismatch_oneof_packed_sint64_and_wire_fallback_edges() {
    // 1. Twice-called open_default() exercises OnceLock fast path (registry.rs L241)
    if let Ok(r1) = QuasarDescriptorRegistry::open_default() {
        let r2 = QuasarDescriptorRegistry::open_default().unwrap();
        assert_eq!(r1.files.len(), r2.files.len());
    }

    // 2. Real descriptor DB against mutated temp manifest files exercises L380-384, L390-393, L396-399
    let manifest_dir_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let sqlite_path = std::env::var("EVOMU_QUASAR_DESCRIPTORS_PATH")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| manifest_dir_root.join("data").join("quasar_descriptors.sqlite"));
    let manifest_src = sqlite_path
        .parent()
        .map(|p| p.join("quasar_descriptor_manifest.json"))
        .filter(|p| p.exists())
        .unwrap_or_else(|| manifest_dir_root.join("data").join("quasar_descriptor_manifest.json"));
    if sqlite_path.exists() && manifest_src.exists() {
        let raw_manifest = std::fs::read_to_string(&manifest_src).unwrap();
        let mut parsed: QuasarManifest = serde_json::from_str(&raw_manifest).unwrap();
        let tmp_dir = std::env::temp_dir().join("evomu_quasar_manifest_mismatch");
        let _ = std::fs::remove_dir_all(&tmp_dir);
        std::fs::create_dir_all(&tmp_dir).unwrap();

        // 2a. per_domain_file_counts mismatch (L380-384)
        let mut bad_domain = parsed.clone();
        bad_domain.per_domain_file_counts.insert("__bogus_domain__".into(), 1);
        let m1 = tmp_dir.join("m1.json");
        std::fs::write(&m1, serde_json::to_string(&bad_domain).unwrap()).unwrap();
        assert!(matches!(
            QuasarDescriptorRegistry::open(&sqlite_path, &m1),
            Err(QuasarError::ManifestMismatch(_))
        ));

        // 2b. combined SHA-256 mismatch (L390-393)
        let mut bad_sha = parsed.clone();
        bad_sha.combined_serialized_pb_sha256 = "0".repeat(64);
        let m2 = tmp_dir.join("m2.json");
        std::fs::write(&m2, serde_json::to_string(&bad_sha).unwrap()).unwrap();
        assert!(matches!(
            QuasarDescriptorRegistry::open(&sqlite_path, &m2),
            Err(QuasarError::ManifestMismatch(_))
        ));

        // 2c. combined MD5 mismatch (L396-399)
        parsed.combined_serialized_pb_md5 = "0".repeat(32);
        let m3 = tmp_dir.join("m3.json");
        std::fs::write(&m3, serde_json::to_string(&parsed).unwrap()).unwrap();
        assert!(matches!(
            QuasarDescriptorRegistry::open(&sqlite_path, &m3),
            Err(QuasarError::ManifestMismatch(_))
        ));

        let _ = std::fs::remove_dir_all(&tmp_dir);
    }

    // 3. Registry dynamic edge cases: unknown nested sample type (L563), wire fallback depth (L614)
    //    & control-char corrupt-wire Bytes fallback (L633), packed scalar mismatch (L735),
    //    decode_dynamic recursion limit (L764), packed sint64 truncated varint (L874),
    //    unpacked repeated push (L944).
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.messages_by_full_name.insert(
        "edge.UnknownChild".into(),
        DescriptorProto {
            name: "UnknownChild".into(),
            full_name: "edge.UnknownChild".into(),
            fields: vec![FieldDescriptorProto {
                name: "sub".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Message,
                type_name: "edge.DoesNotExist".into(),
                oneof_index: None,
                json_name: "sub".into(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );
    assert_eq!(
        reg.instantiate_populated_sample("edge.UnknownChild", 1),
        Err(QuasarError::UnknownMessageType("edge.DoesNotExist".into()))
    );

    // Wire fallback: valid UTF-8 with ASCII control byte + invalid protobuf submessage -> DynamicValue::Bytes (L633)
    let ctrl_non_pb = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(vec![0x01, 0x7F]), // tag field 0/1 invalid varint EOF
        }],
    }
    .encode();
    let fb = reg.decode_dynamic_or_wire("unknown.CtrlBytes", &ctrl_non_pb).unwrap();
    assert_eq!(fb.fields.get(&1), Some(&DynamicValue::Bytes(vec![0x01, 0x7F])));

    // Wire fallback recursion depth > MAX_RECURSION_DEPTH: hits L614 inside recursive call and
    // safely degrades the deepest element to DynamicValue::Bytes instead of overflowing stack
    let mut nested_fb = vec![0x01, 0x02]; // valid UTF-8 with control byte 0x01
    for _ in 0..=(MAX_RECURSION_DEPTH + 2) {
        let mut outer = Vec::new();
        encode_bytes_field(1, &nested_fb, &mut outer);
        nested_fb = outer;
    }
    let mut cur = reg
        .decode_dynamic_or_wire("unknown.Deep", &nested_fb)
        .expect("wire fallback caps recursion via Bytes fallback");
    for _ in 0..MAX_RECURSION_DEPTH {
        cur = match cur.fields.get(&1) {
            Some(DynamicValue::Message(sub)) => (**sub).clone(),
            other => panic!("expected nested Message before cap, got {other:?}"),
        };
    }
    assert!(matches!(cur.fields.get(&1), Some(DynamicValue::Bytes(_))));

    // Packed scalar mismatch inside Repeated (L735), truncated Sint64 packed varint (L874),
    // unpacked repeated string push (L944), and decode_dynamic recursion limit (L764)
    reg.messages_by_full_name.insert(
        "edge.Rich".into(),
        DescriptorProto {
            name: "Rich".into(),
            full_name: "edge.Rich".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "nums".into(),
                    number: 1,
                    label: FieldLabel::Repeated,
                    field_type: FieldType::Int32,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "nums".into(),
                },
                FieldDescriptorProto {
                    name: "s64s".into(),
                    number: 2,
                    label: FieldLabel::Repeated,
                    field_type: FieldType::Sint64,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "s64s".into(),
                },
                FieldDescriptorProto {
                    name: "tags".into(),
                    number: 3,
                    label: FieldLabel::Repeated,
                    field_type: FieldType::String,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "tags".into(),
                },
                FieldDescriptorProto {
                    name: "child".into(),
                    number: 4,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Message,
                    type_name: "edge.Rich".into(),
                    oneof_index: None,
                    json_name: "child".into(),
                },
            ],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );

    let mut dyn_msg = DynamicQuasarMessage::new("edge.Rich");
    dyn_msg.set(
        1,
        DynamicValue::Repeated(vec![
            DynamicValue::Int32(7),
            DynamicValue::String("ignored_mismatch".into()), // L735
        ]),
    );
    dyn_msg.set(
        3,
        DynamicValue::Repeated(vec![
            DynamicValue::String("one".into()),
            DynamicValue::String("two".into()), // hits L944 on second occurrence
        ]),
    );
    let encoded_rich = reg.encode_dynamic(&dyn_msg).unwrap();
    let decoded_rich = reg.decode_dynamic("edge.Rich", &encoded_rich).unwrap();
    assert_eq!(
        decoded_rich.fields.get(&3),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::String("one".into()),
            DynamicValue::String("two".into()),
        ]))
    );

    // Truncated varint inside packed Sint64 -> L874 UnexpectedEof
    let mut bad_s64 = Vec::new();
    encode_bytes_field(2, &[0x80], &mut bad_s64);
    assert_eq!(
        reg.decode_dynamic("edge.Rich", &bad_s64),
        Err(QuasarError::UnexpectedEof)
    );

    // Recursive known Message exceeding MAX_RECURSION_DEPTH -> L764
    let mut deep_rich = Vec::new();
    for _ in 0..=(MAX_RECURSION_DEPTH + 1) {
        let mut outer = Vec::new();
        encode_bytes_field(4, &deep_rich, &mut outer);
        deep_rich = outer;
    }
    assert_eq!(
        reg.decode_dynamic("edge.Rich", &deep_rich),
        Err(QuasarError::RecursionLimitExceeded)
    );
}

#[test]
fn quasar_envelopes_and_types_full_roundtrips_wrong_wire_unknown_fields_and_nested_errors() {
    // 1. Populated roundtrips + helper methods on AuthoritativeContext & NoticeTargetGroup
    let base_ctx = AuthoritativeContext::for_session(Some(-12), Some(555), Some(777), 0xBEEF, "tranquility", vec![1, 2, 3])
        .with_service_roles(0xCAFE);
    assert_eq!(base_ctx.effective_user_id(), Some(-12));
    assert_eq!(base_ctx.effective_character_id(), Some(555));
    assert_eq!(base_ctx.effective_corporation_id(), Some(777));
    assert_eq!(base_ctx.now_unix(), 0);
    assert_eq!(base_ctx.service_role_mask, 0xCAFE);

    let anon_ctx = AuthoritativeContext::for_session_at(None, None, None, 0xFF, "sisi", vec![4], 1_700_000_000);
    assert_eq!(anon_ctx.effective_user_id(), None);
    assert_eq!(anon_ctx.effective_character_id(), None);
    assert_eq!(anon_ctx.effective_corporation_id(), None);
    assert_eq!(anon_ctx.now_unix(), 1_700_000_000);
    assert_eq!(anon_ctx.corp_role_mask, 0);

    for ctx in [&base_ctx, &anon_ctx] {
        let mut rt = AuthoritativeContext::decode_proto(&ctx.encode_proto()).unwrap();
        rt.service_role_mask = ctx.service_role_mask;
        assert_eq!(rt, *ctx);
    }

    let ts = GoogleTimestamp { seconds: 1_700_000_123, nanos: 456 };
    let dur = GoogleDuration { seconds: -5, nanos: 10 };
    let tc = TraceContext {
        trace_id: vec![1, 2],
        parent_id: vec![3, 4],
        flags: Some(TraceFlags { sampled: true }),
        state: Some(TraceState { sample_rate: 100 }),
    };
    let any = EvePublicCharacterId { sequential: 555 }.to_any();
    let req = EvePublicRequest {
        issued: Some(ts),
        correlation_uuid: vec![9, 9],
        payload: Some(any.clone()),
        external_origin: "client".into(),
        application_instance_uuid: vec![8, 8],
        authoritative_context: Some(AuthoritativeContext { service_role_mask: 0, ..base_ctx.clone() }),
        trace_context: Some(tc.clone()),
    };
    assert_eq!(EvePublicRequest::decode_proto(&req.encode_proto()).unwrap(), req);

    let rsp = EvePublicResponse {
        dispatched: Some(ts),
        correlation_uuid: vec![7],
        status_code: 200,
        status_message: "OK".into(),
        payload: Some(any.clone()),
        internal_origin: "gw".into(),
        application_instance_uuid: vec![6],
        gateway_instance_uuid: vec![5],
        trace_context: Some(tc.clone()),
    };
    assert_eq!(EvePublicResponse::decode_proto(&rsp.encode_proto()).unwrap(), rsp);

    for tg in [
        NoticeTargetGroup::ApplicationInstanceUuid(vec![1]),
        NoticeTargetGroup::SolarSystem(30000142),
        NoticeTargetGroup::User(-42),
        NoticeTargetGroup::Character(555),
        NoticeTargetGroup::Corporation(777),
        NoticeTargetGroup::Alliance(-99),
        NoticeTargetGroup::BubbleInstanceUuid(vec![2]),
    ] {
        assert!(!tg.target_group_name().is_empty());
        assert_eq!(NoticeTargetGroup::decode_proto(&tg.encode_proto()).unwrap(), tg);
    }

    let notice = EvePublicNotice {
        dispatched: Some(ts),
        uuid: vec![3],
        internal_origin: "svc".into(),
        tenant: "tq".into(),
        payload: Some(any.clone()),
        target_group: Some(NoticeTargetGroup::User(-7)),
        trace_context: Some(tc.clone()),
    };
    assert_eq!(EvePublicNotice::decode_proto(&notice.encode_proto()).unwrap(), notice);

    let ev = EvePublicEvent {
        occurred: Some(ts),
        uuid: vec![4],
        payload: Some(any.clone()),
        journey: vec![5],
        external_origin: "cli".into(),
        authoritative_context: Some(AuthoritativeContext { service_role_mask: 0, ..base_ctx.clone() }),
        application_instance_uuid: vec![6],
        trace_context: Some(tc.clone()),
    };
    assert_eq!(EvePublicEvent::decode_proto(&ev.encode_proto()).unwrap(), ev);

    let ireq = EveInternalRequest {
        correlation_uuid: "c1".into(),
        payload: Some(any.clone()),
        language: 1,
        tenant: "tq".into(),
        publisher: "pub".into(),
        forwarded_for: "127.0.0.1".into(),
        trace_context: Some(tc.clone()),
        ignore_cache: true,
        created: Some(ts),
    };
    assert_eq!(EveInternalRequest::decode_proto(&ireq.encode_proto()).unwrap(), ireq);

    let ci = EveResponseCacheInfo {
        cached_on: Some(ts),
        cached_until: Some(ts),
    };
    let pi = EveResponsePagingInfo { total_pages_count: 12 };
    let irsp = EveInternalResponse {
        correlation_uuid: "c1".into(),
        status_code: 200,
        status_message: "OK".into(),
        payload: Some(any.clone()),
        tenant: "tq".into(),
        publisher: "pub".into(),
        payload_not_cached: true,
        payload_cache_info: Some(ci.clone()),
        payload_not_paged: true,
        payload_paging_info: Some(pi.clone()),
        recipient: "gw".into(),
        created: Some(ts),
        trace_context: Some(tc.clone()),
    };
    assert_eq!(EveInternalResponse::decode_proto(&irsp.encode_proto()).unwrap(), irsp);

    let igreq = InternalGatewayRequest {
        ack_id: 77,
        request: Some(req.clone()),
    };
    let sub_info = InternalGatewayRequestsSubscriptionInfo {
        bind_requests: vec![req.clone()],
        consumer: Some(InternalGatewayConsumerInfo {
            name: "c".into(),
            context: "ctx".into(),
            tag: "tag".into(),
        }),
        in_flight_max: 8,
        unbind_requests: vec![req.clone()],
    };
    let igrsp = InternalGatewayResponse {
        response: Some(rsp.clone()),
        ack_id: 77,
        subscription: Some(sub_info.clone()),
    };
    assert_eq!(InternalGatewayRequest::decode_proto(&igreq.encode_proto()).unwrap(), igreq);
    assert_eq!(InternalGatewayResponse::decode_proto(&igrsp.encode_proto()).unwrap(), igrsp);

    // types.rs populated roundtrips + unknown/wrong-wire/error coverage
    let empty = GoogleEmpty;
    assert_eq!(GoogleEmpty::decode_proto(&empty.encode_proto()).unwrap(), empty);
    let _ = GoogleTimestamp::decode_proto(&[0xF8, 0x06, 0x01]).unwrap(); // unknown field
    let _ = GoogleDuration::decode_proto(&dur.encode_proto()).unwrap();
    let _ = GoogleDuration::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();
    let _ = GoogleAny::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();
    let _ = EvePublicUserId::decode_proto(&EvePublicUserId { sequential: -9 }.encode_proto()).unwrap();
    let _ = EvePublicUserId::decode_proto(&[0x0A, 0x00, 0xF8, 0x06, 0x01]).unwrap();
    let _ = EvePublicCharacterId::decode_proto(&[0x0A, 0x00, 0xF8, 0x06, 0x01]).unwrap();
    let _ = EvePublicCorporationId::decode_proto(&[0x0A, 0x00, 0xF8, 0x06, 0x01]).unwrap();
    let _ = EvePublicCorpRoleCollection::decode_proto(&[0x0A, 0x00, 0xF8, 0x06, 0x01]).unwrap();
    let _ = EveIdempotencyToken::decode_proto(&[0x08, 0x01, 0xF8, 0x06, 0x01]).unwrap();
    assert_eq!(EveIpAddress::default(), EveIpAddress::V4(0x7F00_0001));
    let ip6 = EveIpAddress::V6(vec![0; 16]);
    assert_eq!(EveIpAddress::decode_proto(&ip6.encode_proto()).unwrap(), ip6);
    let _ = EveIpAddress::decode_proto(&[0x08, 0x01, 0x10, 0x01, 0xF8, 0x06, 0x01]).unwrap();
    let _ = TraceContext::decode_proto(&tc.encode_proto()).unwrap();
    let _ = TraceContext::decode_proto(&[0x08, 0x01, 0x10, 0x01, 0x18, 0x01, 0x20, 0x01, 0xF8, 0x06, 0x01]).unwrap();
    assert!(TraceContext::decode_proto(&[0x1A, 0x01, 0x80]).is_err());
    assert!(TraceContext::decode_proto(&[0x22, 0x01, 0x80]).is_err());

    let sem = SemanticVersionSpec {
        major: 1,
        minor: 2,
        patch: 3,
        prerelease_tags: vec!["rc1".into()],
        build_tags: vec!["b1".into()],
    };
    assert_eq!(SemanticVersionSpec::decode_proto(&sem.encode_proto()).unwrap(), sem);
    let _ = SemanticVersionSpec::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();
    let rgb = ColorRgb { red: 1, green: 2, blue: 3 };
    assert_eq!(ColorRgb::decode_proto(&rgb.encode_proto()).unwrap(), rgb);
    let _ = ColorRgb::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();
    let frac = MathFraction { numerator: 3, denominator: 4 };
    assert_eq!(MathFraction::decode_proto(&frac.encode_proto()).unwrap(), frac);
    let _ = MathFraction::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();
    let v3 = MathVector3 { x: 1.0, y: 2.0, z: 3.0 };
    assert_eq!(MathVector3::decode_proto(&v3.encode_proto()).unwrap(), v3);
    let _ = MathVector3::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();
    let v4 = MathVector4 { x: 1.0, y: 2.0, z: 3.0, w: 4.0 };
    assert_eq!(MathVector4::decode_proto(&v4.encode_proto()).unwrap(), v4);
    let _ = MathVector4::decode_proto(&[0xF8, 0x06, 0x01]).unwrap();

    let dev = DeviceUserIdentifier {
        device: vec![7, 8],
        user: Some(EvePublicUserId { sequential: -3 }),
    };
    assert_eq!(DeviceUserIdentifier::decode_proto(&dev.encode_proto()).unwrap(), dev);
    let _ = DeviceUserIdentifier::decode_proto(&[0x08, 0x01, 0x10, 0x01, 0xF8, 0x06, 0x01]).unwrap();
    assert!(DeviceUserIdentifier::decode_proto(&[0x12, 0x01, 0x80]).is_err());

    let rtt = NetworkRttSample { protocol: 2, rtt: Some(dur) };
    assert_eq!(NetworkRttSample::decode_proto(&rtt.encode_proto()).unwrap(), rtt);
    let _ = NetworkRttSample::decode_proto(&[0x10, 0x01, 0xF8, 0x06, 0x01]).unwrap();
    assert!(NetworkRttSample::decode_proto(&[0x12, 0x01, 0x80]).is_err());

    // 2. Unknown fields + wrong wire type + corrupt inner payload `?` propagation for every envelope in envelopes.rs
    let unknown = WireField { field_number: 99, value: WireValue::Varint(1) };
    let bad_sub = vec![0x08, 0x80]; // valid wire tag + truncated varint -> UnexpectedEof inside sub-decode

    let wrong_wire_bytes_fields = |field_nums: &[u32]| -> Vec<u8> {
        let mut fields: Vec<WireField> = field_nums
            .iter()
            .map(|&n| WireField { field_number: n, value: WireValue::Varint(1) })
            .collect();
        fields.push(unknown.clone());
        WireMessage { fields }.encode()
    };
    let corrupt_sub = |field_num: u32| -> Vec<u8> {
        WireMessage {
            fields: vec![WireField {
                field_number: field_num,
                value: WireValue::LengthDelimited(bad_sub.clone()),
            }],
        }
        .encode()
    };

    // ActiveIdentity (f1, f2, f3)
    let _ = ActiveIdentity::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 3])).unwrap();
    for f in [1, 2, 3] {
        assert!(ActiveIdentity::decode_proto(&corrupt_sub(f)).is_err());
    }

    // AuthoritativeContext (f1, f2, f5, f7, f10)
    let _ = AuthoritativeContext::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 5, 7, 10])).unwrap();
    for f in [1, 2, 5, 7, 10] {
        assert!(AuthoritativeContext::decode_proto(&corrupt_sub(f)).is_err());
    }

    // EvePublicRequest (f1, f3, f6, f7)
    let _ = EvePublicRequest::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 3, 5, 6, 7])).unwrap();
    for f in [1, 3, 6, 7] {
        assert!(EvePublicRequest::decode_proto(&corrupt_sub(f)).is_err());
    }

    // EvePublicResponse (f1, f5, f9)
    let _ = EvePublicResponse::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 5, 7, 8, 9])).unwrap();
    for f in [1, 5, 9] {
        assert!(EvePublicResponse::decode_proto(&corrupt_sub(f)).is_err());
    }

    // NoticeTargetGroup (wrong wire types + unknown field)
    let _ = NoticeTargetGroup::decode_proto(&wrong_wire_bytes_fields(&[1, 7])).unwrap();

    // EvePublicNotice (f1, f5, f6, f7)
    let _ = EvePublicNotice::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 5, 6, 7])).unwrap();
    for f in [1, 5, 6, 7] {
        assert!(EvePublicNotice::decode_proto(&corrupt_sub(f)).is_err());
    }

    // EvePublicEvent (f1, f3, f6, f8)
    let _ = EvePublicEvent::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 3, 4, 6, 7, 8])).unwrap();
    for f in [1, 3, 6, 8] {
        assert!(EvePublicEvent::decode_proto(&corrupt_sub(f)).is_err());
    }

    // EveInternalRequest (f2, f7, f9)
    let _ = EveInternalRequest::decode_proto(&wrong_wire_bytes_fields(&[2, 7, 9])).unwrap();
    for f in [2, 7, 9] {
        assert!(EveInternalRequest::decode_proto(&corrupt_sub(f)).is_err());
    }

    // EveResponseCacheInfo (f1, f2, and unknown bytes field 3 for envelopes.rs L902)
    let _ = EveResponseCacheInfo::decode_proto(&wrong_wire_bytes_fields(&[1, 2])).unwrap();
    let mut cache_unknown_bytes = Vec::new();
    encode_bytes_field(3, b"extra", &mut cache_unknown_bytes);
    assert_eq!(
        EveResponseCacheInfo::decode_proto(&cache_unknown_bytes).unwrap(),
        EveResponseCacheInfo::default()
    );
    for f in [1, 2] {
        assert!(EveResponseCacheInfo::decode_proto(&corrupt_sub(f)).is_err());
    }

    // EveResponsePagingInfo
    let _ = EveResponsePagingInfo::decode_proto(&wrong_wire_bytes_fields(&[])).unwrap();

    // EveInternalResponse (f4, f8, f10, f12, f13)
    let _ = EveInternalResponse::decode_proto(&wrong_wire_bytes_fields(&[4, 8, 10, 12, 13])).unwrap();
    for f in [4, 8, 10, 12, 13] {
        assert!(EveInternalResponse::decode_proto(&corrupt_sub(f)).is_err());
    }

    // InternalGatewayRequest (f2)
    let _ = InternalGatewayRequest::decode_proto(&wrong_wire_bytes_fields(&[2])).unwrap();
    assert!(InternalGatewayRequest::decode_proto(&corrupt_sub(2)).is_err());

    // InternalGatewayConsumerInfo
    let _ = InternalGatewayConsumerInfo::decode_proto(&wrong_wire_bytes_fields(&[])).unwrap();

    // InternalGatewayRequestsSubscriptionInfo (f1, f2, f4)
    let _ = InternalGatewayRequestsSubscriptionInfo::decode_proto(&wrong_wire_bytes_fields(&[1, 2, 4])).unwrap();
    for f in [1, 2, 4] {
        assert!(InternalGatewayRequestsSubscriptionInfo::decode_proto(&corrupt_sub(f)).is_err());
    }

    // InternalGatewayResponse (f1, f3)
    let _ = InternalGatewayResponse::decode_proto(&wrong_wire_bytes_fields(&[1, 3])).unwrap();
    for f in [1, 3] {
        assert!(InternalGatewayResponse::decode_proto(&corrupt_sub(f)).is_err());
    }
}
