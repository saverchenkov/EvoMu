//! `quasar_cosmetics_skinr_seq` — State-backed V24.01 Quasar handlers for custom 3rd-party SKIN
//! designs (`thirdparty.api.GetRequest`), 3rd-party SKIN licenses (`GetOwned`, `Get`, `Activate`,
//! `Apply`, `Unapply`, `admin.Grant`, `admin.Revoke`, and `Added`/`Updated`/`Deleted` notices),
//! and SKINR sequencing pricing, durations, early completion pricing, and sequencing jobs
//! (`Initiate`, `GetAllActive`, `Get`, `CompleteEarly`, and `Started`/`Completed`/`Failed` notices).

#![forbid(unsafe_code)]

use super::quasar_cosmetics_skinr::{
    any_msg, as_u64, parse_component_id, put_duration, put_seq, put_timestamp, require_char,
    seq_of, str_of, sub, HandlerResult, SequencingJobRecord, SkinDesignRecord, SkinLayoutSlot,
    SkinrCosmeticsState, ThirdPartySkinLicense, CATEGORY_SHIP,
};
use super::quasar_social_chat::ctx_queue_notice;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND,
};
use eve_db::IskCents;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage,
};
use std::sync::Arc;

const DEFAULT_SHIP_TYPE_ID: u32 = 587; // Rifter
const EVERMARK_CORP_ID: i64 = 1_000_419;

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn job_seq_to_uuid(seq: u64) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..8].copy_from_slice(&0x534B_494E_524A_4F42u64.to_be_bytes());
    b[8..16].copy_from_slice(&seq.to_be_bytes());
    b
}

// ============================================================================
// Wire Encoding Helpers for Custom 3rd-Party SKINs & Licenses
// ============================================================================

fn encode_skin_id(skin_hex: &str) -> Vec<u8> {
    let mut b = Vec::new();
    encode_string_field(1, skin_hex, &mut b);
    b
}

fn encode_skin_license_id(character_id: u32, skin_hex: &str) -> Vec<u8> {
    let mut b = Vec::new();
    put_seq(1, character_id as u64, &mut b);
    encode_bytes_field(2, &encode_skin_id(skin_hex), &mut b);
    b
}

fn encode_skin_license_attrs(lic: &ThirdPartySkinLicense) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, u64::from(lic.activated), &mut b);
    encode_varint_field(2, lic.unactivated, &mut b);
    encode_varint_field(3, lic.escrowed, &mut b);
    put_timestamp(4, lic.first_acquired_unix, &mut b);
    b
}

fn encode_layout_slot(slot: &SkinLayoutSlot) -> Vec<u8> {
    let mut id_msg = Vec::new();
    encode_varint_field(1, slot.slot_id as u64, &mut id_msg);

    let mut cfg_msg = Vec::new();
    if slot.is_pattern {
        let mut pat_cfg = Vec::new();
        let mut pat_id = Vec::new();
        encode_varint_field(1, slot.component_id as u64, &mut pat_id);
        encode_bytes_field(1, &pat_id, &mut pat_cfg);
        if let Some(p_coat) = slot.primary_coating_id {
            let mut pc = Vec::new();
            encode_varint_field(1, p_coat as u64, &mut pc);
            encode_bytes_field(2, &pc, &mut pat_cfg);
        }
        if let Some(s_coat) = slot.secondary_coating_id {
            let mut sc = Vec::new();
            encode_varint_field(1, s_coat as u64, &mut sc);
            encode_bytes_field(3, &sc, &mut pat_cfg);
        }
        encode_bytes_field(2, &pat_cfg, &mut cfg_msg);
    } else {
        let mut coat_cfg = Vec::new();
        let mut coat_id = Vec::new();
        encode_varint_field(1, slot.component_id as u64, &mut coat_id);
        encode_bytes_field(1, &coat_id, &mut coat_cfg);
        encode_bytes_field(1, &coat_cfg, &mut cfg_msg);
    }

    let mut out = Vec::new();
    encode_bytes_field(1, &id_msg, &mut out);
    encode_bytes_field(2, &cfg_msg, &mut out);
    out
}

fn encode_skin_attributes(rec: &SkinDesignRecord) -> Vec<u8> {
    let mut out = Vec::new();
    encode_string_field(1, &rec.name, &mut out);
    encode_string_field(2, &rec.line_name, &mut out);
    put_seq(3, rec.creator_id as u64, &mut out);

    let mut layout = Vec::new();
    for slot in &rec.slots {
        encode_bytes_field(1, &encode_layout_slot(slot), &mut layout);
    }
    put_seq(2, rec.ship_type_id.max(1) as u64, &mut layout);
    encode_varint_field(3, rec.pattern_blend_mode.max(1) as u64, &mut layout);
    encode_bytes_field(4, &layout, &mut out);

    let mut tier_msg = Vec::new();
    encode_varint_field(1, rec.tier_level.max(1) as u64, &mut tier_msg);
    encode_bytes_field(5, &tier_msg, &mut out);

    match rec.first_sequenced_unix {
        Some(ts) => put_timestamp(7, ts, &mut out),
        None => encode_varint_field(6, 1, &mut out),
    }
    out
}

fn queue_skin_license_added_notice(lic: &ThirdPartySkinLicense, now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(lic.character_id, &lic.skin_hex), &mut payload);
    encode_bytes_field(2, &encode_skin_license_attrs(lic), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(lic.character_id),
        any_msg("eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice", payload),
        now_unix,
    );
}

fn queue_skin_license_updated_notice(
    lic: &ThirdPartySkinLicense,
    prev: &ThirdPartySkinLicense,
    now_unix: u64,
) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(lic.character_id, &lic.skin_hex), &mut payload);
    encode_bytes_field(2, &encode_skin_license_attrs(lic), &mut payload);
    encode_bytes_field(3, &encode_skin_license_attrs(prev), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(lic.character_id),
        any_msg("eve_public.cosmetic.ship.skin.thirdparty.license.api.UpdatedNotice", payload),
        now_unix,
    );
}

fn queue_skin_license_deleted_notice(character_id: u32, skin_hex: &str, now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(character_id, skin_hex), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg("eve_public.cosmetic.ship.skin.thirdparty.license.api.DeletedNotice", payload),
        now_unix,
    );
}

fn queue_ship_skin_set_notice(
    character_id: u32,
    ship_id: u64,
    thirdparty_skin_hex: Option<&str>,
    now_unix: u64,
) {
    let mut skin_state = Vec::new();
    match thirdparty_skin_hex {
        Some(hex) => {
            let mut tp = Vec::new();
            encode_bytes_field(1, &encode_skin_id(hex), &mut tp);
            encode_bytes_field(2, &tp, &mut skin_state);
        }
        None => encode_varint_field(3, 1, &mut skin_state),
    }
    let mut state_msg = Vec::new();
    put_seq(1, character_id as u64, &mut state_msg);
    put_seq(2, ship_id, &mut state_msg);
    encode_bytes_field(3, &skin_state, &mut state_msg);

    let mut payload = Vec::new();
    encode_bytes_field(1, &state_msg, &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg("eve_public.cosmetic.ship.api.SetNotice", payload),
        now_unix,
    );
}

fn parse_skin_hex(dyn_msg: &DynamicQuasarMessage, raw: &[u8]) -> Option<String> {
    if let Some(s) = sub(dyn_msg, 1).and_then(|m| str_of(m, 1)).filter(|s| !s.is_empty()) {
        return Some(s);
    }
    if let Some(s) = str_of(dyn_msg, 1).filter(|s| !s.is_empty()) {
        return Some(s);
    }
    if let Some(id) = sub(dyn_msg, 1).and_then(|m| m.get(1)).and_then(as_u64) {
        return Some(format!("skin_{id:08x}"));
    }
    let wm = WireMessage::decode(raw).ok()?;
    let f1 = wm.fields.iter().find(|f| f.field_number == 1)?;
    if let Some(b) = f1.value.as_bytes() {
        if let Ok(inner) = WireMessage::decode(b) {
            if let Some(inf1) = inner.fields.iter().find(|x| x.field_number == 1) {
                if let Some(s) = inf1
                    .value
                    .as_bytes()
                    .and_then(|sb| String::from_utf8(sb.to_vec()).ok())
                    .filter(|s| !s.is_empty())
                {
                    return Some(s);
                }
                if let Some(v) = inf1.value.as_varint() {
                    return Some(format!("skin_{v:08x}"));
                }
            }
        }
        if let Some(s) = String::from_utf8(b.to_vec()).ok().filter(|s| !s.is_empty()) {
            return Some(s);
        }
    }
    None
}

fn parse_skin_license_id(
    dyn_msg: &DynamicQuasarMessage,
    raw: &[u8],
) -> Option<(Option<u32>, String)> {
    if let Some(lic_m) = sub(dyn_msg, 1) {
        let char_id = seq_of(lic_m, 1).map(|u| u as u32);
        if let Some(h) = sub(lic_m, 2)
            .and_then(|sm| str_of(sm, 1))
            .or_else(|| str_of(lic_m, 2))
            .filter(|s| !s.is_empty())
        {
            return Some((char_id, h));
        }
    }
    let wm = WireMessage::decode(raw).ok()?;
    let b1 = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
    let lwm = WireMessage::decode(b1).ok()?;
    let char_id = lwm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_bytes())
        .and_then(|cb| WireMessage::decode(cb).ok())
        .and_then(|cwm| {
            cwm.fields
                .iter()
                .find(|f| f.field_number == 1)
                .and_then(|f| f.value.as_varint())
                .map(|u| u as u32)
        });
    let sb = lwm.fields.iter().find(|f| f.field_number == 2)?.value.as_bytes()?;
    let swm = WireMessage::decode(sb).ok()?;
    let hb = swm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
    let s = String::from_utf8(hb.to_vec()).ok().filter(|s| !s.is_empty())?;
    Some((char_id, s))
}

// ============================================================================
// 3rd-Party SKIN Design & License Handlers
// ============================================================================

fn handle_skin_get(
    state: &SkinrCosmeticsState,
    req_type: &str,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let Some(skin_hex) = parse_skin_hex(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing 3rd-party SKIN identifier hex".into()));
    };
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(design) = data.skins.get(&skin_hex) else {
        return Err((STATUS_NOT_FOUND, format!("3rd-party SKIN design '{skin_hex}' not found")));
    };
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_skin_attributes(design), &mut out);
    let rsp_type = if req_type.starts_with("eve.") {
        "eve.cosmetic.ship.skin.thirdparty.api.requests.GetResponse"
    } else {
        "eve_public.cosmetic.ship.skin.thirdparty.api.GetResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn handle_skin_license_get_owned(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    req_type: &str,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for lic in data.skin_licenses.values() {
        if lic.character_id == cid {
            encode_bytes_field(1, &encode_skin_license_id(cid, &lic.skin_hex), &mut out);
        }
    }
    if req_type.starts_with("eve.") {
        if out.is_empty() {
            encode_varint_field(1, cid as u64, &mut out);
        }
        return Ok(any_msg(
            "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse",
            out,
        ));
    }
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedResponse",
        out,
    ))
}

fn handle_skin_license_get(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    req_type: &str,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let Some((req_char, skin_hex)) = parse_skin_license_id(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing 3rd-party SKIN license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    if target_cid != caller && require_admin_role(ctx).is_err() {
        return Err((STATUS_FORBIDDEN, "Cannot read another character's SKIN license".into()));
    }
    let key = SkinrCosmeticsState::skin_lic_key(target_cid, &skin_hex);
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(lic) = data.skin_licenses.get(&key) else {
        return Err((STATUS_NOT_FOUND, "3rd-party SKIN license not found".into()));
    };
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_skin_license_attrs(lic), &mut out);
    let rsp_type = if req_type.starts_with("eve.") {
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetResponse"
    } else {
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn handle_skin_license_activate(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let Some((req_char, skin_hex)) = parse_skin_license_id(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing 3rd-party SKIN license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    if target_cid != caller {
        return Err((STATUS_FORBIDDEN, "Cannot activate another character's SKIN license".into()));
    }
    let key = SkinrCosmeticsState::skin_lic_key(caller, &skin_hex);
    let (updated, prev) = {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(lic) = data.skin_licenses.get_mut(&key) else {
            return Err((STATUS_NOT_FOUND, "3rd-party SKIN license not found".into()));
        };
        if lic.activated || lic.unactivated == 0 {
            return Err((
                STATUS_BAD_REQUEST,
                "SKIN license is already activated or has no unactivated charges".into(),
            ));
        }
        let prev = lic.clone();
        lic.unactivated -= 1;
        lic.activated = true;
        (lic.clone(), prev)
    };
    state.save();
    queue_skin_license_updated_notice(&updated, &prev, ctx.now_unix());
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateResponse",
        Vec::new(),
    ))
}

fn handle_skin_license_apply(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let Some((req_char, skin_hex)) = parse_skin_license_id(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing 3rd-party SKIN license identifier".into()));
    };
    if req_char.is_some_and(|c| c != caller) {
        return Err((STATUS_FORBIDDEN, "Cannot apply another character's SKIN license".into()));
    }
    let ship_id = seq_of(dyn_msg, 2).unwrap_or(0);
    if ship_id == 0 {
        return Err((STATUS_BAD_REQUEST, "ApplyRequest requires a valid ship identifier".into()));
    }
    let ship_info = state.db.with_snapshot(|s| {
        s.items.get(&(ship_id as i64)).map(|it| (it.owner_id, it.type_id))
    });
    let Some((owner_id, ship_type_id)) = ship_info else {
        return Err((STATUS_NOT_FOUND, "Target ship not found".into()));
    };
    if owner_id != caller as i64 {
        return Err((STATUS_FORBIDDEN, "Character does not own target ship".into()));
    }
    if state.sde.types.get(&ship_type_id).map(|t| t.category_id) != Some(CATEGORY_SHIP) {
        return Err((STATUS_BAD_REQUEST, "Target item is not a ship hull".into()));
    }

    let key = SkinrCosmeticsState::skin_lic_key(caller, &skin_hex);
    {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(lic) = data.skin_licenses.get(&key) else {
            return Err((STATUS_NOT_FOUND, "3rd-party SKIN license not found".into()));
        };
        if !lic.activated {
            return Err((STATUS_BAD_REQUEST, "3rd-party SKIN license is not activated".into()));
        }
        if let Some(design) = data.skins.get(&skin_hex) {
            if design.ship_type_id != 0 && design.ship_type_id != ship_type_id {
                return Err((STATUS_BAD_REQUEST, "Ship hull type does not match SKIN design".into()));
            }
        }
        data.applied_thirdparty_skins.insert(ship_id, skin_hex.clone());
    }
    state.save();
    queue_ship_skin_set_notice(caller, ship_id, Some(&skin_hex), ctx.now_unix());
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyResponse",
        Vec::new(),
    ))
}

fn handle_skin_license_unapply(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    if parse_skin_license_id(dyn_msg, raw).is_some_and(|(rc, _)| rc.is_some_and(|c| c != caller)) {
        return Err((STATUS_FORBIDDEN, "Cannot unapply SKIN for another character".into()));
    }
    let ship_id = seq_of(dyn_msg, 2).unwrap_or(0);
    if ship_id == 0 {
        return Err((STATUS_BAD_REQUEST, "UnapplyRequest requires a valid ship identifier".into()));
    }
    let owner_id = state
        .db
        .with_snapshot(|s| s.items.get(&(ship_id as i64)).map(|it| it.owner_id));
    let Some(owner) = owner_id else {
        return Err((STATUS_NOT_FOUND, "Target ship not found".into()));
    };
    if owner != caller as i64 {
        return Err((STATUS_FORBIDDEN, "Character does not own target ship".into()));
    }
    state
        .data
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .applied_thirdparty_skins
        .remove(&ship_id);
    state.save();
    queue_ship_skin_set_notice(caller, ship_id, None, ctx.now_unix());
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyResponse",
        Vec::new(),
    ))
}

fn grant_or_increment_skin_license(
    state: &SkinrCosmeticsState,
    character_id: u32,
    skin_hex: &str,
    unactivated_delta: u64,
    now_unix: u64,
) {
    let key = SkinrCosmeticsState::skin_lic_key(character_id, skin_hex);
    let mut added = None;
    let mut updated = None;
    {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        data.skins.entry(skin_hex.to_string()).or_insert_with(|| SkinDesignRecord {
            skin_hex: skin_hex.to_string(),
            name: format!("Custom SKIN {skin_hex}"),
            line_name: "Paragon Custom Line".into(),
            creator_id: character_id,
            ship_type_id: DEFAULT_SHIP_TYPE_ID,
            pattern_blend_mode: 1,
            slots: Vec::new(),
            tier_level: 1,
            first_sequenced_unix: Some(now_unix),
        });
        if let Some(existing) = data.skin_licenses.get_mut(&key) {
            let prev = existing.clone();
            existing.unactivated = existing.unactivated.saturating_add(unactivated_delta);
            updated = Some((existing.clone(), prev));
        } else {
            let lic = ThirdPartySkinLicense {
                character_id,
                skin_hex: skin_hex.to_string(),
                activated: false,
                unactivated: unactivated_delta,
                escrowed: 0,
                first_acquired_unix: now_unix,
            };
            data.skin_licenses.insert(key, lic.clone());
            added = Some(lic);
        }
    }
    state.save();
    if let Some(lic) = added {
        queue_skin_license_added_notice(&lic, now_unix);
    } else if let Some((lic, prev)) = updated {
        queue_skin_license_updated_notice(&lic, &prev, now_unix);
    }
}

fn handle_skin_license_admin_grant(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let cid = require_char(ctx)?;
    let Some(skin_hex) = parse_skin_hex(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing 3rd-party SKIN identifier".into()));
    };
    let unactivated = dyn_msg
        .get(2)
        .and_then(as_u64)
        .or_else(|| {
            WireMessage::decode(raw)
                .ok()?
                .fields
                .iter()
                .find(|f| f.field_number == 2)?
                .value
                .as_varint()
        })
        .unwrap_or(0);
    if unactivated == 0 {
        return Err((STATUS_BAD_REQUEST, "Unactivated license count must be > 0".into()));
    }
    grant_or_increment_skin_license(state, cid, &skin_hex, unactivated, ctx.now_unix());

    let mut out = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(cid, &skin_hex), &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantResponse",
        out,
    ))
}

fn handle_skin_license_admin_revoke(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    require_admin_role(ctx)?;
    let caller = require_char(ctx)?;
    let Some((req_char, skin_hex)) = parse_skin_license_id(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing 3rd-party SKIN license identifier".into()));
    };
    let target_cid = req_char.unwrap_or(caller);
    let key = SkinrCosmeticsState::skin_lic_key(target_cid, &skin_hex);
    let removed = state
        .data
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .skin_licenses
        .remove(&key);
    if removed.is_none() {
        return Err((STATUS_NOT_FOUND, "3rd-party SKIN license not found".into()));
    }
    state.save();
    queue_skin_license_deleted_notice(target_cid, &skin_hex, ctx.now_unix());
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeResponse",
        Vec::new(),
    ))
}

// ============================================================================
// Sequencing Pricing, Durations & Job Lifecycle Handlers
// ============================================================================

fn skill_discounts_and_cap(state: &SkinrCosmeticsState, cid: Option<u32>) -> (u32, u32, usize) {
    let Some(c) = cid else {
        return (0, 0, 5);
    };
    state.db.with_snapshot(|s| {
        let Some(sk_map) = s.skills.get(&(c as i64)) else {
            return (0, 0, 5);
        };
        let lvl = |sid: u32| sk_map.get(&sid).map(|sk| sk.level as u32).unwrap_or(0);
        let price_disc = ((lvl(81044) + lvl(81045)) * 2).min(25);
        let dur_disc = ((lvl(81040) + lvl(81041) + lvl(81042) + lvl(81043) + lvl(3395)) * 3).min(50);
        let max_jobs = 5 + (lvl(81046) + lvl(81047)) as usize;
        (price_disc, dur_disc, max_jobs)
    })
}

fn handle_get_tier_pricing(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    req_type: &str,
) -> HandlerResult {
    let (discount_pct, _, _) = skill_discounts_and_cap(state, ctx.effective_character_id());
    let mut out = Vec::new();
    for (&tier, &min_pts) in &state.catalog.tier_thresholds {
        let base_plex_cents = ((tier as u64) * 1_500).saturating_add(min_pts as u64 * 2);
        let discounted = base_plex_cents.saturating_mul(100 - discount_pct as u64) / 100;

        let mut tier_msg = Vec::new();
        encode_varint_field(1, tier as u64, &mut tier_msg);
        let mut plex_msg = Vec::new();
        encode_varint_field(1, discounted.max(100), &mut plex_msg);

        let mut price_entry = Vec::new();
        encode_bytes_field(1, &tier_msg, &mut price_entry);
        encode_bytes_field(2, &plex_msg, &mut price_entry);
        encode_bytes_field(1, &price_entry, &mut out);
    }
    encode_varint_field(2, discount_pct as u64, &mut out);
    let rsp_type = if req_type.starts_with("eve.") {
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse"
    } else {
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn handle_get_tier_durations(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    req_type: &str,
) -> HandlerResult {
    let (_, discount_pct, _) = skill_discounts_and_cap(state, ctx.effective_character_id());
    let mut out = Vec::new();
    for (&tier, &min_pts) in &state.catalog.tier_thresholds {
        let base_secs = ((tier as u64) * 3_600).saturating_add((min_pts as u64) * 10);
        let discounted_secs = (base_secs.saturating_mul(100 - discount_pct as u64) / 100).max(600);

        let mut tier_msg = Vec::new();
        encode_varint_field(1, tier as u64, &mut tier_msg);
        let mut entry = Vec::new();
        encode_bytes_field(1, &tier_msg, &mut entry);
        put_duration(2, discounted_secs, &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    encode_varint_field(2, discount_pct as u64, &mut out);
    let rsp_type = if req_type.starts_with("eve.") {
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterResponse"
    } else {
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn handle_get_early_completion_pricing(req_type: &str) -> HandlerResult {
    let brackets = [
        (0u64, 3_600u64, 500u64),
        (3_600, 86_400, 2_500),
        (86_400, 604_800, 10_000),
    ];
    let mut out = Vec::new();
    for (gt_secs, lte_secs, plex_cents) in brackets {
        let mut entry = Vec::new();
        put_duration(1, gt_secs, &mut entry);
        put_duration(2, lte_secs, &mut entry);
        let mut plex_msg = Vec::new();
        encode_varint_field(1, plex_cents, &mut plex_msg);
        encode_bytes_field(3, &plex_msg, &mut entry);
        encode_bytes_field(1, &entry, &mut out);
    }
    let rsp_type = if req_type.starts_with("eve.") {
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingResponse"
    } else {
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingResponse"
    };
    Ok(any_msg(rsp_type, out))
}

fn encode_job_id(uuid: &[u8; 16]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    b
}

fn encode_job_attributes(job: &SequencingJobRecord) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_skin_id(&job.skin_hex), &mut out);
    put_seq(2, job.sequencer_id as u64, &mut out);
    encode_varint_field(3, job.quantity as u64, &mut out);
    if let Some(done_ts) = job.completed_unix {
        put_timestamp(7, done_ts, &mut out);
    } else if let Some(fail_code) = job.failed_reason {
        encode_varint_field(6, fail_code as u64, &mut out);
    } else {
        put_timestamp(5, job.initiated_unix, &mut out);
    }
    put_timestamp(
        8,
        job.initiated_unix.saturating_add(job.planned_duration_secs),
        &mut out,
    );
    out
}

fn queue_job_started_notice(character_id: u32, uuid: &[u8; 16], now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_job_id(uuid), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg(
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.StartedNotice",
            payload,
        ),
        now_unix,
    );
}

fn queue_job_completed_notice(character_id: u32, uuid: &[u8; 16], now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_job_id(uuid), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg(
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompletedNotice",
            payload,
        ),
        now_unix,
    );
}

fn queue_job_failed_notice(character_id: u32, uuid: &[u8; 16], reason_enum: u32, now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_job_id(uuid), &mut payload);
    encode_varint_field(2, reason_enum.max(1) as u64, &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg(
            "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.FailedNotice",
            payload,
        ),
        now_unix,
    );
}

fn parse_initiate_slots(
    state: &SkinrCosmeticsState,
    layout_msg: &DynamicQuasarMessage,
    ship_type_id: u32,
) -> Result<(Vec<SkinLayoutSlot>, Vec<(u32, bool)>), (u32, String)> {
    let allowed_slots = state.catalog.allowed_slots_for_ship(ship_type_id);
    let mut slots = Vec::new();
    let mut required_comps = Vec::new();

    if let Some(DynamicValue::Repeated(slot_vals)) = layout_msg.get(1) {
        for sv in slot_vals {
            let DynamicValue::Message(sm) = sv else { continue };
            let slot_id = seq_of(sm, 1)
                .or_else(|| sm.get(1).and_then(as_u64))
                .unwrap_or(0) as u32;
            if slot_id == 0 || !allowed_slots.contains(&slot_id) {
                return Err((STATUS_BAD_REQUEST, format!("Invalid cosmetic slot {slot_id} for ship {ship_type_id}")));
            }
            let Some(cfg_m) = sub(sm, 2) else {
                return Err((STATUS_BAD_REQUEST, format!("Missing slot configuration for slot {slot_id}")));
            };
            let (comp_id, is_pattern, p_coat, s_coat) = if let Some(coat_cfg) = sub(cfg_m, 1) {
                let cid = seq_of(coat_cfg, 1)
                    .or_else(|| coat_cfg.get(1).and_then(as_u64))
                    .unwrap_or(0) as u32;
                (cid, false, None, None)
            } else if let Some(pat_cfg) = sub(cfg_m, 2) {
                let cid = seq_of(pat_cfg, 1)
                    .or_else(|| pat_cfg.get(1).and_then(as_u64))
                    .unwrap_or(0) as u32;
                let pc = seq_of(pat_cfg, 2).map(|u| u as u32);
                let sc = seq_of(pat_cfg, 3).map(|u| u as u32);
                (cid, true, pc, sc)
            } else {
                return Err((STATUS_BAD_REQUEST, format!("Empty slot configuration for slot {slot_id}")));
            };

            let Some(comp_def) = state.catalog.components.get(&comp_id) else {
                return Err((STATUS_BAD_REQUEST, format!("Unknown SKINR component {comp_id}")));
            };
            if comp_def.is_pattern() != is_pattern {
                return Err((STATUS_BAD_REQUEST, format!("Component {comp_id} kind does not match slot configuration")));
            }
            if comp_def.disallowed_slots.contains(&slot_id) {
                return Err((STATUS_BAD_REQUEST, format!("Component {comp_id} is disallowed in slot {slot_id}")));
            }
            if let Some(allowed_cats) = state.catalog.slot_allowed_categories.get(&slot_id) {
                if !allowed_cats.is_empty() && !allowed_cats.contains(&comp_def.category) {
                    return Err((
                        STATUS_BAD_REQUEST,
                        format!("Component {comp_id} category {} not allowed in slot {slot_id}", comp_def.category),
                    ));
                }
            }
            slots.push(SkinLayoutSlot {
                slot_id,
                component_id: comp_id,
                is_pattern,
                primary_coating_id: p_coat,
                secondary_coating_id: s_coat,
            });
            if !required_comps.contains(&(comp_id, is_pattern)) {
                required_comps.push((comp_id, is_pattern));
            }
            for extra_coat in [p_coat, s_coat].into_iter().flatten() {
                if !state.catalog.components.contains_key(&extra_coat) {
                    return Err((STATUS_BAD_REQUEST, format!("Unknown pattern coating component {extra_coat}")));
                }
                if !required_comps.contains(&(extra_coat, false)) {
                    required_comps.push((extra_coat, false));
                }
            }
        }
    }
    Ok((slots, required_comps))
}

fn handle_job_initiate(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let now = ctx.now_unix();
    let (_, dur_disc, max_jobs) = skill_discounts_and_cap(state, Some(cid));

    let preview_uuid = {
        let data = state.data.read().unwrap_or_else(|e| e.into_inner());
        let active_count = data
            .sequencing_jobs
            .values()
            .filter(|j| j.sequencer_id == cid && j.is_active())
            .count();
        let next_uuid = job_seq_to_uuid(data.next_job_seq);
        if active_count >= max_jobs {
            drop(data);
            queue_job_failed_notice(cid, &next_uuid, 1, now);
            return Err((STATUS_BAD_REQUEST, "Maximum active sequencing jobs reached".into()));
        }
        next_uuid
    };

    let mut name = str_of(dyn_msg, 1).unwrap_or_else(|| "Custom SKINR Design".to_string());
    if name.is_empty() {
        name = "Custom SKINR Design".to_string();
    }
    let line_name = str_of(dyn_msg, 2).unwrap_or_else(|| "Paragon Custom Line".to_string());
    let licenses_qty = if let Some(v) = dyn_msg.get(5).and_then(as_u64) {
        let Some(q) = u32::try_from(v).ok().filter(|&q| (1..=100).contains(&q)) else {
            queue_job_failed_notice(cid, &preview_uuid, 1, now);
            return Err((
                STATUS_BAD_REQUEST,
                "Sequencing job licenses quantity must be between 1 and 100".into(),
            ));
        };
        q
    } else {
        1
    };

    let (ship_type_id, blend_mode, slots, mut required_comps) = if let Some(layout_m) = sub(dyn_msg, 3) {
        let st_id = seq_of(layout_m, 2)
            .or_else(|| layout_m.get(2).and_then(as_u64))
            .unwrap_or(0) as u32;
        if st_id == 0
            || state
                .sde
                .types
                .get(&st_id)
                .map(|t| t.category_id)
                != Some(CATEGORY_SHIP)
        {
            queue_job_failed_notice(cid, &preview_uuid, 1, now);
            return Err((STATUS_BAD_REQUEST, format!("Invalid ship hull type {st_id} for SKINR layout")));
        }
        let blend = layout_m.get(3).and_then(as_u64).unwrap_or(1) as u32;
        match parse_initiate_slots(state, layout_m, st_id) {
            Ok((sl, rc)) => (st_id, blend, sl, rc),
            Err(e) => {
                queue_job_failed_notice(cid, &preview_uuid, 1, now);
                return Err(e);
            }
        }
    } else if !raw.is_empty() {
        queue_job_failed_notice(cid, &preview_uuid, 1, now);
        return Err((STATUS_BAD_REQUEST, "InitiateRequest missing valid layout".into()));
    } else {
        let default_hull = state.db.with_snapshot(|s| {
            s.characters
                .get(&(cid as i64))
                .and_then(|c| s.items.get(&c.active_ship_id))
                .map(|it| it.type_id)
                .filter(|tid| state.sde.types.get(tid).map(|t| t.category_id) == Some(CATEGORY_SHIP))
                .unwrap_or(DEFAULT_SHIP_TYPE_ID)
        });
        (default_hull, 1, Vec::new(), Vec::new())
    };

    if let Some(DynamicValue::Repeated(kind_vals)) = dyn_msg.get(4) {
        for kv in kind_vals {
            if let DynamicValue::Message(km) = kv {
                if let Some(pair) = sub(km, 1).and_then(parse_component_id) {
                    if !required_comps.contains(&pair) {
                        required_comps.push(pair);
                    }
                }
            }
        }
    }

    // Validate component license ownership before mutating
    {
        let data = state.data.read().unwrap_or_else(|e| e.into_inner());
        for &(comp_id, is_pat) in &required_comps {
            let key = SkinrCosmeticsState::comp_key(cid, is_pat, comp_id);
            if data.infinite_licenses.contains_key(&key) {
                continue;
            }
            let cap = data.finite_licenses.get(&key).map(|l| l.capacity).unwrap_or(0);
            if cap < licenses_qty as u64 {
                drop(data);
                queue_job_failed_notice(cid, &preview_uuid, 3, now);
                return Err((
                    STATUS_BAD_REQUEST,
                    format!("Insufficient component license capacity for component {comp_id}"),
                ));
            }
        }
    }

    // Check and debit ISK fee + award Paragon EverMarks if character exists in DB
    let fee = IskCents::from_isk(10_000);
    let mut insufficient_funds = false;
    state.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&(cid as i64)) {
            if let Some(rem) = ch.balance.checked_sub(fee) {
                ch.balance = rem;
                let cur = ch.loyalty_points.entry(EVERMARK_CORP_ID).or_insert(0);
                *cur = cur.saturating_add(500);
                crate::services::lp_store::record_lp_reward_log(
                    s,
                    cid as i64,
                    EVERMARK_CORP_ID,
                    500,
                    "SKINR sequencing EverMarks reward".into(),
                    0,
                );
            } else {
                insufficient_funds = true;
            }
        }
    });
    if insufficient_funds {
        queue_job_failed_notice(cid, &preview_uuid, 3, now);
        return Err((STATUS_BAD_REQUEST, "Insufficient funds for SKINR sequencing".into()));
    }

    let comp_ids: Vec<u32> = required_comps.iter().map(|&(c, _)| c).collect();
    let (_pts, tier_level) = state.catalog.compute_points_and_tier(&comp_ids);
    let base_secs = (tier_level as u64) * 3_600;
    let planned_secs = (base_secs.saturating_mul(100 - dur_disc as u64) / 100).max(600);

    let (job_uuid, job_rec) = {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        for &(comp_id, is_pat) in &required_comps {
            let key = SkinrCosmeticsState::comp_key(cid, is_pat, comp_id);
            if !data.infinite_licenses.contains_key(&key) {
                if let Some(fl) = data.finite_licenses.get_mut(&key) {
                    fl.capacity = fl.capacity.saturating_sub(licenses_qty as u64);
                }
            }
        }
        let seq = data.next_job_seq;
        data.next_job_seq += 1;
        let uuid = job_seq_to_uuid(seq);
        let skin_hex = format!("skin_{cid:08x}_{seq:04x}");
        data.skins.insert(
            skin_hex.clone(),
            SkinDesignRecord {
                skin_hex: skin_hex.clone(),
                name,
                line_name,
                creator_id: cid,
                ship_type_id,
                pattern_blend_mode: blend_mode,
                slots,
                tier_level,
                first_sequenced_unix: None,
            },
        );
        let job = SequencingJobRecord {
            job_uuid: uuid,
            skin_hex,
            quantity: licenses_qty,
            sequencer_id: cid,
            initiated_unix: now,
            planned_duration_secs: planned_secs,
            completed_unix: None,
            failed_reason: None,
        };
        data.sequencing_jobs.insert(hex_bytes(&uuid), job.clone());
        (uuid, job)
    };
    state.save();
    queue_job_started_notice(cid, &job_uuid, now);

    let mut out = Vec::new();
    encode_bytes_field(1, &encode_job_id(&job_uuid), &mut out);
    encode_bytes_field(2, &encode_job_attributes(&job_rec), &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateResponse",
        out,
    ))
}

fn handle_job_get_all_active(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
) -> HandlerResult {
    let cid = require_char(ctx)?;
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let mut out = Vec::new();
    for job in data.sequencing_jobs.values() {
        if job.sequencer_id == cid && job.is_active() {
            let mut job_entry = Vec::new();
            encode_bytes_field(1, &encode_job_id(&job.job_uuid), &mut job_entry);
            encode_bytes_field(2, &encode_job_attributes(job), &mut job_entry);
            encode_bytes_field(1, &job_entry, &mut out);
        }
    }
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveResponse",
        out,
    ))
}

fn parse_job_uuid(dyn_msg: &DynamicQuasarMessage, raw: &[u8]) -> Option<[u8; 16]> {
    if let Some(DynamicValue::Bytes(b)) = sub(dyn_msg, 1).and_then(|m| m.get(1)) {
        if b.len() == 16 {
            return b.as_slice().try_into().ok();
        }
    }
    let wm = WireMessage::decode(raw).ok()?;
    let b1 = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
    let inner = WireMessage::decode(b1).ok()?;
    let ib = inner.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
    if ib.len() == 16 {
        return ib.try_into().ok();
    }
    None
}

fn handle_job_get(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let Some(uuid) = parse_job_uuid(dyn_msg, raw) else {
        return Err((STATUS_BAD_REQUEST, "Missing sequencing job UUID".into()));
    };
    let key = hex_bytes(&uuid);
    let data = state.data.read().unwrap_or_else(|e| e.into_inner());
    let Some(job) = data.sequencing_jobs.get(&key) else {
        return Err((STATUS_NOT_FOUND, "Sequencing job not found".into()));
    };
    if job.sequencer_id != caller && require_admin_role(ctx).is_err() {
        return Err((STATUS_FORBIDDEN, "Cannot inspect another character's sequencing job".into()));
    }
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_job_attributes(job), &mut out);
    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetResponse",
        out,
    ))
}

fn handle_job_complete_early(
    state: &SkinrCosmeticsState,
    ctx: &AuthoritativeContext,
    raw: &[u8],
    dyn_msg: &DynamicQuasarMessage,
) -> HandlerResult {
    let caller = require_char(ctx)?;
    let now = ctx.now_unix();
    let target_uuid = match parse_job_uuid(dyn_msg, raw) {
        Some(u) => u,
        None if raw.is_empty() => {
            let data = state.data.read().unwrap_or_else(|e| e.into_inner());
            let Some(first_active) = data
                .sequencing_jobs
                .values()
                .find(|j| j.sequencer_id == caller && j.is_active())
                .map(|j| j.job_uuid)
            else {
                return Err((STATUS_BAD_REQUEST, "No active sequencing job to complete".into()));
            };
            first_active
        }
        None => return Err((STATUS_BAD_REQUEST, "Invalid sequencing job identifier".into())),
    };

    let key = hex_bytes(&target_uuid);
    let (skin_hex, qty) = {
        let mut data = state.data.write().unwrap_or_else(|e| e.into_inner());
        let Some(job) = data.sequencing_jobs.get_mut(&key) else {
            return Err((STATUS_NOT_FOUND, "Sequencing job not found".into()));
        };
        if job.sequencer_id != caller {
            return Err((STATUS_FORBIDDEN, "Cannot complete another character's sequencing job".into()));
        }
        if !job.is_active() {
            return Err((STATUS_BAD_REQUEST, "Sequencing job is already finished".into()));
        }
        let end_unix = job.initiated_unix.saturating_add(job.planned_duration_secs);
        let rem_secs = end_unix.saturating_sub(now);
        let plex_cost: i64 = if rem_secs <= 3_600 {
            5
        } else if rem_secs <= 86_400 {
            25
        } else {
            100
        };
        let mut insufficient_plex = false;
        state.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&(caller as i64)) {
                if ch.plex_balance < plex_cost {
                    insufficient_plex = true;
                } else {
                    ch.plex_balance -= plex_cost;
                }
            }
        });
        if insufficient_plex {
            return Err((
                STATUS_BAD_REQUEST,
                format!("Insufficient PLEX balance for early sequencing completion (requires {plex_cost} PLEX)"),
            ));
        }
        job.completed_unix = Some(now);
        let sh = job.skin_hex.clone();
        let q = job.quantity;
        if let Some(skin_rec) = data.skins.get_mut(&sh) {
            skin_rec.first_sequenced_unix.get_or_insert(now);
        }
        (sh, q)
    };
    state.save();
    queue_job_completed_notice(caller, &target_uuid, now);
    grant_or_increment_skin_license(state, caller, &skin_hex, qty as u64, now);

    Ok(any_msg(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyResponse",
        Vec::new(),
    ))
}

pub fn register(router: &PublicGatewayRouter, state: Arc<SkinrCosmeticsState>) {
    macro_rules! reg {
        ($name:expr, $closure:expr) => {{
            let s = Arc::clone(&state);
            router.register_handler($name, Arc::new(move |ctx, any, dyn_msg| ($closure)(&s, ctx, any, dyn_msg)));
        }};
    }

    // 1. Custom 3rd-party SKIN design lookup
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.api.GetRequest",
        "eve.cosmetic.ship.skin.thirdparty.api.requests.GetRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, _ctx, any: &GoogleAny, d| {
            handle_skin_get(s, req_name, &any.value, d)
        });
    }

    // 2. 3rd-party SKIN license endpoints
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetOwnedRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, ctx, _any: &GoogleAny, _d| {
            handle_skin_license_get_owned(s, ctx, req_name)
        });
    }
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
        "eve.cosmetic.ship.skin.thirdparty.license.api.GetRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| {
            handle_skin_license_get(s, ctx, req_name, &any.value, d)
        });
    }
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ActivateRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_skin_license_activate(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.ApplyRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_skin_license_apply(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.UnapplyRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_skin_license_unapply(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.GrantRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_skin_license_admin_grant(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.license.api.admin.RevokeRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_skin_license_admin_revoke(s, ctx, &any.value, d)
    );

    // 3. Sequencing pricing, durations, and capacity
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierPricingRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, ctx, _any: &GoogleAny, _d| {
            handle_get_tier_pricing(s, ctx, req_name)
        });
    }
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetTierDurationsForCharacterRequest",
    ] {
        reg!(req_name, move |s: &Arc<SkinrCosmeticsState>, ctx, _any: &GoogleAny, _d| {
            handle_get_tier_durations(s, ctx, req_name)
        });
    }
    for req_name in [
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetEarlyCompletionPricingRequest",
    ] {
        reg!(req_name, move |_s: &Arc<SkinrCosmeticsState>, _ctx, _any: &GoogleAny, _d| {
            handle_get_early_completion_pricing(req_name)
        });
    }
    reg!(
        "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx: &AuthoritativeContext, _any: &GoogleAny, _d| {
            let (_, _, max_jobs) = skill_discounts_and_cap(s, ctx.effective_character_id());
            let mut out = Vec::new();
            encode_varint_field(1, max_jobs as u64, &mut out);
            Ok(any_msg(
                "eve.cosmetic.ship.skin.thirdparty.sequencing.api.GetSkillBasedJobsCapacityResponse",
                out,
            ))
        }
    );

    // 4. Sequencing job lifecycle
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.InitiateRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_job_initiate(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetAllActiveRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, _any: &GoogleAny, _d| handle_job_get_all_active(s, ctx)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.GetRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_job_get(s, ctx, &any.value, d)
    );
    reg!(
        "eve_public.cosmetic.ship.skin.thirdparty.sequencing.job.api.CompleteEarlyRequest",
        |s: &Arc<SkinrCosmeticsState>, ctx, any: &GoogleAny, d| handle_job_complete_early(s, ctx, &any.value, d)
    );
}
