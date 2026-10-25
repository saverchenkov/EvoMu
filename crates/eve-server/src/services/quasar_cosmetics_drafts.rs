//! `quasar_cosmetics_drafts` — SKINR design drafts & cosmetic titles handler implementation.
//!
//! Implements server-side Quasar RPC handlers and notice publishing for:
//! - `eve_public.cosmetic.ship.skin.thirdparty.draft.api`:
//!   `GetAllSavedRequest`, `GetRequest`, `SaveRequest`, `UpdateRequest`, `DeleteRequest`, `GetSaveCapacityRequest`
//! - `eve_public.cosmetic.character.title.api`:
//!   `GetOwnedRequest`, `GetEquippedForCharacterRequest`, `EquipRequest`, `UnequipRequest`
//! - `eve_public.cosmetic.character.title.api.admin`:
//!   `GrantRequest`, `RevokeRequest`

#![forbid(unsafe_code)]

use super::quasar_cosmetics_services::{QuasarCosmeticsState, QuasarCosmeticsStateData};
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter,
    STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry, WireMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashSet;
use std::sync::Arc;

/// Persistent record of a SKINR saved design draft.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkinrDraftRecord {
    pub draft_id: u64,
    pub uuid: Vec<u8>,
    pub name: String,
    pub line_name: String,
    pub design_slot_id: u32,
    pub pattern_id: u32,
    pub material_ids: Vec<u32>,
    pub updated_at_unix: u64,
    #[serde(default)]
    pub editor_cfg: Vec<u8>,
    #[serde(default)]
    pub ship_type_id: u32,
}

pub fn id_to_uuid_bytes(id: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[8..16].copy_from_slice(&id.to_be_bytes());
    b
}

pub fn extract_id_from_uuid_bytes(uuid: &[u8]) -> u64 {
    if uuid.len() >= 8 {
        let mut arr = [0u8; 8];
        arr.copy_from_slice(&uuid[uuid.len() - 8..]);
        u64::from_be_bytes(arr)
    } else if uuid.len() >= 4 {
        let mut arr = [0u8; 4];
        arr.copy_from_slice(&uuid[uuid.len() - 4..]);
        u32::from_be_bytes(arr) as u64
    } else {
        0
    }
}

pub fn extract_draft_id_or_uuid(dyn_msg: &DynamicQuasarMessage, raw_bytes: &[u8]) -> (Option<Vec<u8>>, Option<u64>) {
    // 1. From dyn_msg field 1
    if let Some(val) = dyn_msg.get(1) {
        match val {
            DynamicValue::Message(m) => {
                if let Some(DynamicValue::Bytes(b)) = m.get(1) {
                    if !b.is_empty() {
                        let id = extract_id_from_uuid_bytes(b);
                        return (Some(b.clone()), if id > 0 { Some(id) } else { None });
                    }
                }
                if let Some(DynamicValue::Uint64(u)) = m.get(1) {
                    return (Some(id_to_uuid_bytes(*u).to_vec()), Some(*u));
                }
            }
            DynamicValue::Bytes(b) if !b.is_empty() => {
                let id = extract_id_from_uuid_bytes(b);
                return (Some(b.clone()), if id > 0 { Some(id) } else { None });
            }
            DynamicValue::Uint64(u) => return (Some(id_to_uuid_bytes(*u).to_vec()), Some(*u)),
            DynamicValue::Int64(i) => return (Some(id_to_uuid_bytes(*i as u64).to_vec()), Some(*i as u64)),
            DynamicValue::Uint32(u) => return (Some(id_to_uuid_bytes(*u as u64).to_vec()), Some(*u as u64)),
            DynamicValue::Int32(i) => return (Some(id_to_uuid_bytes(*i as u64).to_vec()), Some(*i as u64)),
            _ => {}
        }
    }
    // 2. Fallback to raw wire decode
    if let Ok(wm) = WireMessage::decode(raw_bytes) {
        if let Some(f1) = wm.fields.iter().find(|f| f.field_number == 1) {
            if let Some(b) = f1.value.as_bytes() {
                if let Ok(sub_wm) = WireMessage::decode(b) {
                    if let Some(sub_f1) = sub_wm.fields.iter().find(|f| f.field_number == 1) {
                        if let Some(sub_b) = sub_f1.value.as_bytes() {
                            let id = extract_id_from_uuid_bytes(sub_b);
                            return (Some(sub_b.to_vec()), if id > 0 { Some(id) } else { None });
                        }
                    }
                } else {
                    let id = extract_id_from_uuid_bytes(b);
                    return (Some(b.to_vec()), if id > 0 { Some(id) } else { None });
                }
            }
        }
    }
    (None, None)
}

pub fn extract_design_parameters(
    dyn_msg: &DynamicQuasarMessage,
    raw_bytes: &[u8],
) -> (Option<u32>, Option<u32>, Option<Vec<u32>>, Vec<u8>, u32) {
    let mut design_slot_id = None;
    let mut pattern_id = None;
    let mut material_ids = Vec::new();
    let mut editor_cfg = Vec::new();
    let mut ship_type_id = 0u32;

    // 1. From dyn_msg: layout can be field 3 (SaveRequest) or field 4 (UpdateRequest)
    let layout_val = dyn_msg.get(3).or_else(|| dyn_msg.get(4));
    if let Some(lm_val) = layout_val {
        if let DynamicValue::Message(lm) = lm_val {
            if let Some(DynamicValue::Enum(e)) = lm.get(3) {
                pattern_id = Some(*e as u32);
            } else if let Some(DynamicValue::Uint32(u)) = lm.get(3) {
                pattern_id = Some(*u);
            }
            if let Some(st_val) = lm.get(2) {
                if let DynamicValue::Message(sm) = st_val {
                    if let Some(id_v) = sm.get(1) {
                        match id_v {
                            DynamicValue::Uint32(u) => ship_type_id = *u,
                            DynamicValue::Uint64(u) => ship_type_id = *u as u32,
                            _ => {}
                        }
                    }
                } else if let DynamicValue::Uint32(u) = st_val {
                    ship_type_id = *u;
                }
            }
            if let Some(DynamicValue::Repeated(slots)) = lm.get(1) {
                for slot_val in slots {
                    if let DynamicValue::Message(sm) = slot_val {
                        if let Some(id_val) = sm.get(1) {
                            let sid = match id_val {
                                DynamicValue::Message(id_m) => id_m.get(1).and_then(|v| match v {
                                    DynamicValue::Uint64(u) => Some(*u as u32),
                                    DynamicValue::Uint32(u) => Some(*u),
                                    _ => None,
                                }),
                                DynamicValue::Uint64(u) => Some(*u as u32),
                                DynamicValue::Uint32(u) => Some(*u),
                                _ => None,
                            };
                            if let Some(s) = sid {
                                material_ids.push(s);
                            }
                        }
                    }
                }
            }
        }
    }
    // editor_configurations can be field 4 (SaveRequest) or field 5 (UpdateRequest)
    let ed_val = dyn_msg.get(4).or_else(|| dyn_msg.get(5));
    if let Some(DynamicValue::Repeated(ed_cfgs)) = ed_val {
        for cfg_val in ed_cfgs {
            if let DynamicValue::Message(cm) = cfg_val {
                if let Some(id_val) = cm.get(1) {
                    let sid = match id_val {
                        DynamicValue::Message(id_m) => id_m.get(1).and_then(|v| match v {
                            DynamicValue::Uint64(u) => Some(*u as u32),
                            DynamicValue::Uint32(u) => Some(*u),
                            _ => None,
                        }),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Uint32(u) => Some(*u),
                        _ => None,
                    };
                    if let Some(s) = sid {
                        if !material_ids.contains(&s) {
                            material_ids.push(s);
                        }
                    }
                }
                if let Some(DynamicValue::Bytes(cb)) = cm.get(2) {
                    if editor_cfg.is_empty() {
                        editor_cfg = cb.clone();
                    }
                }
            }
        }
    }

    // 2. Fallback to raw wire decode
    if let Ok(wm) = WireMessage::decode(raw_bytes) {
        // Field 3 or 4: layout
        if let Some(f_lay) = wm.fields.iter().find(|f| f.field_number == 3 || f.field_number == 4) {
            if let Some(b) = f_lay.value.as_bytes() {
                if let Ok(l_wm) = WireMessage::decode(b) {
                    for slot_f in l_wm.fields.iter().filter(|f| f.field_number == 1) {
                        if let Some(slot_b) = slot_f.value.as_bytes() {
                            if let Ok(s_wm) = WireMessage::decode(slot_b) {
                                if let Some(id_f) = s_wm.fields.iter().find(|f| f.field_number == 1) {
                                    if let Some(id_b) = id_f.value.as_bytes() {
                                        if let Ok(id_wm) = WireMessage::decode(id_b) {
                                            if let Some(u) = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                                                if !material_ids.contains(&(u as u32)) {
                                                    material_ids.push(u as u32);
                                                }
                                            }
                                        }
                                    } else if let Some(u) = id_f.value.as_varint() {
                                        if !material_ids.contains(&(u as u32)) {
                                            material_ids.push(u as u32);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if pattern_id.is_none() {
                        if let Some(p) = l_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()) {
                            pattern_id = Some(p as u32);
                        }
                    }
                    if ship_type_id == 0 {
                        if let Some(st_f) = l_wm.fields.iter().find(|f| f.field_number == 2) {
                            if let Some(st_b) = st_f.value.as_bytes() {
                                if let Ok(st_wm) = WireMessage::decode(st_b) {
                                    if let Some(u) = st_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                                        ship_type_id = u as u32;
                                    }
                                }
                            } else if let Some(u) = st_f.value.as_varint() {
                                ship_type_id = u as u32;
                            }
                        }
                    }
                }
            }
        }
        // Field 4 or 5: editor_configurations
        for ed_f in wm.fields.iter().filter(|f| f.field_number == 4 || f.field_number == 5) {
            if let Some(ed_b) = ed_f.value.as_bytes() {
                if let Ok(e_wm) = WireMessage::decode(ed_b) {
                    if let Some(cfg_f) = e_wm.fields.iter().find(|f| f.field_number == 2) {
                        if let Some(cb) = cfg_f.value.as_bytes() {
                            if editor_cfg.is_empty() {
                                editor_cfg = cb.to_vec();
                            }
                        }
                    }
                    if let Some(id_f) = e_wm.fields.iter().find(|f| f.field_number == 1) {
                        if let Some(id_b) = id_f.value.as_bytes() {
                            if let Ok(id_wm) = WireMessage::decode(id_b) {
                                if let Some(u) = id_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                                    if !material_ids.contains(&(u as u32)) {
                                        material_ids.push(u as u32);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(&first) = material_ids.first() {
        design_slot_id = Some(first);
    }
    let mat_ids_opt = if material_ids.is_empty() { None } else { Some(material_ids) };
    (design_slot_id, pattern_id, mat_ids_opt, editor_cfg, ship_type_id)
}

pub fn extract_target_char_and_title(
    ctx: &AuthoritativeContext,
    dyn_msg: &DynamicQuasarMessage,
    raw_bytes: &[u8],
) -> Result<(u32, u64), (u32, String)> {
    let mut target_char: Option<u32> = None;
    let mut title_id: Option<u64> = None;

    let f1 = dyn_msg.get(1);
    let f2 = dyn_msg.get(2);

    if let Some(v2) = f2 {
        match v2 {
            DynamicValue::Message(m) => {
                if let Some(DynamicValue::Bytes(b)) = m.get(1) {
                    title_id = Some(extract_id_from_uuid_bytes(b));
                } else if let Some(DynamicValue::Uint64(u)) = m.get(1) {
                    title_id = Some(*u);
                } else if let Some(DynamicValue::Uint32(u)) = m.get(1) {
                    title_id = Some(*u as u64);
                }
            }
            DynamicValue::Bytes(b) => title_id = Some(extract_id_from_uuid_bytes(b)),
            DynamicValue::Uint64(u) => title_id = Some(*u),
            DynamicValue::Uint32(u) => title_id = Some(*u as u64),
            _ => {}
        }
        if let Some(v1) = f1 {
            match v1 {
                DynamicValue::Message(m) => {
                    if let Some(DynamicValue::Uint32(u)) = m.get(1) {
                        target_char = Some(*u);
                    } else if let Some(DynamicValue::Uint64(u)) = m.get(1) {
                        target_char = Some(*u as u32);
                    }
                }
                DynamicValue::Uint32(u) => target_char = Some(*u),
                DynamicValue::Uint64(u) => target_char = Some(*u as u32),
                _ => {}
            }
        }
    } else if let Some(v1) = f1 {
        // 1-field format (eve_public): field 1 is title ID, character is effective character
        match v1 {
            DynamicValue::Message(m) => {
                if let Some(DynamicValue::Bytes(b)) = m.get(1) {
                    title_id = Some(extract_id_from_uuid_bytes(b));
                } else if let Some(DynamicValue::Uint64(u)) = m.get(1) {
                    title_id = Some(*u);
                } else if let Some(DynamicValue::Uint32(u)) = m.get(1) {
                    title_id = Some(*u as u64);
                }
            }
            DynamicValue::Bytes(b) => title_id = Some(extract_id_from_uuid_bytes(b)),
            DynamicValue::Uint64(u) => title_id = Some(*u),
            DynamicValue::Uint32(u) => title_id = Some(*u as u64),
            _ => {}
        }
        target_char = ctx.effective_character_id();
    }

    if let Ok(wm) = WireMessage::decode(raw_bytes) {
        let wf1 = wm.fields.iter().find(|f| f.field_number == 1);
        let wf2 = wm.fields.iter().find(|f| f.field_number == 2);
        if let Some(f2) = wf2 {
            if let Some(b) = f2.value.as_bytes() {
                if let Ok(sub) = WireMessage::decode(b) {
                    if let Some(sub_b) = sub.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()) {
                        title_id = Some(extract_id_from_uuid_bytes(sub_b));
                    } else if let Some(u) = sub.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                        title_id = Some(u);
                    }
                } else {
                    title_id = Some(extract_id_from_uuid_bytes(b));
                }
            } else if let Some(u) = f2.value.as_varint() {
                title_id = Some(u);
            }
            if let Some(f1) = wf1 {
                if let Some(b) = f1.value.as_bytes() {
                    if let Ok(sub) = WireMessage::decode(b) {
                        if let Some(u) = sub.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                            target_char = Some(u as u32);
                        }
                    }
                } else if let Some(u) = f1.value.as_varint() {
                    target_char = Some(u as u32);
                }
            }
        } else if let Some(f1) = wf1 {
            if let Some(b) = f1.value.as_bytes() {
                if let Ok(sub) = WireMessage::decode(b) {
                    if let Some(sub_b) = sub.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()) {
                        title_id = Some(extract_id_from_uuid_bytes(sub_b));
                    } else if let Some(u) = sub.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()) {
                        title_id = Some(u);
                    }
                } else {
                    title_id = Some(extract_id_from_uuid_bytes(b));
                }
            } else if let Some(u) = f1.value.as_varint() {
                title_id = Some(u);
            }
            if target_char.is_none() {
                target_char = ctx.effective_character_id();
            }
        }
    }

    let cid = match target_char {
        Some(id) if id > 0 => id,
        _ => return Err((STATUS_BAD_REQUEST, "Target character ID required".into())),
    };
    let tid = match title_id {
        Some(id) if id > 0 => id,
        _ => return Err((STATUS_BAD_REQUEST, "Title ID required".into())),
    };
    Ok((cid, tid))
}

pub fn encode_draft_id_msg(uuid: &[u8]) -> Vec<u8> {
    let mut id_msg = Vec::new();
    encode_bytes_field(1, uuid, &mut id_msg);
    id_msg
}

pub fn encode_draft_attributes(record: &SkinrDraftRecord) -> Vec<u8> {
    let mut attr_bytes = Vec::new();
    // 1: skin (.eve_public.cosmetic.ship.skin.thirdparty.Identifier)
    let mut skin_id = Vec::new();
    let hex_id = format!("{:016x}", record.draft_id);
    encode_string_field(1, &hex_id, &mut skin_id);
    encode_bytes_field(1, &skin_id, &mut attr_bytes);

    // 2: editor_configuration (.eve_public.cosmetic.ship.skin.thirdparty.draft.EditorConfiguration)
    encode_bytes_field(2, &record.editor_cfg, &mut attr_bytes);

    // 3: rep editor_configurations
    for mat_id in &record.material_ids {
        let mut slot_cfg = Vec::new();
        let mut slot_id = Vec::new();
        encode_varint_field(1, *mat_id as u64, &mut slot_id);
        encode_bytes_field(1, &slot_id, &mut slot_cfg);
        encode_bytes_field(2, &record.editor_cfg, &mut slot_cfg);
        encode_bytes_field(3, &slot_cfg, &mut attr_bytes);
    }

    // 4: name & 5: line_name
    encode_string_field(4, &record.name, &mut attr_bytes);
    encode_string_field(5, &record.line_name, &mut attr_bytes);

    // 6: pattern_id, 7: design_slot_id, 8: ship_type_id
    encode_varint_field(6, record.pattern_id as u64, &mut attr_bytes);
    encode_varint_field(7, record.design_slot_id as u64, &mut attr_bytes);
    if record.ship_type_id > 0 {
        encode_varint_field(8, record.ship_type_id as u64, &mut attr_bytes);
    }

    attr_bytes
}

pub fn encode_set_notice(char_id: u32, title_id: Option<u64>) -> Vec<u8> {
    let mut out = Vec::new();
    // 1: character
    let mut char_msg = Vec::new();
    encode_varint_field(1, char_id as u64, &mut char_msg);
    encode_bytes_field(1, &char_msg, &mut out);

    match title_id {
        Some(tid) => {
            // 2: title
            let mut title_msg = Vec::new();
            let uuid = id_to_uuid_bytes(tid);
            encode_bytes_field(1, &uuid, &mut title_msg);
            encode_bytes_field(2, &title_msg, &mut out);
            encode_varint_field(3, 0, &mut out);
        }
        None => {
            encode_varint_field(3, 1, &mut out);
        }
    }
    out
}

pub fn encode_granted_notice(title_id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut title_msg = Vec::new();
    let uuid = id_to_uuid_bytes(title_id);
    encode_bytes_field(1, &uuid, &mut title_msg);
    encode_bytes_field(1, &title_msg, &mut out);
    out
}

pub fn encode_revoked_notice(title_id: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut title_msg = Vec::new();
    let uuid = id_to_uuid_bytes(title_id);
    encode_bytes_field(1, &uuid, &mut title_msg);
    encode_bytes_field(1, &title_msg, &mut out);
    out
}

pub fn get_or_init_titles(char_id: u32, data: &mut QuasarCosmeticsStateData) -> HashSet<u64> {
    let entry = data.owned_titles.entry(char_id).or_default();
    if let Some(&eq) = data.equipped_titles.get(&char_id) {
        entry.insert(eq);
    }
    entry.clone()
}

pub fn register_draft_and_title_handlers(
    router: &PublicGatewayRouter,
    state: Arc<QuasarCosmeticsState>,
    desc: Arc<QuasarDescriptorRegistry>,
) {
    let _ = &desc;

    // ========================================================================
    // 1. SKINR Draft Handlers
    // ========================================================================

    // 1a. GetAllSavedRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let drafts: Vec<SkinrDraftRecord> = {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    let legacy = data.skinr_drafts.iter().find(|(_, (cid, _))| *cid == char_id).map(|(&id, (_, n))| (id, n.clone()));
                    let user_drafts = data.saved_drafts.entry(char_id).or_default();
                    if user_drafts.is_empty() {
                        // Check if legacy draft exists in skinr_drafts
                        if let Some((legacy_id, name)) = legacy {
                            let rec = SkinrDraftRecord {
                                draft_id: legacy_id,
                                uuid: id_to_uuid_bytes(legacy_id).to_vec(),
                                name: name.clone(),
                                line_name: "Default Line".into(),
                                design_slot_id: 1,
                                pattern_id: 1,
                                material_ids: vec![1, 2],
                                updated_at_unix: ctx.now_unix(),
                                editor_cfg: Vec::new(),
                                ship_type_id: 0,
                            };
                            user_drafts.insert(legacy_id, rec);
                        }
                    }
                    user_drafts.values().cloned().collect()
                };

                let mut out = Vec::new();
                for d in &drafts {
                    let mut draft_msg = Vec::new();
                    let id_msg = encode_draft_id_msg(&d.uuid);
                    encode_bytes_field(1, &id_msg, &mut draft_msg);
                    let attr_bytes = encode_draft_attributes(d);
                    encode_bytes_field(2, &attr_bytes, &mut draft_msg);
                    encode_bytes_field(1, &draft_msg, &mut out);
                }

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetAllSavedResponse"),
                    value: out,
                })
            }),
        );
    }

    // 1b. GetRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let (opt_uuid, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                let data = st.data.read().unwrap_or_else(|e| e.into_inner());
                let user_drafts = data.saved_drafts.get(&char_id);

                let draft = if let Some(ref u) = opt_uuid {
                    user_drafts.and_then(|m| m.values().find(|d| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)))
                } else if let Some(id) = opt_id {
                    user_drafts.and_then(|m| m.get(&id))
                } else {
                    user_drafts.and_then(|m| m.values().next())
                };

                let record = match draft {
                    Some(d) => d.clone(),
                    None => {
                        // Fallback: check if owned by another character
                        for (other_char, other_map) in &data.saved_drafts {
                            if *other_char != char_id {
                                if let Some(ref u) = opt_uuid {
                                    if other_map.values().any(|d| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)) {
                                        return Err((STATUS_FORBIDDEN, "Cannot access draft belonging to another character".into()));
                                    }
                                }
                            }
                        }
                        return Err((STATUS_NOT_FOUND, "Draft not found".into()));
                    }
                };

                let mut out = Vec::new();
                let attr_bytes = encode_draft_attributes(&record);
                encode_bytes_field(2, &attr_bytes, &mut out); // GetResponse field 2 is draft (Attributes)

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetResponse"),
                    value: out,
                })
            }),
        );
    }

    // 1c. SaveRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };

                // Capacity check: dynamic capacity from custom_capacity or base 20
                let cap = {
                    let data = st.data.read().unwrap_or_else(|e| e.into_inner());
                    data.custom_capacity.get(&char_id).copied().unwrap_or(20)
                };
                {
                    let data = st.data.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(user_drafts) = data.saved_drafts.get(&char_id) {
                        if (user_drafts.len() as u64) >= cap {
                            return Err((STATUS_BAD_REQUEST, format!("SKINR draft capacity exceeded (max {cap})")));
                        }
                    }
                }

                // Extract name and line_name
                let name = dyn_msg.get(1)
                    .and_then(|v| match v {
                        DynamicValue::String(s) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    })
                    .or_else(|| {
                        WireMessage::decode(&any.value).ok().and_then(|wm| {
                            wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_str().ok().flatten().map(|s| s.to_string()))
                        })
                    })
                    .unwrap_or_else(|| "My SKINR Draft".to_string());

                let line_name = dyn_msg.get(2)
                    .and_then(|v| match v {
                        DynamicValue::String(s) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "Custom Line".to_string());

                let (design_slot_id, pattern_id, material_ids, editor_cfg, ship_type_id) =
                    extract_design_parameters(dyn_msg, &any.value);

                let record = {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    let user_drafts = data.saved_drafts.entry(char_id).or_default();

                    let draft_id = {
                        let mut next = (char_id as u64) * 100_000 + 10_000 + (user_drafts.len() as u64) + 1;
                        while user_drafts.contains_key(&next) {
                            next += 1;
                        }
                        next
                    };

                    let uuid = {
                        // Deterministic unique 16-byte uuid for this draft
                        let mut b = [0u8; 16];
                        b[0..4].copy_from_slice(&char_id.to_be_bytes());
                        b[4..8].copy_from_slice(&(user_drafts.len() as u32 + 1).to_be_bytes());
                        b[8..16].copy_from_slice(&draft_id.to_be_bytes());
                        b.to_vec()
                    };

                    let rec = SkinrDraftRecord {
                        draft_id,
                        uuid: uuid.clone(),
                        name: name.clone(),
                        line_name,
                        design_slot_id: design_slot_id.unwrap_or(1),
                        pattern_id: pattern_id.unwrap_or(1),
                        material_ids: material_ids.unwrap_or_else(|| vec![1, 2]),
                        updated_at_unix: ctx.now_unix(),
                        editor_cfg,
                        ship_type_id,
                    };
                    user_drafts.insert(draft_id, rec.clone());
                    data.skinr_drafts.insert(draft_id, (char_id, name));
                    rec
                };
                st.sync_to_db();

                let mut out = Vec::new();
                let id_msg = encode_draft_id_msg(&record.uuid);
                encode_bytes_field(1, &id_msg, &mut out);
                let attr_bytes = encode_draft_attributes(&record);
                encode_bytes_field(2, &attr_bytes, &mut out);

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.SaveResponse"),
                    value: out,
                })
            }),
        );
    }

    // 1d. UpdateRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let (opt_uuid, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                if opt_uuid.is_none() && opt_id.is_none() {
                    return Err((STATUS_BAD_REQUEST, "Draft identifier required".into()));
                }

                let updated_name = dyn_msg.get(2)
                    .and_then(|v| match v {
                        DynamicValue::String(s) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    })
                    .or_else(|| {
                        dyn_msg.get(1).and_then(|v| match v {
                            DynamicValue::String(s) if !s.is_empty() => Some(s.clone()),
                            _ => None,
                        })
                    });

                let updated_line_name = dyn_msg.get(3)
                    .and_then(|v| match v {
                        DynamicValue::String(s) if !s.is_empty() => Some(s.clone()),
                        _ => None,
                    });

                let (des_slot_id, pat_id, mat_ids, ed_cfg, s_type_id) =
                    extract_design_parameters(dyn_msg, &any.value);

                let record = {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());

                    // Check if owned by another character
                    for (other_char, other_map) in &data.saved_drafts {
                        if *other_char != char_id {
                            if let Some(ref u) = opt_uuid {
                                if other_map.values().any(|d| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)) {
                                    return Err((STATUS_FORBIDDEN, "Cannot update draft belonging to another character".into()));
                                }
                            }
                        }
                    }

                    let (rec, skinr_entry) = {
                        let user_drafts = data.saved_drafts.entry(char_id).or_default();
                        let target_id = if let Some(ref u) = opt_uuid {
                            user_drafts.iter().find(|(_, d)| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)).map(|(&k, _)| k)
                        } else if let Some(id) = opt_id {
                            if user_drafts.contains_key(&id) { Some(id) } else { None }
                        } else {
                            None
                        };

                        let Some(tid) = target_id else {
                            return Err((STATUS_NOT_FOUND, "Draft not found".into()));
                        };

                        let draft = user_drafts.get_mut(&tid).unwrap();
                        if let Some(nm) = updated_name {
                            draft.name = nm;
                        }
                        if let Some(ln) = updated_line_name {
                            draft.line_name = ln;
                        }
                        if let Some(pid) = pat_id {
                            draft.pattern_id = pid;
                        }
                        if let Some(mats) = mat_ids {
                            draft.material_ids = mats;
                        }
                        if let Some(dsid) = des_slot_id {
                            draft.design_slot_id = dsid;
                        }
                        if !ed_cfg.is_empty() {
                            draft.editor_cfg = ed_cfg;
                        }
                        if s_type_id > 0 {
                            draft.ship_type_id = s_type_id;
                        }
                        draft.updated_at_unix = ctx.now_unix();
                        let cloned = draft.clone();
                        (cloned.clone(), (tid, cloned.name))
                    };
                    data.skinr_drafts.insert(skinr_entry.0, (char_id, skinr_entry.1));
                    rec
                };
                st.sync_to_db();

                let mut out = Vec::new();
                let id_msg = encode_draft_id_msg(&record.uuid);
                encode_bytes_field(1, &id_msg, &mut out);
                let attr_bytes = encode_draft_attributes(&record);
                encode_bytes_field(2, &attr_bytes, &mut out);

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.UpdateResponse"),
                    value: out,
                })
            }),
        );
    }

    // 1e. DeleteRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let (opt_uuid, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                if opt_uuid.is_none() && opt_id.is_none() {
                    return Err((STATUS_BAD_REQUEST, "Draft identifier required".into()));
                }

                {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());

                    // Check if owned by another character
                    for (other_char, other_map) in &data.saved_drafts {
                        if *other_char != char_id {
                            if let Some(ref u) = opt_uuid {
                                if other_map.values().any(|d| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)) {
                                    return Err((STATUS_FORBIDDEN, "Cannot delete draft belonging to another character".into()));
                                }
                            }
                        }
                    }

                    let Some(user_drafts) = data.saved_drafts.get_mut(&char_id) else {
                        return Err((STATUS_NOT_FOUND, "Draft not found".into()));
                    };
                    let to_remove = if let Some(ref u) = opt_uuid {
                        user_drafts.iter().find(|(_, d)| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)).map(|(&k, _)| k)
                    } else if let Some(id) = opt_id {
                        if user_drafts.contains_key(&id) { Some(id) } else { None }
                    } else {
                        None
                    };
                    let Some(id) = to_remove else {
                        return Err((STATUS_NOT_FOUND, "Draft not found".into()));
                    };
                    user_drafts.remove(&id);
                    data.skinr_drafts.remove(&id);
                }
                st.sync_to_db();

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.DeleteResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 1f. GetSaveCapacityRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let cap = {
                    let data = st.data.read().unwrap_or_else(|e| e.into_inner());
                    data.custom_capacity.get(&char_id).copied().unwrap_or(20)
                };
                let mut out = Vec::new();
                encode_varint_field(1, cap, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.ship.skin.thirdparty.draft.api.GetSaveCapacityResponse"),
                    value: out,
                })
            }),
        );
    }

    // ========================================================================
    // 2. Cosmetic Titles Handlers
    // ========================================================================

    // 2a. GetOwnedRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.GetOwnedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let titles = {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    get_or_init_titles(char_id, &mut data)
                };

                let mut out = Vec::new();
                for tid in titles {
                    let mut title_id_msg = Vec::new();
                    let uuid = id_to_uuid_bytes(tid);
                    encode_bytes_field(1, &uuid, &mut title_id_msg);
                    encode_bytes_field(1, &title_id_msg, &mut out);
                }

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GetOwnedResponse"),
                    value: out,
                })
            }),
        );
    }

    // 2b. GetEquippedForCharacterRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.GetEquippedForCharacterRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                let Some(active_char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                let req_char_id = dyn_msg.get(1)
                    .and_then(|v| match v {
                        DynamicValue::Message(m) => m.get(1).and_then(|seq| match seq {
                            DynamicValue::Uint32(u) => Some(*u),
                            DynamicValue::Uint64(u) => Some(*u as u32),
                            DynamicValue::Int32(i) => Some(*i as u32),
                            DynamicValue::Int64(i) => Some(*i as u32),
                            _ => None,
                        }),
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    })
                    .or_else(|| {
                        WireMessage::decode(&any.value).ok().and_then(|wm| {
                            wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| {
                                if let Some(b) = f.value.as_bytes() {
                                    WireMessage::decode(b).ok().and_then(|sub_wm| {
                                        sub_wm.fields.iter().find(|sub_f| sub_f.field_number == 1).and_then(|sub_f| sub_f.value.as_varint().map(|v| v as u32))
                                    })
                                } else {
                                    f.value.as_varint().map(|v| v as u32)
                                }
                            })
                        })
                    })
                    .unwrap_or(active_char_id);

                let equipped = {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    let _ = get_or_init_titles(req_char_id, &mut data);
                    data.equipped_titles.get(&req_char_id).copied()
                };

                let mut out = Vec::new();
                match equipped {
                    Some(tid) => {
                        let mut title_id_msg = Vec::new();
                        let uuid = id_to_uuid_bytes(tid);
                        encode_bytes_field(1, &uuid, &mut title_id_msg);
                        encode_bytes_field(1, &title_id_msg, &mut out);
                        encode_varint_field(2, 0, &mut out); // no_title = false
                    }
                    None => {
                        encode_varint_field(2, 1, &mut out); // no_title = true
                    }
                }

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GetEquippedForCharacterResponse"),
                    value: out,
                })
            }),
        );
    }

    // 2c. EquipRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.EquipRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };

                let (opt_uuid, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                let explicit_clear = opt_uuid.as_ref().map_or(false, |u| u.iter().all(|&b| b == 0));
                if !explicit_clear && opt_id.is_none() {
                    return Err((STATUS_BAD_REQUEST, "Title ID required to equip".into()));
                }

                if explicit_clear {
                    {
                        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                        data.equipped_titles.remove(&char_id);
                    }
                    st.sync_to_db();

                    let not_bytes = encode_set_notice(char_id, None);
                    super::quasar_social_chat::ctx_queue_notice(
                        NoticeScope::Character(char_id),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.SetNotice"),
                            value: not_bytes,
                        },
                        ctx.now_unix(),
                    );
                } else {
                    let title_id = opt_id.unwrap();
                    {
                        let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                        let owned = get_or_init_titles(char_id, &mut data);
                        if !owned.contains(&title_id) {
                            return Err((STATUS_BAD_REQUEST, format!("Character {char_id} does not own title {title_id}")));
                        }
                        data.equipped_titles.insert(char_id, title_id);
                    }
                    st.sync_to_db();

                    let not_bytes = encode_set_notice(char_id, Some(title_id));
                    super::quasar_social_chat::ctx_queue_notice(
                        NoticeScope::Character(char_id),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.SetNotice"),
                            value: not_bytes,
                        },
                        ctx.now_unix(),
                    );
                }

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.EquipResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 2d. UnequipRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.UnequipRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let Some(char_id) = ctx.effective_character_id() else {
                    return Err((STATUS_FORBIDDEN, "Active character required".into()));
                };
                {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    data.equipped_titles.remove(&char_id);
                }
                st.sync_to_db();

                // Publish SetNotice with no_title = true
                let not_bytes = encode_set_notice(char_id, None);
                super::quasar_social_chat::ctx_queue_notice(
                    NoticeScope::Character(char_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.SetNotice"),
                        value: not_bytes,
                    },
                    ctx.now_unix(),
                );

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.UnequipResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 2e. Admin GrantRequest
    {
        let st = Arc::clone(&state);
        let grant_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, any, dyn_msg| {
            require_admin_role(ctx)?;
            let (cid, tid) = extract_target_char_and_title(ctx, dyn_msg, &any.value)?;

            {
                let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                let owned = data.owned_titles.entry(cid).or_default();
                owned.insert(tid);
            }
            st.sync_to_db();

            let not_bytes = encode_granted_notice(tid);
            super::quasar_social_chat::ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.GrantedNotice"),
                    value: not_bytes,
                },
                ctx.now_unix(),
            );

            let rsp_pkg = if any.normalized_type_name().starts_with("eve.") {
                "eve.cosmetic.character.title.api.admin.GrantResponse"
            } else {
                "eve_public.cosmetic.character.title.api.admin.GrantResponse"
            };

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_pkg}"),
                value: Vec::new(),
            })
        });
        router.register_handler(
            "eve_public.cosmetic.character.title.api.admin.GrantRequest",
            Arc::clone(&grant_handler),
        );
        router.register_handler(
            "eve.cosmetic.character.title.api.admin.GrantRequest",
            grant_handler,
        );
    }

    // 2f. Admin RevokeRequest
    {
        let st = Arc::clone(&state);
        let revoke_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, any, dyn_msg| {
            require_admin_role(ctx)?;
            let (cid, tid) = extract_target_char_and_title(ctx, dyn_msg, &any.value)?;

            let was_equipped = {
                let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                if let Some(owned) = data.owned_titles.get_mut(&cid) {
                    owned.remove(&tid);
                }
                if data.equipped_titles.get(&cid) == Some(&tid) {
                    data.equipped_titles.remove(&cid);
                    true
                } else {
                    false
                }
            };
            st.sync_to_db();

            let not_bytes = encode_revoked_notice(tid);
            super::quasar_social_chat::ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.RevokedNotice"),
                    value: not_bytes,
                },
                ctx.now_unix(),
            );

            if was_equipped {
                let set_not_bytes = encode_set_notice(cid, None);
                super::quasar_social_chat::ctx_queue_notice(
                    NoticeScope::Character(cid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.SetNotice"),
                        value: set_not_bytes,
                    },
                    ctx.now_unix(),
                );
            }

            let rsp_pkg = if any.normalized_type_name().starts_with("eve.") {
                "eve.cosmetic.character.title.api.admin.RevokeResponse"
            } else {
                "eve_public.cosmetic.character.title.api.admin.RevokeResponse"
            };

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_pkg}"),
                value: Vec::new(),
            })
        });
        router.register_handler(
            "eve_public.cosmetic.character.title.api.admin.RevokeRequest",
            Arc::clone(&revoke_handler),
        );
        router.register_handler(
            "eve.cosmetic.character.title.api.admin.RevokeRequest",
            revoke_handler,
        );
    }
}
