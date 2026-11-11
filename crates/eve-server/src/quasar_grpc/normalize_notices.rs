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
        encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_string_field,
        encode_varint_field, encode_zigzag32, encode_zigzag64, FieldType, WireType,
    };
    if depth > 8 {
        return Vec::new();
    }
    let mut out = Vec::new();
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
        encode_varint_field(vf.number, enc_v, &mut out);
        return out;
    }
    if let Some(sf) = desc.fields.iter().find(|f| f.field_type == FieldType::String) {
        encode_string_field(sf.number, &v.to_string(), &mut out);
        return out;
    }
    if let Some(bf) = desc.fields.iter().find(|f| f.field_type == FieldType::Bytes) {
        encode_bytes_field(bf.number, &v.to_le_bytes(), &mut out);
        return out;
    }
    if let Some(ff) = desc
        .fields
        .iter()
        .find(|f| f.field_type.expected_wire_type() == WireType::Fixed64)
    {
        encode_fixed64_field(ff.number, v, &mut out);
        return out;
    }
    if let Some(ff) = desc
        .fields
        .iter()
        .find(|f| f.field_type.expected_wire_type() == WireType::Fixed32)
    {
        encode_fixed32_field(ff.number, v as u32, &mut out);
        return out;
    }
    if let Some(mf) = desc
        .fields
        .iter()
        .find(|f| matches!(f.field_type, FieldType::Message | FieldType::Group))
    {
        if let Some(sub_desc) = descriptors.get_message(&mf.type_name) {
            let sub = coerce_quasar_varint_to_message(descriptors, sub_desc, v, depth + 1);
            encode_bytes_field(mf.number, &sub, &mut out);
        }
    }
    out
}

fn coerce_raw_leaf_bytes_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    buf: &[u8],
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{encode_bytes_field, encode_string_field, encode_varint_field, FieldType};
    if depth > 8 {
        return Vec::new();
    }
    let mut out = Vec::new();
    if desc.full_name.ends_with("semanticversion.Specification") {
        encode_varint_field(1, 24, &mut out);
        encode_varint_field(2, 1, &mut out);
        encode_varint_field(3, 0, &mut out);
        return out;
    }
    if let Some(sf) = desc.fields.iter().find(|f| f.field_type == FieldType::String) {
        let s = match std::str::from_utf8(buf) {
            Ok(valid) => valid.to_string(),
            Err(_) => buf.iter().map(|b| format!("{b:02x}")).collect(),
        };
        encode_string_field(sf.number, &s, &mut out);
        return out;
    }
    if let Some(bf) = desc.fields.iter().find(|f| f.field_type == FieldType::Bytes) {
        encode_bytes_field(bf.number, buf, &mut out);
        return out;
    }
    if let Some(mf) = desc
        .fields
        .iter()
        .find(|f| matches!(f.field_type, FieldType::Message | FieldType::Group))
    {
        if let Some(sub_desc) = descriptors.get_message(&mf.type_name) {
            let sub = coerce_raw_leaf_bytes_to_message(descriptors, sub_desc, buf, depth + 1);
            encode_bytes_field(mf.number, &sub, &mut out);
        }
    }
    out
}

fn coerce_quasar_bytes_to_message(
    descriptors: &eve_proto::quasar::QuasarDescriptorRegistry,
    desc: &eve_proto::quasar::DescriptorProto,
    buf: &[u8],
    depth: usize,
) -> Vec<u8> {
    use eve_proto::quasar::{
        encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_string_field,
        encode_varint_field, FieldType, WireMessage, WireType, WireValue,
    };
    if depth > 8 || buf.is_empty() {
        return Vec::new();
    }
    if descriptors.decode_dynamic(&desc.full_name, buf).is_ok() {
        return buf.to_vec();
    }
    let Ok(wm) = WireMessage::decode(buf) else {
        return coerce_raw_leaf_bytes_to_message(descriptors, desc, buf, depth);
    };
    let mut out = Vec::new();
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
                        encode_bytes_field(fdesc.number, &sub, &mut out);
                    }
                }
                WireValue::Varint(v) => {
                    if let Some(sub_desc) = descriptors.get_message(&fdesc.type_name) {
                        let sub =
                            coerce_quasar_varint_to_message(descriptors, sub_desc, v, depth + 1);
                        encode_bytes_field(fdesc.number, &sub, &mut out);
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
                    encode_string_field(fdesc.number, &s, &mut out);
                }
                WireValue::Varint(v) => {
                    encode_string_field(fdesc.number, &v.to_string(), &mut out);
                }
                _ => {}
            },
            FieldType::Bytes => match wf.value {
                WireValue::LengthDelimited(b) => encode_bytes_field(fdesc.number, &b, &mut out),
                WireValue::Varint(v) => {
                    encode_bytes_field(fdesc.number, &v.to_le_bytes(), &mut out)
                }
                _ => {}
            },
            ft if ft.expected_wire_type() == WireType::Varint => match wf.value {
                WireValue::Varint(v) => encode_varint_field(fdesc.number, v, &mut out),
                WireValue::LengthDelimited(b) => {
                    let v = WireMessage::decode(&b)
                        .ok()
                        .and_then(|sub| sub.fields.iter().find_map(|sf| sf.value.as_varint()))
                        .unwrap_or(0);
                    encode_varint_field(fdesc.number, v, &mut out);
                }
                WireValue::Fixed64(v) => encode_varint_field(fdesc.number, v, &mut out),
                WireValue::Fixed32(v) => encode_varint_field(fdesc.number, u64::from(v), &mut out),
            },
            ft if ft.expected_wire_type() == WireType::Fixed64 => match wf.value {
                WireValue::Fixed64(v) | WireValue::Varint(v) => {
                    encode_fixed64_field(fdesc.number, v, &mut out)
                }
                _ => {}
            },
            ft if ft.expected_wire_type() == WireType::Fixed32 => match wf.value {
                WireValue::Fixed32(v) => encode_fixed32_field(fdesc.number, v, &mut out),
                WireValue::Varint(v) => encode_fixed32_field(fdesc.number, v as u32, &mut out),
                _ => {}
            },
            _ => {}
        }
    }
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
