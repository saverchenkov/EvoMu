//! `quasar_sov_services` — Quasar Sovereignty, Faction Warfare & Operations Services (`Q-8`).
//!
//! Implements server-side Quasar RPC handlers and notice publishing for:
//! - `eve_public.sovereignty.skyhook.api.ActivateRequest`, `DeactivateRequest` (skyhook queries:
//!   see `quasar_sov_skyhook`)
//! - Equinox hub resources / fuel / upgrades / workforce: see `quasar_sov_hub` and
//!   `quasar_sov_workforce`; mercenary dens and activities: see `quasar_sov_mercden`
//! - `eve.faction.api.GetRequest`, `SearchRequest`
//! - `eve.faction.activity.GetSolarSystemScoresRequest`, `GetAllSolarSystemScoresRequest`
//! - `eve.factionwarfare.systems.GetRequest`, `wars.GetRequest`, `warzone.api.GetAllRequest`
//! - `eve.factionwarfare.leaderboard.characters.GetRequest`, `corporations.GetRequest`, `factions.GetRequest`
//! - `eve.factionwarfare.stats.character.GetRequest`, `corporation.GetRequest`, `factions.GetRequest`
//! - `eve_public.operation.recommendation.GetRequest`

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{caller, owned_structure, require_roles, required_id, transact};
use super::quasar_space_services::{
    encode_domain_message, get_identifier_or_int, get_int_field, get_string_field,
    set_domain_attributes,
};
use super::sov_services::TYPE_ORBITAL_SKYHOOK;
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::{corp_roles, WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

/// Authorizes a skyhook mutation: authenticated character, skyhook owned by the caller's
/// corporation / alliance, and `ROLE_DIRECTOR`. Returns the skyhook's solar system.
pub(crate) fn authorize_skyhook(
    ctx: &AuthoritativeContext,
    snap: &WorldStateSnapshot,
    skyhook_id: i64,
    action: &str,
) -> Result<u32, (u32, String)> {
    let c = caller(ctx, snap)?;
    let sys = owned_structure(snap, &c, skyhook_id, &[TYPE_ORBITAL_SKYHOOK], "Skyhook")?.solar_system_id;
    require_roles(ctx, corp_roles::ROLE_DIRECTOR, action)?;
    Ok(sys)
}

/// Registers all Q-8 sovereignty, faction warfare, and operations service handlers on the gateway router.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let desc = Arc::clone(&router.descriptors);

    // 1. eve_public.sovereignty.skyhook.api handlers
    super::quasar_sov_skyhook::register_query_handlers(router, Arc::clone(&db));
    {
        let db_act = Arc::clone(&db);
        let sde_act = Arc::clone(&sde);
        let pending = Arc::clone(&router.pending_notices);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.ActivateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let structure_id = required_id(dyn_msg, 1, "Skyhook")?;
                let sys_id = transact(&db_act, |snap| {
                    let sys = authorize_skyhook(ctx, snap, structure_id, "activate skyhook")?;
                    if snap.sov_runtime.skyhook_active.get(&structure_id).copied().unwrap_or(true) {
                        return Err((STATUS_BAD_REQUEST, format!("Skyhook {structure_id} is already active")));
                    }
                    snap.sov_runtime.skyhook_active.insert(structure_id, true);
                    Ok::<_, (u32, String)>(sys)
                })?;

                let mut payload = Vec::new();
                encode_id_field(1, sys_id as u64, &mut payload);
                if let Some(pid) = sde_act.solar_systems.get(&sys_id).and_then(|s| s.planet_ids.first().copied()) {
                    encode_id_field(2, pid as u64, &mut payload);
                }
                encode_id_field(3, structure_id as u64, &mut payload);
                encode_varint_field(4, 1, &mut payload);
                pending.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::SolarSystem(sys_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ActivationNotice"),
                        value: payload,
                    },
                ));

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ActivateResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }
    {
        let db_deact = Arc::clone(&db);
        let sde_deact = Arc::clone(&sde);
        let pending = Arc::clone(&router.pending_notices);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.DeactivateRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let structure_id = required_id(dyn_msg, 1, "Skyhook")?;
                let sys_id = transact(&db_deact, |snap| {
                    let sys = authorize_skyhook(ctx, snap, structure_id, "deactivate skyhook")?;
                    if !snap.sov_runtime.skyhook_active.get(&structure_id).copied().unwrap_or(true) {
                        return Err((STATUS_BAD_REQUEST, format!("Skyhook {structure_id} is already inactive")));
                    }
                    snap.sov_runtime.skyhook_active.insert(structure_id, false);
                    Ok::<_, (u32, String)>(sys)
                })?;

                let mut payload = Vec::new();
                encode_id_field(1, sys_id as u64, &mut payload);
                if let Some(pid) = sde_deact.solar_systems.get(&sys_id).and_then(|s| s.planet_ids.first().copied()) {
                    encode_id_field(2, pid as u64, &mut payload);
                }
                encode_id_field(3, structure_id as u64, &mut payload);
                encode_varint_field(4, 0, &mut payload);
                pending.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::SolarSystem(sys_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ActivationNotice"),
                        value: payload,
                    },
                ));

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.DeactivateResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 6. eve.faction.api & eve.faction.activity handlers
    {
        let sde_fac = Arc::clone(&sde);
        router.register_handler(
            "eve.faction.api.GetRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let Some(faction_id) = get_identifier_or_int(dyn_msg, 1).and_then(|v| u32::try_from(v).ok()).filter(|&v| v > 0) else {
                    return Err((STATUS_BAD_REQUEST, "Faction identifier is required".to_string()));
                };
                let Some(fac) = sde_fac.factions.get(&faction_id) else {
                    return Err((STATUS_NOT_FOUND, format!("Faction {faction_id} not found")));
                };
                // Standard EVE empire race bit/sequential mapping for playable factions
                let race_id = match faction_id {
                    500_001 => Some(1u64), // Caldari
                    500_002 => Some(2u64), // Minmatar
                    500_003 => Some(4u64), // Amarr
                    500_004 => Some(8u64), // Gallente
                    500_005 => Some(16u64), // Jove
                    _ => None,
                };
                let mut attr = Vec::new();
                eve_proto::quasar::encode_string_field(1, &fac.name, &mut attr);
                eve_proto::quasar::encode_string_field(2, &format!("res:/ui/texture/corps/{}_128.png", fac.corporation_id.unwrap_or(faction_id as i64)), &mut attr);
                if let Some(rid) = race_id {
                    encode_id_field(3, rid, &mut attr);
                    encode_id_field(4, rid, &mut attr);
                }
                let mut out = Vec::new();
                encode_bytes_field(1, &attr, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.faction.api.GetResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        let sde_fac = Arc::clone(&sde);
        router.register_handler(
            "eve.faction.SearchRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let mut keywords = Vec::new();
                if let Some(s) = get_string_field(dyn_msg, 1) {
                    if !s.trim().is_empty() {
                        keywords.push(s.trim().to_lowercase());
                    }
                } else if let Some(eve_proto::quasar::DynamicValue::Message(q_msg)) = dyn_msg.get(1) {
                    match q_msg.get(1) {
                        Some(eve_proto::quasar::DynamicValue::String(s)) if !s.trim().is_empty() => {
                            keywords.push(s.trim().to_lowercase());
                        }
                        Some(eve_proto::quasar::DynamicValue::Repeated(vals)) => {
                            for v in vals {
                                if let eve_proto::quasar::DynamicValue::String(s) = v {
                                    if !s.trim().is_empty() {
                                        keywords.push(s.trim().to_lowercase());
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if keywords.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Search query cannot be empty".into()));
                }
                let mut matched_ids: Vec<u32> = sde_fac
                    .factions
                    .values()
                    .filter(|f| {
                        let name_lower = f.name.to_lowercase();
                        keywords.iter().all(|kw| name_lower.contains(kw))
                    })
                    .map(|f| f.faction_id)
                    .collect();
                matched_ids.sort_unstable();
                let mut out = Vec::new();
                for fid in matched_ids {
                    encode_id_field(1, fid as u64, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.faction.SearchResponse"),
                    value: out,
                })
            }),
        );
    }
    {
        for (req_name, rsp_name, include_sys_field) in [
            (
                "eve.faction.activity.GetSolarSystemScoresRequest",
                "eve.faction.activity.GetSolarSystemScoresResponse",
                false,
            ),
            (
                "eve_public.faction.activity.GetSolarSystemScoresRequest",
                "eve_public.faction.activity.GetSolarSystemScoresResponse",
                true,
            ),
        ] {
            let db_fw = Arc::clone(&db);
            router.register_handler(
                req_name,
                Arc::new(move |ctx, _any, dyn_msg| {
                    let opt_sys = get_identifier_or_int(dyn_msg, 1).and_then(|v| u32::try_from(v).ok()).or_else(|| {
                        ctx.effective_character_id().and_then(|cid| {
                            db_fw.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.solar_system_id))
                        })
                    });
                    let Some(sys_id) = opt_sys.filter(|&id| id > 0) else {
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
                    if include_sys_field {
                        encode_id_field(1, sys_id as u64, &mut out);
                    }
                    let mut score_entry = Vec::new();
                    encode_id_field(1, occ_fac, &mut score_entry);
                    encode_varint_field(2, vp, &mut score_entry);
                    encode_varint_field(3, adv_floor, &mut score_entry);
                    encode_bytes_field(2, &score_entry, &mut out);
                    Ok(GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                        value: out,
                    })
                }),
            );
        }
        {
            let db_all = Arc::clone(&db);
            router.register_handler(
                "eve.faction.activity.api.GetAllSolarSystemScoresRequest",
                Arc::new(move |_ctx, _any, _dyn_msg| {
                    let (rows, ceiling) = db_all.with_snapshot(|snap| {
                        let mut sys_ids: Vec<u32> = snap.fw_system_occupations.keys().copied().collect();
                        sys_ids.sort_unstable();
                        let mut max_ceil = 3_000u64;
                        let mut entries = Vec::with_capacity(sys_ids.len());
                        for sys_id in sys_ids {
                            if let Some(occ) = snap.fw_system_occupations.get(&sys_id) {
                                max_ceil = max_ceil.max(occ.victory_points_threshold.max(0) as u64);
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
                                entries.push((sys_id, occ.occupier_faction_id as u64, vp, floor));
                            }
                        }
                        (entries, max_ceil)
                    });
                    let mut out = Vec::new();
                    for (sys_id, fac_id, vp, floor) in rows {
                        let mut score_entry = Vec::new();
                        encode_id_field(1, fac_id, &mut score_entry);
                        encode_varint_field(2, vp, &mut score_entry);
                        encode_varint_field(3, floor, &mut score_entry);
                        let mut sys_scores = Vec::new();
                        encode_id_field(1, sys_id as u64, &mut sys_scores);
                        encode_bytes_field(2, &score_entry, &mut sys_scores);
                        encode_bytes_field(1, &sys_scores, &mut out);
                    }
                    encode_varint_field(2, ceiling, &mut out);
                    Ok(GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve.faction.activity.api.GetAllSolarSystemScoresResponse"
                        ),
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
            (
                "eve.faction.activity.admin.IncrementContributionScoreRequest",
                "eve.faction.activity.admin.IncrementContributionScoreResponse",
                true,
            ),
            (
                "eve.faction.activity.admin.SetContributionScoreRequest",
                "eve.faction.activity.admin.SetContributionScoreResponse",
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
                    let opt_fac_input = get_identifier_or_int(dyn_msg, 2).map(|v| v as u32);
                    let delta_or_val = get_int_field(dyn_msg, 3).unwrap_or(0);
                    let mut resolved_fac_id = 0u64;
                    let mutate_res = db_fw_adm.with_mut_snapshot(|snap| {
                        let Some(occ) = snap.fw_system_occupations.get_mut(&sys_id) else {
                            return None;
                        };
                        resolved_fac_id = opt_fac_input
                            .map(|v| v as u64)
                            .unwrap_or(occ.occupier_faction_id as u64);
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
                            occ.occupier_faction_id = resolved_fac_id as i64;
                        }
                        let vp = *entry as u64;
                        let contested = ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0);
                        if let Some(st) = snap.system_states.get_mut(&sys_id) {
                            st.fw_contested_pct = contested;
                        }
                        occ.victory_points = *entry;
                        let floor = snap
                            .fw_runtime
                            .system_advantage
                            .get(&sys_id)
                            .map(|(occ_adv, _)| ((*occ_adv).max(0.0) * 100.0) as u64)
                            .unwrap_or(0);
                        Some((vp, floor))
                    });
                    let Some((new_vp, adv_floor)) = mutate_res else {
                        return Err((
                            STATUS_BAD_REQUEST,
                            format!("Solar system {sys_id} is not part of Faction Warfare warzone"),
                        ));
                    };
                    let fac_id = resolved_fac_id;
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
        let db_wz = Arc::clone(&db);
        let sde_wz = Arc::clone(&sde);
        router.register_handler(
            "eve.factionwarfare.warzone.api.GetAllRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let warzones = db_wz.with_mut_snapshot(|snap| {
                    if snap.fw_system_occupations.is_empty() {
                        snap.backfill_fw_system_occupations();
                    }
                    let mut wz = std::collections::BTreeSet::new();
                    for occ in snap.fw_system_occupations.values() {
                        match occ.occupier_faction_id {
                            500_001 | 500_004 => {
                                if sde_wz.factions.contains_key(&(occ.occupier_faction_id as u32)) {
                                    wz.insert(1u64);
                                }
                            }
                            500_002 | 500_003 => {
                                if sde_wz.factions.contains_key(&(occ.occupier_faction_id as u32)) {
                                    wz.insert(2u64);
                                }
                            }
                            _ => {}
                        }
                    }
                    wz
                });
                let mut out = Vec::new();
                for wz_id in warzones {
                    encode_id_field(1, wz_id, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.factionwarfare.warzone.api.GetAllResponse"),
                    value: out,
                })
            }),
        );

        let db_part = Arc::clone(&db);
        let sde_part = Arc::clone(&sde);
        router.register_handler(
            "eve.factionwarfare.warzone.api.GetParticipantsRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let wz_id = required_id(dyn_msg, 1, "Warzone")? as u64;
                let (factions, pirate_faction) = db_part.with_snapshot(|snap| {
                    let (pair, default_pirate): ([u32; 2], u32) = match wz_id {
                        1 => ([500_001, 500_004], 500_010),
                        2 => ([500_002, 500_003], 500_011),
                        _ => return Err((STATUS_NOT_FOUND, format!("Warzone {wz_id} not found"))),
                    };
                    let factions: Vec<u64> = pair
                        .into_iter()
                        .filter(|fid| sde_part.factions.get(fid).is_some_and(|f| f.militia_corporation_id.is_some()))
                        .map(u64::from)
                        .collect();
                    let pirate = snap
                        .insurgency_runtime
                        .campaigns
                        .values()
                        .find(|c| pair.contains(&(c.target_faction_id as u32)))
                        .map(|c| c.pirate_faction_id as u64)
                        .unwrap_or(u64::from(default_pirate));
                    Ok((factions, pirate))
                })?;
                let mut part_buf = Vec::new();
                for fid in factions {
                    encode_id_field(1, fid, &mut part_buf);
                }
                if sde_part.factions.contains_key(&(pirate_faction as u32)) {
                    encode_id_field(2, pirate_faction, &mut part_buf);
                }
                let mut out = Vec::new();
                encode_bytes_field(1, &part_buf, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.factionwarfare.warzone.api.GetParticipantsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 8. eve_public.operation.recommendation.GetRequest
    {
        let db_rec = Arc::clone(&db);
        let sde_rec = Arc::clone(&sde);
        router.register_handler(
            "eve_public.operation.recommendation.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))?
                    as i64;
                let amount = get_int_field(dyn_msg, 1).unwrap_or(3).clamp(1, 20) as usize;
                let allow_old = get_int_field(dyn_msg, 2).unwrap_or(0) != 0;
                let completed = db_rec.with_snapshot(|snap| {
                    crate::services::operations_manager::completed_operations_for_character(snap, cid)
                });
                let mut ops: Vec<u64> = sde_rec
                    .operations()
                    .map(|map| {
                        map.keys()
                            .filter_map(|k| {
                                let op_id = k.parse::<u64>().ok()?;
                                if !allow_old && completed.contains(&(op_id as i64)) {
                                    return None;
                                }
                                Some(op_id)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                ops.sort_unstable();
                ops.truncate(amount);

                let mut out = Vec::new();
                for op_id in ops {
                    encode_id_field(1, op_id, &mut out);
                    let mut rec = Vec::new();
                    encode_id_field(2, op_id, &mut rec);
                    let mut journey = [0u8; 16];
                    journey[..8].copy_from_slice(&(cid as u64).to_be_bytes());
                    journey[8..].copy_from_slice(&op_id.to_be_bytes());
                    encode_bytes_field(3, &journey, &mut rec);
                    encode_bytes_field(4, &rec, &mut out);
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.operation.recommendation.GetResponse"),
                    value: out,
                })
            }),
        );
    }

    // 9. Equinox hub (resources / fuel / upgrades / workforce) and mercenary den services
    crate::services::quasar_sov_hub::register_handlers(router, Arc::clone(&db), Arc::clone(&sde));
    crate::services::quasar_sov_mercden::register_handlers(router, Arc::clone(&db));

    // 10. Register companion planetary/stellar resources and skyhook admin handlers
    crate::services::quasar_sov_resources::register_handlers(router, Arc::clone(&db), Arc::clone(&sde));
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

/// Publishes `eve_public.stargate.lock.api.ActivatedNotice` for a Zarzakh gate lock created by
/// `CmdStargateJump` (`space_warp_jump`), after the jumping session has entered `solar_system_id`.
pub(crate) fn publish_stargate_lock_activated_notice(
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
