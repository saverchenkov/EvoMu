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
    effective_roles, encode_domain_message, get_int_field, get_string_field, set_domain_attributes,
    set_domain_field,
};
use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
};
use eve_db::WorldRepository;
use eve_sde::SdeRegistry;
use std::sync::Arc;

/// Registers all Q-8 sovereignty, faction warfare, and operations service handlers on the gateway router.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let desc = Arc::clone(&router.descriptors);

    // 1. eve_public.sovereignty.skyhook.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, structure_id, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_sky = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetAllLocalRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u32;
                if sys_id == 0 {
                    return Err((STATUS_BAD_REQUEST, "System ID must be positive".into()));
                }
                let reg_id = sde_sky
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.region_id as u64)
                    .unwrap_or(10_000_002);
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetAllLocalResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, sys_id as u64, reg_id, "LocalSkyhook");
                    },
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
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetAllByCorporationResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, corp_id as u64, 1_000_000_001, "CorpSkyhook");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.ActivateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                let role = effective_roles(ctx);
                if role & 0x01 == 0 && role & 0x02 == 0 && role & 0x04 == 0 {
                    // Requires director or config role
                    return Err((
                        STATUS_FORBIDDEN,
                        "Corporation director role required to activate skyhook".into(),
                    ));
                }
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.ActivateResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, structure_id, "");
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
                let structure_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                let role = effective_roles(ctx);
                if role & 0x01 == 0 && role & 0x02 == 0 && role & 0x04 == 0 {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Corporation director role required to deactivate skyhook".into(),
                    ));
                }
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.DeactivateResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, structure_id, "");
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
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 30_000_142, 10_000_002, "VulnerableSkyhookSystems");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, sys_id, "");
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
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.fuel.api.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, hub_id, 10_000, "SovHubFuel");
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
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.fuel.api.AddResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, hub_id, "");
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
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.GetStateResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, hub_id, 2_500, "SovHubWorkforce");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetConfigurationRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.GetConfigurationResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, hub_id, 2_500, "WorkforceConfig");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.ConfigureRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.ConfigureResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, hub_id, 2_500, "WorkforceConfigured");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.workforce.api.GetNetworkableHubsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 1_000_000_001, 30_000_142, "NetworkableSovHub");
                    },
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
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.GetDefinitionsResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 81_201, 500, "MajorThreatDetectionArray");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.GetHubUpgradesResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, hub_id, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.ProcessConfigurationResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, hub_id, 81_201, "UpgradeConfig");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.hub.upgrade.api.UninstallRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let hub_id = get_int_field(dyn_msg, 1).unwrap_or(1_000_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.hub.upgrade.api.UninstallResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, hub_id, 81_201, "UpgradeUninstalled");
                    },
                )
            }),
        );
    }

    // 5. eve_public.sovereignty.mercenaryden.api handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.mercenaryden.api.GetAllOwnedResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, cid, 30_000_142, "OwnedMercenaryDen");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, 3, "");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.sovereignty.mercenaryden.api.GetAsOwnerResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, cid, 30_000_142, "OwnerMercenaryDen");
                    },
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
                let faction_id = get_int_field(dyn_msg, 1).unwrap_or(500_001) as u64;
                encode_domain_message(&d, "eve.faction.api.GetResponse", |reg, resp| {
                    set_domain_field(reg, resp, 1, faction_id, "");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        let sde_fac = Arc::clone(&sde);
        router.register_handler(
            "eve.faction.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let query = get_string_field(dyn_msg, 1).unwrap_or_default();
                if query.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Search query cannot be empty".into()));
                }
                let q_lower = query.to_lowercase();
                let fac_id = sde_fac
                    .factions
                    .values()
                    .find(|f| f.name.to_lowercase().contains(&q_lower))
                    .map(|f| f.faction_id)
                    .unwrap_or(500_001);
                encode_domain_message(&d, "eve.faction.SearchResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, fac_id as u64, 30_000_142, &query);
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.faction.activity.GetSolarSystemScoresRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                encode_domain_message(
                    &d,
                    "eve.faction.activity.GetSolarSystemScoresResponse",
                    |reg, resp| {
                        set_domain_field(reg, resp, 1, sys_id, "");
                    },
                )
            }),
        );
        let d_pub = Arc::clone(&desc);
        let db_fw = Arc::clone(&db);
        router.register_handler(
            "eve_public.faction.activity.GetSolarSystemScoresRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                let key = Arc::as_ptr(&db_fw) as usize;
                let vp = crate::services::fw_services::read_fw_runtime_by_key(key, |rt| {
                    rt.system_victory_points.get(&(sys_id as u32)).copied().unwrap_or(0)
                });
                encode_domain_message(
                    &d_pub,
                    "eve_public.faction.activity.GetSolarSystemScoresResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, sys_id, vp as u64, "FWSystemScores");
                    },
                )
            }),
        );
    }

    // 7. eve.factionwarfare handlers
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.systems.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let sys_id = get_int_field(dyn_msg, 1).unwrap_or(30_000_142) as u64;
                encode_domain_message(&d, "eve.factionwarfare.systems.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, sys_id, 500_001, "FWSystemStatus");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.leaderboard.characters.GetRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve.factionwarfare.leaderboard.characters.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, cid, 500_001, "FWCharacterLeaderboard");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.leaderboard.corporations.GetRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve.factionwarfare.leaderboard.corporations.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, corp_id, 500_001, "FWCorporationLeaderboard");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.leaderboard.factions.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.factionwarfare.leaderboard.factions.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 500_001, 500_004, "FWFactionLeaderboard");
                    },
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
                    .unwrap_or(90_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve.factionwarfare.stats.character.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, char_id, 12, "FWCharacterStats");
                    },
                )
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.wars.GetRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(&d, "eve.factionwarfare.wars.GetResponse", |reg, resp| {
                    set_domain_attributes(reg, resp, 500_001, 500_004, "CaldariGallenteMilitiaWar");
                })
            }),
        );
    }
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve.factionwarfare.warzone.api.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                encode_domain_message(
                    &d,
                    "eve.factionwarfare.warzone.api.GetAllResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, 1, 500_001, "CaldariGallenteWarzone");
                    },
                )
            }),
        );
    }

    // 8. eve_public.operation.recommendation.GetRequest
    {
        let d = Arc::clone(&desc);
        router.register_handler(
            "eve_public.operation.recommendation.GetRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as u64;
                encode_domain_message(
                    &d,
                    "eve_public.operation.recommendation.GetResponse",
                    |reg, resp| {
                        set_domain_attributes(reg, resp, cid, 30_000_142, "AIRDailyOperation");
                    },
                )
            }),
        );
    }

    // 9. Mercenary den activity, planetary/stellar sov resources, skyhook & faction admin
    for (req_name, rsp_name) in [
        (
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            "eve_public.faction.activity.admin.IncrementContributionScoreResponse",
        ),
        (
            "eve_public.faction.activity.admin.SetContributionScoreRequest",
            "eve_public.faction.activity.admin.SetContributionScoreResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.StartResponse",
        ),
        (
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse",
        ),
        (
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse",
        ),
        (
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse",
        ),
        (
            "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
            "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsResponse",
        ),
        (
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse",
        ),
        (
            "eve_public.sovereignty.hub.api.GetResourcesRequest",
            "eve_public.sovereignty.hub.api.GetResourcesResponse",
        ),
        (
            "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
            "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse",
        ),
        (
            "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest",
            "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowResponse",
        ),
    ] {
        let d = Arc::clone(&desc);
        let db_sov = Arc::clone(&db);
        let sde_sov = Arc::clone(&sde);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, _dyn_msg| {
                if req_name.contains(".admin.") {
                    require_admin_role(ctx)?;
                }
                let cid = ctx.effective_character_id().unwrap_or(90_000_001) as i64;
                let sys_id = db_sov.with_snapshot(|s| {
                    s.characters
                        .get(&cid)
                        .map(|c| c.solar_system_id as u64)
                        .unwrap_or(30_000_142)
                });
                let fac_id = sde_sov
                    .solar_systems
                    .get(&(sys_id as u32))
                    .and_then(|s| sde_sov.regions.get(&s.region_id))
                    .and_then(|r| r.faction_id)
                    .unwrap_or(500_001) as u64;
                encode_domain_message(&d, rsp_name, |reg, resp| {
                    set_domain_attributes(reg, resp, sys_id, fac_id, "SovResourceState");
                })
            }),
        );
    }
}
