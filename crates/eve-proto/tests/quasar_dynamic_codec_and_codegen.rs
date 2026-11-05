#![forbid(unsafe_code)]

use eve_proto::quasar::*;
use std::collections::BTreeMap;

#[test]
fn test_quasar_wire_descriptor_dynamic_codec_and_codegen() {
    // Build a synthetic FileDescriptorProto wire payload manually using WireMessage!
    fn wf_str(n: u32, s: &str) -> WireField {
        WireField {
            field_number: n,
            value: WireValue::LengthDelimited(s.as_bytes().to_vec()),
        }
    }
    fn wf_var(n: u32, v: u64) -> WireField {
        WireField {
            field_number: n,
            value: WireValue::Varint(v),
        }
    }
    fn wf_bytes(n: u32, b: Vec<u8>) -> WireField {
        WireField {
            field_number: n,
            value: WireValue::LengthDelimited(b),
        }
    }

    let enum_bytes = WireMessage {
        fields: vec![
            wf_str(1, "Color"),
            wf_bytes(
                2,
                WireMessage {
                    fields: vec![wf_str(1, "RED"), wf_var(2, 1)],
                }
                .encode(),
            ),
        ],
    }
    .encode();

    let field_spec = |num: u32, name: &str, ftype: FieldType, label: FieldLabel, tname: &str, oneof: Option<u32>| -> Vec<u8> {
        let mut fs = vec![
            wf_str(1, name),
            wf_var(3, num as u64),
            wf_var(4, label as i32 as u64),
            wf_var(5, ftype as i32 as u64),
            wf_str(10, name),
        ];
        if !tname.is_empty() {
            fs.push(wf_str(6, tname));
        }
        if let Some(oi) = oneof {
            fs.push(wf_var(9, oi as u64));
        }
        WireMessage { fields: fs }.encode()
    };

    let nested_bytes = WireMessage {
        fields: vec![
            wf_str(1, "SubMsg"),
            wf_bytes(2, field_spec(1, "code", FieldType::Uint32, FieldLabel::Optional, "", None)),
        ],
    }
    .encode();

    let mut msg_fields = vec![
        wf_str(1, "KitchenSink"),
        wf_bytes(3, nested_bytes),
        wf_bytes(4, enum_bytes.clone()),
        wf_bytes(
            7,
            WireMessage {
                fields: vec![wf_var(7, 1)],
            }
            .encode(),
        ),
        wf_bytes(
            8,
            WireMessage {
                fields: vec![wf_str(1, "choice")],
            }
            .encode(),
        ),
    ];
    let scalar_types = [
        (1, "f_double", FieldType::Double),
        (2, "f_float", FieldType::Float),
        (3, "f_i64", FieldType::Int64),
        (4, "f_u64", FieldType::Uint64),
        (5, "f_i32", FieldType::Int32),
        (6, "f_f64", FieldType::Fixed64),
        (7, "f_f32", FieldType::Fixed32),
        (8, "f_bool", FieldType::Bool),
        (9, "f_str", FieldType::String),
        (12, "f_bytes", FieldType::Bytes),
        (13, "f_u32", FieldType::Uint32),
        (15, "f_sf32", FieldType::Sfixed32),
        (16, "f_sf64", FieldType::Sfixed64),
        (17, "f_si32", FieldType::Sint32),
        (18, "f_si64", FieldType::Sint64),
        (19, "type", FieldType::Uint32),
    ];
    for (num, name, ft) in scalar_types {
        msg_fields.push(wf_bytes(
            2,
            field_spec(num, name, ft, FieldLabel::Optional, "", None),
        ));
    }
    msg_fields.push(wf_bytes(
        2,
        field_spec(20, "f_enum", FieldType::Enum, FieldLabel::Optional, ".acme.KitchenSink.Color", None),
    ));
    msg_fields.push(wf_bytes(
        2,
        field_spec(21, "f_sub", FieldType::Message, FieldLabel::Optional, ".acme.KitchenSink.SubMsg", None),
    ));
    msg_fields.push(wf_bytes(
        2,
        field_spec(22, "rep_u32", FieldType::Uint32, FieldLabel::Repeated, "", None),
    ));
    msg_fields.push(wf_bytes(
        2,
        field_spec(23, "rep_str", FieldType::String, FieldLabel::Repeated, "", None),
    ));
    msg_fields.push(wf_bytes(
        2,
        field_spec(24, "oneof_a", FieldType::Uint32, FieldLabel::Optional, "", Some(0)),
    ));
    msg_fields.push(wf_bytes(
        2,
        field_spec(25, "oneof_b", FieldType::String, FieldLabel::Optional, "", Some(0)),
    ));

    let msg_bytes = WireMessage { fields: msg_fields }.encode();
    let svc_bytes = WireMessage {
        fields: vec![
            wf_str(1, "EchoService"),
            wf_bytes(
                2,
                WireMessage {
                    fields: vec![
                        wf_str(1, "Echo"),
                        wf_str(2, ".acme.KitchenSink"),
                        wf_str(3, ".acme.KitchenSink.SubMsg"),
                        wf_var(5, 1),
                        wf_var(6, 1),
                    ],
                }
                .encode(),
            ),
        ],
    }
    .encode();

    let file_bytes = WireMessage {
        fields: vec![
            wf_str(1, "acme/kitchen_sink.proto"),
            wf_str(2, "acme"),
            wf_str(3, "google/protobuf/timestamp.proto"),
            wf_bytes(4, msg_bytes),
            wf_bytes(5, enum_bytes),
            wf_bytes(6, svc_bytes),
            wf_str(12, "proto3"),
        ],
    }
    .encode();

    let fdp = FileDescriptorProto::decode_wire("acme/kitchen_sink.pyj", "acme", file_bytes).unwrap();
    assert_eq!(fdp.package, "acme");
    assert_eq!(fdp.syntax, "proto3");
    assert_eq!(fdp.messages.len(), 1);
    assert!(fdp.messages[0].is_map_entry);
    assert_eq!(fdp.messages[0].nested_types.len(), 1);
    assert_eq!(fdp.services[0].methods[0].client_streaming, true);
    assert_eq!(fdp.services[0].methods[0].server_streaming, true);
    assert!(fdp.messages[0].type_url().ends_with("acme.KitchenSink"));

    // Build registry with this synthetic descriptor
    let mut messages_by_full_name = std::collections::HashMap::new();
    let mut enums_by_full_name = std::collections::HashMap::new();
    let mut services_by_full_name = std::collections::HashMap::new();
    let top_msg = fdp.messages[0].clone();
    for nt in &top_msg.nested_types {
        messages_by_full_name.insert(nt.full_name.clone(), nt.clone());
    }
    for en in &top_msg.enum_types {
        enums_by_full_name.insert(en.full_name.clone(), en.clone());
    }
    messages_by_full_name.insert(top_msg.full_name.clone(), top_msg);
    for sv in &fdp.services {
        services_by_full_name.insert(sv.full_name.clone(), sv.clone());
    }
    let reg = QuasarDescriptorRegistry {
        manifest: QuasarManifest {
            schema_version: 1,
            domain_count: 1,
            descriptor_file_count: 1,
            unique_package_count: 1,
            top_level_message_count: 1,
            total_message_count: 2,
            total_enum_count: 2,
            total_service_count: 1,
            total_field_count: 25,
            total_serialized_pb_bytes: fdp.raw_bytes.len(),
            combined_serialized_pb_sha256: fdp.sha256.clone(),
            combined_serialized_pb_md5: fdp.md5.clone(),
            per_domain_file_counts: BTreeMap::from([("acme".into(), 1)]),
        },
        files: vec![fdp.clone()],
        files_by_pyj_path: [("acme/kitchen_sink.pyj".into(), 0)].into(),
        files_by_proto_name: [("acme/kitchen_sink.proto".into(), 0)].into(),
        files_by_domain: BTreeMap::from([("acme".into(), vec![0])]),
        messages_by_full_name,
        enums_by_full_name,
        services_by_full_name,
    };

    assert!(reg.get_file_by_pyj("acme/kitchen_sink.pyj").is_some());
    assert!(reg.get_file_by_pyj("missing.pyj").is_none());

    // Sample instantiation + encode_dynamic + decode_dynamic roundtrip
    let sample = reg.instantiate_populated_sample("acme.KitchenSink", 7).unwrap();
    let wire = reg.encode_dynamic(&sample).unwrap();
    let decoded = reg.decode_dynamic_or_wire("acme.KitchenSink", &wire).unwrap();
    assert_eq!(decoded.fields.len(), sample.fields.len());

    // Wire fallback decoding for unknown message type
    let fallback = reg
        .decode_dynamic_or_wire("unknown.DynamicMsg", &wire)
        .unwrap();
    assert_eq!(fallback.full_name, "unknown.DynamicMsg");
    assert!(!fallback.fields.is_empty());

    // Wire type mismatch error branch
    let bad_wire = WireMessage {
        fields: vec![wf_str(1, "not_a_double")],
    }
    .encode();
    assert!(matches!(
        reg.decode_dynamic("acme.KitchenSink", &bad_wire),
        Err(QuasarError::WireTypeMismatch { .. })
    ));

    // Rust codegen check (includes `pub r#type:` keyword escaping!)
    let generated = QuasarDescriptorRegistry::generate_rust_module_for_file(&fdp);
    assert!(generated.contains("pub struct KitchenSink"));
    assert!(generated.contains("pub r#type: u32"));
    assert!(generated.contains("pub struct SubMsg"));
    assert!(generated.contains("pub enum KitchenSink_Color"));
}
