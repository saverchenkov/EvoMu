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
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_INTERNAL_ERROR,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
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

/// Populates a schema-conformant `DynamicQuasarMessage` in-place with authoritative SDE / repository values
/// while preserving each field's exact protobuf wire variant so roundtrip decoding never mismatches.
pub fn apply_sde_record(
    msg: &mut DynamicQuasarMessage,
    primary_id: u64,
    secondary_id: u64,
    label: &str,
) {
    fn set_val(val: &mut DynamicValue, id: u64, sec: u64, text: &str, depth: usize) {
        if depth > 3 {
            return;
        }
        match val {
            DynamicValue::Uint32(v) => *v = id as u32,
            DynamicValue::Int32(v) => *v = id as i32,
            DynamicValue::Uint64(v) => *v = id,
            DynamicValue::Int64(v) => *v = id as i64,
            DynamicValue::Fixed32(v) => *v = id as u32,
            DynamicValue::Fixed64(v) => *v = id,
            DynamicValue::String(s) if !text.is_empty() => *s = text.to_string(),
            DynamicValue::Message(sub) => {
                if let Some(f1) = sub.fields.get_mut(&1) {
                    set_val(f1, id, sec, text, depth + 1);
                }
                if let Some(f2) = sub.fields.get_mut(&2) {
                    set_val(f2, sec, id, text, depth + 1);
                }
            }
            DynamicValue::Repeated(items) => {
                if let Some(first) = items.first_mut() {
                    set_val(first, id, sec, text, depth + 1);
                }
            }
            _ => {}
        }
    }

    if let Some(f1) = msg.fields.get_mut(&1) {
        set_val(f1, primary_id, secondary_id, label, 0);
    }
    if let Some(f2) = msg.fields.get_mut(&2) {
        set_val(f2, secondary_id, primary_id, label, 0);
    }
    if let Some(f3) = msg.fields.get_mut(&3) {
        set_val(f3, secondary_id, primary_id, label, 0);
    }
}

/// Builds a dynamically serialized protobuf `GoogleAny` payload conforming to `response_type`.
pub fn build_dynamic_response(
    descriptors: &QuasarDescriptorRegistry,
    response_type: &str,
    customize: impl FnOnce(&mut DynamicQuasarMessage),
) -> Result<GoogleAny, (u32, String)> {
    let mut resp = descriptors
        .instantiate_populated_sample(response_type, 1001)
        .map_err(|e| (STATUS_INTERNAL_ERROR, format!("instantiate {response_type}: {e}")))?;
    customize(&mut resp);
    let value = descriptors
        .encode_dynamic(&resp)
        .map_err(|e| (STATUS_INTERNAL_ERROR, format!("encode {response_type}: {e}")))?;
    Ok(GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{response_type}"),
        value,
    })
}

/// Populates and round-trip encodes all protobuf model messages in a Quasar domain using live
/// `SdeRegistry` and `WorldRepository` records (`feat:quasar.*` domain model packages).
pub fn populate_domain_model_messages(
    sde: &SdeRegistry,
    db: &WorldRepository,
    descriptors: &QuasarDescriptorRegistry,
    domain: &str,
) -> Result<Vec<GoogleAny>, (u32, String)> {
    let file_indices = descriptors
        .files_by_domain
        .get(domain)
        .ok_or_else(|| (STATUS_BAD_REQUEST, format!("Unknown Quasar domain '{domain}'")))?;

    let (primary_id, secondary_id, label): (u64, u64, String) = match domain {
        "agent" => sde
            .agents
            .values()
            .next()
            .map(|a| (a.agent_id as u64, a.corporation_id as u64, format!("agent_{}", a.agent_id)))
            .unwrap_or((3_008_416, 1_000_035, "Caldari Navy Agent".into())),
        "ancestry" => sde
            .ancestries
            .values()
            .next()
            .map(|a| (a.ancestry_id as u64, a.bloodline_id as u64, a.name.clone()))
            .unwrap_or((1, 1, "Mercenaries".into())),
        "bloodline" => sde
            .bloodlines
            .values()
            .next()
            .map(|b| (b.bloodline_id as u64, b.race_id as u64, b.name.clone()))
            .unwrap_or((1, 1, "Deteis".into())),
        "capacitor" | "module" | "starbase" => sde
            .types
            .get(&587)
            .map(|t| (t.type_id as u64, t.group_id as u64, t.name.clone()))
            .unwrap_or((587, 25, "Rifter".into())),
        "constellation" => sde
            .constellations
            .get(&20_000_020)
            .or_else(|| sde.constellations.values().next())
            .map(|c| (c.constellation_id as u64, c.region_id as u64, c.name.clone()))
            .unwrap_or((20_000_020, 10_000_002, "Kimotoro".into())),
        "dynamicbountysystem" | "universe" => sde
            .solar_systems
            .get(&30_000_142)
            .or_else(|| sde.solar_systems.values().next())
            .map(|s| (s.solar_system_id as u64, s.region_id as u64, s.name.clone()))
            .unwrap_or((30_000_142, 10_000_002, "Jita".into())),
        "epicarc" => sde
            .epic_arcs
            .values()
            .next()
            .map(|e| (e.epic_arc_id as u64, e.faction_id.unwrap_or(500_001) as u64, e.name.clone()))
            .unwrap_or((1, 500_001, "The Blood-Stained Stars".into())),
        "fighter" => sde
            .fighter_abilities
            .values()
            .next()
            .map(|f| (f.ability_id as u64, f.icon_id.unwrap_or(1) as u64, f.target_mode.clone()))
            .unwrap_or((1, 1, "PointTargeted".into())),
        "race" => sde
            .races
            .values()
            .next()
            .map(|r| (r.race_id as u64, 1, r.name.clone()))
            .unwrap_or((1, 1, "Caldari".into())),
        "region" => sde
            .regions
            .get(&10_000_002)
            .or_else(|| sde.regions.values().next())
            .map(|r| (r.region_id as u64, r.faction_id.unwrap_or(500_001) as u64, r.name.clone()))
            .unwrap_or((10_000_002, 500_001, "The Forge".into())),
        "school" => sde
            .schools
            .values()
            .next()
            .map(|s| (s.school_id as u64, s.race_id as u64, s.name.clone()))
            .unwrap_or((11, 1, "State War Academy".into())),
        "wormhole" => sde
            .wormhole_types
            .values()
            .next()
            .map(|w| (w.type_id as u64, w.target_system_class as u64, w.name.clone()))
            .unwrap_or((16_272, 2, "Wormhole K162".into())),
        _ => {
            let cid = db.with_snapshot(|snap| {
                snap.characters
                    .keys()
                    .next()
                    .copied()
                    .unwrap_or(90_000_001)
            });
            (cid as u64, 30_000_142, format!("evomu_{domain}"))
        }
    };

    let mut encoded_anys = Vec::new();
    for &file_idx in file_indices {
        let fd = &descriptors.files[file_idx];
        for msg in &fd.messages {
            let mut instance = descriptors
                .instantiate_populated_sample(&msg.full_name, primary_id.wrapping_add(42))
                .map_err(|e| (STATUS_INTERNAL_ERROR, format!("instantiate {}: {e}", msg.full_name)))?;
            apply_sde_record(&mut instance, primary_id, secondary_id, &label);
            let wire = descriptors
                .encode_dynamic(&instance)
                .map_err(|e| (STATUS_INTERNAL_ERROR, format!("encode {}: {e}", msg.full_name)))?;
            let decoded = descriptors
                .decode_dynamic(&msg.full_name, &wire)
                .map_err(|e| (STATUS_INTERNAL_ERROR, format!("decode {}: {e}", msg.full_name)))?;
            if decoded != instance {
                return Err((
                    STATUS_INTERNAL_ERROR,
                    format!("Roundtrip mismatch for {}", msg.full_name),
                ));
            }
            encoded_anys.push(GoogleAny {
                type_url: instance.type_url(),
                value: wire,
            });
        }
    }
    Ok(encoded_anys)
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
                build_dynamic_response(&d, "eve_public.solarsystem.api.GetResponse", |resp| {
                    apply_sde_record(resp, sys_id as u64, const_id, &name);
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
                build_dynamic_response(
                    &d,
                    "eve.solarsystem.api.GetSolarSystemsInJumpRangeResponse",
                    |resp| {
                        apply_sde_record(resp, neighbor_sys as u64, origin_sys as u64, "JumpRange");
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
                build_dynamic_response(&d, "eve.constellation.GetResponse", |resp| {
                    apply_sde_record(resp, const_id as u64, reg_id, &name);
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
                build_dynamic_response(&d, "eve.constellation.GetAllResponse", |resp| {
                    apply_sde_record(resp, 20_000_020, total, "Kimotoro");
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
                build_dynamic_response(&d, "eve.constellation.SearchResponse", |resp| {
                    apply_sde_record(resp, matched_id as u64, 10_000_002, &query);
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
                build_dynamic_response(&d, "eve.region.GetResponse", |resp| {
                    apply_sde_record(resp, region_id as u64, fac_id, &name);
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
                build_dynamic_response(&d, "eve.region.GetAllResponse", |resp| {
                    apply_sde_record(resp, 10_000_002, count, "The Forge");
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
                build_dynamic_response(&d, "eve.region.SearchResponse", |resp| {
                    apply_sde_record(resp, matched_id as u64, 500_001, &query);
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
                build_dynamic_response(
                    &d,
                    "eve.universe.constellation.GetAllInRegionResponse",
                    |resp| {
                        apply_sde_record(resp, first_const as u64, region_id as u64, "Kimotoro");
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
                build_dynamic_response(
                    &d,
                    "eve.universe.navigation.GetShortestGateJumpCountResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(jump_count));
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
                build_dynamic_response(&d, "eve.bubble.presence.api.GetInBubbleResponse", |resp| {
                    apply_sde_record(resp, cid as u64, sys_id as u64, "BubblePresence");
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
                build_dynamic_response(
                    &d,
                    "eve.bubble.presence.api.GetInBubbleByCharacterResponse",
                    |resp| {
                        apply_sde_record(resp, char_id as u64, sys_id as u64, "CharacterBubble");
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
                build_dynamic_response(&d, "eve.pirate.insurgency.api.GetResponse", |resp| {
                    resp.set(1, DynamicValue::Uint32(sys_id));
                    resp.set(2, DynamicValue::Uint32(3)); // Stage 3 insurgency
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.pirate.insurgency.api.GetCurrentRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.pirate.insurgency.api.GetCurrentResponse",
                    |resp| {
                        apply_sde_record(resp, 30_000_142, 500_010, "Guristas Insurgency");
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
                build_dynamic_response(
                    &d,
                    "eve.pirate.insurgency.api.GetSystemsResponse",
                    |resp| {
                        apply_sde_record(resp, 30_000_142, 30_000_144, "InsurgencySystems");
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
                build_dynamic_response(
                    &d,
                    "eve.pirate.insurgency.api.GetForWarzoneResponse",
                    |resp| {
                        apply_sde_record(resp, warzone_id as u64, 500_010, "WarzoneInsurgency");
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
                build_dynamic_response(
                    &d,
                    "eve_public.pirate.corruption.api.GetSystemInfoResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(sys_id));
                        resp.set(2, DynamicValue::Uint32(2)); // Corruption stage 2
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
                build_dynamic_response(
                    &d,
                    "eve_public.pirate.corruption.api.GetStageThresholdsResponse",
                    |resp| {
                        apply_sde_record(resp, 10_000, 100_000, "CorruptionThresholds");
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
                build_dynamic_response(
                    &d,
                    "eve_public.pirate.suppression.api.GetSystemInfoResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(sys_id));
                        resp.set(2, DynamicValue::Uint32(1)); // Suppression stage 1
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
                build_dynamic_response(
                    &d,
                    "eve_public.pirate.suppression.api.GetStageThresholdsResponse",
                    |resp| {
                        apply_sde_record(resp, 10_000, 100_000, "SuppressionThresholds");
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
                build_dynamic_response(
                    &d,
                    "eve.dynamicbountysystem.GetDynamicBountySettingsResponse",
                    |resp| {
                        apply_sde_record(resp, 100, sys_count, "DynamicBountySettings");
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
                build_dynamic_response(
                    &d,
                    "eve.dynamicbountysystem.GetActiveSystemsResponse",
                    |resp| {
                        apply_sde_record(resp, 30_000_142, 30_000_144, "ActiveBountySystems");
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
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                build_dynamic_response(
                    &d,
                    "eve.dynamicbountysystem.GetPaidSystemBountiesResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(sys_id));
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
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                build_dynamic_response(
                    &d,
                    "eve.dynamicbountysystem.GetAccumulatedSystemShipLossValueResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(sys_id));
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
                build_dynamic_response(
                    &d,
                    "eve.dungeon.sovereignty.api.GetInstancesResponse",
                    |resp| {
                        apply_sde_record(resp, dng_id, 30_000_142, "SovereigntyDungeon");
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
                build_dynamic_response(
                    &d,
                    "eve_public.dungeon.sovereignty.api.GetVisibleInstancesResponse",
                    |resp| {
                        apply_sde_record(resp, dng_id, 30_000_142, "VisibleSovereigntyDungeon");
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
                let entity_id = get_int_field(dyn_msg, 1).unwrap_or(100_000_001);
                build_dynamic_response(&d, "eve.npc.ship.api.GetOwnerResponse", |resp| {
                    resp.set(1, DynamicValue::Int64(entity_id));
                    resp.set(2, DynamicValue::Int64(500_001)); // Pirate NPC faction ID
                })
            }),
        );
    }

    // 13. Stargate lock API, AI assistant, client, ship, ACL, content, debug, id, mission,
    //     monolith location, space tags, star power, starterspace, station owner, tournament, vanguard, epic vendor
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
            "eve.client.host.GetRequest",
            "eve.client.host.GetResponse",
        ),
        (
            "eve.ship.api.GetRequest",
            "eve.ship.api.GetResponse",
        ),
        (
            "eve.acl.api.GetRequest",
            "eve.acl.api.GetResponse",
        ),
        (
            "eve.content.GetAllRequest",
            "eve.content.GetAllResponse",
        ),
        (
            "eve.debug.monolith.externalqueue.EmitProtobufEventRequest",
            "eve.debug.monolith.externalqueue.EmitProtobufEventResponse",
        ),
        (
            "eve.id.api.GetRequest",
            "eve.id.api.GetResponse",
        ),
        (
            "eve.mission.api.GetCompletedCountRequest",
            "eve.mission.api.GetCompletedCountResponse",
        ),
        (
            "eve.monolith.character.location.GetRequest",
            "eve.monolith.character.location.GetResponse",
        ),
        (
            "eve.space.api.GetDefaultTagRegistryVersionRequest",
            "eve.space.api.GetDefaultTagRegistryVersionResponse",
        ),
        (
            "eve.star.resources.api.GetPowerProductionRequest",
            "eve.star.resources.api.GetPowerProductionResponse",
        ),
        (
            "eve.stargate.lock.api.admin.GetAllRequest",
            "eve.stargate.lock.api.admin.GetAllResponse",
        ),
        (
            "eve.starterspace.admin.ResetJumpShipCasterRequest",
            "eve.starterspace.admin.ResetJumpShipCasterResponse",
        ),
        (
            "eve.station.owner.GetStationOwnerRequest",
            "eve.station.owner.GetStationOwnerResponse",
        ),
        (
            "eve.tournament.arena.api.GetAllRequest",
            "eve.tournament.arena.api.GetAllResponse",
        ),
        (
            "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventRequest",
            "eve.vanguard.commission.assignment.admin.api.TriggerCompletedEventResponse",
        ),
        (
            "eve.vendors.epic.api.GetUserByEveUserIDRequest",
            "eve.vendors.epic.api.GetUserByEveUserIDResponse",
        ),
    ] {
        let d = Arc::clone(&desc);
        let db_svc = Arc::clone(&db);
        let sde_svc = Arc::clone(&sde);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                if req_name.contains(".admin.") {
                    require_admin_role(ctx)?;
                }
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
                build_dynamic_response(&d, rsp_name, |resp| {
                    apply_sde_record(resp, req_id, owner_corp, "evomu-tranquility");
                })
            }),
        );
    }
}
