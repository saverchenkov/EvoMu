//! Unit and mutation-killing tests for `quasar` wire codec, hashing, and descriptor proto.

use super::*;

#[test]
fn quasar_type_url_prefixes_and_rfc_hash_vectors() {
    assert_eq!(EXPECTED_QUASAR_DOMAIN_COUNT, 135);
    assert_eq!(EXPECTED_QUASAR_FILE_COUNT, 1_455);

    assert!(has_quasar_type_url_prefix(
        "type.googleapis.com/eve_public.Request"
    ));
    assert!(has_quasar_type_url_prefix(
        "type.evetech.net/eve_public.Request"
    ));
    assert!(!has_quasar_type_url_prefix("eve_public.Request"));
    assert!(!has_quasar_type_url_prefix(""));

    assert_eq!(
        strip_quasar_type_url_prefix("type.googleapis.com/eve_public.Request"),
        "eve_public.Request"
    );
    assert_eq!(
        strip_quasar_type_url_prefix("type.evetech.net/.eve_public.Notice"),
        "eve_public.Notice"
    );
    assert_eq!(
        strip_quasar_type_url_prefix(".eve.Response"),
        "eve.Response"
    );
    assert_eq!(strip_quasar_type_url_prefix("eve.Request"), "eve.Request");

    // SHA-256 RFC 6234 vectors (empty, 1-block, 2-block boundary >= 56 bytes)
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );

    // MD5 RFC 1321 vectors (empty, 1-block, multi-block)
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(
        md5_hex(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
        "d174ab98d277d9f5a5611c2c9f419d9f"
    );
}

#[test]
fn varint_zigzag_wire_type_and_wire_message_codec_boundaries() {
    for v in [
        0u64,
        1,
        127,
        128,
        255,
        16_383,
        16_384,
        u32::MAX as u64,
        (1u64 << 63) - 1,
        u64::MAX,
    ] {
        let mut buf = Vec::new();
        encode_varint(v, &mut buf);
        let mut pos = 0;
        assert_eq!(decode_varint(&buf, &mut pos).unwrap(), v);
        assert_eq!(pos, buf.len());
    }

    let mut pos = 0;
    assert_eq!(decode_varint(&[], &mut pos), Err(QuasarError::UnexpectedEof));
    pos = 0;
    assert_eq!(
        decode_varint(&[0x80, 0x80], &mut pos),
        Err(QuasarError::UnexpectedEof)
    );
    // 10-byte varint with 11th continuation -> VarintOverflow
    let overflow_buf = [0x80u8; 11];
    pos = 0;
    assert_eq!(
        decode_varint(&overflow_buf, &mut pos),
        Err(QuasarError::VarintOverflow)
    );

    // ZigZag 32 and 64
    assert_eq!(encode_zigzag32(0), 0);
    assert_eq!(encode_zigzag32(-1), 1);
    assert_eq!(encode_zigzag32(1), 2);
    assert_eq!(encode_zigzag32(-2), 3);
    for v32 in [0i32, -1, 1, -2, 2, 123_456, -123_456, i32::MAX, i32::MIN] {
        assert_eq!(decode_zigzag32(encode_zigzag32(v32)), v32);
    }
    assert_eq!(encode_zigzag64(0), 0);
    assert_eq!(encode_zigzag64(-1), 1);
    assert_eq!(encode_zigzag64(1), 2);
    assert_eq!(encode_zigzag64(-2), 3);
    for v64 in [0i64, -1, 1, -2, 2, 9_999_999_999, -9_999_999_999, i64::MAX, i64::MIN] {
        assert_eq!(decode_zigzag64(encode_zigzag64(v64)), v64);
    }

    // WireType and WireValue accessors
    assert_eq!(WireType::from_u8(0), Ok(WireType::Varint));
    assert_eq!(WireType::from_u8(1), Ok(WireType::Fixed64));
    assert_eq!(WireType::from_u8(2), Ok(WireType::LengthDelimited));
    assert_eq!(WireType::from_u8(5), Ok(WireType::Fixed32));
    assert_eq!(WireType::from_u8(3), Err(QuasarError::InvalidWireType(3)));
    assert_eq!(WireType::from_u8(4), Err(QuasarError::InvalidWireType(4)));

    let w_var = WireValue::Varint(42);
    assert_eq!(w_var.wire_type(), WireType::Varint);
    assert_eq!(w_var.as_varint(), Some(42));
    assert_eq!(w_var.as_bytes(), None);
    assert_eq!(w_var.as_str(), Ok(None));
    assert_eq!(w_var.as_fixed32(), None);
    assert_eq!(w_var.as_fixed64(), None);

    let w_f64 = WireValue::Fixed64(0x1122_3344_5566_7788);
    assert_eq!(w_f64.wire_type(), WireType::Fixed64);
    assert_eq!(w_f64.as_fixed64(), Some(0x1122_3344_5566_7788));
    assert_eq!(w_f64.as_varint(), None);

    let w_f32 = WireValue::Fixed32(0x1122_3344);
    assert_eq!(w_f32.wire_type(), WireType::Fixed32);
    assert_eq!(w_f32.as_fixed32(), Some(0x1122_3344));

    let w_str = WireValue::LengthDelimited(b"hello".to_vec());
    assert_eq!(w_str.wire_type(), WireType::LengthDelimited);
    assert_eq!(w_str.as_bytes(), Some(&b"hello"[..]));
    assert_eq!(w_str.as_str(), Ok(Some("hello")));

    let w_bad_utf8 = WireValue::LengthDelimited(vec![0xFF, 0xFE]);
    assert!(matches!(
        w_bad_utf8.as_str(),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // WireMessage encode/decode roundtrip & error cases
    let msg = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(999),
            },
            WireField {
                field_number: 2,
                value: WireValue::Fixed64(0xDEAD_BEEF_CAFE_BABE),
            },
            WireField {
                field_number: 3,
                value: WireValue::LengthDelimited(b"payload".to_vec()),
            },
            WireField {
                field_number: 4,
                value: WireValue::Fixed32(0x1234_5678),
            },
        ],
    };
    let encoded = msg.encode();
    let decoded = WireMessage::decode(&encoded).unwrap();
    assert_eq!(decoded, msg);

    // Field number 0 is rejected
    assert_eq!(
        WireMessage::decode(&[0x00, 0x01]),
        Err(QuasarError::InvalidWireType(0xFF))
    );
    // Truncated Fixed64, Fixed32, LengthDelimited
    assert_eq!(
        WireMessage::decode(&[0x09, 1, 2, 3]),
        Err(QuasarError::UnexpectedEof)
    );
    assert_eq!(
        WireMessage::decode(&[0x0D, 1, 2]),
        Err(QuasarError::UnexpectedEof)
    );
    assert_eq!(
        WireMessage::decode(&[0x0A, 10, 1, 2]),
        Err(QuasarError::UnexpectedEof)
    );
}

#[test]
fn field_types_labels_and_file_descriptor_wire_roundtrip() {
    let all_types = [
        (1, FieldType::Double, WireType::Fixed64, true),
        (2, FieldType::Float, WireType::Fixed32, true),
        (3, FieldType::Int64, WireType::Varint, true),
        (4, FieldType::Uint64, WireType::Varint, true),
        (5, FieldType::Int32, WireType::Varint, true),
        (6, FieldType::Fixed64, WireType::Fixed64, true),
        (7, FieldType::Fixed32, WireType::Fixed32, true),
        (8, FieldType::Bool, WireType::Varint, true),
        (9, FieldType::String, WireType::LengthDelimited, false),
        (10, FieldType::Group, WireType::LengthDelimited, false),
        (11, FieldType::Message, WireType::LengthDelimited, false),
        (12, FieldType::Bytes, WireType::LengthDelimited, false),
        (13, FieldType::Uint32, WireType::Varint, true),
        (14, FieldType::Enum, WireType::Varint, true),
        (15, FieldType::Sfixed32, WireType::Fixed32, true),
        (16, FieldType::Sfixed64, WireType::Fixed64, true),
        (17, FieldType::Sint32, WireType::Varint, true),
        (18, FieldType::Sint64, WireType::Varint, true),
    ];
    for (num, expected_ft, expected_wt, packable) in all_types {
        let ft = FieldType::from_i32(num);
        assert_eq!(ft, expected_ft);
        assert_eq!(ft.expected_wire_type(), expected_wt);
        assert_eq!(ft.is_packable_scalar(), packable);
    }
    assert_eq!(FieldType::from_i32(99), FieldType::Bytes);

    assert_eq!(FieldLabel::from_i32(1), FieldLabel::Optional);
    assert_eq!(FieldLabel::from_i32(2), FieldLabel::Required);
    assert_eq!(FieldLabel::from_i32(3), FieldLabel::Repeated);
    assert_eq!(FieldLabel::from_i32(99), FieldLabel::Optional);

    let orig_fd = FileDescriptorProto {
        pyj_path: "eve_public/ship/ship.pyj".into(),
        domain: "ship".into(),
        name: "eve_public/ship/ship.proto".into(),
        package: "eve_public.ship".into(),
        syntax: "proto3".into(),
        dependencies: vec!["google/protobuf/timestamp.proto".into()],
        messages: vec![DescriptorProto {
            name: "GetRequest".into(),
            full_name: "eve_public.ship.GetRequest".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "ship_id".into(),
                    number: 1,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Uint64,
                    type_name: String::new(),
                    oneof_index: Some(0),
                    json_name: "shipId".into(),
                },
                FieldDescriptorProto {
                    name: "nested".into(),
                    number: 2,
                    label: FieldLabel::Repeated,
                    field_type: FieldType::Message,
                    type_name: "eve_public.ship.GetRequest.Entry".into(),
                    oneof_index: None,
                    json_name: "nested".into(),
                },
            ],
            nested_types: vec![DescriptorProto {
                name: "Entry".into(),
                full_name: "eve_public.ship.GetRequest.Entry".into(),
                fields: vec![],
                nested_types: vec![],
                enum_types: vec![],
                oneofs: vec![],
                is_map_entry: true,
            }],
            enum_types: vec![EnumDescriptorProto {
                name: "Mode".into(),
                full_name: "eve_public.ship.GetRequest.Mode".into(),
                values: vec![EnumValueDescriptorProto {
                    name: "MODE_UNSPECIFIED".into(),
                    number: 0,
                }],
            }],
            oneofs: vec![OneofDescriptorProto {
                name: "identifier".into(),
            }],
            is_map_entry: false,
        }],
        enums: vec![EnumDescriptorProto {
            name: "Status".into(),
            full_name: "eve_public.ship.Status".into(),
            values: vec![
                EnumValueDescriptorProto {
                    name: "STATUS_UNKNOWN".into(),
                    number: 0,
                },
                EnumValueDescriptorProto {
                    name: "STATUS_ACTIVE".into(),
                    number: 1,
                },
            ],
        }],
        services: vec![ServiceDescriptorProto {
            name: "ShipService".into(),
            full_name: "eve_public.ship.ShipService".into(),
            methods: vec![MethodDescriptorProto {
                name: "StreamShips".into(),
                input_type: "eve_public.ship.GetRequest".into(),
                output_type: "eve_public.ship.GetRequest".into(),
                client_streaming: true,
                server_streaming: true,
            }],
        }],
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: Vec::new(),
    };

    assert_eq!(
        orig_fd.messages[0].type_url(),
        "type.googleapis.com/eve_public.ship.GetRequest"
    );

    let wire = orig_fd.encode_wire();
    let decoded = FileDescriptorProto::decode_wire(
        orig_fd.pyj_path.clone(),
        orig_fd.domain.clone(),
        wire.clone(),
    )
    .unwrap();
    assert_eq!(decoded.name, orig_fd.name);
    assert_eq!(decoded.package, orig_fd.package);
    assert_eq!(decoded.syntax, "proto3");
    assert_eq!(decoded.dependencies, orig_fd.dependencies);
    assert_eq!(decoded.messages, orig_fd.messages);
    assert_eq!(decoded.enums, orig_fd.enums);
    assert_eq!(decoded.services, orig_fd.services);
    assert_eq!(decoded.sha256, sha256_hex(&wire));
    assert_eq!(decoded.md5, md5_hex(&wire));
}

#[test]
fn quasar_wire_message_limits_and_descriptor_error_and_fallback_arms() {
    // 1. WireMessage::decode exceeding MAX_FRAME_SIZE & MAX_COLLECTION_LEN (quasar.rs L427, L433)
    let oversize = vec![0u8; MAX_FRAME_SIZE + 1];
    assert_eq!(
        WireMessage::decode(&oversize),
        Err(QuasarError::FrameTooLarge(MAX_FRAME_SIZE + 1))
    );

    let mut over_collection = Vec::with_capacity((MAX_COLLECTION_LEN + 1) * 2);
    for _ in 0..=MAX_COLLECTION_LEN {
        over_collection.push(0x08); // tag: field 1 varint
        over_collection.push(0x01);
    }
    assert_eq!(
        WireMessage::decode(&over_collection),
        Err(QuasarError::CollectionTooLarge(MAX_COLLECTION_LEN))
    );

    // 2. decode_enum_descriptor: unknown field + non-bytes field 2 + invalid UTF-8 enum value (quasar.rs L767)
    let mut bad_enum_val = Vec::new();
    encode_bytes_field(1, &[0xFF], &mut bad_enum_val); // invalid UTF-8 enum value name
    let mut bad_enum_desc = Vec::new();
    encode_string_field(1, "BadEnum", &mut bad_enum_desc);
    encode_varint_field(2, 1, &mut bad_enum_desc); // wrong wire type for value
    encode_bytes_field(2, &bad_enum_val, &mut bad_enum_desc);
    let mut fd_bad_enum = Vec::new();
    encode_string_field(1, "e.proto", &mut fd_bad_enum);
    encode_bytes_field(5, &bad_enum_desc, &mut fd_bad_enum);
    assert!(matches!(
        FileDescriptorProto::decode_wire("e.pyj", "e", fd_bad_enum),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // 3. decode_message_descriptor: bad FieldDescriptorProto + options(f7) without f7 sub + oneof(f8) bad/extra fields (quasar.rs L806, L826, L837, L840)
    let mut bad_field_bytes = Vec::new();
    encode_bytes_field(1, &[0xFF], &mut bad_field_bytes); // invalid UTF-8 field name
    let mut msg_bad_field = Vec::new();
    encode_string_field(1, "M", &mut msg_bad_field);
    encode_bytes_field(2, &bad_field_bytes, &mut msg_bad_field);
    let mut fd_bad_field = Vec::new();
    encode_bytes_field(4, &msg_bad_field, &mut fd_bad_field);
    assert!(matches!(
        FileDescriptorProto::decode_wire("f.pyj", "f", fd_bad_field),
        Err(QuasarError::InvalidUtf8(_))
    ));

    let mut non_map_opts = Vec::new();
    encode_varint_field(1, 1, &mut non_map_opts); // option field_number != 7
    let mut oneof_good_with_unknown = Vec::new();
    encode_string_field(1, "choice", &mut oneof_good_with_unknown);
    encode_varint_field(9, 42, &mut oneof_good_with_unknown); // unknown sub-field in OneofDescriptorProto
    let mut msg_opts_and_oneof = Vec::new();
    encode_string_field(1, "OptsMsg", &mut msg_opts_and_oneof);
    encode_varint_field(2, 1, &mut msg_opts_and_oneof); // wrong wire type for field descriptor (quasar.rs L806)
    encode_varint_field(7, 1, &mut msg_opts_and_oneof); // wrong wire type for options
    encode_bytes_field(7, &non_map_opts, &mut msg_opts_and_oneof);
    encode_varint_field(8, 1, &mut msg_opts_and_oneof); // wrong wire type for oneof
    encode_bytes_field(8, &oneof_good_with_unknown, &mut msg_opts_and_oneof);
    let mut fd_opts = Vec::new();
    encode_bytes_field(4, &msg_opts_and_oneof, &mut fd_opts);
    let decoded_opts = FileDescriptorProto::decode_wire("o.pyj", "o", fd_opts).unwrap();
    assert!(!decoded_opts.messages[0].is_map_entry);
    assert_eq!(decoded_opts.messages[0].oneofs.len(), 1);

    let mut bad_oneof = Vec::new();
    encode_bytes_field(1, &[0xFF], &mut bad_oneof);
    let mut msg_bad_oneof = Vec::new();
    encode_bytes_field(8, &bad_oneof, &mut msg_bad_oneof);
    let mut fd_bad_oneof = Vec::new();
    encode_bytes_field(4, &msg_bad_oneof, &mut fd_bad_oneof);
    assert!(matches!(
        FileDescriptorProto::decode_wire("o2.pyj", "o2", fd_bad_oneof),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // 4. decode_service_descriptor: unknown subfields + wrong wire type (quasar.rs L912, L922, L924)
    let mut method_bytes = Vec::new();
    encode_string_field(1, "DoWork", &mut method_bytes);
    encode_varint_field(99, 1, &mut method_bytes); // unknown method subfield (L912)
    let mut svc_bytes = Vec::new();
    encode_string_field(1, "Svc", &mut svc_bytes);
    encode_varint_field(2, 1, &mut svc_bytes); // wrong wire type for method (L922)
    encode_bytes_field(2, &method_bytes, &mut svc_bytes);
    encode_varint_field(99, 1, &mut svc_bytes); // unknown service field (L924)
    let mut fd_svc = Vec::new();
    encode_bytes_field(6, &svc_bytes, &mut fd_svc);
    let decoded_svc = FileDescriptorProto::decode_wire("s.pyj", "s", fd_svc).unwrap();
    assert_eq!(decoded_svc.services[0].methods[0].name, "DoWork");
}
