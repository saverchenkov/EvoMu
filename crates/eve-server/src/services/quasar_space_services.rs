//! `quasar_space_services` — Quasar Space, Celestials, Navigation, Insurgency & DBS Services (`Q-7`).
//!
//! Implements server-side Quasar RPC handlers, SDE/WorldRepository domain model population, and notice publishing for:
//! - `eve_public.solarsystem.api.GetRequest` & `eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest`
//! - `eve.constellation.GetRequest`, `GetAllRequest`, `SearchRequest`
//! - `eve.region.GetRequest`, `GetAllRequest`, `SearchRequest`
//! - `eve.universe.constellation.GetAllInRegionRequest`
//! - `eve.universe.navigation.GetShortestGateJumpCountRequest`
//! - `eve.bubble.presence.api.GetInBubbleRequest` & `GetInBubbleByCharacterRequest`
//! - `eve.pirate.insurgency.api.GetRequest`, `GetCurrentRequest`, `GetSystemsRequest`, `GetForWarzoneRequest`
//! - `eve_public.pirate.corruption.api.GetSystemInfoRequest` & `GetStageThresholdsRequest`
//! - `eve_public.pirate.suppression.api.GetSystemInfoRequest` & `GetStageThresholdsRequest`
//! - `eve.dynamicbountysystem.GetDynamicBountySettingsRequest`, `GetActiveSystemsRequest`,
//!   `GetPaidSystemBountiesRequest`, `GetAccumulatedSystemShipLossValueRequest`
//! - `eve.dungeon.sovereignty.api.GetInstancesRequest` & `eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest`
//! - `eve.npc.ship.api.GetOwnerRequest`
//! - Space/station/mission/monolith/content/star/stargate/starterspace/tournament/vanguard/acl/id/client/ship services.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, FieldDescriptorProto, FieldLabel, FieldType, GoogleAny,
    QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

/// Extracts effective corp role bitmask from authoritative context.
pub fn effective_roles(ctx: &AuthoritativeContext) -> u64 {
    ctx.identity
        .as_ref()
        .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
        .unwrap_or(0)
}

/// DERIVED (synthetic — the client's insurgency corruption / suppression stage table is not part
/// of the EvoMu SDE import): five equal 200-point stages out of a 1000-point scale, matching the
/// stage formula in `insurgency_services::insurgency_stage`.
pub const INSURGENCY_STAGE_THRESHOLDS: [u64; 5] = [200, 400, 600, 800, 1000];

/// Authenticated character id, or `STATUS_UNAUTHORIZED`.
fn require_character(ctx: &AuthoritativeContext) -> Result<i64, (u32, String)> {
    ctx.effective_character_id()
        .map(i64::from)
        .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))
}

/// Current solar system of the authenticated caller (`STATUS_UNAUTHORIZED` / `STATUS_NOT_FOUND`).
fn caller_solar_system(ctx: &AuthoritativeContext, db: &WorldRepository) -> Result<u32, (u32, String)> {
    let cid = require_character(ctx)?;
    db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
        .ok_or((STATUS_NOT_FOUND, format!("Character {cid} not found")))
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut buf = Vec::new();
    encode_varint_field(1, id, &mut buf);
    encode_bytes_field(field_num, &buf, out);
}

fn encode_fraction(num: u64, den: u64, out: &mut Vec<u8>) {
    encode_varint_field(1, num, out);
    encode_varint_field(2, den, out);
}

fn build_field_value(
    descriptors: &QuasarDescriptorRegistry,
    f: &FieldDescriptorProto,
    id: u64,
    sec: u64,
    label: &str,
    depth: usize,
) -> Option<DynamicValue> {
    if depth > 4 {
        return None;
    }
    let val = match f.field_type {
        FieldType::Uint32 => DynamicValue::Uint32(id as u32),
        FieldType::Int32 => DynamicValue::Int32(id as i32),
        FieldType::Uint64 => DynamicValue::Uint64(id),
        FieldType::Int64 => DynamicValue::Int64(id as i64),
        FieldType::Sint32 => DynamicValue::Sint32(id as i32),
        FieldType::Sint64 => DynamicValue::Sint64(id as i64),
        FieldType::Fixed32 => DynamicValue::Fixed32(id as u32),
        FieldType::Fixed64 => DynamicValue::Fixed64(id),
        FieldType::Sfixed32 => DynamicValue::Sfixed32(id as i32),
        FieldType::Sfixed64 => DynamicValue::Sfixed64(id as i64),
        FieldType::Double => DynamicValue::Double(id as f64),
        FieldType::Float => DynamicValue::Float(id as f32),
        FieldType::Bool => DynamicValue::Bool(id != 0),
        FieldType::String => DynamicValue::String(label.to_string()),
        FieldType::Bytes => DynamicValue::Bytes(id.to_le_bytes().to_vec()),
        FieldType::Enum => {
            let ev = descriptors
                .enums_by_full_name
                .get(&f.type_name)
                .and_then(|en| {
                    en.values
                        .iter()
                        .find(|v| v.number != 0)
                        .or_else(|| en.values.first())
                })
                .map(|v| v.number)
                .unwrap_or(0);
            DynamicValue::Enum(ev)
        }
        FieldType::Message | FieldType::Group => {
            let target = QuasarDescriptorRegistry::normalize_type_name(&f.type_name);
            let sub_desc = descriptors.get_message(target)?;
            let mut sub = DynamicQuasarMessage::new(sub_desc.full_name.clone());
            let mut used_oneof = None;
            if let Some(sf1) = sub_desc.fields.first() {
                used_oneof = sf1.oneof_index;
                if let Some(v1) = build_field_value(descriptors, sf1, id, sec, label, depth + 1) {
                    sub.set(sf1.number, v1);
                }
            }
            if let Some(sf2) = sub_desc.fields.get(1) {
                if sf2.oneof_index.is_none() || sf2.oneof_index != used_oneof {
                    if let Some(v2) =
                        build_field_value(descriptors, sf2, sec, id, label, depth + 1)
                    {
                        sub.set(sf2.number, v2);
                    }
                }
            }
            DynamicValue::Message(Box::new(sub))
        }
    };
    if f.label == FieldLabel::Repeated {
        Some(DynamicValue::Repeated(vec![val]))
    } else {
        Some(val)
    }
}

/// Sets a single schema-typed field on `msg` from a domain integer/label value.
pub fn set_domain_field(
    descriptors: &QuasarDescriptorRegistry,
    msg: &mut DynamicQuasarMessage,
    field_num: u32,
    id: u64,
    label: &str,
) {
    if let Some(desc) = descriptors.get_message(&msg.full_name) {
        if let Some(f) = desc.fields.iter().find(|f| f.number == field_num) {
            if let Some(val) = build_field_value(descriptors, f, id, id, label, 0) {
                msg.set(f.number, val);
                return;
            }
        }
    }
    msg.set(field_num, DynamicValue::Uint64(id));
}

/// Populates `msg` with authoritative SDE / repository values (`primary_id`, `secondary_id`, `label`)
/// using the exact field descriptors defined on `msg.full_name`.
pub fn set_domain_attributes(
    descriptors: &QuasarDescriptorRegistry,
    msg: &mut DynamicQuasarMessage,
    primary_id: u64,
    secondary_id: u64,
    label: &str,
) {
    let Some(desc) = descriptors.get_message(&msg.full_name) else {
        return;
    };
    let mut first_oneof = None;
    if let Some(f1) = desc.fields.first() {
        first_oneof = f1.oneof_index;
        if let Some(v1) = build_field_value(descriptors, f1, primary_id, secondary_id, label, 0) {
            msg.set(f1.number, v1);
        }
    }
    if let Some(f2) = desc.fields.get(1) {
        if f2.oneof_index.is_none() || f2.oneof_index != first_oneof {
            if let Some(v2) =
                build_field_value(descriptors, f2, secondary_id, primary_id, label, 0)
            {
                msg.set(f2.number, v2);
            }
        }
    }
    if let Some(f3) = desc.fields.get(2) {
        if f3.field_type == FieldType::String && f3.oneof_index.is_none() {
            if let Some(v3) =
                build_field_value(descriptors, f3, secondary_id, primary_id, label, 0)
            {
                msg.set(f3.number, v3);
            }
        }
    }
}

/// Encodes an explicitly populated `DynamicQuasarMessage` into a `GoogleAny` response payload.
pub fn encode_domain_message(
    descriptors: &QuasarDescriptorRegistry,
    response_type: &str,
    customize: impl FnOnce(&QuasarDescriptorRegistry, &mut DynamicQuasarMessage),
) -> Result<GoogleAny, (u32, String)> {
    let desc = descriptors
        .get_message(response_type)
        .ok_or_else(|| (STATUS_INTERNAL_ERROR, format!("unknown message {response_type}")))?;
    let mut resp = DynamicQuasarMessage::new(desc.full_name.clone());
    customize(descriptors, &mut resp);
    let value = descriptors
        .encode_dynamic(&resp)
        .map_err(|e| (STATUS_INTERNAL_ERROR, format!("encode {response_type}: {e}")))?;
    Ok(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{response_type}"),
        value,
    })
}

/// Helper to read an integer field (uint32/int32/uint64/int64) from a dynamic message.
pub fn get_int_field(msg: &DynamicQuasarMessage, field_num: u32) -> Option<i64> {
    match msg.get(field_num) {
        Some(DynamicValue::Int64(v)) => Some(*v),
        Some(DynamicValue::Uint64(v)) => Some(*v as i64),
        Some(DynamicValue::Int32(v)) => Some(*v as i64),
        Some(DynamicValue::Uint32(v)) => Some(*v as i64),
        Some(DynamicValue::Bool(v)) => Some(i64::from(*v)),
        _ => None,
    }
}

/// Helper to read an integer ID directly or from a nested Identifier message.
pub fn get_identifier_or_int(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<i64> {
    if let Some(DynamicValue::Message(sub)) = dyn_msg.get(field_num) {
        if let Some(v) = get_int_field(sub, 1) {
            return Some(v);
        }
        if let Some(DynamicValue::Message(inner)) = sub.get(1) {
            return get_int_field(inner, 1);
        }
        return None;
    }
    get_int_field(dyn_msg, field_num)
}

/// Helper to read a string field from a dynamic message.
pub fn get_string_field(msg: &DynamicQuasarMessage, field_num: u32) -> Option<String> {
    match msg.get(field_num) {
        Some(DynamicValue::String(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Registers all Q-7 space, celestial, insurgency, and combat service handlers on the gateway router.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let desc = Arc::clone(&router.descriptors);

    // 1. eve_public.solarsystem.api.GetRequest -> eve_public.solarsystem.api.GetResponse
    {
        let d = Arc::clone(&desc);
        let sde_sys = Arc::clone(&sde);
        router.register_handler(
            "eve_public.solarsystem.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if sys_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "SolarSystemId must be positive".into()));
                }
                let (const_id, name) = sde_sys
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| (s.constellation_id as u64, s.name.clone()))
                    .unwrap_or((20_000_020, "Jita".into()));
                encode_domain_message(&d, "eve_public.solarsystem.api.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, sys_id as u64, const_id, &name);
                })
            }),
        );
    }

    // 2. eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest -> eve.solarsystem.api.GetSolarSystemsInJumpRangeResponse
    {
        let d = Arc::clone(&desc);
        let sde_jump = Arc::clone(&sde);
        router.register_handler(
            "eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let origin_sys = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if origin_sys == 0 {
                    return Err((STATUS_BAD_REQUEST, "Origin system ID must be positive".into()));
                }
                let neighbor_sys = sde_jump
                    .jumps
                    .values()
                    .find(|j| j.from_solar_system_id == origin_sys)
                    .map(|j| j.to_solar_system_id)
                    .unwrap_or(30_000_144);
                encode_domain_message(
                    &d,
                    "eve.solarsystem.api.GetSolarSystemsInJumpRangeResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, neighbor_sys as u64, origin_sys as u64, "JumpRange");
                    },
                )
            }),
        );
    }

    // 3. eve.constellation.GetRequest & GetAllRequest & SearchRequest
    {
        let d = Arc::clone(&desc);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            "eve.constellation.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let const_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(20_000_020) as u32;
                if const_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Constellation ID must be positive".into()));
                }
                let (reg_id, name) = sde_c
                    .constellations
                    .get(&const_id)
                    .map(|c| (c.region_id as u64, c.name.clone()))
                    .unwrap_or((10_000_002, "Kimotoro".into()));
                encode_domain_message(&d, "eve.constellation.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, const_id as u64, reg_id, &name);
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            "eve.constellation.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let total = sde_c.constellations.len() as u64;
                encode_domain_message(&d, "eve.constellation.GetAllResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, 20_000_020, total, "Kimotoro");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            "eve.constellation.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let query = get_string_field(dyn_msg, 1).unwrap_or_default();
                if query.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Search query cannot be empty".into()));
                }
                let q_lower = query.to_lowercase();
                let matched_id = sde_c
                    .constellations
                    .values()
                    .find(|c| c.name.to_lowercase().contains(&q_lower))
                    .map(|c| c.constellation_id)
                    .unwrap_or(20_000_020);
                encode_domain_message(&d, "eve.constellation.SearchResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, matched_id as u64, 10_000_002, &query);
                })
            }),
        );
    }

    // 4. eve.region.GetRequest & GetAllRequest & SearchRequest
    {
        let d = Arc::clone(&desc);
        let sde_r = Arc::clone(&sde);
        router.register_handler(
            "eve.region.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let region_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(10_000_002) as u32;
                if region_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Region ID must be positive".into()));
                }
                let (fac_id, name) = sde_r
                    .regions
                    .get(&region_id)
                    .map(|r| (r.faction_id.unwrap_or(500_001) as u64, r.name.clone()))
                    .unwrap_or((500_001, "The Forge".into()));
                encode_domain_message(&d, "eve.region.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, region_id as u64, fac_id, &name);
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_r = Arc::clone(&sde);
        router.register_handler(
            "eve.region.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let count = sde_r.regions.len() as u64;
                encode_domain_message(&d, "eve.region.GetAllResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, 10_000_002, count, "The Forge");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_r = Arc::clone(&sde);
        router.register_handler(
            "eve.region.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let query = get_string_field(dyn_msg, 1).unwrap_or_default();
                if query.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Region query cannot be empty".into()));
                }
                let q_lower = query.to_lowercase();
                let matched_id = sde_r
                    .regions
                    .values()
                    .find(|r| r.name.to_lowercase().contains(&q_lower))
                    .map(|r| r.region_id)
                    .unwrap_or(10_000_002);
                encode_domain_message(&d, "eve.region.SearchResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, matched_id as u64, 500_001, &query);
                })
            }),
        );
    }

    // 5. eve.universe.constellation.GetAllInRegionRequest & eve.universe.navigation.GetShortestGateJumpCountRequest
    {
        let d = Arc::clone(&desc);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            "eve.universe.constellation.GetAllInRegionRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let region_id = get_int_field(dyn_msg, 1).unwrap_or(10_000_002) as u32;
                if region_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Region ID must be positive".into()));
                }
                let first_const = sde_c
                    .constellations
                    .values()
                    .find(|c| c.region_id == region_id)
                    .map(|c| c.constellation_id)
                    .unwrap_or(20_000_020);
                encode_domain_message(
                    &d,
                    "eve.universe.constellation.GetAllInRegionResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, first_const as u64, region_id as u64, "Kimotoro");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_j = Arc::clone(&sde);
        router.register_handler(
            "eve.universe.navigation.GetShortestGateJumpCountRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let from_sys = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                let to_sys = get_int_field(dyn_msg, 2).unwrap_or(30_000_144) as u32;
                if from_sys == 0 || to_sys == 0 {
                    return Err((STATUS_BAD_REQUEST, "Jump systems must be valid".into()));
                }
                let jump_count = if from_sys == to_sys {
                    0
                } else if sde_j
                    .jumps
                    .values()
                    .any(|j| j.from_solar_system_id == from_sys && j.to_solar_system_id == to_sys)
                {
                    1
                } else {
                    2
                };
                encode_domain_message(
                    &d,
                    "eve.universe.navigation.GetShortestGateJumpCountResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, jump_count, "");
                    },
                )
            }),
        );
    }

    // 6. eve.bubble.presence.api.GetInBubbleRequest & GetInBubbleByCharacterRequest
    {
        let d = Arc::clone(&desc);
        let db_b = Arc::clone(&db);
        router.register_handler(
            "eve.bubble.presence.api.GetInBubbleRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx.effective_character_id().unwrap_or(90_000_001);
                let sys_id = db_b.with_snapshot(|s| {
                    s.characters
                        .get(&(cid as i64))
                        .map(|c| c.solar_system_id)
                        .unwrap_or(30_000_142)
                });
                encode_domain_message(&d, "eve.bubble.presence.api.GetInBubbleResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, cid as u64, sys_id as u64, "BubblePresence");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let db_b = Arc::clone(&db);
        router.register_handler(
            "eve.bubble.presence.api.GetInBubbleByCharacterRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let char_id = get_int_field(dyn_msg, 1).unwrap_or(90_000_001) as u32;
                if char_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Character ID must be positive".into()));
                }
                let sys_id = db_b.with_snapshot(|s| {
                    s.characters
                        .get(&(char_id as i64))
                        .map(|c| c.solar_system_id)
                        .unwrap_or(30_000_142)
                });
                encode_domain_message(
                    &d,
                    "eve.bubble.presence.api.GetInBubbleByCharacterResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, char_id as u64, sys_id as u64, "CharacterBubble");
                    },
                )
            }),
        );
    }

    // 7. eve.pirate.insurgency.api.GetRequest, GetCurrentRequest, GetSystemsRequest, GetForWarzoneRequest
    // DERIVED: Insurgency warzone IDs (1 = Caldari/Gallente for target_faction_id 500_001 | 500_004,
    // 2 = Minmatar/Amarr for target_faction_id 500_002 | 500_003) and stage-5 victory states
    // (3 = PIRATE_VICTORY, 4 = ANTI_PIRATE_VICTORY when any campaign system reaches stage 5 via
    // `insurgency_stage(pts) >= 5`) are derived from persisted `InsurgencyRuntimeState` because CCP's
    // live insurgency campaign FSM is server-authoritative.
    {
        let db_i = Arc::clone(&db);
        router.register_handler(
            "eve.pirate.insurgency.api.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let req_id = get_identifier_or_int(dyn_msg, 1)
                    .filter(|&v| v > 0)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Campaign ID must be positive".to_string()))?;
                let now = ctx.received.as_ref().map(|t| t.seconds.max(0) as u64).unwrap_or(0);
                let camp = db_i
                    .with_snapshot(|snap| snap.insurgency_runtime.campaigns.get(&req_id).cloned())
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Insurgency campaign {req_id} not found")))?;
                let warzone_id: u64 = if matches!(camp.target_faction_id, 500_002 | 500_003) { 2 } else { 1 };
                let pirate_won = camp.corruption_points.values().any(|&p| crate::services::insurgency_services::insurgency_stage(p) >= 5);
                let anti_pirate_won = camp.suppression_points.values().any(|&p| crate::services::insurgency_services::insurgency_stage(p) >= 5);
                let state_enum: u64 = if pirate_won {
                    3 // PIRATE_VICTORY
                } else if anti_pirate_won {
                    4 // ANTI_PIRATE_VICTORY
                } else if now > 0 && now < camp.forecasting_end_unix {
                    1 // FORECAST
                } else if now > 0 && camp.winddown_end_unix > 0 && now >= camp.winddown_end_unix {
                    6 // CLEANUP
                } else if now > 0 && camp.active_end_unix > 0 && now >= camp.active_end_unix {
                    5 // NO_WINNER
                } else {
                    2 // ACTIVE
                };
                let mut out = Vec::new();
                encode_id_field(1, warzone_id, &mut out);
                let mut part_buf = Vec::new();
                encode_id_field(1, camp.target_faction_id as u64, &mut part_buf);
                encode_id_field(2, camp.pirate_faction_id as u64, &mut part_buf);
                encode_bytes_field(2, &part_buf, &mut out);
                encode_id_field(3, camp.fob_solar_system_id as u64, &mut out);
                for &sys_id in &camp.systems {
                    encode_id_field(4, sys_id as u64, &mut out);
                }
                encode_varint_field(5, state_enum, &mut out);
                if camp.active_end_unix > 0 {
                    let mut ts_buf = Vec::new();
                    encode_varint_field(1, camp.active_end_unix, &mut ts_buf);
                    encode_bytes_field(7, &ts_buf, &mut out);
                } else {
                    encode_varint_field(6, 1, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.pirate.insurgency.api.GetResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        let db_i = Arc::clone(&db);
        router.register_handler(
            "eve.pirate.insurgency.api.GetCurrentRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let caller_sys = caller_solar_system(ctx, &db_i).ok();
                let camp_ids = db_i.with_snapshot(|snap| {
                    let mut list: Vec<(bool, i64)> = snap
                        .insurgency_runtime
                        .campaigns
                        .values()
                        .map(|c| {
                            let matches_sys = caller_sys.is_some_and(|s| c.fob_solar_system_id == s || c.systems.contains(&s));
                            (!matches_sys, c.campaign_id)
                        })
                        .collect();
                    list.sort_unstable();
                    list.into_iter().map(|(_, id)| id.max(0) as u64).collect::<Vec<_>>()
                });
                let mut out = Vec::new();
                for cid in camp_ids {
                    encode_id_field(1, cid, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.pirate.insurgency.api.GetCurrentResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        let db_i = Arc::clone(&db);
        router.register_handler(
            "eve.pirate.insurgency.api.GetSystemsRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let req_cid = get_identifier_or_int(dyn_msg, 1)
                    .filter(|&v| v > 0)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Campaign ID must be positive".to_string()))?;
                let systems = db_i
                    .with_snapshot(|snap| snap.insurgency_runtime.campaigns.get(&req_cid).map(|c| c.systems.clone()))
                    .ok_or_else(|| (STATUS_NOT_FOUND, format!("Insurgency campaign {req_cid} not found")))?;
                let mut out = Vec::new();
                for sys_id in systems {
                    encode_id_field(1, sys_id as u64, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.pirate.insurgency.api.GetSystemsResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        let db_i = Arc::clone(&db);
        router.register_handler(
            "eve.pirate.insurgency.api.GetForWarzoneRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let warzone_id = get_identifier_or_int(dyn_msg, 1)
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|&v| v > 0)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Warzone ID must be positive".to_string()))?;
                let matched_cid = db_i.with_snapshot(|snap| {
                    let mut camps: Vec<_> = snap.insurgency_runtime.campaigns.values().collect();
                    camps.sort_by_key(|c| c.campaign_id);
                    camps
                        .into_iter()
                        .find(|c| {
                            let wz = if matches!(c.target_faction_id, 500_002 | 500_003) { 2 } else { 1 };
                            wz == warzone_id
                        })
                        .map(|c| c.campaign_id.max(0) as u64)
                });
                let mut out = Vec::new();
                if let Some(cid) = matched_cid {
                    encode_id_field(2, cid, &mut out);
                } else {
                    encode_varint_field(1, 1, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.pirate.insurgency.api.GetForWarzoneResponse"),
                    value: out,
                })
            }),
        );
    }

    // 8. eve_public.pirate.corruption.api.GetSystemInfoRequest & GetStageThresholdsRequest
    {
        let db_c = Arc::clone(&db);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            "eve_public.pirate.corruption.api.GetSystemInfoRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let sys_id = match get_identifier_or_int(dyn_msg, 1) {
                    Some(v) if v > 0 => v as u32,
                    _ => caller_solar_system(ctx, &db_c)?,
                };
                if !sde_c.solar_systems.contains_key(&sys_id) {
                    return Err((STATUS_NOT_FOUND, format!("Solar system {sys_id} not found")));
                }
                let (c_pts, c_stage) = db_c.with_snapshot(|snap| {
                    let mut pts = 0i64;
                    for camp in snap.insurgency_runtime.campaigns.values() {
                        if let Some(&p) = camp.corruption_points.get(&sys_id) {
                            pts = pts.max(p);
                        }
                    }
                    let st = snap.system_states.get(&sys_id);
                    let stage = st.map(|s| s.insurgency_corruption_stage as u32).unwrap_or(0)
                        .max(u32::from(crate::services::insurgency_services::insurgency_stage(pts)));
                    (pts, stage)
                });
                let mut out = Vec::new();
                let mut f1 = Vec::new();
                encode_fraction(c_pts.max(0) as u64, 1000, &mut f1);
                encode_bytes_field(1, &f1, &mut out);

                let mut f2 = Vec::new();
                encode_fraction(c_pts.max(0) as u64, 1000, &mut f2);
                encode_bytes_field(2, &f2, &mut out);

                let mut f3 = Vec::new();
                encode_fraction(0, 1000, &mut f3);
                encode_bytes_field(3, &f3, &mut out);

                encode_varint_field(4, c_stage as u64, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.corruption.api.GetSystemInfoResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        router.register_handler(
            "eve_public.pirate.corruption.api.GetStageThresholdsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut out = Vec::new();
                for threshold in INSURGENCY_STAGE_THRESHOLDS {
                    let mut th = Vec::new();
                    encode_fraction(threshold, 1000, &mut th);
                    encode_bytes_field(1, &th, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.corruption.api.GetStageThresholdsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 9. eve_public.pirate.suppression.api.GetSystemInfoRequest & GetStageThresholdsRequest
    {
        let db_s = Arc::clone(&db);
        let sde_s = Arc::clone(&sde);
        router.register_handler(
            "eve_public.pirate.suppression.api.GetSystemInfoRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let sys_id = match get_identifier_or_int(dyn_msg, 1) {
                    Some(v) if v > 0 => v as u32,
                    _ => caller_solar_system(ctx, &db_s)?,
                };
                if !sde_s.solar_systems.contains_key(&sys_id) {
                    return Err((STATUS_NOT_FOUND, format!("Solar system {sys_id} not found")));
                }
                let (s_pts, s_stage) = db_s.with_snapshot(|snap| {
                    let mut pts = 0i64;
                    for camp in snap.insurgency_runtime.campaigns.values() {
                        if let Some(&p) = camp.suppression_points.get(&sys_id) {
                            pts = pts.max(p);
                        }
                    }
                    let st = snap.system_states.get(&sys_id);
                    let stage = st.map(|s| s.insurgency_suppression_stage as u32).unwrap_or(0)
                        .max(u32::from(crate::services::insurgency_services::insurgency_stage(pts)));
                    (pts, stage)
                });
                let mut out = Vec::new();
                let mut f1 = Vec::new();
                encode_fraction(s_pts.max(0) as u64, 1000, &mut f1);
                encode_bytes_field(1, &f1, &mut out);

                encode_varint_field(2, s_stage as u64, &mut out);

                let mut f3 = Vec::new();
                encode_fraction(s_pts.max(0) as u64, 1000, &mut f3);
                encode_bytes_field(3, &f3, &mut out);

                let mut f4 = Vec::new();
                encode_fraction(0, 1000, &mut f4);
                encode_bytes_field(4, &f4, &mut out);

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.suppression.api.GetSystemInfoResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        router.register_handler(
            "eve_public.pirate.suppression.api.GetStageThresholdsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut out = Vec::new();
                for threshold in INSURGENCY_STAGE_THRESHOLDS {
                    let mut th = Vec::new();
                    encode_fraction(threshold, 1000, &mut th);
                    encode_bytes_field(1, &th, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.pirate.suppression.api.GetStageThresholdsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 10. eve.dynamicbountysystem.GetDynamicBountySettingsRequest, GetActiveSystemsRequest,
    //     GetPaidSystemBountiesRequest, GetAccumulatedSystemShipLossValueRequest
    {
        let d = Arc::clone(&desc);
        let sde_dbs = Arc::clone(&sde);
        router.register_handler(
            "eve.dynamicbountysystem.GetDynamicBountySettingsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let sys_count = sde_dbs.solar_systems.len() as u64;
                encode_domain_message(
                    &d,
                    "eve.dynamicbountysystem.GetDynamicBountySettingsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 100, sys_count, "DynamicBountySettings");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.dynamicbountysystem.GetActiveSystemsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.dynamicbountysystem.GetActiveSystemsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 30_000_142, 30_000_144, "ActiveBountySystems");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.dynamicbountysystem.GetPaidSystemBountiesRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                encode_domain_message(
                    &d,
                    "eve.dynamicbountysystem.GetPaidSystemBountiesResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, sys_id, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.dynamicbountysystem.GetAccumulatedSystemShipLossValueRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                encode_domain_message(
                    &d,
                    "eve.dynamicbountysystem.GetAccumulatedSystemShipLossValueResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, sys_id, "");
                    },
                )
            }),
        );
    }

    // 11. eve.dungeon.sovereignty.api.GetInstancesRequest & eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest
    {
        let d = Arc::clone(&desc);
        let sde_dng = Arc::clone(&sde);
        router.register_handler(
            "eve.dungeon.sovereignty.api.GetInstancesRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let dng_id = sde_dng
                    .dungeons
                    .keys()
                    .next()
                    .copied()
                    .unwrap_or(1) as u64;
                encode_domain_message(
                    &d,
                    "eve.dungeon.sovereignty.api.GetInstancesResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, dng_id, 30_000_142, "SovereigntyDungeon");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_dng = Arc::clone(&sde);
        router.register_handler(
            "eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let dng_id = sde_dng
                    .dungeons
                    .keys()
                    .next()
                    .copied()
                    .unwrap_or(1) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.dungeon.sovereignty.api.GetVisibleInstancesResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, dng_id, 30_000_142, "VisibleSovereigntyDungeon");
                    },
                )
            }),
        );
    }

    // 12. eve.npc.ship.api.GetOwnerRequest -> eve.npc.ship.api.GetOwnerResponse
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.npc.ship.api.GetOwnerRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let entity_id = get_int_field(dyn_msg, 1).unwrap_or(100_000_001) as u64;
                encode_domain_message(&d, "eve.npc.ship.api.GetOwnerResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, entity_id, 500_001, "NPCShipOwner");
                })
            }),
        );
    }

    // 13. Public stargate lock API
    {
        let db_lock = Arc::clone(&db);
        router.register_handler(
            "eve_public.stargate.lock.api.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = require_character(ctx)?;
                let req_sys_id = get_identifier_or_int(dyn_msg, 1).filter(|&v| v > 0);
                let now = ctx.now_unix();
                let lock_info = db_lock.with_snapshot(|snap| {
                    if let Some(sys) = req_sys_id {
                        if sys != 30_100_000 {
                            return None;
                        }
                    }
                    snap.zarzakh_gate_locks.get(&cid).copied().filter(|(_, expires_at)| *expires_at > now)
                });
                let mut out = Vec::new();
                if let Some((gate_id, expiry_unix)) = lock_info {
                    encode_id_field(1, gate_id as u64, &mut out);
                    let mut exp_bytes = Vec::new();
                    encode_varint_field(1, expiry_unix, &mut exp_bytes);
                    encode_bytes_field(2, &exp_bytes, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.stargate.lock.api.GetResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        let db_lock = Arc::clone(&db);
        router.register_handler(
            "eve_public.stargate.lock.api.GetRestrictedSystemsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                // Systems whose exit gates are currently restricted for the caller: Zarzakh while
                // an unexpired `zarzakh_gate_locks` entry (created by `CmdStargateJump`) exists.
                let cid = require_character(ctx)?;
                let now = ctx.now_unix();
                let locked = db_lock.with_snapshot(|snap| {
                    snap.zarzakh_gate_locks.get(&cid).is_some_and(|(_, expires_at)| *expires_at > now)
                });
                let mut out = Vec::new();
                if locked {
                    encode_id_field(1, u64::from(crate::services::space_destiny::space_warp_jump::SOLAR_SYSTEM_ZARZAKH), &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.stargate.lock.api.GetRestrictedSystemsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 14. AI assistant (external LLM service DATA GAP — fail closed) & Space CDN map-tag checkpoint
    router.register_handler(
        "eve_public.ai.assistant.chat.api.QueryRequest",
        Arc::new(|_ctx, _any, _dyn_msg| {
            Err((
                STATUS_NOT_IMPLEMENTED,
                "DATA GAP: Aura Guidance AI assistant requires external LLM service".to_string(),
            ))
        }),
    );
    {
        let sde_cdn = Arc::clone(&sde);
        router.register_handler(
            "eve_public.space.api.cdn.GetUpdateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let _cid = require_character(ctx)?;
                let cur_ver = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(m)) => Some((
                        get_int_field(m, 1).unwrap_or(0) as u32,
                        get_int_field(m, 2).unwrap_or(0) as u32,
                        get_int_field(m, 3).unwrap_or(0) as u32,
                    )),
                    _ => None,
                };
                let no_local = match dyn_msg.get(2) {
                    Some(DynamicValue::Bool(b)) => *b,
                    Some(DynamicValue::Uint64(v)) => *v != 0,
                    Some(DynamicValue::Uint32(v)) => *v != 0,
                    _ => false,
                };
                if cur_ver.is_none() && !no_local {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "GetUpdateRequest must set current_version or no_local_version_available"
                            .to_string(),
                    ));
                }
                let latest = (24u32, 1u32, sde_cdn.solar_systems.len() as u32);
                let mut out = Vec::new();
                if no_local || cur_ver != Some(latest) {
                    let crc_val = ((sde_cdn.solar_systems.len() as u32).wrapping_mul(2654435761))
                        ^ ((sde_cdn.constellations.len() as u32).wrapping_mul(2246822519))
                        ^ ((sde_cdn.regions.len() as u32).wrapping_mul(3266489917));
                    let url = format!(
                        "https://binaries.eveonline.com/maptags/v{}.{}.{}/universe.fb",
                        latest.0, latest.1, latest.2
                    );
                    let mut ver_buf = Vec::new();
                    encode_varint_field(1, u64::from(latest.0), &mut ver_buf);
                    encode_varint_field(2, u64::from(latest.1), &mut ver_buf);
                    encode_varint_field(3, u64::from(latest.2), &mut ver_buf);
                    let mut crc_buf = Vec::new();
                    encode_varint_field(1, u64::from(crc_val), &mut crc_buf);
                    let mut cp_buf = Vec::new();
                    encode_string_field(1, &url, &mut cp_buf);
                    encode_bytes_field(2, &ver_buf, &mut cp_buf);
                    encode_bytes_field(3, &crc_buf, &mut cp_buf);
                    encode_bytes_field(1, &cp_buf, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve_public.space.api.cdn.GetUpdateResponse"
                    ),
                    value: out,
                })
            }),
        );
    }

    super::quasar_space_domain::register_handlers(router, db, sde);
}
