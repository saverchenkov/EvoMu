//! Comprehensive unit tests for `quasar_grpc::normalize_notices`.

use super::normalize_notices::{normalize_quasar_any_payload, normalize_quasar_client_response};
use eve_proto::quasar::{
    DescriptorProto, DynamicValue, EvePublicResponse, FieldDescriptorProto, FieldLabel, FieldType,
    GoogleAny, QuasarDescriptorRegistry, WireField, WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};

fn field(number: i32, name: &str, field_type: FieldType, type_name: &str) -> FieldDescriptorProto {
    FieldDescriptorProto {
        name: name.into(),
        number: number as u32,
        label: FieldLabel::Optional,
        field_type,
        type_name: type_name.into(),
        oneof_index: None,
        json_name: name.into(),
    }
}

fn msg(full_name: &str, fields: Vec<FieldDescriptorProto>) -> DescriptorProto {
    let name = full_name.rsplit('.').next().unwrap_or(full_name).to_string();
    DescriptorProto {
        name,
        full_name: full_name.into(),
        fields,
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    }
}

fn build_test_descriptors() -> QuasarDescriptorRegistry {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    for m in [
        msg(
            "test.VarintPrefer1",
            vec![
                field(2, "second", FieldType::Int64, ""),
                field(1, "first", FieldType::Sint32, ""),
            ],
        ),
        msg(
            "test.Sint64Box",
            vec![field(3, "val", FieldType::Sint64, "")],
        ),
        msg(
            "test.StringBox",
            vec![field(1, "text", FieldType::String, "")],
        ),
        msg(
            "test.BytesBox",
            vec![field(1, "raw", FieldType::Bytes, "")],
        ),
        msg(
            "test.Fixed64Box",
            vec![field(1, "f64", FieldType::Fixed64, "")],
        ),
        msg(
            "test.Fixed32Box",
            vec![field(1, "f32", FieldType::Fixed32, "")],
        ),
        msg(
            "test.MsgWrap",
            vec![field(1, "inner", FieldType::Message, "test.StringBox")],
        ),
        msg(
            "test.DeepCycle",
            vec![field(1, "next", FieldType::Message, "test.DeepCycle")],
        ),
        msg(
            "eve.semanticversion.Specification",
            vec![
                field(1, "major", FieldType::Int32, ""),
                field(2, "minor", FieldType::Int32, ""),
                field(3, "patch", FieldType::Int32, ""),
            ],
        ),
        msg(
            "test.AllFields",
            vec![
                field(1, "m_sub", FieldType::Message, "test.VarintPrefer1"),
                field(2, "s_val", FieldType::String, ""),
                field(3, "b_val", FieldType::Bytes, ""),
                field(4, "v_val", FieldType::Int64, ""),
                field(5, "f64_val", FieldType::Fixed64, ""),
                field(6, "f32_val", FieldType::Fixed32, ""),
                field(7, "m_str", FieldType::Message, "test.StringBox"),
                field(8, "m_bytes", FieldType::Message, "test.BytesBox"),
                field(9, "m_f64", FieldType::Message, "test.Fixed64Box"),
                field(10, "m_f32", FieldType::Message, "test.Fixed32Box"),
                field(11, "m_s64", FieldType::Message, "test.Sint64Box"),
                field(12, "m_wrap", FieldType::Message, "test.MsgWrap"),
                field(13, "m_cycle", FieldType::Message, "test.DeepCycle"),
            ],
        ),
        msg(
            "eve.cosmetic.character.title.definition.api.ListDefinitionsResponse",
            vec![field(1, "title", FieldType::String, "")],
        ),
    ] {
        reg.messages_by_full_name.insert(m.full_name.clone(), m);
    }
    reg
}

#[test]
fn quasar_response_and_any_payload_coercion_covers_wire_variants() {
    let reg = build_test_descriptors();

    // None payload in EvePublicResponse is a no-op
    let mut rsp_none = EvePublicResponse::default();
    normalize_quasar_client_response(&reg, &mut rsp_none);
    assert!(rsp_none.payload.is_none());

    // GetAllResponse type_url rewrite + valid payload preserved
    let valid_title_bytes = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(b"Admiral".to_vec()),
        }],
    }
    .encode();
    let mut rsp_titles = EvePublicResponse {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.character.title.definition.api.GetAllResponse"),
            value: valid_title_bytes.clone(),
        }),
        ..Default::default()
    };
    normalize_quasar_client_response(&reg, &mut rsp_titles);
    let p = rsp_titles.payload.unwrap();
    assert!(p.type_url.ends_with("ListDefinitionsResponse"));
    assert_eq!(p.value, valid_title_bytes);

    let mut any_pub = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.definition.api.GetAllResponse"),
        value: Vec::new(),
    };
    normalize_quasar_any_payload(&reg, &mut any_pub);
    assert!(any_pub.type_url.ends_with("ListDefinitionsResponse"));

    // eve.mail.cspa.SetResponse -> google.protobuf.Empty early return
    let mut any_cspa = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.mail.cspa.SetResponse"),
        value: vec![1, 2, 3],
    };
    normalize_quasar_any_payload(&reg, &mut any_cspa);
    assert_eq!(any_cspa.type_url, format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"));
    assert_eq!(any_cspa.value, vec![1, 2, 3]);

    // Unknown descriptor leaves payload intact
    let mut any_unknown = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}unknown.DoesNotExist"),
        value: vec![0xff, 0xff],
    };
    normalize_quasar_any_payload(&reg, &mut any_unknown);
    assert_eq!(any_unknown.value, vec![0xff, 0xff]);

    // semanticversion.Specification raw leaf coercion on invalid wire bytes
    let mut any_semver = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.semanticversion.Specification"),
        value: vec![0xff, 0xff, 0xff], // invalid varint wire
    };
    normalize_quasar_any_payload(&reg, &mut any_semver);
    let dec_sv = reg.decode_dynamic("eve.semanticversion.Specification", &any_semver.value).unwrap();
    assert_eq!(dec_sv.fields.get(&1), Some(&DynamicValue::Int32(24)));
    assert_eq!(dec_sv.fields.get(&2), Some(&DynamicValue::Int32(1)));
    assert_eq!(dec_sv.fields.get(&3), Some(&DynamicValue::Int32(0)));

    // StringBox raw leaf coercion: valid UTF-8 vs invalid UTF-8 hex fallback
    let mut any_str_valid = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.StringBox"),
        value: vec![0x80, 0x80], // invalid wire, invalid UTF-8
    };
    normalize_quasar_any_payload(&reg, &mut any_str_valid);
    let dec_str = reg.decode_dynamic("test.StringBox", &any_str_valid.value).unwrap();
    assert_eq!(dec_str.fields.get(&1), Some(&DynamicValue::String("8080".into())));

    let mut any_str_utf8 = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.StringBox"),
        value: b"hi\xff".to_vec(),
    };
    normalize_quasar_any_payload(&reg, &mut any_str_utf8);
    assert!(reg.decode_dynamic("test.StringBox", &any_str_utf8.value).is_ok());

    // MsgWrap raw leaf coercion (recurses into nested StringBox) + BytesBox raw leaf coercion
    let mut any_wrap = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.MsgWrap"),
        value: vec![0x80, 0x81],
    };
    normalize_quasar_any_payload(&reg, &mut any_wrap);
    assert!(reg.decode_dynamic("test.MsgWrap", &any_wrap.value).is_ok());

    let mut any_bytes = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.BytesBox"),
        value: vec![0x80, 0x81],
    };
    normalize_quasar_any_payload(&reg, &mut any_bytes);
    assert!(reg.decode_dynamic("test.BytesBox", &any_bytes.value).is_ok());

    // Unknown wire field number falls back to coerce_raw_leaf_bytes_to_message
    let unknown_only = WireMessage {
        fields: vec![WireField {
            field_number: 99,
            value: WireValue::Varint(123),
        }],
    }
    .encode();
    let mut any_unknown_field = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.BytesBox"),
        value: unknown_only,
    };
    normalize_quasar_any_payload(&reg, &mut any_unknown_field);
    assert!(reg.decode_dynamic("test.BytesBox", &any_unknown_field.value).is_ok());

    // Wire-type mismatch coercion across all field types in test.AllFields
    let sub_varint = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(77),
        }],
    }
    .encode();
    let bad_sub = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(vec![0xfe, 0xff]),
        }],
    }
    .encode();
    let mismatched = WireMessage {
        fields: vec![
            WireField { field_number: 999, value: WireValue::Varint(1) }, // unknown field skipped
            WireField { field_number: 1, value: WireValue::Varint(5) },   // Message <- Varint (VarintPrefer1 picks #1 Sint32)
            WireField { field_number: 2, value: WireValue::Varint(42) },  // String <- Varint ("42")
            WireField { field_number: 3, value: WireValue::Varint(7) },   // Bytes <- Varint (7u64.to_le_bytes())
            WireField { field_number: 4, value: WireValue::LengthDelimited(sub_varint) }, // Int64 <- LengthDelimited(sub_varint)
            WireField { field_number: 5, value: WireValue::Varint(88) },  // Fixed64 <- Varint
            WireField { field_number: 6, value: WireValue::Varint(99) },  // Fixed32 <- Varint
            WireField { field_number: 7, value: WireValue::Varint(11) },  // Msg(StringBox) <- Varint
            WireField { field_number: 8, value: WireValue::Varint(22) },  // Msg(BytesBox) <- Varint
            WireField { field_number: 9, value: WireValue::Varint(33) },  // Msg(Fixed64Box) <- Varint
            WireField { field_number: 10, value: WireValue::Varint(44) }, // Msg(Fixed32Box) <- Varint
            WireField { field_number: 11, value: WireValue::Varint(55) }, // Msg(Sint64Box) <- Varint
            WireField { field_number: 12, value: WireValue::Varint(66) }, // Msg(MsgWrap) <- Varint
            // DeepCycle (>8 levels) varint triggers depth > 8 guard -> empty submessage
            WireField { field_number: 13, value: WireValue::Varint(1) },
            // Nested StringBox inside MsgWrap with invalid UTF-8 bytes
            WireField { field_number: 12, value: WireValue::LengthDelimited(bad_sub) },
        ],
    }
    .encode();

    let mut any_all = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.AllFields"),
        value: mismatched,
    };
    normalize_quasar_any_payload(&reg, &mut any_all);
    let dec_all = reg.decode_dynamic("test.AllFields", &any_all.value).unwrap();
    assert_eq!(dec_all.fields.get(&2), Some(&DynamicValue::String("42".into())));
    assert_eq!(dec_all.fields.get(&4), Some(&DynamicValue::Int64(77)));

    // Also test Fixed64/Fixed32 -> Varint field coercion
    let fixed_to_varint = WireMessage {
        fields: vec![WireField {
            field_number: 4,
            value: WireValue::Fixed64(1234),
        }],
    }
    .encode();
    let mut any_f64 = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.AllFields"),
        value: fixed_to_varint,
    };
    normalize_quasar_any_payload(&reg, &mut any_f64);
    let dec_f64 = reg.decode_dynamic("test.AllFields", &any_f64.value).unwrap();
    assert_eq!(dec_f64.fields.get(&4), Some(&DynamicValue::Int64(1234)));

    let fixed32_to_varint = WireMessage {
        fields: vec![WireField {
            field_number: 4,
            value: WireValue::Fixed32(4321),
        }],
    }
    .encode();
    let mut any_f32 = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}test.AllFields"),
        value: fixed32_to_varint,
    };
    normalize_quasar_any_payload(&reg, &mut any_f32);
    let dec_f32 = reg.decode_dynamic("test.AllFields", &any_f32.value).unwrap();
    assert_eq!(dec_f32.fields.get(&4), Some(&DynamicValue::Int64(4321)));
}
