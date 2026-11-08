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
                Vec::new()
            }
        }
        "OnSharedBookmarksFolderUpdated" => {
            let mut out = args;
            if let Some(PyRep::List(entries)) = out.first_mut() {
                for entry in entries.iter_mut() {
                    if let PyRep::Tuple(ref mut parts) = entry {
                        if parts.len() == 3 && !matches!(parts[2], PyRep::Tuple(_)) {
                            let bms: Vec<PyRep> = match &parts[2] {
                                PyRep::List(items) => items
                                    .iter()
                                    .map(|it| {
                                        if it.dict_get("bookmarkID").is_some() {
                                            it.clone()
                                        } else {
                                            PyRep::keyval([("bookmarkID", it.clone())])
                                        }
                                    })
                                    .collect(),
                                _ => Vec::new(),
                            };
                            parts[2] = PyRep::Tuple(vec![PyRep::List(bms), PyRep::List(Vec::new())]);
                        }
                    }
                }
            }
            out
        }
        "OnBeanPrime" => {
            let dict_to_bean_kv = |entries: Vec<(PyRep, PyRep)>| {
                let mut map = std::collections::HashMap::new();
                for (k, v) in entries {
                    if let Some(ks) = k.as_str() {
                        map.insert(ks.to_string(), v);
                    }
                }
                map.entry("errorID".into()).or_insert(PyRep::Integer(0));
                map.entry("errorKeyHash".into())
                    .or_insert(PyRep::String(String::new()));
                map.entry("logMode".into()).or_insert(PyRep::Integer(0));
                PyRep::KeyVal(map)
            };
            let raw_items = if args.len() == 1 {
                match args.into_iter().next().unwrap() {
                    PyRep::List(items) => items,
                    other => vec![other],
                }
            } else {
                args
            };
            let beans: Vec<PyRep> = raw_items
                .into_iter()
                .filter_map(|a| match a {
                    PyRep::Integer(err_id) => Some(PyRep::keyval([
                        ("errorID", PyRep::Integer(err_id)),
                        ("errorKeyHash", PyRep::String(String::new())),
                        ("logMode", PyRep::Integer(0)),
                    ])),
                    PyRep::Dict(entries) => Some(dict_to_bean_kv(entries)),
                    PyRep::KeyVal(mut map) => {
                        map.entry("errorID".into()).or_insert(PyRep::Integer(0));
                        map.entry("errorKeyHash".into())
                            .or_insert(PyRep::String(String::new()));
                        map.entry("logMode".into()).or_insert(PyRep::Integer(0));
                        Some(PyRep::KeyVal(map))
                    }
                    _ => None,
                })
                .collect();
            vec![PyRep::List(beans)]
        }
        "OnCorporationRecruitmentAdChanged" => Vec::new(),
        "OnOrganizationContactsUpdated" => {
            let Some(owner_id) = args.first().cloned() else {
                return Vec::new();
            };
            let mut updates_out = Vec::new();
            if let Some(PyRep::List(items)) = args.get(1) {
                for it in items {
                    let cid = it
                        .dict_get("contactID")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    let rel = it
                        .dict_get("standing")
                        .or_else(|| it.dict_get("relationshipID"))
                        .cloned()
                        .unwrap_or(PyRep::Real(0.0));
                    let mask = it
                        .dict_get("labelMask")
                        .cloned()
                        .unwrap_or(PyRep::Integer(0));
                    updates_out.push(PyRep::Tuple(vec![cid, rel, mask]));
                }
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
            } else if args.is_empty() {
                Vec::new()
            } else {
                let item_id = args[0].clone();
                let effect_id = args.get(1).cloned().unwrap_or(PyRep::None);
                let active = args.get(2).cloned().unwrap_or(PyRep::Bool(false));
                let default_ft = 132_000_000_000_000_000_i64;
                let env = PyRep::Tuple(vec![
                    item_id.clone(),
                    PyRep::None,
                    item_id.clone(),
                    PyRep::None,
                    PyRep::None,
                    PyRep::List(Vec::new()),
                    effect_id.clone(),
                ]);
                vec![
                    item_id,
                    effect_id,
                    PyRep::Integer(default_ft),
                    active.clone(),
                    active,
                    env,
                    PyRep::Integer(default_ft),
                    PyRep::Integer(0),
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
            let raw_items = if args.len() == 1 {
                match args.into_iter().next().unwrap() {
                    PyRep::List(items) | PyRep::Tuple(items) => items,
                    other => vec![other],
                }
            } else {
                args
            };
            let norm_items: Vec<PyRep> = raw_items
                .into_iter()
                .map(|el| match el {
                    PyRep::Tuple(t) if t.len() == 8 => PyRep::Tuple(t),
                    PyRep::Tuple(mut t) if t.len() == 7 => {
                        t.push(PyRep::Bool(true));
                        PyRep::Tuple(t)
                    }
                    other => PyRep::Tuple(vec![
                        PyRep::String("OnModuleAttributeChange".into()),
                        PyRep::None,
                        other,
                        PyRep::Integer(0),
                        PyRep::Integer(0),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                        PyRep::Bool(true),
                    ]),
                })
                .collect();
            vec![PyRep::List(norm_items)]
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
            if let Some(first) = out.first_mut() {
                let qty = first.dict_get("quantity").cloned().unwrap_or(PyRep::Integer(1));
                let set_default = |target: &mut PyRep, key: &str, val: PyRep| match target {
                    PyRep::KeyVal(ref mut map) => {
                        map.entry(key.to_string()).or_insert(val);
                    }
                    PyRep::Dict(ref mut entries) => {
                        if !entries.iter().any(|(ek, _)| ek.as_str() == Some(key)) {
                            entries.push((PyRep::String(key.to_string()), val));
                        }
                    }
                    _ => {}
                };
                set_default(first, "categoryID", PyRep::Integer(0));
                set_default(first, "ownerID", PyRep::None);
                set_default(first, "stacksize", qty);
            }
            if out.len() == 1 {
                out.push(PyRep::Dict(Vec::new()));
            }
            if out.len() == 2 {
                let loc_val = out
                    .first()
                    .and_then(|r| r.dict_get("locationID").cloned())
                    .unwrap_or(PyRep::Integer(0));
                out.push(loc_val);
            }
            out
        }
        "ProcessInventoryChange" => {
            if args.len() == 3 && !matches!(args[0], PyRep::List(_)) {
                let item_id = args[0].clone();
                let loc_id = args[1].clone();
                let flag_id = args[2].clone();
                let item_kv = PyRep::keyval([
                    ("itemID", item_id),
                    ("typeID", PyRep::Integer(0)),
                    ("ownerID", PyRep::None),
                    ("locationID", loc_id.clone()),
                    ("flagID", flag_id.clone()),
                    ("categoryID", PyRep::Integer(0)),
                    ("groupID", PyRep::Integer(0)),
                    ("singleton", PyRep::Integer(1)),
                    ("quantity", PyRep::Integer(1)),
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
            if args.is_empty() {
                vec![
                    PyRep::Integer(0),
                    PyRep::Integer(0),
                    PyRep::None,
                    PyRep::Integer(0),
                    PyRep::None,
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
