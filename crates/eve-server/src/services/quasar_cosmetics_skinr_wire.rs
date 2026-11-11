//! Wire encoding, decoding, notice emission, and tier pricing helpers for `quasar_cosmetics_skinr`
//! and `quasar_cosmetics_skinr_seq`.

#![forbid(unsafe_code)]

use super::{
    SdeSkinComponent, SkinDesignRecord, SkinLayoutSlot, SkinrCosmeticsState, ThirdPartySkinLicense,
};
use crate::quasar_gateway::{NoticeScope, STATUS_BAD_REQUEST, STATUS_FORBIDDEN};
use crate::services::quasar_social_chat::ctx_queue_notice;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};

pub fn any_msg(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

pub fn put_seq(field: u32, id: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, id, &mut b);
    encode_bytes_field(field, &b, out);
}

pub fn put_timestamp(field: u32, unix_secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, unix_secs, &mut b);
    encode_bytes_field(field, &b, out);
}

pub fn put_duration(field: u32, secs: u64, out: &mut Vec<u8>) {
    let mut b = Vec::new();
    encode_varint_field(1, secs, &mut b);
    encode_bytes_field(field, &b, out);
}

pub fn require_char(ctx: &AuthoritativeContext) -> Result<u32, (u32, String)> {
    ctx.effective_character_id()
        .ok_or_else(|| (STATUS_FORBIDDEN, "Active character required".to_string()))
}

pub fn as_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint64(u) => Some(*u),
        DynamicValue::Uint32(u) => Some(*u as u64),
        DynamicValue::Int64(i) => u64::try_from(*i).ok(),
        DynamicValue::Int32(i) => u64::try_from(*i).ok(),
        DynamicValue::Enum(e) => u64::try_from(*e).ok(),
        _ => None,
    }
}

pub fn sub(msg: &DynamicQuasarMessage, field: u32) -> Option<&DynamicQuasarMessage> {
    match msg.get(field)? {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

pub fn seq_of(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    sub(msg, field).and_then(|m| m.get(1)).and_then(as_u64)
}

pub fn str_of(msg: &DynamicQuasarMessage, field: u32) -> Option<String> {
    match msg.get(field)? {
        DynamicValue::String(s) => Some(s.clone()),
        DynamicValue::Bytes(b) => String::from_utf8(b.clone()).ok(),
        _ => None,
    }
}

/// Encodes `eve_public.cosmetic.ship.skin.thirdparty.component.Identifier`.
pub fn encode_component_id(component_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut inner = Vec::new();
    encode_varint_field(1, component_id as u64, &mut inner);
    let mut out = Vec::new();
    encode_bytes_field(if is_pattern { 2 } else { 1 }, &inner, &mut out);
    out
}

/// Parses `component.Identifier { 1: coating { 1: seq }, 2: pattern { 1: seq } }`.
pub fn parse_component_id(msg: &DynamicQuasarMessage) -> Option<(u32, bool)> {
    if let Some(id) = seq_of(msg, 1) {
        if id > 0 {
            return Some((id as u32, false));
        }
    }
    if let Some(id) = seq_of(msg, 2) {
        if id > 0 {
            return Some((id as u32, true));
        }
    }
    None
}

pub fn parse_component_id_wire(raw: &[u8]) -> Option<(u32, bool)> {
    let wm = WireMessage::decode(raw).ok()?;
    for f in &wm.fields {
        if f.field_number == 1 || f.field_number == 2 {
            if let Some(b) = f.value.as_bytes() {
                if let Ok(inner) = WireMessage::decode(b) {
                    if let Some(seq) = inner
                        .fields
                        .iter()
                        .find(|x| x.field_number == 1)
                        .and_then(|x| x.value.as_varint())
                    {
                        if seq > 0 {
                            return Some((seq as u32, f.field_number == 2));
                        }
                    }
                }
            }
        }
    }
    None
}

/// Encodes `finite.Identifier` or `infinite.Identifier` (`{ 1: character, 2: component }`).
pub fn encode_comp_license_id(character_id: u32, component_id: u32, is_pattern: bool) -> Vec<u8> {
    let mut out = Vec::new();
    put_seq(1, character_id as u64, &mut out);
    encode_bytes_field(2, &encode_component_id(component_id, is_pattern), &mut out);
    out
}

/// Encodes `eve_public.cosmetic.ship.skin.thirdparty.component.license.Kind`.
pub fn encode_license_kind(component_id: u32, is_pattern: bool, finite_qty: Option<u64>) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, &encode_component_id(component_id, is_pattern), &mut out);
    match finite_qty {
        Some(qty) => encode_varint_field(3, qty, &mut out),
        None => encode_varint_field(2, 1, &mut out),
    }
    out
}

/// Emits `eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice`.
pub fn queue_component_license_granted_notice(
    character_id: u32,
    component_id: u32,
    is_pattern: bool,
    finite_qty: Option<u64>,
    now_unix: u64,
) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_license_kind(component_id, is_pattern, finite_qty), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg(
            "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice",
            payload,
        ),
        now_unix,
    );
}

pub(crate) fn validate_component_in_sde(
    state: &SkinrCosmeticsState,
    comp_id: u32,
    is_pattern: bool,
) -> Result<&SdeSkinComponent, (u32, String)> {
    let Some(comp) = state.catalog.components.get(&comp_id) else {
        return Err((STATUS_BAD_REQUEST, format!("Unknown SKINR design component {comp_id}")));
    };
    if comp.is_pattern() != is_pattern {
        return Err((
            STATUS_BAD_REQUEST,
            format!("Component {comp_id} category mismatch for coating/pattern selector"),
        ));
    }
    Ok(comp)
}

pub(crate) fn parse_license_identifier(
    dyn_msg: &DynamicQuasarMessage,
    raw: &[u8],
) -> Option<(Option<u32>, u32, bool)> {
    if let Some(lic_msg) = sub(dyn_msg, 1) {
        let char_id = seq_of(lic_msg, 1).map(|u| u as u32);
        if let Some((cid, is_pat)) = sub(lic_msg, 2).and_then(parse_component_id) {
            return Some((char_id, cid, is_pat));
        }
    }
    let wm = WireMessage::decode(raw).ok()?;
    let lic_bytes = wm.fields.iter().find(|f| f.field_number == 1)?.value.as_bytes()?;
    let lic_wm = WireMessage::decode(lic_bytes).ok()?;
    let char_id = lic_wm
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
    let comp_bytes = lic_wm.fields.iter().find(|f| f.field_number == 2)?.value.as_bytes()?;
    let (cid, is_pat) = parse_component_id_wire(comp_bytes)?;
    Some((char_id, cid, is_pat))
}

pub(crate) fn tier_base_plex_cents(tier: u32) -> u64 {
    let plex = match tier {
        0 | 1 => 25,
        2 => 30,
        3 => 35,
        4 => 40,
        5 => 50,
        6 => 60,
        7 => 75,
        8 => 90,
        9 => 110,
        t => 110 + u64::from(t - 9) * 20,
    };
    plex * 100
}

pub(crate) fn tier_base_duration_secs(tier: u32) -> u64 {
    let hours = match tier {
        0 | 1 => 3,
        2 => 7,
        3 => 12,
        4 => 19,
        5 => 28,
        6 => 40,
        7 => 56,
        8 => 75,
        9 => 97,
        10 => 121,
        11 => 150,
        12 => 184,
        13 => 224,
        14 => 271,
        15 => 325,
        16 => 386,
        17 => 455,
        t => 532 + u64::from(t - 18) * 80,
    };
    hours * 3_600
}

pub(crate) fn encode_skin_id(skin_hex: &str) -> Vec<u8> {
    let mut b = Vec::new();
    encode_string_field(1, skin_hex, &mut b);
    b
}

pub(crate) fn encode_skin_license_id(character_id: u32, skin_hex: &str) -> Vec<u8> {
    let mut b = Vec::new();
    put_seq(1, character_id as u64, &mut b);
    encode_bytes_field(2, &encode_skin_id(skin_hex), &mut b);
    b
}

pub(crate) fn encode_skin_license_attrs(lic: &ThirdPartySkinLicense) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, u64::from(lic.activated), &mut b);
    encode_varint_field(2, lic.unactivated, &mut b);
    encode_varint_field(3, lic.escrowed, &mut b);
    put_timestamp(4, lic.first_acquired_unix, &mut b);
    b
}

pub(crate) fn encode_layout_slot(slot: &SkinLayoutSlot) -> Vec<u8> {
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

pub(crate) fn encode_skin_attributes(rec: &SkinDesignRecord) -> Vec<u8> {
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

pub(crate) fn queue_skin_license_added_notice(lic: &ThirdPartySkinLicense, now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(lic.character_id, &lic.skin_hex), &mut payload);
    encode_bytes_field(2, &encode_skin_license_attrs(lic), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(lic.character_id),
        any_msg("eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice", payload),
        now_unix,
    );
}

pub(crate) fn queue_skin_license_updated_notice(
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

pub(crate) fn queue_skin_license_deleted_notice(character_id: u32, skin_hex: &str, now_unix: u64) {
    let mut payload = Vec::new();
    encode_bytes_field(1, &encode_skin_license_id(character_id, skin_hex), &mut payload);
    ctx_queue_notice(
        NoticeScope::Character(character_id),
        any_msg("eve_public.cosmetic.ship.skin.thirdparty.license.api.DeletedNotice", payload),
        now_unix,
    );
}

pub(crate) fn queue_ship_skin_set_notice(
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

pub(crate) fn parse_skin_hex(dyn_msg: &DynamicQuasarMessage, raw: &[u8]) -> Option<String> {
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

pub(crate) fn parse_skin_license_id(
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

#[cfg(test)]
#[path = "quasar_cosmetics_skinr_wire_edges_tests.rs"]
mod quasar_cosmetics_skinr_wire_edges_tests;
