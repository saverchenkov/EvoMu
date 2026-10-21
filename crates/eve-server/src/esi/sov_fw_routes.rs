//! `esi::sov_fw_routes` — Sovereignty, Incursions, and Faction Warfare war-matrix routes (`ESI-3`),
//! backed by `SdeRegistry` and live `WorldRepository` state.

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use eve_sde::{SdeRegistry, TypeId};
use serde_json::{json, Value};
use std::collections::BTreeSet;

/// SDE group names whose published types are sovereignty structures reported by ESI.
/// `Sovereignty Hub` (formerly Infrastructure Hub) and `Territorial Claim Unit`; the type IDs are
/// read from the SDE, so QA/test hubs (unpublished) are excluded automatically.
pub const SOV_STRUCTURE_GROUP_NAMES: [&str; 2] = ["Sovereignty Hub", "Territorial Claim Unit"];

/// Published SDE types in the sovereignty-structure groups.
pub fn sov_structure_type_ids(sde: &SdeRegistry) -> BTreeSet<TypeId> {
    let groups: BTreeSet<u32> = sde
        .groups
        .values()
        .filter(|g| SOV_STRUCTURE_GROUP_NAMES.contains(&g.name.as_str()))
        .map(|g| g.group_id)
        .collect();
    sde.types
        .values()
        .filter(|t| t.published && groups.contains(&t.group_id))
        .map(|t| t.type_id)
        .collect()
}

/// Returns `"tcu_defense"` if `type_id` belongs to SDE group `"Territorial Claim Unit"`,
/// otherwise `"ihub_defense"`.
pub fn sov_campaign_event_type(sde: &SdeRegistry, type_id: TypeId) -> &'static str {
    let is_tcu = sde
        .types
        .get(&type_id)
        .and_then(|t| sde.groups.get(&t.group_id))
        .is_some_and(|g| g.name == "Territorial Claim Unit");
    if is_tcu {
        "tcu_defense"
    } else {
        "ihub_defense"
    }
}

// DERIVED: Activity Defense Multiplier per index level (`eve-wiki/articles/Sovereignty.md:263-269`):
// military and industrial add 0 / 0.6 / 1.2 / 1.7 / 2.1 / 2.5, strategic adds
// 0 / 0.4 / 0.6 / 0.8 / 0.9 / 1.0, on a base of 1.0, capped at 6.0.
const MIL_IND_ADM_BONUS: [f64; 6] = [0.0, 0.6, 1.2, 1.7, 2.1, 2.5];
const STRAT_ADM_BONUS: [f64; 6] = [0.0, 0.4, 0.6, 0.8, 0.9, 1.0];

pub fn activity_defense_multiplier(military: u8, industrial: u8, strategic: u8) -> f64 {
    let mil = MIL_IND_ADM_BONUS[usize::from(military.min(5))];
    let ind = MIL_IND_ADM_BONUS[usize::from(industrial.min(5))];
    let strat = STRAT_ADM_BONUS[usize::from(strategic.min(5))];
    ((1.0 + mil + ind + strat).clamp(1.0, 6.0) * 10.0).round() / 10.0
}

// DERIVED: Empire militia faction war pairings resolved by SDE faction name
// (`Caldari State` <-> `Gallente Federation`, `Minmatar Republic` <-> `Amarr Empire`).
pub fn sde_empire_fw_war_pairs(sde: &SdeRegistry) -> Vec<(i64, i64)> {
    let find_fid = |name: &str| -> Option<i64> {
        sde.factions
            .values()
            .find(|f| f.name == name && f.militia_corporation_id.is_some())
            .map(|f| f.faction_id as i64)
    };
    let mut pairs = Vec::new();
    if let (Some(cal), Some(gal)) = (find_fid("Caldari State"), find_fid("Gallente Federation")) {
        pairs.push((cal, gal));
        pairs.push((gal, cal));
    }
    if let (Some(min), Some(ama)) = (find_fid("Minmatar Republic"), find_fid("Amarr Empire")) {
        pairs.push((min, ama));
        pairs.push((ama, min));
    }
    pairs.sort_unstable();
    pairs
}

pub fn dispatch_sov_route(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
) -> Option<RouteExecutionResult> {
    match (method, path_pattern) {
        ("GET", "/sovereignty/map") => Some(ctx.db.with_snapshot(|snap| {
            let mut sys_ids: Vec<_> = ctx.sde.solar_systems.keys().copied().collect();
            sys_ids.sort_unstable();
            let mut out = Vec::with_capacity(sys_ids.len());
            for sid in sys_ids {
                let mut entry = json!({ "system_id": sid as i64 });
                let sov_alliance = snap.system_states.get(&sid).and_then(|st| st.sov_alliance_id);
                if let Some(aid) = sov_alliance {
                    entry["alliance_id"] = json!(aid);
                    if let Some(al) = snap.alliances.get(&aid) {
                        entry["corporation_id"] = json!(al.executor_corp_id);
                    }
                } else if let Some(fid) = ctx.sde.solar_systems.get(&sid).and_then(|s| {
                    ctx.sde
                        .constellations
                        .get(&s.constellation_id)
                        .and_then(|c| c.faction_id)
                        .or_else(|| ctx.sde.regions.get(&s.region_id).and_then(|r| r.faction_id))
                }) {
                    entry["faction_id"] = json!(fid as i64);
                }
                out.push(entry);
            }
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        // Sovereignty structures anchored by alliance corporations, from the live structure table.
        // ADM comes from the system's live military/industrial/strategic indices and the vulnerability
        // window from the stored structure timer; both are omitted while no window is scheduled
        // (e.g. during a command-node event), as ESI does.
        ("GET", "/sovereignty/structures") => {
            let sov_types = sov_structure_type_ids(&ctx.sde);
            Some(ctx.db.with_snapshot(|snap| {
                let mut structs: Vec<_> = snap
                    .structures
                    .values()
                    .filter(|st| st.state != "Destroyed" && sov_types.contains(&st.type_id))
                    .collect();
                structs.sort_by_key(|st| st.structure_id);
                let mut out = Vec::new();
                for st in structs {
                    let Some(aid) = snap
                        .corporations
                        .get(&st.owner_corp_id)
                        .and_then(|c| c.alliance_id)
                    else {
                        continue;
                    };
                    let adm = snap
                        .system_states
                        .get(&st.solar_system_id)
                        .map(|s| {
                            activity_defense_multiplier(
                                s.military_index,
                                s.industrial_index,
                                s.strategic_index,
                            )
                        })
                        .unwrap_or(1.0);
                    let window = if st.state == "CommandNodeEvent" {
                        None
                    } else {
                        snap.structure_vulnerability_windows
                            .get(&st.structure_id)
                            .copied()
                            .filter(|&(v_start, v_end)| v_start > 0 && v_end > v_start)
                    };
                    let mut entry = json!({
                        "alliance_id": aid,
                        "solar_system_id": st.solar_system_id as i64,
                        "structure_id": st.structure_id,
                        "structure_type_id": st.type_id as i64,
                    });
                    if let Some((vuln_start, vuln_end)) = window {
                        entry["vulnerability_occupancy_level"] = json!(adm);
                        entry["vulnerable_start_time"] = json!(format_iso8601_utc(vuln_start));
                        entry["vulnerable_end_time"] = json!(format_iso8601_utc(vuln_end));
                    }
                    out.push(entry);
                }
                RouteExecutionResult::ok_json(json!(out), 120)
            }))
        }

        // Active sovereignty entosis command-node campaigns on reinforced sovereignty structures.
        ("GET", "/sovereignty/campaigns") => {
            let sov_types = sov_structure_type_ids(&ctx.sde);
            Some(ctx.db.with_snapshot(|snap| {
                let mut structs: Vec<_> = snap
                    .structures
                    .values()
                    .filter(|st| {
                        st.state != "Destroyed"
                            && sov_types.contains(&st.type_id)
                            && st.reinforced_until_unix > 0
                            && (st.state == "CommandNodeEvent"
                                || st.state == "ArmorReinforced"
                                || st.state == "HullReinforced")
                    })
                    .collect();
                structs.sort_by_key(|st| st.structure_id);
                let mut campaigns = Vec::new();
                for st in structs {
                    let Some(defender_id) = snap
                        .corporations
                        .get(&st.owner_corp_id)
                        .and_then(|c| c.alliance_id)
                        .or_else(|| {
                            snap.system_states
                                .get(&st.solar_system_id)
                                .and_then(|s| s.sov_alliance_id)
                        })
                    else {
                        continue;
                    };
                    let Some(sys) = ctx.sde.solar_systems.get(&st.solar_system_id) else {
                        continue;
                    };
                    let (attackers_score, defender_score) = snap
                        .sov_campaign_scores
                        .get(&st.structure_id)
                        .map(|s| (s.attackers_score, s.defender_score))
                        .unwrap_or((0.4, 0.6));
                    campaigns.push(json!({
                        "campaign_id": st.structure_id,
                        "constellation_id": sys.constellation_id as i64,
                        "solar_system_id": st.solar_system_id as i64,
                        "structure_id": st.structure_id,
                        "event_type": sov_campaign_event_type(&ctx.sde, st.type_id),
                        "defender_id": defender_id,
                        "defender_score": defender_score,
                        "attackers_score": attackers_score,
                        "start_time": format_iso8601_utc(st.reinforced_until_unix),
                    }));
                }
                RouteExecutionResult::ok_json(json!(campaigns), 5)
            }))
        }

        // Active incursions from live `snap.incursions` state.
        ("GET", "/incursions") => Some(ctx.db.with_snapshot(|snap| {
            let mut list: Vec<_> = snap.incursions.values().collect();
            list.sort_by_key(|inc| inc.constellation_id);
            let out: Vec<Value> = list
                .into_iter()
                .map(|inc| {
                    let infested: Vec<i64> =
                        inc.infested_solar_systems.iter().map(|&s| s as i64).collect();
                    json!({
                        "constellation_id": inc.constellation_id as i64,
                        "faction_id": inc.faction_id,
                        "has_boss": inc.has_boss,
                        "infested_solar_systems": infested,
                        "influence": inc.influence,
                        "staging_solar_system_id": inc.staging_solar_system_id as i64,
                        "state": inc.state,
                        "type": inc.incursion_type,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 300)
        })),

        // Faction Warfare war matrix from live `snap.fw_wars`, SDE empire militia war pairs, and active wars.
        ("GET", "/fw/wars") => Some(ctx.db.with_snapshot(|snap| {
            let mut pairs: Vec<(i64, i64)> = if !snap.fw_wars.is_empty() {
                snap.fw_wars.clone()
            } else {
                sde_empire_fw_war_pairs(&ctx.sde)
            };
            let resolve_faction = |id: i64| -> Option<i64> {
                if let Ok(fid) = u32::try_from(id) {
                    if ctx.sde.factions.contains_key(&fid) {
                        return Some(id);
                    }
                }
                ctx.sde
                    .factions
                    .values()
                    .find(|f| {
                        f.corporation_id == Some(id) || f.militia_corporation_id == Some(id)
                    })
                    .map(|f| f.faction_id as i64)
                    .or_else(|| {
                        ctx.sde
                            .npc_corporations
                            .get(&id)
                            .and_then(|nc| nc.faction_id)
                            .map(|fid| fid as i64)
                    })
            };
            for w in snap.wars.values().filter(|w| w.active) {
                if let (Some(fa), Some(fb)) = (
                    resolve_faction(w.aggressor_corp_id),
                    resolve_faction(w.defender_corp_id),
                ) {
                    if fa != fb {
                        if !pairs.contains(&(fa, fb)) {
                            pairs.push((fa, fb));
                        }
                        if !pairs.contains(&(fb, fa)) {
                            pairs.push((fb, fa));
                        }
                    }
                }
            }
            let out: Vec<Value> = pairs
                .into_iter()
                .map(|(faction_id, against_id)| {
                    json!({
                        "faction_id": faction_id,
                        "against_id": against_id,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 86400)
        })),

        _ => None,
    }
}
