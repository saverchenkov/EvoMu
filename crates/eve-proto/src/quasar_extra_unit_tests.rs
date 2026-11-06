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
