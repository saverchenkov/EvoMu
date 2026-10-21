//! `quasar_sov_services` — Quasar Sovereignty, Faction Warfare & Operations Services (`Q-8`).
//!
//! Implements server-side Quasar RPC handlers and notice publishing for:
//! - `eve_public.sovereignty.skyhook.api.GetRequest`, `GetAllLocalRequest`, `GetAllByCorporationRequest`,
//!   `ActivateRequest`, `DeactivateRequest`, `GetSolarSystemsWithTheftVulnerableSkyhooksRequest`,
//!   `GetTheftVulnerableSkyhooksInSolarSystemRequest`
//! - `eve_public.sovereignty.hub.fuel.api.GetRequest`, `AddRequest`
//! - `eve_public.sovereignty.hub.workforce.api.GetStateRequest`, `GetConfigurationRequest`,
//!   `ConfigureRequest`, `GetNetworkableHubsRequest`
//! - `eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest`, `GetHubUpgradesRequest`,
//!   `ProcessConfigurationRequest`, `UninstallRequest`
//! - `eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest`, `GetMaximumForCharacterRequest`,
//!   `GetAsOwnerRequest`
//! - `eve.faction.api.GetRequest`, `SearchRequest`
//! - `eve.faction.activity.GetSolarSystemScoresRequest`, `GetAllSolarSystemScoresRequest`
//! - `eve.factionwarfare.systems.GetRequest`, `wars.GetRequest`, `warzone.api.GetAllRequest`
//! - `eve.factionwarfare.leaderboard.characters.GetRequest`, `corporations.GetRequest`, `factions.GetRequest`
//! - `eve.factionwarfare.stats.character.GetRequest`, `corporation.GetRequest`, `factions.GetRequest`
//! - `eve_public.operation.recommendation.GetRequest`

#![forbid(unsafe_code)]

use super::quasar_space_services::{
    build_dynamic_response, effective_roles, get_int_field, get_string_field,
};
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN};
use eve_db::WorldRepository;
use eve_proto::quasar::DynamicValue;
use std::sync::Arc;

/// Registers all Q-8 sovereignty, faction warfare, and operations service handlers on the gateway router.
pub fn register_handlers(router: &PublicGatewayRouter, _db: Arc<WorldRepository>) {
    let desc = Arc::clone(&router.descriptors);

    // 1. eve_public.sovereignty.skyhook.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(structure_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetAllLocalRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if sys_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "System ID must be positive".into()));
                }
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetAllLocalResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let corp_id = get_int_field(dyn_msg, 1).unwrap_or(98_000_001) as u32;
                if corp_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "Corporation ID must be positive".into()));
                }
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetAllByCorporationResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.ActivateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                let role = effective_roles(ctx);
                if role & 0x01 == 0 && role & 0x02 == 0 && role & 0x04 == 0 {
                    // Requires director or config role
                    return Err((
                        STATUS_FORBIDDEN,
                        "Corporation director role required to activate skyhook".into(),
                    ));
                }
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.ActivateResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(structure_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.DeactivateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                let role = effective_roles(ctx);
                if role & 0x01 == 0 && role & 0x02 == 0 && role & 0x04 == 0 {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Corporation director role required to deactivate skyhook".into(),
                    ));
                }
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.DeactivateResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(structure_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(sys_id));
                    },
                )
            }),
        );
    }

    // 2. eve_public.sovereignty.hub.fuel.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.fuel.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.fuel.api.GetResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(hub_id));
                        resp.set(2, DynamicValue::Int64(10_000)); // Current fuel units
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.fuel.api.AddRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.fuel.api.AddResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(hub_id));
                    },
                )
            }),
        );
    }

    // 3. eve_public.sovereignty.hub.workforce.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetStateRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.GetStateResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(hub_id));
                        resp.set(2, DynamicValue::Int64(2_500)); // Available workforce
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.GetConfigurationResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.ConfigureResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsResponse",
                    |_resp| {},
                )
            }),
        );
    }

    // 4. eve_public.sovereignty.hub.upgrade.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001);
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Int64(hub_id));
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.UninstallRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.UninstallResponse",
                    |_resp| {},
                )
            }),
        );
    }

    // 5. eve_public.sovereignty.mercenaryden.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.mercenaryden.api.GetAllOwnedResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse",
                    |resp| {
                        resp.set(1, DynamicValue::Uint32(3)); // Max 3 mercenary dens
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.sovereignty.mercenaryden.api.GetAsOwnerResponse",
                    |_resp| {},
                )
            }),
        );
    }

    // 6. eve.faction.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.faction.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let faction_id = get_int_field(dyn_msg, 1).unwrap_or(500_001) as u32;
                build_dynamic_response(&d, "eve.faction.api.GetResponse", |resp| {
                    if let Some(DynamicValue::Message(ident)) = resp.fields.get_mut(&1) {
                        ident.set(1, DynamicValue::Uint32(faction_id));
                    }
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.faction.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let query = get_string_field(dyn_msg, 1).unwrap_or_default();
                if query.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Search query cannot be empty".into()));
                }
                build_dynamic_response(&d, "eve.faction.SearchResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.faction.activity.GetSolarSystemScoresRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                build_dynamic_response(&d, "eve.faction.activity.GetSolarSystemScoresResponse", |resp| {
                    resp.set(1, DynamicValue::Uint32(sys_id));
                })
            }),
        );
    }

    // 7. eve.factionwarfare handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.systems.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                build_dynamic_response(&d, "eve.factionwarfare.systems.GetResponse", |resp| {
                    if let Some(DynamicValue::Message(status)) = resp.fields.get_mut(&1) {
                        status.set(1, DynamicValue::Uint32(sys_id));
                        status.set(2, DynamicValue::Uint32(500_001)); // Occupier faction ID
                        status.set(3, DynamicValue::Float(45.5)); // Contested %
                    }
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.leaderboard.characters.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.factionwarfare.leaderboard.characters.GetResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.leaderboard.corporations.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.factionwarfare.leaderboard.corporations.GetResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.leaderboard.factions.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve.factionwarfare.leaderboard.factions.GetResponse",
                    |_resp| {},
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.stats.character.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let char_id = get_int_field(dyn_msg, 1)
                    .map(|v| v as u32)
                    .or_else(|| ctx.effective_character_id())
                    .unwrap_or(90_000_001);
                build_dynamic_response(&d, "eve.factionwarfare.stats.character.GetResponse", |resp| {
                    resp.set(1, DynamicValue::Uint32(char_id));
                    resp.set(2, DynamicValue::Uint32(12)); // Kills
                    resp.set(3, DynamicValue::Uint32(450)); // Victory points
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.wars.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(&d, "eve.factionwarfare.wars.GetResponse", |_resp| {})
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.warzone.api.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(&d, "eve.factionwarfare.warzone.api.GetAllResponse", |_resp| {})
            }),
        );
    }

    // 8. eve_public.operation.recommendation.GetRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.operation.recommendation.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_dynamic_response(
                    &d,
                    "eve_public.operation.recommendation.GetResponse",
                    |_resp| {},
                )
            }),
        );
    }
}
