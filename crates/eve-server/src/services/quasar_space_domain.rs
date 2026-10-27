//! `quasar_space_domain` — Quasar `eve.space.{location,npcstation,measurement,tag,api}` handlers.
//!
//! State for space measurements and space location tags persists in
//! `WorldStateSnapshot::quasar_state_blobs["quasar_space_domain"]`.

#![forbid(unsafe_code)]

use super::quasar_social_chat::ctx_queue_notice;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::BTreeMap;
use std::sync::Arc;

fn encode_double_field(field_number: u32, v: f64, out: &mut Vec<u8>) {
    out.push(((field_number as u8) << 3) | 1);
    out.extend_from_slice(&v.to_le_bytes());
}

const BLOB_KEY: &str = "quasar_space_domain";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SpaceDomainState {
    /// `measurement_name -> Vec<(location_hex, value)>`
    pub measurements: BTreeMap<String, Vec<(String, f64)>>,
    /// `location_hex -> Vec<tag_name>`
    pub location_tags: BTreeMap<String, Vec<String>>,
}

fn load(db: &WorldRepository) -> SpaceDomainState {
    db.with_snapshot(|s| {
        s.quasar_state_blobs
            .get(BLOB_KEY)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default()
    })
}

fn save(db: &WorldRepository, state: &SpaceDomainState) {
    if let Ok(j) = serde_json::to_string(state) {
        db.with_mut_snapshot(|s| {
            s.quasar_state_blobs.insert(BLOB_KEY.into(), j);
        });
    }
}

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .filter_map(|i| s.get(i..i + 2).and_then(|b| u8::from_str_radix(b, 16).ok()))
        .collect()
}

fn sub(msg: &DynamicQuasarMessage, field: u32) -> Option<&DynamicQuasarMessage> {
    match msg.get(field)? {
        DynamicValue::Message(m) => Some(m),
        _ => None,
    }
}

fn repeated(msg: &DynamicQuasarMessage, field: u32) -> Vec<&DynamicQuasarMessage> {
    match msg.get(field) {
        Some(DynamicValue::Repeated(v)) => v
            .iter()
            .filter_map(|x| match x {
                DynamicValue::Message(m) => Some(m.as_ref()),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn as_u32(v: &DynamicValue) -> Option<u32> {
    match v {
        DynamicValue::Uint32(u) => Some(*u),
        DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
        DynamicValue::Int32(i) => u32::try_from(*i).ok(),
        DynamicValue::Int64(i) => u32::try_from(*i).ok(),
        _ => None,
    }
}

fn as_f64(v: &DynamicValue) -> Option<f64> {
    match v {
        DynamicValue::Double(d) => Some(*d),
        DynamicValue::Float(f) => Some(*f as f64),
        DynamicValue::Uint64(u) => Some(*u as f64),
        DynamicValue::Int64(i) => Some(*i as f64),
        _ => None,
    }
}

fn seq_id(id: u32) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, id as u64, &mut b);
    b
}

/// Encodes `.eve.space.location.Identifier`.
pub fn encode_space_location(
    region_id: u32,
    constellation_id: Option<u32>,
    solar_system_id: Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(1, &seq_id(region_id), &mut out);
    if let Some(cid) = constellation_id {
        let mut c_msg = Vec::new();
        encode_bytes_field(1, &seq_id(cid), &mut c_msg);
        if let Some(sid) = solar_system_id {
            encode_bytes_field(2, &seq_id(sid), &mut c_msg);
        } else {
            encode_varint_field(3, 1, &mut c_msg);
        }
        encode_bytes_field(2, &c_msg, &mut out);
    } else {
        encode_varint_field(3, 1, &mut out);
    }
    out
}

fn raw_field_bytes(raw: &[u8], field_number: u32) -> Option<Vec<u8>> {
    WireMessage::decode(raw)
        .ok()?
        .fields
        .into_iter()
        .find(|f| f.field_number == field_number)
        .and_then(|f| f.value.as_bytes().map(|b| b.to_vec()))
}

fn resolve_location_from_req(sde: &SdeRegistry, loc: &DynamicQuasarMessage) -> Option<Vec<u8>> {
    if let Some(rid) = sub(loc, 1).and_then(|r| r.get(1)).and_then(as_u32) {
        if sde.regions.contains_key(&rid) {
            return Some(encode_space_location(rid, None, None));
        }
    }
    if let Some(cid) = sub(loc, 2).and_then(|c| c.get(1)).and_then(as_u32) {
        if let Some(c) = sde.constellations.get(&cid) {
            return Some(encode_space_location(c.region_id, Some(cid), None));
        }
    }
    if let Some(sid) = sub(loc, 3).and_then(|s| s.get(1)).and_then(as_u32) {
        if let Some(s) = sde.solar_systems.get(&sid) {
            return Some(encode_space_location(s.region_id, Some(s.constellation_id), Some(sid)));
        }
    }
    None
}

pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve.space.location.api.ResolveLocationsRequest
    {
        let sde_res = Arc::clone(&sde);
        router.register_handler(
            "eve.space.location.api.ResolveLocationsRequest",
            Arc::new(move |_ctx, _any, msg| {
                let mut resolved = Vec::new();
                let mut missing_sys = Vec::new();
                let mut missing_con = Vec::new();
                for loc in repeated(msg, 1) {
                    if let Some(cid) = sub(loc, 1).and_then(|c| c.get(1)).and_then(as_u32) {
                        if let Some(c) = sde_res.constellations.get(&cid) {
                            resolved.push(encode_space_location(c.region_id, Some(cid), None));
                        } else {
                            missing_con.push(cid);
                        }
                    } else if let Some(sid) = sub(loc, 2).and_then(|s| s.get(1)).and_then(as_u32) {
                        if let Some(s) = sde_res.solar_systems.get(&sid) {
                            resolved.push(encode_space_location(
                                s.region_id,
                                Some(s.constellation_id),
                                Some(sid),
                            ));
                        } else {
                            missing_sys.push(sid);
                        }
                    }
                }
                let mut out = Vec::new();
                if !missing_sys.is_empty() || !missing_con.is_empty() {
                    let mut nf = Vec::new();
                    for sid in missing_sys {
                        encode_bytes_field(1, &seq_id(sid), &mut nf);
                    }
                    for cid in missing_con {
                        encode_bytes_field(2, &seq_id(cid), &mut nf);
                    }
                    encode_bytes_field(3, &nf, &mut out);
                } else {
                    let mut locs = Vec::new();
                    for r in resolved {
                        encode_bytes_field(1, &r, &mut locs);
                    }
                    encode_bytes_field(1, &locs, &mut out);
                }
                Ok(any("eve.space.location.api.ResolveLocationsResponse", out))
            }),
        );
    }

    // 2. eve.space.location.api.GetAllSolarSystemsRequest
    {
        let sde_all = Arc::clone(&sde);
        router.register_handler(
            "eve.space.location.api.GetAllSolarSystemsRequest",
            Arc::new(move |_ctx, _any, _msg| {
                let mut ids: Vec<u32> = sde_all.solar_systems.keys().copied().collect();
                ids.sort_unstable();
                let mut out = Vec::new();
                for sid in ids {
                    encode_bytes_field(1, &seq_id(sid), &mut out);
                }
                Ok(any("eve.space.location.api.GetAllSolarSystemsResponse", out))
            }),
        );
    }

    // 3. eve.space.api.GetLocationsWithinSecurityRequest
    {
        let sde_sec = Arc::clone(&sde);
        router.register_handler(
            "eve.space.api.GetLocationsWithinSecurityRequest",
            Arc::new(move |_ctx, _any, msg| {
                let min_sec = sub(msg, 1).and_then(|m| m.get(1)).and_then(as_f64).unwrap_or(-10.0);
                let max_sec = sub(msg, 2).and_then(|m| m.get(1)).and_then(as_f64).unwrap_or(10.0);
                if !min_sec.is_finite() || !max_sec.is_finite() || min_sec > max_sec {
                    return Err((STATUS_BAD_REQUEST, "Invalid security band".into()));
                }
                let mut sys_ids: Vec<u32> = sde_sec
                    .solar_systems
                    .values()
                    .filter(|s| s.security_status >= min_sec && s.security_status <= max_sec)
                    .map(|s| s.solar_system_id)
                    .collect();
                sys_ids.sort_unstable();
                let mut out = Vec::new();
                for sid in sys_ids {
                    let s = &sde_sec.solar_systems[&sid];
                    encode_bytes_field(
                        1,
                        &encode_space_location(s.region_id, Some(s.constellation_id), Some(sid)),
                        &mut out,
                    );
                }
                Ok(any("eve.space.api.GetLocationsWithinSecurityResponse", out))
            }),
        );
    }

    // 4. eve.space.npcstation.api.GetRequest
    {
        let sde_st = Arc::clone(&sde);
        router.register_handler(
            "eve.space.npcstation.api.GetRequest",
            Arc::new(move |_ctx, _any, msg| {
                let st_id = sub(msg, 1)
                    .and_then(|i| i.get(1))
                    .and_then(as_u32)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing station identifier".into()))?;
                let st = sde_st
                    .stations
                    .get(&(st_id as i64))
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Station {st_id} not found")))?;
                let sys = sde_st.solar_systems.get(&st.solar_system_id).ok_or_else(|| {
                    (STATUS_NOT_FOUND, format!("Solar system {} not found", st.solar_system_id))
                })?;
                let mut out = Vec::new();
                encode_bytes_field(
                    1,
                    &encode_space_location(
                        sys.region_id,
                        Some(sys.constellation_id),
                        Some(sys.solar_system_id),
                    ),
                    &mut out,
                );
                encode_bytes_field(2, &seq_id(st.owner_corporation_id as u32), &mut out);
                Ok(any("eve.space.npcstation.api.GetResponse", out))
            }),
        );
    }

    // 5. Space measurements: RecordRequest, admin.api.RecordRequest, QueryRequest
    for (req_name, rsp_name) in [
        (
            "eve.space.measurement.api.RecordRequest",
            "eve.space.measurement.api.RecordResponse",
        ),
        (
            "eve.space.measurement.admin.api.RecordRequest",
            "eve.space.measurement.admin.api.RecordResponse",
        ),
    ] {
        let db_m = Arc::clone(&db);
        router.register_handler(
            req_name,
            Arc::new(move |ctx: &AuthoritativeContext, raw_any, msg| {
                require_admin_role(ctx)?;
                let name = match sub(msg, 1).and_then(|i| i.get(1)) {
                    Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => return Err((STATUS_BAD_REQUEST, "Missing measurement name".into())),
                };
                let m_attrs = sub(msg, 2)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing measurement attributes".into()))?;
                let val = m_attrs.get(2).and_then(as_f64).unwrap_or(0.0);
                if !val.is_finite() {
                    return Err((STATUS_BAD_REQUEST, "Measurement value must be finite".into()));
                }
                let loc_bytes = raw_field_bytes(&raw_any.value, 2)
                    .and_then(|b| raw_field_bytes(&b, 1))
                    .unwrap_or_default();
                let mut state = load(&db_m);
                if state.measurements.len() >= 4096 && !state.measurements.contains_key(&name) {
                    if let Some(first_key) = state.measurements.keys().next().cloned() {
                        state.measurements.remove(&first_key);
                    }
                }
                let entries = state.measurements.entry(name.clone()).or_default();
                let loc_hex = hex_encode(&loc_bytes);
                if let Some(existing) = entries.iter_mut().find(|(lh, _)| lh == &loc_hex) {
                    existing.1 = val;
                } else {
                    if entries.len() >= 4096 {
                        entries.remove(0);
                    }
                    entries.push((loc_hex, val));
                }
                save(&db_m, &state);

                let (mut id_b, mut at_b, mut not_b) = (Vec::new(), Vec::new(), Vec::new());
                encode_string_field(1, &name, &mut id_b);
                if !loc_bytes.is_empty() {
                    encode_bytes_field(1, &loc_bytes, &mut at_b);
                }
                encode_double_field(2, val, &mut at_b);
                encode_bytes_field(1, &id_b, &mut not_b);
                encode_bytes_field(2, &at_b, &mut not_b);
                ctx_queue_notice(
                    NoticeScope::Broadcast,
                    any("eve.space.measurement.api.Recorded", not_b),
                    ctx.now_unix(),
                );
                Ok(any(rsp_name, Vec::new()))
            }),
        );
    }

    {
        let db_q = Arc::clone(&db);
        router.register_handler(
            "eve.space.measurement.api.QueryRequest",
            Arc::new(move |_ctx, raw_any, msg| {
                let name = match sub(msg, 1).and_then(|i| i.get(1)) {
                    Some(DynamicValue::String(s)) => s.trim().to_string(),
                    _ => String::new(),
                };
                let filter_loc = raw_field_bytes(&raw_any.value, 2).map(|b| hex_encode(&b));
                let state = load(&db_q);
                let mut rows: Vec<(String, f64)> = state
                    .measurements
                    .get(&name)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|(lh, _)| filter_loc.as_ref().map_or(true, |fl| fl.is_empty() || lh == fl))
                    .collect();
                if let Some(top_n) = msg.get(3).and_then(as_u32) {
                    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    rows.truncate(top_n as usize);
                } else if let Some(bot_n) = msg.get(4).and_then(as_u32) {
                    rows.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                    rows.truncate(bot_n as usize);
                }
                let mut out = Vec::new();
                for (lh, v) in rows {
                    let mut at_b = Vec::new();
                    let lb = hex_decode(&lh);
                    if !lb.is_empty() {
                        encode_bytes_field(1, &lb, &mut at_b);
                    }
                    encode_double_field(2, v, &mut at_b);
                    encode_bytes_field(1, &at_b, &mut out);
                }
                Ok(any("eve.space.measurement.api.QueryResponse", out))
            }),
        );
    }

    // 6. Space tags: ApplyRequest, RemoveRequest, GetTagsRequest, GetLocationsRequest, QueryRequest
    {
        let db_ap = Arc::clone(&db);
        router.register_handler(
            "eve.space.tag.admin.api.ApplyRequest",
            Arc::new(move |ctx, raw_any, msg| {
                require_admin_role(ctx)?;
                let loc_bytes = raw_field_bytes(&raw_any.value, 2)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing location".into()))?;
                let mut tags = Vec::new();
                for spec in repeated(msg, 3) {
                    if let Some(DynamicValue::String(t)) = sub(spec, 1).and_then(|ti| ti.get(1)) {
                        if !t.trim().is_empty() {
                            tags.push(t.trim().to_string());
                        }
                    }
                }
                if tags.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "At least one tag specification required".into()));
                }
                let mut state = load(&db_ap);
                let loc_key = hex_encode(&loc_bytes);
                if state.location_tags.len() >= 4096 && !state.location_tags.contains_key(&loc_key) {
                    if let Some(first_key) = state.location_tags.keys().next().cloned() {
                        state.location_tags.remove(&first_key);
                    }
                }
                let entry = state.location_tags.entry(loc_key).or_default();
                for t in &tags {
                    if !entry.contains(t) {
                        if entry.len() >= 256 {
                            entry.remove(0);
                        }
                        entry.push(t.clone());
                    }
                }
                save(&db_ap, &state);
                let mut not_b = Vec::new();
                encode_bytes_field(1, &loc_bytes, &mut not_b);
                for t in &tags {
                    let mut tb = Vec::new();
                    encode_string_field(1, t, &mut tb);
                    encode_bytes_field(2, &tb, &mut not_b);
                }
                ctx_queue_notice(
                    NoticeScope::Broadcast,
                    any("eve.space.tag.api.Applied", not_b),
                    ctx.now_unix(),
                );
                Ok(any("eve.space.tag.admin.api.ApplyResponse", Vec::new()))
            }),
        );
    }

    {
        let db_rm = Arc::clone(&db);
        router.register_handler(
            "eve.space.tag.admin.api.RemoveRequest",
            Arc::new(move |ctx, raw_any, msg| {
                require_admin_role(ctx)?;
                let loc_bytes = raw_field_bytes(&raw_any.value, 2)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing location".into()))?;
                let mut tags = Vec::new();
                for ti in repeated(msg, 3) {
                    if let Some(DynamicValue::String(t)) = ti.get(1) {
                        tags.push(t.trim().to_string());
                    }
                }
                let mut state = load(&db_rm);
                if let Some(entry) = state.location_tags.get_mut(&hex_encode(&loc_bytes)) {
                    entry.retain(|x| !tags.contains(x));
                }
                save(&db_rm, &state);
                let mut not_b = Vec::new();
                encode_bytes_field(1, &loc_bytes, &mut not_b);
                for t in &tags {
                    let mut tb = Vec::new();
                    encode_string_field(1, t, &mut tb);
                    encode_bytes_field(2, &tb, &mut not_b);
                }
                ctx_queue_notice(
                    NoticeScope::Broadcast,
                    any("eve.space.tag.api.Removed", not_b),
                    ctx.now_unix(),
                );
                Ok(any("eve.space.tag.admin.api.RemoveResponse", Vec::new()))
            }),
        );
    }

    {
        let (db_gt, sde_gt) = (Arc::clone(&db), Arc::clone(&sde));
        router.register_handler(
            "eve.space.location.api.GetTagsRequest",
            Arc::new(move |_ctx, _any, msg| {
                let state = load(&db_gt);
                let mut locs_msg = Vec::new();
                for req_loc in repeated(msg, 1) {
                    if let Some(loc_bytes) = resolve_location_from_req(&sde_gt, req_loc) {
                        let tags = state
                            .location_tags
                            .get(&hex_encode(&loc_bytes))
                            .cloned()
                            .unwrap_or_default();
                        let mut tl = Vec::new();
                        encode_bytes_field(1, &loc_bytes, &mut tl);
                        for t in tags {
                            let mut tb = Vec::new();
                            encode_string_field(1, &t, &mut tb);
                            encode_bytes_field(2, &tb, &mut tl);
                        }
                        encode_bytes_field(1, &tl, &mut locs_msg);
                    }
                }
                let mut out = Vec::new();
                encode_bytes_field(1, &locs_msg, &mut out);
                Ok(any("eve.space.location.api.GetTagsResponse", out))
            }),
        );
    }

    {
        let db_gl = Arc::clone(&db);
        router.register_handler(
            "eve.space.tag.api.GetLocationsRequest",
            Arc::new(move |_ctx, _any, msg| {
                let state = load(&db_gl);
                let mut out = Vec::new();
                for ti in repeated(msg, 1) {
                    if let Some(DynamicValue::String(tag_name)) = ti.get(1) {
                        let mut res = Vec::new();
                        let mut tb = Vec::new();
                        encode_string_field(1, tag_name, &mut tb);
                        encode_bytes_field(1, &tb, &mut res);
                        for (lh, tlist) in &state.location_tags {
                            if tlist.contains(tag_name) {
                                encode_bytes_field(2, &hex_decode(lh), &mut res);
                            }
                        }
                        encode_bytes_field(1, &res, &mut out);
                    }
                }
                Ok(any("eve.space.tag.api.GetLocationsResponse", out))
            }),
        );
    }

    {
        let db_tq = Arc::clone(&db);
        router.register_handler(
            "eve.space.tag.api.QueryRequest",
            Arc::new(move |_ctx, _any, msg| {
                let state = load(&db_tq);
                let tag_name = sub(msg, 1)
                    .and_then(|term| sub(term, 2))
                    .and_then(|ti| match ti.get(1) {
                        Some(DynamicValue::String(s)) => Some(s.clone()),
                        _ => None,
                    })
                    .unwrap_or_default();
                let mut out = Vec::new();
                for (lh, tlist) in &state.location_tags {
                    if tlist.contains(&tag_name) {
                        encode_bytes_field(1, &hex_decode(lh), &mut out);
                    }
                }
                Ok(any("eve.space.tag.api.QueryResponse", out))
            }),
        );
    }
}
