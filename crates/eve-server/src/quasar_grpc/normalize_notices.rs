//! Quasar V24.01 notification and dynamic payload normalization (`quasar_grpc`).

#![forbid(unsafe_code)]

#[allow(unused_imports)]
pub use crate::router::client_compat::normalize_macho_notification;

/// Normalizes a Quasar `EvePublicResponse` payload against the 1,455-file `QuasarDescriptorRegistry`
/// so all responses transmitted over `:26005` strictly conform to the V24.01 `.proto` wire schema.
pub fn normalize_quasar_client_response(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    rsp: &mut eve_proto::quasar::EvePublicResponse,
) {
    if let Some(ref mut any_out) = rsp.payload {
        normalize_quasar_any_payload(descriptors, any_out);
    }
}

/// Normalizes a `google.protobuf.Any` payload (`type_url` and wire `value`) against `descriptors`.
pub fn normalize_quasar_any_payload(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    any_out: &mut eve_proto::quasar::GoogleAny,
) {
    use eve_proto::quasar::TYPE_GOOGLEAPIS_PREFIX;
    let norm = any_out.normalized_type_name().to_string();
    if matches!(
        norm.as_str(),
        "eve.cosmetic.character.title.definition.api.GetAllResponse"
            | "eve_public.cosmetic.character.title.definition.api.GetAllResponse"
    ) {
        any_out.type_url = format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve.cosmetic.character.title.definition.api.ListDefinitionsResponse"
        );
    } else if norm == "eve.mail.cspa.SetResponse" {
        any_out.type_url = format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty");
        return;
    }
    let norm = any_out.normalized_type_name().to_string();
    if descriptors.decode_dynamic(&norm, &any_out.value).is_ok() {
        return;
    }
    if let Some(desc) = descriptors.get_message(&norm) {
        any_out.value = coerce_quasar_bytes_to_message(descriptors, desc, &any_out.value, 0);
    }
}

fn coerce_quasar_varint_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    v: u64,
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{
        encode_zigzag32, encode_zigzag64, FieldType, WireField, WireMessage, WireType, WireValue,
    };
    if depth > 8 {
        return Vec::new();
    }
    let varint_field = desc
        .fields
        .iter()
        .find(|f| f.number == 1 && f.field_type.expected_wire_type() == WireType::Varint)
        .or_else(|| {
            desc.fields
                .iter()
                .find(|f| f.field_type.expected_wire_type() == WireType::Varint)
        });
    if let Some(vf) = varint_field {
        let enc_v = match vf.field_type {
            FieldType::Sint32 => encode_zigzag32(v as i32) as u64,
            FieldType::Sint64 => encode_zigzag64(v as i64),
            _ => v,
        };
        return WireMessage {
            fields: vec![WireField {
                field_number: vf.number,
                value: WireValue::Varint(enc_v),
            }],
        }
        .encode();
    }
    if let Some(sf) = desc.fields.iter().find(|f| f.field_type == FieldType::String) {
        return WireMessage {
            fields: vec![WireField {
                field_number: sf.number,
                value: WireValue::LengthDelimited(v.to_string().into_bytes()),
            }],
        }
        .encode();
    }
    if let Some(bf) = desc.fields.iter().find(|f| f.field_type == FieldType::Bytes) {
        return WireMessage {
            fields: vec![WireField {
                field_number: bf.number,
                value: WireValue::LengthDelimited(v.to_le_bytes().to_vec()),
            }],
        }
        .encode();
    }
    if let Some(ff) = desc
        .fields
        .iter()
        .find(|f| f.field_type.expected_wire_type() == WireType::Fixed64)
    {
        return WireMessage {
            fields: vec![WireField {
                field_number: ff.number,
                value: WireValue::Fixed64(v),
            }],
        }
        .encode();
    }
    if let Some(ff) = desc
        .fields
        .iter()
        .find(|f| f.field_type.expected_wire_type() == WireType::Fixed32)
    {
        return WireMessage {
            fields: vec![WireField {
                field_number: ff.number,
                value: WireValue::Fixed32(v as u32),
            }],
        }
        .encode();
    }
    if let Some(mf) = desc
        .fields
        .iter()
        .find(|f| matches!(f.field_type, FieldType::Message | FieldType::Group))
    {
        if let Some(sub_desc) = descriptors.get_message(&mf.type_name) {
            let sub = coerce_quasar_varint_to_message(descriptors, sub_desc, v, depth + 1);
            return WireMessage {
                fields: vec![WireField {
                    field_number: mf.number,
                    value: WireValue::LengthDelimited(sub),
                }],
            }
            .encode();
        }
    }
    Vec::new()
}

fn coerce_raw_leaf_bytes_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    buf: &[u8],
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{FieldType, WireField, WireMessage, WireValue};
    if depth > 8 {
        return Vec::new();
    }
    if desc.full_name.ends_with("semanticversion.Specification") {
        return WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::Varint(24),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::Varint(1),
                },
                WireField {
                    field_number: 3,
                    value: WireValue::Varint(0),
                },
            ],
        }
        .encode();
    }
    if let Some(sf) = desc.fields.iter().find(|f| f.field_type == FieldType::String) {
        let s = match std::str::from_utf8(buf) {
            Ok(valid) => valid.to_string(),
            Err(_) => buf.iter().map(|b| format!("{b:02x}")).collect(),
        };
        return WireMessage {
            fields: vec![WireField {
                field_number: sf.number,
                value: WireValue::LengthDelimited(s.into_bytes()),
            }],
        }
        .encode();
    }
    if let Some(bf) = desc.fields.iter().find(|f| f.field_type == FieldType::Bytes) {
        return WireMessage {
            fields: vec![WireField {
                field_number: bf.number,
                value: WireValue::LengthDelimited(buf.to_vec()),
            }],
        }
        .encode();
    }
    if let Some(mf) = desc
        .fields
        .iter()
        .find(|f| matches!(f.field_type, FieldType::Message | FieldType::Group))
    {
        if let Some(sub_desc) = descriptors.get_message(&mf.type_name) {
            let sub = coerce_raw_leaf_bytes_to_message(descriptors, sub_desc, buf, depth + 1);
            return WireMessage {
                fields: vec![WireField {
                    field_number: mf.number,
                    value: WireValue::LengthDelimited(sub),
                }],
            }
            .encode();
        }
    }
    Vec::new()
}

fn coerce_quasar_bytes_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    buf: &[u8],
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{FieldType, WireField, WireMessage, WireType, WireValue};
    if depth > 8 || buf.is_empty() {
        return Vec::new();
    }
    if descriptors.decode_dynamic(&desc.full_name, buf).is_ok() {
        return buf.to_vec();
    }
    let Ok(wm) = WireMessage::decode(buf) else {
        return coerce_raw_leaf_bytes_to_message(descriptors, desc, buf, depth);
    };
    let mut out_fields = Vec::new();
    for wf in wm.fields {
        let Some(fdesc) = desc.fields.iter().find(|f| f.number == wf.field_number) else {
            continue;
        };
        match fdesc.field_type {
            FieldType::Message | FieldType::Group => match wf.value {
                WireValue::LengthDelimited(sub_bytes) => {
                    if let Some(sub_desc) = descriptors.get_message(&fdesc.type_name) {
                        let sub = coerce_quasar_bytes_to_message(
                            descriptors,
                            sub_desc,
                            &sub_bytes,
                            depth + 1,
                        );
                        out_fields.push(WireField {
                            field_number: fdesc.number,
                            value: WireValue::LengthDelimited(sub),
                        });
                    }
                }
                WireValue::Varint(v) => {
                    if let Some(sub_desc) = descriptors.get_message(&fdesc.type_name) {
                        let sub =
                            coerce_quasar_varint_to_message(descriptors, sub_desc, v, depth + 1);
                        out_fields.push(WireField {
                            field_number: fdesc.number,
                            value: WireValue::LengthDelimited(sub),
                        });
                    }
                }
                _ => {}
            },
            FieldType::String => match wf.value {
                WireValue::LengthDelimited(b) => {
                    let s = match std::str::from_utf8(&b) {
                        Ok(valid) => valid.to_string(),
                        Err(_) => b.iter().map(|byte| format!("{byte:02x}")).collect(),
                    };
                    out_fields.push(WireField {
                        field_number: fdesc.number,
                        value: WireValue::LengthDelimited(s.into_bytes()),
                    });
                }
                WireValue::Varint(v) => {
                    out_fields.push(WireField {
                        field_number: fdesc.number,
                        value: WireValue::LengthDelimited(v.to_string().into_bytes()),
                    });
                }
                _ => {}
            },
            FieldType::Bytes => match wf.value {
                WireValue::LengthDelimited(b) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::LengthDelimited(b),
                }),
                WireValue::Varint(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::LengthDelimited(v.to_le_bytes().to_vec()),
                }),
                _ => {}
            },
            ft if ft.expected_wire_type() == WireType::Varint => match wf.value {
                WireValue::Varint(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::Varint(v),
                }),
                WireValue::LengthDelimited(b) => {
                    let v = WireMessage::decode(&b)
                        .ok()
                        .and_then(|sub| sub.fields.iter().find_map(|sf| sf.value.as_varint()))
                        .unwrap_or(0);
                    out_fields.push(WireField {
                        field_number: fdesc.number,
                        value: WireValue::Varint(v),
                    });
                }
                WireValue::Fixed64(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::Varint(v),
                }),
                WireValue::Fixed32(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::Varint(u64::from(v)),
                }),
            },
            ft if ft.expected_wire_type() == WireType::Fixed64 => match wf.value {
                WireValue::Fixed64(v) | WireValue::Varint(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::Fixed64(v),
                }),
                _ => {}
            },
            ft if ft.expected_wire_type() == WireType::Fixed32 => match wf.value {
                WireValue::Fixed32(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::Fixed32(v),
                }),
                WireValue::Varint(v) => out_fields.push(WireField {
                    field_number: fdesc.number,
                    value: WireValue::Fixed32(v as u32),
                }),
                _ => {}
            },
            _ => {}
        }
    }
    let out = WireMessage { fields: out_fields }.encode();
    if out.is_empty() && !buf.is_empty() {
        let leaf = coerce_raw_leaf_bytes_to_message(descriptors, desc, buf, depth);
        if descriptors.decode_dynamic(&desc.full_name, &leaf).is_ok() {
            return leaf;
        }
    }
    if descriptors.decode_dynamic(&desc.full_name, &out).is_ok() {
        out
    } else {
        Vec::new()
    }
}
