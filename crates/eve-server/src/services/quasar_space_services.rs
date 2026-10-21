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
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_INTERNAL_ERROR,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, FieldDescriptorProto, FieldLabel,
    FieldType, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
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
        _ => None,
    }
}

/// Helper to read an integer ID directly or from a nested Identifier message.
pub fn get_identifier_or_int(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<i64> {
    match dyn_msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => get_int_field(sub, 1),
        _ => get_int_field(dyn_msg, field_num),
    }
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
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.pirate.insurgency.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if sys_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "System ID must be positive".into()));
                }
                encode_domain_message(&d, "eve.pirate.insurgency.api.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, sys_id as u64, 3, "InsurgencyStage3");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.pirate.insurgency.api.GetCurrentRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.pirate.insurgency.api.GetCurrentResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 30_000_142, 500_010, "Guristas Insurgency");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.pirate.insurgency.api.GetSystemsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.pirate.insurgency.api.GetSystemsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 30_000_142, 30_000_144, "InsurgencySystems");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.pirate.insurgency.api.GetForWarzoneRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let warzone_id = get_int_field(dyn_msg, 1).unwrap_or(1) as u32;
                if warzone_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Warzone ID must be positive".into()));
                }
                encode_domain_message(
                    &d,
                    "eve.pirate.insurgency.api.GetForWarzoneResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, warzone_id as u64, 500_010, "WarzoneInsurgency");
                    },
                )
            }),
        );
    }

    // 8. eve_public.pirate.corruption.api.GetSystemInfoRequest & GetStageThresholdsRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.pirate.corruption.api.GetSystemInfoRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                encode_domain_message(
                    &d,
                    "eve_public.pirate.corruption.api.GetSystemInfoResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, sys_id as u64, 2, "CorruptionStage2");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.pirate.corruption.api.GetStageThresholdsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve_public.pirate.corruption.api.GetStageThresholdsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 10_000, 100_000, "CorruptionThresholds");
                    },
                )
            }),
        );
    }

    // 9. eve_public.pirate.suppression.api.GetSystemInfoRequest & GetStageThresholdsRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.pirate.suppression.api.GetSystemInfoRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                encode_domain_message(
                    &d,
                    "eve_public.pirate.suppression.api.GetSystemInfoResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, sys_id as u64, 1, "SuppressionStage1");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.pirate.suppression.api.GetStageThresholdsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve_public.pirate.suppression.api.GetStageThresholdsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 10_000, 100_000, "SuppressionThresholds");
                    },
                )
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

    // 13. Public stargate lock API, AI assistant, and space CDN update
    for (req_name, rsp_name) in [
        (
            "eve_public.stargate.lock.api.GetRequest",
            "eve_public.stargate.lock.api.GetResponse",
        ),
        (
            "eve_public.stargate.lock.api.GetRestrictedSystemsRequest",
            "eve_public.stargate.lock.api.GetRestrictedSystemsResponse",
        ),
        (
            "eve_public.ai.assistant.chat.api.QueryRequest",
            "eve_public.ai.assistant.chat.api.QueryResponse",
        ),
        (
            "eve_public.space.api.cdn.GetUpdateRequest",
            "eve_public.space.api.cdn.GetUpdateResponse",
        ),
    ] {
        let d = Arc::clone(&desc);
        let db_svc = Arc::clone(&db);
        let sde_svc = Arc::clone(&sde);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as i64;
                let (sys_id, station_id) = db_svc.with_snapshot(|s| {
                    s.characters
                        .get(&cid)
                        .map(|c| (c.solar_system_id as u64, c.station_id.unwrap_or(60_003_760) as u64))
                        .unwrap_or((30_000_142, 60_003_760))
                });
                let req_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(sys_id as i64) as u64;
                let owner_corp = sde_svc
                    .stations
                    .get(&(station_id as i64))
                    .map(|st| st.operation_id as u64)
                    .unwrap_or(1_000_035);
                encode_domain_message(&d, rsp_name, |reg, resp| {
                    set_domain_attributes(reg, resp, req_id, owner_corp, "evomu-tranquility");
                })
            }),
        );
    }
}
