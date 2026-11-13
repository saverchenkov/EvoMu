//! Wire encoding, decoding, notice emission, and tier pricing helpers for `quasar_cosmetics_skinr`
//! and `quasar_cosmetics_skinr_seq`.

#![forbid(unsafe_code)]

use super::{
    SdeSkinComponent, SkinDesignRecord, SkinLayoutSlot, SkinrCosmeticsState, ThirdPartySkinLicense,
};
use crate::quasar_gateway::{NoticeScope, STATUS_BAD_REQUEST, STATUS_FORBIDDEN};
use crate::services::quasar_social_chat::ctx_queue_notice;
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp,
    WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};

pub fn any_msg(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
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

pub(crate) fn build_component_id(component_id: u32, is_pattern: bool) -> ProtoBuilder {
    let b = pb::message("eve_public.cosmetic.ship.skin.thirdparty.component.Identifier");
    if is_pattern {
        b.set("pattern.sequential", component_id as u64)
    } else {
        b.set("coating.sequential", component_id as u64)
    }
}

/// Encodes `eve_public.cosmetic.ship.skin.thirdparty.component.Identifier`.
pub fn encode_component_id(component_id: u32, is_pattern: bool) -> Vec<u8> {
    build_component_id(component_id, is_pattern).must_encode()
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

pub(crate) fn build_comp_license_id(
    type_name: &str,
    character_id: u32,
    component_id: u32,
    is_pattern: bool,
) -> ProtoBuilder {
    pb::message(type_name)
        .set("character.sequential", character_id)
        .set("component", build_component_id(component_id, is_pattern))
}

/// Encodes `finite.Identifier` or `infinite.Identifier` (`{ 1: character, 2: component }`).
pub fn encode_comp_license_id(character_id: u32, component_id: u32, is_pattern: bool) -> Vec<u8> {
    build_comp_license_id(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.finite.Identifier",
        character_id,
        component_id,
        is_pattern,
    )
    .must_encode()
}

pub(crate) fn build_license_kind(
    component_id: u32,
    is_pattern: bool,
    finite_qty: Option<u64>,
) -> ProtoBuilder {
    let b = pb::message("eve_public.cosmetic.ship.skin.thirdparty.component.license.Kind")
        .set("component", build_component_id(component_id, is_pattern));
    match finite_qty {
        Some(qty) => b.set("finite", qty),
        None => b.set("infinite", true),
    }
}

/// Encodes `eve_public.cosmetic.ship.skin.thirdparty.component.license.Kind`.
pub fn encode_license_kind(component_id: u32, is_pattern: bool, finite_qty: Option<u64>) -> Vec<u8> {
    build_license_kind(component_id, is_pattern, finite_qty).must_encode()
}

/// Emits `eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice`.
pub fn queue_component_license_granted_notice(
    character_id: u32,
    component_id: u32,
    is_pattern: bool,
    finite_qty: Option<u64>,
    now_unix: u64,
) {
    let notice = pb::message(
        "eve_public.cosmetic.ship.skin.thirdparty.component.license.api.GrantedNotice",
    )
    .set("kind", build_license_kind(component_id, is_pattern, finite_qty))
    .must_to_any();
    ctx_queue_notice(NoticeScope::Character(character_id), notice, now_unix);
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

pub(crate) fn build_skin_id(prefix: &str, skin_hex: &str) -> ProtoBuilder {
    pb::message(format!("{prefix}.cosmetic.ship.skin.thirdparty.Identifier")).set("hex", skin_hex)
}

pub(crate) fn build_skin_license_id(
    prefix: &str,
    character_id: u32,
    skin_hex: &str,
) -> ProtoBuilder {
    pb::message(format!("{prefix}.cosmetic.ship.skin.thirdparty.license.Identifier"))
        .set("character.sequential", character_id)
        .set("skin", build_skin_id(prefix, skin_hex))
}

pub(crate) fn build_skin_license_attrs(
    prefix: &str,
    lic: &ThirdPartySkinLicense,
) -> ProtoBuilder {
    pb::message(format!("{prefix}.cosmetic.ship.skin.thirdparty.license.Attributes"))
        .set("activated", lic.activated)
        .set("unactivated", lic.unactivated)
        .set("escrowed", lic.escrowed)
        .set("first_acquired", GoogleTimestamp::from_unix(lic.first_acquired_unix as i64, 0))
}

pub(crate) fn build_layout_slot(prefix: &str, slot: &SkinLayoutSlot) -> ProtoBuilder {
    let b = pb::message(format!("{prefix}.cosmetic.ship.skin.thirdparty.Layout.Slot"))
        .set("id.sequential", slot.slot_id as u64);
    if slot.is_pattern {
        b.set("configuration.pattern.pattern.sequential", slot.component_id as u64)
    } else {
        b.set("configuration.coating.coating.sequential", slot.component_id as u64)
    }
}

#[cfg(test)]
pub(crate) fn encode_layout_slot(slot: &SkinLayoutSlot) -> Vec<u8> {
    build_layout_slot("eve_public", slot).must_encode()
}

pub(crate) fn build_skin_layout(prefix: &str, rec: &SkinDesignRecord) -> ProtoBuilder {
    pb::message(format!("{prefix}.cosmetic.ship.skin.thirdparty.Layout"))
        .with(|mut b| {
            for slot in &rec.slots {
                b = b.add("slots", build_layout_slot(prefix, slot));
            }
            b
        })
        .set("ship_type.sequential", rec.ship_type_id.max(1) as u64)
        .set("pattern_blend_mode", rec.pattern_blend_mode.max(1) as i32)
}

pub(crate) fn build_skin_attributes(prefix: &str, rec: &SkinDesignRecord) -> ProtoBuilder {
    let mut out = pb::message(format!("{prefix}.cosmetic.ship.skin.thirdparty.Attributes"))
        .set("name", &rec.name);
    if prefix == "eve" {
        out = out.set("line.hex", &rec.line_name);
    } else {
        out = out.set("line_name", &rec.line_name);
    }
    out = out
        .set("creator.sequential", rec.creator_id)
        .set("layout", build_skin_layout(prefix, rec))
        .set("tier.level", rec.tier_level.max(1));
    match rec.first_sequenced_unix {
        Some(ts) => out.set("first_sequenced", GoogleTimestamp::from_unix(ts as i64, 0)),
        None => out.set("not_sequenced", true),
    }
}

#[cfg(test)]
pub(crate) fn encode_skin_attributes(rec: &SkinDesignRecord) -> Vec<u8> {
    build_skin_attributes("eve_public", rec).must_encode()
}

pub(crate) fn queue_skin_license_added_notice(lic: &ThirdPartySkinLicense, now_unix: u64) {
    let notice = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.AddedNotice")
        .set("id", build_skin_license_id("eve_public", lic.character_id, &lic.skin_hex))
        .set("attributes", build_skin_license_attrs("eve_public", lic))
        .must_to_any();
    ctx_queue_notice(NoticeScope::Character(lic.character_id), notice, now_unix);
}

pub(crate) fn queue_skin_license_updated_notice(
    lic: &ThirdPartySkinLicense,
    prev: &ThirdPartySkinLicense,
    now_unix: u64,
) {
    let notice = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.UpdatedNotice")
        .set("id", build_skin_license_id("eve_public", lic.character_id, &lic.skin_hex))
        .set("attributes", build_skin_license_attrs("eve_public", lic))
        .set("previous_attributes", build_skin_license_attrs("eve_public", prev))
        .must_to_any();
    ctx_queue_notice(NoticeScope::Character(lic.character_id), notice, now_unix);
}

pub(crate) fn queue_skin_license_deleted_notice(character_id: u32, skin_hex: &str, now_unix: u64) {
    let notice = pb::message("eve_public.cosmetic.ship.skin.thirdparty.license.api.DeletedNotice")
        .set("id", build_skin_license_id("eve_public", character_id, skin_hex))
        .must_to_any();
    ctx_queue_notice(NoticeScope::Character(character_id), notice, now_unix);
}

pub(crate) fn queue_ship_skin_set_notice(
    character_id: u32,
    ship_id: u64,
    thirdparty_skin_hex: Option<&str>,
    now_unix: u64,
) {
    let notice = pb::message("eve_public.cosmetic.ship.api.SetNotice")
        .set("state.character.sequential", character_id)
        .set("state.ship.sequential", ship_id)
        .with(|b| match thirdparty_skin_hex {
            Some(hex) => b.set("state.skin.thirdparty.identifier", build_skin_id("eve_public", hex)),
            None => b.set("state.skin.no_skin", true),
        })
        .must_to_any();
    ctx_queue_notice(NoticeScope::Character(character_id), notice, now_unix);
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
