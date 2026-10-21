//! `quasar_space_services` — Quasar Space, Celestials, Navigation, Insurgency & DBS Services (`Q-7`).
//!
//! Implements server-side Quasar RPC handlers and notice publishing for:
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

#![forbid(unsafe_code)]

use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_INTERNAL_ERROR};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

/// Extracts effective corp role bitmask from authoritative context.
pub fn effective_roles(ctx: &AuthoritativeContext) -> u64 {
    ctx.identity
        .as_ref()
        .and_then(|id| id.roles.as_ref().map(|r| r.bitmask))
        .unwrap_or(0)
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
pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let desc = Arc::clone(&router.descriptors);

    // 1. eve_public.solarsystem.api.GetRequest -> eve_public.solarsystem.api.GetResponse
    {
        let d = Arc::clone(&desc);
        let _db = Arc::clone(&db);
        router.register_handler(
            "eve_public.solarsystem.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if sys_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "SolarSystemId must be positive".into()));
                }
                build_dynamic_response(&d, "eve_public.solarsystem.api.GetResponse", |_resp| {})
            }),
        );
    }

    // 2. eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest -> eve.solarsystem.api.GetSolarSystemsInJumpRangeResponse
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.solarsystem.api.GetSolarSystemsInJumpRangeRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let origin_sys = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if origin_sys == 0 {
                    return Err((STATUS_BAD_REQUEST, "Origin system ID must be positive".into()));
                }
                build_dynamic_response(
                    &d,
                    "eve.solarsystem.api.GetSolarSystemsInJumpRangeResponse",
                    |_resp| {},
                )
            }),
        );
    }

    // 3. eve.constellation.GetRequest & GetAllRequest & SearchRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.constellation.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let const_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(20_000_020) as u32;
                if const_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Constellation ID must be positive".into()));
                }
                build_dynamic_response(&d, "eve.constellation.GetResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.constellation.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(&d, "eve.constellation.GetAllResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.constellation.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let query = get_string_field(dyn_msg, 1).unwrap_or_default();
                if query.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Search query cannot be empty".into()));
                }
                build_dynamic_response(&d, "eve.constellation.SearchResponse", |_resp| {})
            }),
        );
    }

    // 4. eve.region.GetRequest & GetAllRequest & SearchRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.region.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let region_id = get_identifier_or_int(dyn_msg, 1).unwrap_or(10_000_002) as u32;
                if region_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Region ID must be positive".into()));
                }
                build_dynamic_response(&d, "eve.region.GetResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.region.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(&d, "eve.region.GetAllResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.region.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let query = get_string_field(dyn_msg, 1).unwrap_or_default();
                if query.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Region query cannot be empty".into()));
                }
                build_dynamic_response(&d, "eve.region.SearchResponse", |_resp| {})
            }),
        );
    }

    // 5. eve.universe.constellation.GetAllInRegionRequest & eve.universe.navigation.GetShortestGateJumpCountRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.universe.constellation.GetAllInRegionRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let region_id = get_int_field(dyn_msg, 1).unwrap_or(10_000_002) as u32;
                if region_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Region ID must be positive".into()));
                }
                build_dynamic_response(
                    &d,
                    "eve.universe.constellation.GetAllInRegionResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.universe.navigation.GetShortestGateJumpCountRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let from_sys = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                let to_sys = get_int_field(dyn_msg, 2).unwrap_or(30_000_144) as u32;
                if from_sys == 0 || to_sys == 0 {
                    return Err((STATUS_BAD_REQUEST, "Jump systems must be valid".into()));
                }
                build_dynamic_response(
                    &d,
                    "eve.universe.navigation.GetShortestGateJumpCountResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(2));
                    },
                )
            }),
        );
    }

    // 6. eve.bubble.presence.api.GetInBubbleRequest & GetInBubbleByCharacterRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.bubble.presence.api.GetInBubbleRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(&d, "eve.bubble.presence.api.GetInBubbleResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.bubble.presence.api.GetInBubbleByCharacterRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let char_id = get_int_field(dyn_msg, 1).unwrap_or(90_000_001) as u32;
                if char_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Character ID must be positive".into()));
                }
                build_dynamic_response(
                    &d,
                    "eve.bubble.presence.api.GetInBubbleByCharacterResponse",
                    |_resp| {},
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
                    |_resp| {},
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
                    |_resp| {},
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
                    |_resp| {},
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
                    |_resp| {},
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
                    |_resp| {},
                )
            }),
        );
    }

    // 10. eve.dynamicbountysystem.GetDynamicBountySettingsRequest, GetActiveSystemsRequest,
    //     GetPaidSystemBountiesRequest, GetAccumulatedSystemShipLossValueRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.dynamicbountysystem.GetDynamicBountySettingsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.dynamicbountysystem.GetDynamicBountySettingsResponse",
                    |_resp| {},
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
                    |_resp| {},
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
        router.register_handler(
            "eve.dungeon.sovereignty.api.GetInstancesRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.dungeon.sovereignty.api.GetInstancesResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.dungeon.sovereignty.api.GetVisibleInstancesRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.dungeon.sovereignty.api.GetVisibleInstancesResponse",
                    |_resp| {},
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
}
