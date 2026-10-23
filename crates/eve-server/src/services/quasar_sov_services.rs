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
    effective_roles, encode_domain_message, get_identifier_or_int, get_int_field, get_string_field,
    set_domain_attributes, set_domain_field,
};
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
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
        // parity: quasar:eveProto/generated/eve_public/faction/activity/activity_pb2.pyj
        // parity: quasar:eveProto/generated/eve_public/faction/activity/admin_pb2.pyj
        {
            let db_fw = Arc::clone(&db);
            router.register_handler(
                "eve_public.faction.activity.GetSolarSystemScoresRequest",
                Arc::new(move |ctx, _any, dyn_msg| {
                    let opt_sys = get_identifier_or_int(dyn_msg, 1).map(|v| v as u32).or_else(|| {
                        ctx.effective_character_id().and_then(|cid| {
                            db_fw.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.solar_system_id))
                        })
                    });
                    let Some(sys_id) = opt_sys else {
                        return Err((
                            STATUS_BAD_REQUEST,
                            "SolarSystemId is required in GetSolarSystemScoresRequest".to_string(),
                        ));
                    };
                    let (vp, adv_floor, occ_fac) = db_fw.with_snapshot(|snap| {
                        let occ = snap.fw_system_occupations.get(&sys_id)?;
                        let occ_fac = occ.occupier_faction_id as u64;
                        let vp = snap
                            .fw_runtime
                            .system_victory_points
                            .get(&sys_id)
                            .copied()
                            .unwrap_or(occ.victory_points)
                            .max(0) as u64;
                        let floor = snap
                            .fw_runtime
                            .system_advantage
                            .get(&sys_id)
                            .map(|(occ_adv, _)| ((*occ_adv).max(0.0) * 100.0) as u64)
                            .unwrap_or(0);
                        Some((vp, floor, occ_fac))
                    }).ok_or_else(|| {
                        (
                            STATUS_BAD_REQUEST,
                            format!("Solar system {sys_id} is not part of Faction Warfare warzone"),
                        )
                    })?;
                    let mut out = Vec::new();
                    encode_id_field(1, sys_id as u64, &mut out);
                    let mut score_entry = Vec::new();
                    encode_id_field(1, occ_fac, &mut score_entry);
                    encode_varint_field(2, vp, &mut score_entry);
                    encode_varint_field(3, adv_floor, &mut score_entry);
                    encode_bytes_field(2, &score_entry, &mut out);
                    Ok(GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.faction.activity.GetSolarSystemScoresResponse"),
                        value: out,
                    })
                }),
            );
        }
        for (req_name, rsp_name, is_increment) in [
            (
                "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
                "eve_public.faction.activity.admin.IncrementContributionScoreResponse",
                true,
            ),
            (
                "eve_public.faction.activity.admin.SetContributionScoreRequest",
                "eve_public.faction.activity.admin.SetContributionScoreResponse",
                false,
            ),
        ] {
            let db_fw_adm = Arc::clone(&db);
            let pending_fw = Arc::clone(&router.pending_notices);
            router.register_handler(
                req_name,
                Arc::new(move |ctx, _any, dyn_msg| {
                    require_admin_role(ctx)?;
                    let caller_cid = ctx.effective_character_id().unwrap_or(90_000_001) as u64;
                    let opt_sys = get_identifier_or_int(dyn_msg, 1).map(|v| v as u32).or_else(|| {
                        ctx.effective_character_id().and_then(|cid| {
                            db_fw_adm.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.solar_system_id))
                        })
                    });
                    let Some(sys_id) = opt_sys else {
                        return Err((STATUS_BAD_REQUEST, "SolarSystemId is required".to_string()));
                    };
                    if !db_fw_adm.with_snapshot(|s| s.fw_system_occupations.contains_key(&sys_id)) {
                        return Err((
                            STATUS_BAD_REQUEST,
                            format!("Solar system {sys_id} is not part of Faction Warfare warzone"),
                        ));
                    }
                    let opt_fac = get_identifier_or_int(dyn_msg, 2).map(|v| v as u32).or_else(|| {
                        db_fw_adm.with_snapshot(|snap| {
                            snap.fw_system_occupations
                                .get(&sys_id)
                                .map(|o| o.occupier_faction_id as u32)
                        })
                    });
                    let Some(fac_id) = opt_fac else {
                        return Err((STATUS_BAD_REQUEST, "FactionId is required".to_string()));
                    };
                    let delta_or_val = get_int_field(dyn_msg, 3).unwrap_or(0);
                    let (new_vp, adv_floor) = db_fw_adm.with_mut_snapshot(|snap| {
                        let occ = snap
                            .fw_system_occupations
                            .get_mut(&sys_id)
                            .expect("fw warzone system validated above");
                        let threshold = (occ.victory_points_threshold as f64).max(1.0);
                        let entry = snap
                            .fw_runtime
                            .system_victory_points
                            .entry(sys_id)
                            .or_insert(occ.victory_points);
                        if is_increment {
                            *entry = entry.saturating_add(delta_or_val).max(0);
                        } else {
                            *entry = delta_or_val.max(0);
                        }
                        let vp = *entry as u64;
                        let contested = ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0);
                        if let Some(st) = snap.system_states.get_mut(&sys_id) {
                            st.fw_contested_pct = contested;
                        }
                        occ.victory_points = *entry;
                        occ.occupier_faction_id = fac_id as i64;
                        let floor = snap
                            .fw_runtime
                            .system_advantage
                            .get(&sys_id)
                            .map(|(occ_adv, _)| ((*occ_adv).max(0.0) * 100.0) as u64)
                            .unwrap_or(0);
                        (vp, floor)
                    });
                    if is_increment {
                        let mut contrib_notice = Vec::new();
                        encode_id_field(1, sys_id as u64, &mut contrib_notice);
                        encode_id_field(2, caller_cid, &mut contrib_notice);
                        encode_id_field(3, fac_id as u64, &mut contrib_notice);
                        encode_varint_field(4, delta_or_val.max(0) as u64, &mut contrib_notice);
                        pending_fw.write().unwrap_or_else(|e| e.into_inner()).push((
                            NoticeScope::SolarSystem(sys_id),
                            GoogleAny {
                                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.faction.activity.ScoreContributionNotice"),
                                value: contrib_notice,
                            },
                        ));
                    }
                    let mut scores_notice = Vec::new();
                    encode_id_field(1, sys_id as u64, &mut scores_notice);
                    let mut score_entry = Vec::new();
                    encode_id_field(1, fac_id as u64, &mut score_entry);
                    encode_varint_field(2, new_vp, &mut score_entry);
                    encode_varint_field(3, adv_floor, &mut score_entry);
                    encode_bytes_field(2, &score_entry, &mut scores_notice);
                    pending_fw.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::SolarSystem(sys_id),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.faction.activity.SolarSystemScoresNotice"),
                            value: scores_notice,
                        },
                    ));
                    Ok(GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                        value: Vec::new(),
                    })
                }),
            );
        }
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

    // 9. Mercenary den activity, planetary/stellar sov resources, and skyhook admin
    for (req_name, rsp_name) in [
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
        let pending = Arc::clone(&router.pending_notices);
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
                if req_name.ends_with(".skyhook.api.ActivateRequest") || req_name.ends_with(".skyhook.api.DeactivateRequest") {
                    let mut payload = Vec::new();
                    encode_id_field(1, sys_id, &mut payload);
                    encode_id_field(2, 40_000_001, &mut payload);
                    encode_id_field(3, 70_000_001, &mut payload);
                    encode_varint_field(4, if req_name.ends_with(".ActivateRequest") { 1 } else { 0 }, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::SolarSystem(sys_id as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ActivationNotice"),
                            value: payload,
                        },
                    ));
                } else if req_name.ends_with(".hub.upgrade.api.ProcessConfigurationRequest") {
                    let mut payload = Vec::new();
                    encode_id_field(1, 60_000_001, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::SolarSystem(sys_id as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"),
                            value: payload,
                        },
                    ));
                } else if req_name.ends_with(".hub.workforce.api.ConfigureRequest") {
                    let mut payload = Vec::new();
                    encode_id_field(1, sys_id, &mut payload);
                    encode_id_field(4, 60_000_001, &mut payload);
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::SolarSystem(sys_id as u32),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice"),
                            value: payload,
                        },
                    ));
                }
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

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

/// Helper for broadcasting `ResourcesSimulatedNotice` (`eve_public.sovereignty.hub.api.ResourcesSimulatedNotice`).
pub fn publish_sov_hub_resources_simulated_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    hub_id: u64,
    power: u64,
    workforce: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut power_buf = Vec::new();
    encode_varint_field(1, power, &mut power_buf);
    encode_varint_field(2, power, &mut power_buf);

    let mut wf_buf = Vec::new();
    encode_varint_field(1, workforce, &mut wf_buf);
    encode_varint_field(2, workforce, &mut wf_buf);

    let mut payload = Vec::new();
    encode_id_field(1, hub_id, &mut payload);
    encode_id_field(2, solar_system_id as u64, &mut payload);
    encode_bytes_field(3, &power_buf, &mut payload);
    encode_bytes_field(4, &wf_buf, &mut payload);

    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.hub.api.ResourcesSimulatedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `HubUpgradesConfiguredNotice` (`eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice`).
pub fn publish_sov_hub_upgrade_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    hub_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, hub_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.hub.upgrade.api.HubUpgradesConfiguredNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `ConfiguredNotice` (`eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice`).
pub fn publish_sov_hub_workforce_configured_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    hub_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, solar_system_id as u64, &mut payload);
    encode_id_field(4, hub_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.transfer.workforce.api.ConfiguredNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `SimulatedNotice` (`eve_public.sovereignty.mercenaryden.activity.api.SimulatedNotice`).
pub fn publish_sov_mercenary_den_activity_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    den_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, solar_system_id as u64, &mut payload);
    encode_id_field(2, den_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.mercenaryden.activity.api.SimulatedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `SimulationNotice` (`eve_public.sovereignty.mercenaryden.api.SimulationNotice`).
pub fn publish_sov_mercenary_den_simulation_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    den_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, den_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.mercenaryden.api.SimulationNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `ActivationNotice` (`eve_public.sovereignty.skyhook.api.ActivationNotice`).
pub fn publish_sov_skyhook_activation_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    planet_id: u64,
    skyhook_id: u64,
    active: bool,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, solar_system_id as u64, &mut payload);
    encode_id_field(2, planet_id, &mut payload);
    encode_id_field(3, skyhook_id, &mut payload);
    encode_varint_field(4, if active { 1 } else { 0 }, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ActivationNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `ActivatedNotice` (`eve_public.stargate.lock.api.ActivatedNotice`).
pub fn publish_stargate_lock_activated_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    gate_id: u64,
    expiry_unix: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut exp_buf = Vec::new();
    encode_varint_field(1, expiry_unix, &mut exp_buf);

    let mut payload = Vec::new();
    encode_id_field(1, solar_system_id as u64, &mut payload);
    encode_id_field(2, gate_id, &mut payload);
    encode_bytes_field(3, &exp_buf, &mut payload);

    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.stargate.lock.api.ActivatedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}
