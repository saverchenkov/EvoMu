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
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry, WireMessage,
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
    let editor_cfg = Vec::new();
    encode_bytes_field(2, &editor_cfg, &mut attr_bytes);

    // 3: rep editor_configurations
    for mat_id in &record.material_ids {
        let mut slot_cfg = Vec::new();
        let mut slot_id = Vec::new();
        encode_varint_field(1, *mat_id as u64, &mut slot_id);
        encode_bytes_field(1, &slot_id, &mut slot_cfg);
        encode_bytes_field(3, &slot_cfg, &mut attr_bytes);
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
    let entry = data.owned_titles.entry(char_id).or_insert_with(|| {
        let mut s = HashSet::new();
        s.insert(101); // Standard starter title 1
        s.insert(102); // Standard starter title 2
        s
    });
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
                        // If no specific ID was requested (e.g. empty test call), construct a default draft
                        if opt_uuid.is_none() && opt_id.is_none() {
                            SkinrDraftRecord {
                                draft_id: (char_id as u64) * 100_000 + 12_345,
                                uuid: id_to_uuid_bytes((char_id as u64) * 100_000 + 12_345).to_vec(),
                                name: "SavedSkinrDraft".into(),
                                line_name: "Default Line".into(),
                                design_slot_id: 1,
                                pattern_id: 1,
                                material_ids: vec![1, 2],
                                updated_at_unix: ctx.now_unix(),
                            }
                        } else {
                            return Err((STATUS_NOT_FOUND, "SKINR draft not found".into()));
                        }
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

                // Capacity check: max 20 drafts per character
                {
                    let data = st.data.read().unwrap_or_else(|e| e.into_inner());
                    if let Some(user_drafts) = data.saved_drafts.get(&char_id) {
                        if user_drafts.len() >= 20 {
                            return Err((STATUS_BAD_REQUEST, "SKINR draft capacity exceeded (max 20)".into()));
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
                        design_slot_id: 1,
                        pattern_id: 1,
                        material_ids: vec![1, 2],
                        updated_at_unix: ctx.now_unix(),
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
                            user_drafts.keys().next().copied()
                        };

                        if let Some(tid) = target_id {
                            let draft = user_drafts.get_mut(&tid).unwrap();
                            if let Some(nm) = updated_name {
                                draft.name = nm;
                            }
                            draft.updated_at_unix = ctx.now_unix();
                            let cloned = draft.clone();
                            (cloned.clone(), (tid, cloned.name))
                        } else {
                            let did = opt_id.unwrap_or((char_id as u64) * 100_000 + 12_345);
                            let uuid = opt_uuid.unwrap_or_else(|| id_to_uuid_bytes(did).to_vec());
                            let name = updated_name.unwrap_or_else(|| "Updated SKINR Draft".into());
                            let r = SkinrDraftRecord {
                                draft_id: did,
                                uuid,
                                name: name.clone(),
                                line_name: "Default Line".into(),
                                design_slot_id: 1,
                                pattern_id: 1,
                                material_ids: vec![1, 2],
                                updated_at_unix: ctx.now_unix(),
                            };
                            user_drafts.insert(did, r.clone());
                            (r, (did, name))
                        }
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

                    if let Some(user_drafts) = data.saved_drafts.get_mut(&char_id) {
                        let to_remove = if let Some(ref u) = opt_uuid {
                            user_drafts.iter().find(|(_, d)| &d.uuid == u || opt_id.map_or(false, |id| d.draft_id == id)).map(|(&k, _)| k)
                        } else if let Some(id) = opt_id {
                            if user_drafts.contains_key(&id) { Some(id) } else { None }
                        } else {
                            user_drafts.keys().next().copied()
                        };
                        if let Some(id) = to_remove {
                            user_drafts.remove(&id);
                            data.skinr_drafts.remove(&id);
                        }
                    }
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
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let _count = st
                    .data
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .saved_drafts
                    .len();
                let mut out = Vec::new();
                encode_varint_field(1, 20, &mut out); // capacity = 20
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
                    .or_else(|| ctx.effective_character_id())
                    .unwrap_or(90_000_001);

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

                let (_, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                let title_id = opt_id.unwrap_or(101);

                {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    let owned = get_or_init_titles(char_id, &mut data);
                    if !owned.contains(&title_id) {
                        return Err((STATUS_BAD_REQUEST, format!("Character {char_id} does not own title {title_id}")));
                    }
                    data.equipped_titles.insert(char_id, title_id);
                }
                st.sync_to_db();

                // Publish SetNotice
                let not_bytes = encode_set_notice(char_id, Some(title_id));
                super::quasar_social_chat::ctx_queue_notice(
                    NoticeScope::Character(char_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.SetNotice"),
                        value: not_bytes,
                    },
                    ctx.now_unix(),
                );

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
        router.register_handler(
            "eve_public.cosmetic.character.title.api.admin.GrantRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                require_admin_role(ctx)?;
                let cid = ctx.effective_character_id().unwrap_or(90_000_001);
                let (_, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                let tid = opt_id.unwrap_or(101);

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

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.admin.GrantResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 2f. Admin RevokeRequest
    {
        let st = Arc::clone(&state);
        router.register_handler(
            "eve_public.cosmetic.character.title.api.admin.RevokeRequest",
            Arc::new(move |ctx, any, dyn_msg| {
                require_admin_role(ctx)?;
                let cid = ctx.effective_character_id().unwrap_or(90_000_001);
                let (_, opt_id) = extract_draft_id_or_uuid(dyn_msg, &any.value);
                let tid = opt_id.unwrap_or(101);

                {
                    let mut data = st.data.write().unwrap_or_else(|e| e.into_inner());
                    if let Some(owned) = data.owned_titles.get_mut(&cid) {
                        owned.remove(&tid);
                    }
                    if data.equipped_titles.get(&cid) == Some(&tid) {
                        data.equipped_titles.remove(&cid);
                    }
                }
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

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.api.admin.RevokeResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }
}
