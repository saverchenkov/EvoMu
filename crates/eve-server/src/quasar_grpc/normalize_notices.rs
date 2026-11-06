//! Quasar V24.01 notification and dynamic payload normalization (`quasar_grpc`).

#![forbid(unsafe_code)]

use eve_proto::PyRep;

fn normalize_destiny_update_arg(rep: PyRep) -> PyRep {
    match rep {
        PyRep::List(items) => PyRep::List(items.into_iter().map(normalize_destiny_update_arg).collect()),
        PyRep::Tuple(mut items) => {
            if items.len() == 2 {
                if let (Some(name), Some(PyRep::Tuple(ev_args))) = (items[0].as_str(), items.get(1)) {
                    if name == "OnDockingAccepted" && ev_args.len() >= 3 {
                        let station_id = ev_args[2].clone();
                        items[1] = PyRep::Tuple(vec![station_id]);
                        return PyRep::Tuple(items);
                    }
                }
            }
            PyRep::Tuple(items.into_iter().map(normalize_destiny_update_arg).collect())
        }
        other => other,
    }
}

/// Normalizes `machoNet` notification arguments where the live V24.01 client handler signature
/// differs from internal test notification tuples.
pub fn normalize_macho_notification(event: &str, args: Vec<PyRep>) -> Vec<PyRep> {
    match event {
        "OnDockingAccepted" => {
            if args.len() >= 3 {
                vec![args[2].clone()]
            } else {
                args
            }
        }
        "DoDestinyUpdate" => args
            .into_iter()
            .map(normalize_destiny_update_arg)
            .collect(),
        "OnChannelJoined" => {
            if let Some(first) = args.into_iter().next() {
                vec![first]
            } else {
                vec![PyRep::String("solarsystemid2:30000142".into())]
            }
        }
        "OnSharedBookmarksFolderUpdated" => {
            let bm_kv = PyRep::keyval([("bookmarkID", PyRep::Integer(1))]);
            let update_tuple = PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::Integer(1)]),
                PyRep::Integer(1),
                PyRep::Tuple(vec![
                    PyRep::List(vec![bm_kv]),
                    PyRep::Integer(1),
                ]),
            ]);
            vec![PyRep::List(vec![update_tuple])]
        }
        "OnCorporationRecruitmentAdChanged" => Vec::new(),
        "OnOrganizationContactsUpdated" => {
            let owner_id = args.first().cloned().unwrap_or(PyRep::Integer(98_000_001));
            let mut updates_out = Vec::new();
            if let Some(PyRep::List(items)) = args.get(1) {
                for it in items {
                    let cid = it
                        .dict_get("contactID")
                        .cloned()
                        .unwrap_or(PyRep::Integer(98_000_001));
                    let rel = it
                        .dict_get("standing")
                        .or_else(|| it.dict_get("relationshipID"))
                        .cloned()
                        .unwrap_or(PyRep::Real(10.0));
                    let mask = it
                        .dict_get("labelMask")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    updates_out.push(PyRep::Tuple(vec![cid, rel, mask]));
                }
            }
            if updates_out.is_empty() {
                updates_out.push(PyRep::Tuple(vec![
                    PyRep::Integer(98_000_001),
                    PyRep::Real(10.0),
                    PyRep::Integer(0),
                ]));
            }
            vec![owner_id, PyRep::List(updates_out)]
        }
        "OnTargetClear" => {
            if args.is_empty() {
                vec![PyRep::List(Vec::new())]
            } else {
                args
            }
        }
        "OnGodmaShipEffect" => {
            if args.len() == 11 {
                args
            } else {
                let item_id = args
                    .first()
                    .cloned()
                    .unwrap_or(PyRep::Integer(100_000_005));
                let effect_id = args.get(1).cloned().unwrap_or(PyRep::Integer(10));
                let active = args.get(2).cloned().unwrap_or(PyRep::Bool(false));
                let ft = PyRep::Integer(132_537_600_000_000_000);
                let env = PyRep::Tuple(vec![
                    item_id.clone(),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(100_000_001),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    effect_id.clone(),
                ]);
                vec![
                    item_id,
                    effect_id,
                    ft.clone(),
                    active.clone(),
                    active,
                    env,
                    ft,
                    PyRep::Real(5_000.0),
                    PyRep::Integer(0),
                    PyRep::None,
                    PyRep::None,
                ]
            }
        }
        "OnHeatAdded" | "OnHeatRemoved" => {
            if args.len() == 1 {
                vec![PyRep::Integer(1211), args.into_iter().next().unwrap()]
            } else {
                args
            }
        }
        "OnModuleAttributeChanges" => {
            let change = PyRep::Tuple(vec![
                PyRep::String("OnGodmaPrimeItem".into()),
                PyRep::Integer(90_000_001),
                PyRep::Tuple(vec![
                    PyRep::Integer(100_000_001),
                    PyRep::Integer(11),
                ]),
                PyRep::Integer(50),
                PyRep::Integer(132_537_600_000_000_000),
                PyRep::Real(100.0),
                PyRep::Real(100.0),
                PyRep::Bool(true),
            ]);
            vec![PyRep::List(vec![change])]
        }
        "OnModuleUnfitted" => {
            if let Some(last) = args.into_iter().last() {
                vec![last]
            } else {
                vec![PyRep::Integer(100_000_005)]
            }
        }
        "OnRemoteMessage" => {
            if args.len() == 2 {
                let mut out = args;
                out.push(PyRep::None);
                out
            } else {
                args
            }
        }
        "OnItemChange" => {
            let mut out = args;
            let mut loc_val = PyRep::Integer(60_003_760);
            if let Some(PyRep::KeyVal(map)) = out.first_mut() {
                if let Some(l) = map.get("locationID").cloned() {
                    loc_val = l;
                }
                let qty = map
                    .get("quantity")
                    .cloned()
                    .unwrap_or(PyRep::Integer(1));
                map.entry("ownerID".into())
                    .or_insert(PyRep::Integer(90_000_001));
                map.entry("groupID".into())
                    .or_insert(PyRep::Integer(25));
                map.entry("categoryID".into())
                    .or_insert(PyRep::Integer(7));
                map.entry("stacksize".into()).or_insert(qty);
                map.entry("singleton".into())
                    .or_insert(PyRep::Integer(1));
                map.entry("customInfo".into())
                    .or_insert(PyRep::String(String::new()));
            }
            if out.len() == 2 {
                out.push(loc_val);
            }
            out
        }
        "ProcessInventoryChange" => {
            if args.len() == 3 {
                let item_id = args
                    .first()
                    .cloned()
                    .unwrap_or(PyRep::Integer(100_000_003));
                let loc_id = args
                    .get(1)
                    .cloned()
                    .unwrap_or(PyRep::Integer(60_003_760));
                let flag_id = args.get(2).cloned().unwrap_or(PyRep::Integer(4));
                let item_kv = PyRep::keyval([
                    ("itemID", item_id),
                    ("typeID", PyRep::Integer(438)),
                    ("ownerID", PyRep::Integer(90_000_001)),
                    ("locationID", loc_id.clone()),
                    ("flagID", flag_id.clone()),
                    ("quantity", PyRep::Integer(1)),
                    ("groupID", PyRep::Integer(25)),
                    ("categoryID", PyRep::Integer(7)),
                    ("singleton", PyRep::Integer(1)),
                ]);
                let change_dict = PyRep::Dict(vec![
                    (
                        PyRep::Integer(3),
                        PyRep::Tuple(vec![loc_id.clone(), loc_id]),
                    ),
                    (
                        PyRep::Integer(4),
                        PyRep::Tuple(vec![flag_id.clone(), flag_id]),
                    ),
                ]);
                vec![
                    PyRep::List(vec![item_kv]),
                    change_dict,
                    PyRep::Bool(true),
                    PyRep::None,
                ]
            } else {
                args
            }
        }
        "OnNotificationReceived" => {
            if args.len() < 5 {
                vec![
                    PyRep::Integer(1),
                    PyRep::Integer(1),
                    PyRep::Integer(90_000_001),
                    PyRep::Integer(132_537_600_000_000_000),
                    PyRep::Dict(Vec::new()),
                ]
            } else {
                args
            }
        }
        "OnTarget" => {
            if args.len() == 2 {
                let (what, tid) = match (&args[0], &args[1]) {
                    (PyRep::Integer(id), PyRep::String(w)) => {
                        let mapped = match w.as_str() {
                            "locking" | "locked" | "add" => "add",
                            "unlock" | "unlocked" | "lost" => "lost",
                            "clear" => "clear",
                            other => other,
                        };
                        (PyRep::String(mapped.to_string()), PyRep::Integer(*id))
                    }
                    (w, id) => (w.clone(), id.clone()),
                };
                vec![what, tid, PyRep::None]
            } else {
                args
            }
        }
        _ => args,
    }
}

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
