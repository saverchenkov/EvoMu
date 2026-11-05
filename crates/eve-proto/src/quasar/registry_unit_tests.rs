//! Unit and mutation-killing tests for `eve_proto::quasar::registry`.

use super::*;

fn build_all_types_registry() -> QuasarDescriptorRegistry {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.enums_by_full_name.insert(
        "test.SampleEnum".into(),
        EnumDescriptorProto {
            name: "SampleEnum".into(),
            full_name: "test.SampleEnum".into(),
            values: vec![
                EnumValueDescriptorProto {
                    name: "ZERO".into(),
                    number: 0,
                },
                EnumValueDescriptorProto {
                    name: "NON_ZERO".into(),
                    number: 7,
                },
            ],
        },
    );

    let sub_desc = DescriptorProto {
        name: "SubMsg".into(),
        full_name: "test.SubMsg".into(),
        fields: vec![
            FieldDescriptorProto {
                name: "label".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::String,
                type_name: String::new(),
                oneof_index: None,
                json_name: "label".into(),
            },
            // Self-recursive field to exercise visited cycle protection
            FieldDescriptorProto {
                name: "self_ref".into(),
                number: 2,
                label: FieldLabel::Optional,
                field_type: FieldType::Message,
                type_name: "test.SubMsg".into(),
                oneof_index: None,
                json_name: "selfRef".into(),
            },
        ],
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    };
    reg.messages_by_full_name
        .insert(sub_desc.full_name.clone(), sub_desc);

    let scalar_types = [
        (1, "f_double", FieldType::Double, ""),
        (2, "f_float", FieldType::Float, ""),
        (3, "f_int64", FieldType::Int64, ""),
        (4, "f_uint64", FieldType::Uint64, ""),
        (5, "f_int32", FieldType::Int32, ""),
        (6, "f_fixed64", FieldType::Fixed64, ""),
        (7, "f_fixed32", FieldType::Fixed32, ""),
        (8, "f_bool", FieldType::Bool, ""),
        (9, "f_string", FieldType::String, ""),
        (10, "f_bytes", FieldType::Bytes, ""),
        (11, "f_uint32", FieldType::Uint32, ""),
        (12, "f_enum", FieldType::Enum, "test.SampleEnum"),
        (13, "f_sfixed32", FieldType::Sfixed32, ""),
        (14, "f_sfixed64", FieldType::Sfixed64, ""),
        (15, "f_sint32", FieldType::Sint32, ""),
        (16, "f_sint64", FieldType::Sint64, ""),
        (17, "f_msg", FieldType::Message, "test.SubMsg"),
    ];

    let mut fields = Vec::new();
    for (num, name, ft, tname) in scalar_types {
        fields.push(FieldDescriptorProto {
            name: name.into(),
            number: num,
            label: FieldLabel::Optional,
            field_type: ft,
            type_name: tname.into(),
            oneof_index: None,
            json_name: name.into(),
        });
        fields.push(FieldDescriptorProto {
            name: format!("rep_{name}"),
            number: num + 20,
            label: FieldLabel::Repeated,
            field_type: ft,
            type_name: tname.into(),
            oneof_index: None,
            json_name: format!("rep_{name}"),
        });
    }
    // Add two oneof sibling fields
    fields.push(FieldDescriptorProto {
        name: "choice_a".into(),
        number: 60,
        label: FieldLabel::Optional,
        field_type: FieldType::Int32,
        type_name: String::new(),
        oneof_index: Some(0),
        json_name: "choiceA".into(),
    });
    fields.push(FieldDescriptorProto {
        name: "choice_b".into(),
        number: 61,
        label: FieldLabel::Optional,
        field_type: FieldType::String,
        type_name: String::new(),
        oneof_index: Some(0),
        json_name: "choiceB".into(),
    });

    reg.messages_by_full_name.insert(
        "test.AllTypes".into(),
        DescriptorProto {
            name: "AllTypes".into(),
            full_name: "test.AllTypes".into(),
            fields,
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: vec![OneofDescriptorProto {
                name: "choice".into(),
            }],
            is_map_entry: false,
        },
    );
    reg
}

#[test]
fn empty_registry_builtins_and_sample_instantiation_roundtrip_all_types() {
    let reg = build_all_types_registry();
    assert!(reg.get_message("google.protobuf.Timestamp").is_some());
    assert!(reg
        .get_message("type.googleapis.com/google.protobuf.Value")
        .is_some());
    assert_eq!(
        reg.instantiate_populated_sample("unknown.Msg", 1),
        Err(QuasarError::UnknownMessageType("unknown.Msg".into()))
    );

    let sample = reg
        .instantiate_populated_sample("type.googleapis.com/test.AllTypes", 42)
        .unwrap();
    assert_eq!(sample.full_name, "test.AllTypes");
    assert_eq!(sample.type_url(), "type.googleapis.com/test.AllTypes");
    // Oneof sibling 60 is populated, 61 is skipped
    assert!(sample.get(60).is_some());
    assert!(sample.get(61).is_none());

    let encoded = reg.encode_dynamic(&sample).unwrap();
    let decoded = reg.decode_dynamic("test.AllTypes", &encoded).unwrap();
    assert_eq!(decoded, sample);

    // Int32 value on a Sint32 field (both scalar and packed repeated) encodes with ZigZag
    let mut custom = DynamicQuasarMessage::new("test.AllTypes");
    custom.set(15, DynamicValue::Int32(-25));
    custom.set(
        35,
        DynamicValue::Repeated(vec![DynamicValue::Int32(-10), DynamicValue::Int32(10)]),
    );
    let custom_bytes = reg.encode_dynamic(&custom).unwrap();
    let custom_dec = reg.decode_dynamic("test.AllTypes", &custom_bytes).unwrap();
    assert_eq!(custom_dec.get(15), Some(&DynamicValue::Sint32(-25)));
    assert_eq!(
        custom_dec.get(35),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::Sint32(-10),
            DynamicValue::Sint32(10)
        ]))
    );
}

#[test]
fn decode_dynamic_handles_oneof_eviction_wire_mismatch_and_truncated_packed_fields() {
    let reg = build_all_types_registry();

    // Send both choice_a (60) and choice_b (61) on the wire -> choice_b evicts choice_a
    let mut oneof_wire = Vec::new();
    encode_varint_field(60, 123, &mut oneof_wire);
    encode_string_field(61, "winner", &mut oneof_wire);
    let dec = reg.decode_dynamic("test.AllTypes", &oneof_wire).unwrap();
    assert!(dec.get(60).is_none());
    assert_eq!(dec.get(61), Some(&DynamicValue::String("winner".into())));

    // WireTypeMismatch when Fixed32 is sent for Int64 field #3
    let mut bad_wt = Vec::new();
    encode_fixed32_field(3, 99, &mut bad_wt);
    assert!(matches!(
        reg.decode_dynamic("test.AllTypes", &bad_wt),
        Err(QuasarError::WireTypeMismatch { number: 3, .. })
    ));

    // Truncated packed fixed-width fields (Double #21, Float #22, Fixed64 #26, Fixed32 #27, Sfixed32 #33, Sfixed64 #34)
    for fnum in [21u32, 22, 26, 27, 33, 34] {
        let mut trunc_packed = Vec::new();
        encode_bytes_field(fnum, &[0x01, 0x02, 0x03], &mut trunc_packed);
        assert_eq!(
            reg.decode_dynamic("test.AllTypes", &trunc_packed),
            Err(QuasarError::UnexpectedEof)
        );
    }
}

#[test]
fn decode_dynamic_or_wire_fallback_and_rust_codegen_cover_all_branches() {
    let reg = QuasarDescriptorRegistry::empty_with_builtins();

    // Unknown type falls back to wire inspection (varint, fixed64, fixed32, empty bytes, string, nested msg, raw bytes, repeated)
    let mut sub_wire = Vec::new();
    encode_varint_field(1, 77, &mut sub_wire);

    let mut fallback_wire = Vec::new();
    encode_varint_field(1, 10, &mut fallback_wire);
    encode_varint_field(1, 20, &mut fallback_wire);
    encode_varint_field(1, 30, &mut fallback_wire);
    encode_fixed64_field(2, 0x1234, &mut fallback_wire);
    encode_fixed32_field(3, 0x5678, &mut fallback_wire);
    encode_bytes_field(4, &[], &mut fallback_wire);
    encode_string_field(5, "hello\nworld", &mut fallback_wire);
    encode_bytes_field(6, &sub_wire, &mut fallback_wire);
    encode_bytes_field(7, &[0xFF, 0x00, 0xFE], &mut fallback_wire);

    let msg = reg
        .decode_dynamic_or_wire("type.googleapis.com/custom.Unknown", &fallback_wire)
        .unwrap();
    assert_eq!(msg.full_name, "custom.Unknown");
    assert_eq!(
        msg.get(1),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::Uint64(10),
            DynamicValue::Uint64(20),
            DynamicValue::Uint64(30),
        ]))
    );
    assert_eq!(msg.get(2), Some(&DynamicValue::Fixed64(0x1234)));
    assert_eq!(msg.get(3), Some(&DynamicValue::Fixed32(0x5678)));
    assert!(matches!(msg.get(4), Some(DynamicValue::Message(_))));
    assert_eq!(
        msg.get(5),
        Some(&DynamicValue::String("hello\nworld".into()))
    );
    assert!(matches!(msg.get(6), Some(DynamicValue::Message(_))));
    assert_eq!(
        msg.get(7),
        Some(&DynamicValue::Bytes(vec![0xFF, 0x00, 0xFE]))
    );

    // FileDescriptorProto wire roundtrip + Rust codegen with reserved keywords
    let file = FileDescriptorProto {
        pyj_path: "eveProto/generated/test_pb2.pyj".into(),
        domain: "test".into(),
        name: "test.proto".into(),
        package: "eve_public.test".into(),
        syntax: "proto3".into(),
        dependencies: vec!["google/protobuf/timestamp.proto".into()],
        messages: vec![DescriptorProto {
            name: "SampleMsg".into(),
            full_name: "eve_public.test.SampleMsg".into(),
            fields: vec![
                FieldDescriptorProto {
                    name: "type".into(),
                    number: 1,
                    label: FieldLabel::Optional,
                    field_type: FieldType::String,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "type".into(),
                },
                FieldDescriptorProto {
                    name: "match".into(),
                    number: 2,
                    label: FieldLabel::Repeated,
                    field_type: FieldType::Uint32,
                    type_name: String::new(),
                    oneof_index: None,
                    json_name: "match".into(),
                },
                FieldDescriptorProto {
                    name: "nested".into(),
                    number: 3,
                    label: FieldLabel::Optional,
                    field_type: FieldType::Message,
                    type_name: "eve_public.test.SampleMsg.Inner".into(),
                    oneof_index: Some(0),
                    json_name: "nested".into(),
                },
            ],
            nested_types: vec![DescriptorProto {
                name: "Inner".into(),
                full_name: "eve_public.test.SampleMsg.Inner".into(),
                fields: vec![],
                nested_types: vec![],
                enum_types: vec![],
                oneofs: vec![],
                is_map_entry: true,
            }],
            enum_types: vec![EnumDescriptorProto {
                name: "State".into(),
                full_name: "eve_public.test.SampleMsg.State".into(),
                values: vec![EnumValueDescriptorProto {
                    name: "IDLE".into(),
                    number: 0,
                }],
            }],
            oneofs: vec![OneofDescriptorProto {
                name: "kind".into(),
            }],
            is_map_entry: false,
        }],
        enums: vec![EnumDescriptorProto {
            name: "TopEnum".into(),
            full_name: "eve_public.test.TopEnum".into(),
            values: vec![EnumValueDescriptorProto {
                name: "FIRST".into(),
                number: 1,
            }],
        }],
        services: vec![ServiceDescriptorProto {
            name: "TestSvc".into(),
            full_name: "eve_public.test.TestSvc".into(),
            methods: vec![MethodDescriptorProto {
                name: "Call".into(),
                input_type: "eve_public.test.SampleMsg".into(),
                output_type: "eve_public.test.SampleMsg".into(),
                client_streaming: true,
                server_streaming: true,
            }],
        }],
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: Vec::new(),
    };
    let wire = file.encode_wire();
    let dec_file =
        FileDescriptorProto::decode_wire("eveProto/generated/test_pb2.pyj", "test", wire).unwrap();
    assert_eq!(dec_file.name, "test.proto");
    assert_eq!(dec_file.services.len(), 1);
    assert!(dec_file.messages[0].nested_types[0].is_map_entry);

    let rust_src = QuasarDescriptorRegistry::generate_rust_module_for_file(&dec_file);
    assert!(rust_src.contains("pub struct SampleMsg"));
    assert!(rust_src.contains("pub r#type: String"));
    assert!(rust_src.contains("pub r#match: Vec<u32>"));
    assert!(rust_src.contains("pub nested: Option<Box<Inner>>"));
    assert!(rust_src.contains("pub enum SampleMsg_State"));
    assert!(rust_src.contains("pub enum TopEnum"));

    // Missing sqlite path error
    assert!(matches!(
        QuasarDescriptorRegistry::open(
            std::path::Path::new("/nonexistent/quasar_descriptors.sqlite"),
            std::path::Path::new("/nonexistent/manifest.json"),
        ),
        Err(QuasarError::MissingDescriptorDatabase(_))
    ));
}
