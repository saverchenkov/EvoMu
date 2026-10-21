//! `esi::sov_fw_routes` — Sovereignty, Incursions, and Faction Warfare war-matrix routes (`ESI-3`),
//! backed by `SdeRegistry` and live `WorldRepository` state.

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use eve_sde::{SdeRegistry, TypeId};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

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

/// Activity Defense Multiplier per index level (`eve-wiki/articles/Sovereignty.md:263-269`):
/// military and industrial add 0 / 0.6 / 1.2 / 1.7 / 2.1 / 2.5, strategic adds
/// 0 / 0.4 / 0.6 / 0.8 / 0.9 / 1.0, on a base of 1.0, capped at 6.0.
const MIL_IND_ADM_BONUS: [f64; 6] = [0.0, 0.6, 1.2, 1.7, 2.1, 2.5];
const STRAT_ADM_BONUS: [f64; 6] = [0.0, 0.4, 0.6, 0.8, 0.9, 1.0];

pub fn activity_defense_multiplier(military: u8, industrial: u8, strategic: u8) -> f64 {
    let mil = MIL_IND_ADM_BONUS[usize::from(military.min(5))];
    let ind = MIL_IND_ADM_BONUS[usize::from(industrial.min(5))];
    let strat = STRAT_ADM_BONUS[usize::from(strategic.min(5))];
    ((1.0 + mil + ind + strat).clamp(1.0, 6.0) * 10.0).round() / 10.0
}

pub fn dispatch_sov_route(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
) -> Option<RouteExecutionResult> {
    let now = ctx.now_unix();
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
                            && (st.state == "CommandNodeEvent" || st.reinforced_until_unix > 0)
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
                    let total_nodes = (st.shield_hp + st.armor_hp).max(1.0);
                    let defender_score =
                        ((st.shield_hp / total_nodes).clamp(0.0, 1.0) * 100.0).round() / 100.0;
                    let attackers_score = ((1.0 - defender_score) * 100.0).round() / 100.0;
                    let start_unix = if st.reinforced_until_unix > 0 {
                        st.reinforced_until_unix
                    } else {
                        now
                    };
                    campaigns.push(json!({
                        "campaign_id": st.structure_id,
                        "constellation_id": sys.constellation_id as i64,
                        "solar_system_id": st.solar_system_id as i64,
                        "structure_id": st.structure_id,
                        "event_type": sov_campaign_event_type(&ctx.sde, st.type_id),
                        "defender_id": defender_id,
                        "defender_score": defender_score,
                        "attackers_score": attackers_score,
                        "start_time": format_iso8601_utc(start_unix),
                    }));
                }
                RouteExecutionResult::ok_json(json!(campaigns), 5)
            }))
        }

        // Active Sansha's Nation incursions derived from live constellation system state & anomalies.
        ("GET", "/incursions") => {
            let sansha_faction_id = ctx
                .sde
                .factions
                .values()
                .find(|f| f.name.contains("Sansha"))
                .map(|f| f.faction_id as i64)
                .unwrap_or(500_019);
            Some(ctx.db.with_snapshot(|snap| {
                let mut by_constellation: BTreeMap<u32, Vec<(u32, u8, u8)>> = BTreeMap::new();
                for (&sys_id, st) in &snap.system_states {
                    if sys_id >= 31_000_000 || st.insurgency_corruption_stage == 0 {
                        continue;
                    }
                    if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
                        by_constellation.entry(sys.constellation_id).or_default().push((
                            sys_id,
                            st.insurgency_corruption_stage,
                            st.insurgency_suppression_stage,
                        ));
                    }
                }
                let mut out = Vec::new();
                for (constellation_id, mut systems) in by_constellation {
                    systems.sort_by_key(|(sid, _, _)| *sid);
                    let infested_solar_systems: Vec<i64> =
                        systems.iter().map(|(sid, _, _)| *sid as i64).collect();
                    let staging_solar_system_id = infested_solar_systems[0];
                    let count = systems.len() as f64;
                    let avg_corr = systems.iter().map(|(_, c, _)| *c as f64).sum::<f64>() / count;
                    let avg_supp = systems
                        .iter()
                        .map(|(_, _, s)| (*s).max(1) as f64)
                        .sum::<f64>()
                        / count;
                    let cleared_sites: usize = systems
                        .iter()
                        .filter_map(|(sid, _, _)| snap.cosmic_anomalies.get(sid))
                        .flat_map(|list| list.iter())
                        .filter(|a| a.cleared)
                        .count();
                    let raw_influence =
                        (avg_corr * 0.45 / avg_supp) - (cleared_sites as f64 * 0.25);
                    let influence = (raw_influence.clamp(0.0, 1.0) * 100.0).round() / 100.0;
                    let has_boss = avg_corr >= 2.0 && influence > 0.25;
                    let state = if influence <= 0.25 {
                        "withdrawing"
                    } else if influence < 0.6 {
                        "mobilizing"
                    } else {
                        "established"
                    };
                    out.push(json!({
                        "constellation_id": constellation_id as i64,
                        "faction_id": sansha_faction_id,
                        "has_boss": has_boss,
                        "infested_solar_systems": infested_solar_systems,
                        "influence": influence,
                        "staging_solar_system_id": staging_solar_system_id,
                        "state": state,
                        "type": "Incursion",
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        // Faction Warfare war matrix from SDE militia factions and live active faction wars.
        ("GET", "/fw/wars") => Some(ctx.db.with_snapshot(|snap| {
            let mut militia_factions: Vec<u32> = ctx
                .sde
                .factions
                .values()
                .filter(|f| f.militia_corporation_id.is_some() && f.size_factor >= 5.0)
                .map(|f| f.faction_id)
                .collect();
            militia_factions.sort_unstable();
            let mut pairs: Vec<(i64, i64)> = Vec::new();
            let n = militia_factions.len();
            for i in 0..(n / 2) {
                let f_a = militia_factions[i] as i64;
                let f_b = militia_factions[n - 1 - i] as i64;
                if f_a != f_b {
                    pairs.push((f_a, f_b));
                    pairs.push((f_b, f_a));
                }
            }
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
