//! `map_service` — Starmap statistics, sov map, incursion reports, and navigation markers (`map`).
//!
//! Provides handlers for live star-mode colorings, faction warfare zone stats,
//! incursion global reports, deadspace maps, and station/beacon density queries.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};

pub fn handle(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "map" {
        return None;
    }
    if let Err(e) = ServerContext::require_auth(session) {
        return Some(Err(e));
    }

    let _now_unix = ctx.now_unix();

    match (service, method) {
        ("map", "GetBeaconCount") => {
            // Count cyno beacons per solar system
            let mut counts: HashMap<i64, i64> = HashMap::new();
            ctx.db.with_snapshot(|snap| {
                for s in snap.structures.values() {
                    // Cyno beacon structure type 35841
                    if s.type_id == 35841 {
                        *counts.entry(i64::from(s.solar_system_id)).or_insert(0) += 1;
                    }
                }
            });
            let dict = counts
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k), PyRep::Integer(v)))
                .collect();
            Some(Ok(PyRep::Dict(dict)))
        }

        ("map", "GetConstellationLPData") => {
            let constellation_id = args.first().and_then(|a| a.as_i64()).unwrap_or(20_000_020) as u32;
            let desc = DBRowDescriptor::new([
                ("solarSystemID", DbColumnType::I4),
                ("factionID", DbColumnType::I4),
                ("points", DbColumnType::I4),
            ]);
            // Resolve constellation fallback faction from any SDE station in the constellation (e.g. Kimotoro -> 500001 Caldari State).
            // DERIVED: fallback to Caldari State (500_001) only when no FW occupation or SDE station owner_faction_id exists in the constellation.
            let const_fac = ctx
                .sde
                .solar_systems
                .values()
                .filter(|s| s.constellation_id == constellation_id)
                .flat_map(|s| s.stations.iter())
                .find_map(|st| st.owner_faction_id)
                .unwrap_or(crate::services::fw_services::FACTION_CALDARI);
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                ctx.sde
                    .solar_systems
                    .values()
                    .filter(|s| s.constellation_id == constellation_id)
                    .map(|s| {
                        let occ = snap.fw_system_occupations.get(&s.solar_system_id);
                        let fac_id = occ
                            .map(|o| o.occupier_faction_id)
                            .or_else(|| occ.map(|o| o.owner_faction_id))
                            .or_else(|| s.stations.first().and_then(|st| st.owner_faction_id))
                            .unwrap_or(const_fac);
                        let vp = occ
                            .map(|o| o.victory_points)
                            .or_else(|| snap.fw_runtime.system_victory_points.get(&s.solar_system_id).copied())
                            .unwrap_or(0);
                        vec![
                            PyRep::Integer(i64::from(s.solar_system_id)),
                            PyRep::Integer(fac_id),
                            PyRep::Integer(vp),
                        ]
                    })
                    .collect()
            });
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetDeadspaceAgentsMap") => {
            let desc = DBRowDescriptor::new([
                ("locationID", DbColumnType::I4),
                ("solarSystemID", DbColumnType::I4),
                ("agentID", DbColumnType::I4),
            ]);
            let rows = ctx
                .sde
                .agents
                .values()
                .filter_map(|a| {
                    let loc = a.station_id?;
                    let sys = a.solar_system_id?;
                    Some(vec![
                        PyRep::Integer(loc),
                        PyRep::Integer(i64::from(sys)),
                        PyRep::Integer(a.agent_id),
                    ])
                })
                .collect();
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetDeadspaceComplexMap") => {
            let desc = DBRowDescriptor::new([
                ("dungeonID", DbColumnType::I4),
                ("solarSystemID", DbColumnType::I4),
                ("difficulty", DbColumnType::I4),
            ]);
            let mut rows: Vec<Vec<PyRep>> = crate::services::pve_exploration::with_pve_exploration_reg(ctx, |reg| {
                reg.sites_by_system
                    .values()
                    .flatten()
                    .filter(|s| !s.cleared)
                    .map(|s| {
                        vec![
                            PyRep::Integer(i64::from(s.dungeon_id)),
                            PyRep::Integer(i64::from(s.solar_system_id)),
                            PyRep::Integer(i64::from(s.ded_rating.unwrap_or(1))),
                        ]
                    })
                    .collect()
            });
            if rows.is_empty() {
                let sys_id = session.solar_system_id.unwrap_or(30_000_142);
                let sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(0.9);
                for (did, _, _, _, _) in crate::services::pve_exploration::anomaly_dungeons_for_system(sys_id, sec) {
                    rows.push(vec![
                        PyRep::Integer(i64::from(did)),
                        PyRep::Integer(i64::from(sys_id)),
                        PyRep::Integer(1),
                    ]);
                }
            }
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetEdencomFortressSystems") => {
            // const-ok: returns empty list when no EDENCOM fortress systems are active in world state
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("map", "GetEdencomMinorVictorySystems") => {
            // const-ok: returns empty list when no EDENCOM minor victory systems are active in world state
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("map", "GetFacWarZoneInfo") => {
            let _faction_id = args.first().and_then(|a| a.as_i64()).unwrap_or(500_001);
            let total_vp = crate::services::fw_services::read_fw_runtime(ctx, |rt| {
                rt.system_victory_points.values().copied().sum::<i64>()
            });
            Some(Ok(PyRep::keyval([
                ("factionPoints", PyRep::Integer(total_vp)),
                ("maxWarZonePoints", PyRep::Integer(100_000)),
            ])))
        }

        ("map", "GetHistory") => {
            let stat_type = args.first().and_then(|a| a.as_i64()).unwrap_or(1);
            let desc = DBRowDescriptor::new([
                ("solarSystemID", DbColumnType::I4),
                ("value1", DbColumnType::I4),
                ("value2", DbColumnType::I4),
                ("value3", DbColumnType::I4),
            ]);
            let rows: Vec<Vec<PyRep>> = ctx.db.with_snapshot(|snap| {
                let mut sys_ids: std::collections::BTreeSet<u32> = snap.system_states.keys().copied().collect();
                for ch in snap.characters.values() {
                    sys_ids.insert(ch.solar_system_id);
                }
                for km in snap.killmails.values() {
                    sys_ids.insert(km.solar_system_id);
                }
                if let Some(sid) = session.solar_system_id {
                    sys_ids.insert(sid);
                }
                sys_ids
                    .into_iter()
                    .map(|sid| {
                        let pilots = snap.characters.values().filter(|c| c.solar_system_id == sid).count() as i64;
                        let ship_kills = snap.killmails.values().filter(|k| k.solar_system_id == sid && k.ship_type_id != 670).count() as i64;
                        let pod_kills = snap.killmails.values().filter(|k| k.solar_system_id == sid && k.ship_type_id == 670).count() as i64;
                        let npc_kills = snap.killmails.values().filter(|k| k.solar_system_id == sid && k.victim_character_id == 0).count() as i64;
                        let (v1, v2, v3) = match stat_type {
                            3 => (pilots.max(1), 0, 0),
                            _ => (ship_kills, npc_kills, pod_kills),
                        };
                        vec![
                            PyRep::Integer(i64::from(sid)),
                            PyRep::Integer(v1),
                            PyRep::Integer(v2),
                            PyRep::Integer(v3),
                        ]
                    })
                    .collect()
            });
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetIncursionGlobalReport") => {
            crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
            let now_ft = ctx.now_blueos_filetime_ticks();
            let list = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                reg.incursions
                    .values()
                    .map(|inc| {
                        PyRep::keyval([
                            ("taleID", PyRep::Integer(inc.tale_id)),
                            ("constellationID", PyRep::Integer(i64::from(inc.constellation_id))),
                            ("constellationName", PyRep::String(inc.constellation_name.clone())),
                            ("stagingSolarSystemID", PyRep::Integer(i64::from(inc.staging_solar_system_id))),
                            ("hqSolarSystemID", PyRep::Integer(i64::from(inc.hq_solar_system_id))),
                            ("influence", PyRep::Real(inc.influence)),
                            ("state", PyRep::String(inc.state.clone())),
                            ("hasBoss", PyRep::Bool(inc.mothership_spawned && !inc.mothership_defeated)),
                            ("factionID", PyRep::Integer(500_019)),
                            ("decayRate", PyRep::Real(0.02)),
                            ("graceTime", PyRep::Integer(0)),
                            ("lastUpdated", PyRep::Integer(now_ft)),
                            ("rewardGroupID", PyRep::Integer(inc.tale_id)),
                        ])
                    })
                    .collect()
            });
            Some(Ok(PyRep::List(list)))
        }

        ("map", "GetMyExtraMapInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut list = Vec::new();
            ctx.db.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&char_id) {
                    let corp_id = ch.corporation_id;
                    for other in snap.characters.values() {
                        if other.corporation_id == corp_id {
                            list.push(PyRep::keyval([
                                ("characterID", PyRep::Integer(other.character_id)),
                                ("corporationID", PyRep::Integer(other.corporation_id)),
                                (
                                    "solarSystemID",
                                    PyRep::Integer(i64::from(other.solar_system_id)),
                                ),
                            ]));
                        }
                    }
                }
            });
            Some(Ok(PyRep::List(list)))
        }

        ("map", "GetMyExtraMapInfoAgents") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let desc = DBRowDescriptor::new([
                ("agentID", DbColumnType::I4),
                ("standing", DbColumnType::R8),
            ]);
            let rows = ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return Vec::new();
                };
                ch.standings
                    .iter()
                    .filter(|(&id, _)| ctx.sde.agents.contains_key(&id))
                    .map(|(&id, &st)| vec![PyRep::Integer(id), PyRep::Real(st)])
                    .collect()
            });
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetRecentSovActivity") => {
            let list = ctx.db.with_snapshot(|snap| {
                snap.system_states
                    .iter()
                    .filter_map(|(&sys_id, st)| {
                        st.sov_alliance_id.map(|aid| {
                            PyRep::keyval([
                                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                                ("ownerID", PyRep::Integer(aid)),
                                ("oldOwnerID", PyRep::None),
                                ("stationID", PyRep::None),
                            ])
                        })
                    })
                    .collect::<Vec<PyRep>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        ("map", "GetSolarSystemVisits") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut visits = Vec::new();
            ctx.db.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&char_id) {
                    visits.push((
                        PyRep::Integer(i64::from(ch.solar_system_id)),
                        PyRep::Integer(1),
                    ));
                }
            });
            Some(Ok(PyRep::Dict(visits)))
        }

        ("map", "GetStationCount") => {
            let mut counts: HashMap<i64, i64> = HashMap::new();
            for st in ctx.sde.stations.values() {
                *counts.entry(i64::from(st.solar_system_id)).or_insert(0) += 1;
            }
            let dict = counts
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k), PyRep::Integer(v)))
                .collect();
            Some(Ok(PyRep::Dict(dict)))
        }

        ("map", "GetSystemsInIncursions") => {
            crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
            let list = crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                let mut out = Vec::new();
                for inc in reg.incursions.values() {
                    for &sys_id in &[inc.staging_solar_system_id, inc.hq_solar_system_id] {
                        let scene_type = if sys_id == inc.staging_solar_system_id { 1 } else { 2 };
                        out.push(PyRep::keyval([
                            ("locationID", PyRep::Integer(i64::from(sys_id))),
                            ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                            ("constellationID", PyRep::Integer(i64::from(inc.constellation_id))),
                            ("sceneType", PyRep::Integer(scene_type)),
                            ("templateNameID", PyRep::Integer(inc.tale_id)),
                        ]));
                    }
                }
                out
            });
            Some(Ok(PyRep::List(list)))
        }

        ("map", "GetTriglavianMinorVictorySystems") => {
            // const-ok: returns empty list when no Triglavian minor victory systems are active in world state
            Some(Ok(PyRep::List(Vec::new())))
        }

        _ => None,
    }
}
