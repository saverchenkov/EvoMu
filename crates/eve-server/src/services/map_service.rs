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
            // Ensure at least one system is reported
            if counts.is_empty() {
                counts.insert(30_000_142, 1);
            }
            let dict = counts
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k), PyRep::Integer(v)))
                .collect();
            Some(Ok(PyRep::Dict(dict)))
        }

        ("map", "GetConstellationLPData") => {
            let constellation_id = args.first().and_then(|a| a.as_i64()).unwrap_or(20_000_020);
            let desc = DBRowDescriptor::new([
                ("solarSystemID", DbColumnType::I4),
                ("factionID", DbColumnType::I4),
                ("points", DbColumnType::I4),
            ]);
            let mut rows = Vec::new();
            for sys in ctx.sde.solar_systems.values() {
                if i64::from(sys.constellation_id) == constellation_id {
                    rows.push(vec![
                        PyRep::Integer(i64::from(sys.solar_system_id)),
                        PyRep::Integer(500_001),
                        PyRep::Integer(100),
                    ]);
                }
            }
            if rows.is_empty() {
                rows.push(vec![
                    PyRep::Integer(30_000_142),
                    PyRep::Integer(500_001),
                    PyRep::Integer(100),
                ]);
            }
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
                .take(32)
                .map(|a| {
                    vec![
                        PyRep::Integer(a.station_id.unwrap_or(60_003_760)),
                        PyRep::Integer(i64::from(a.solar_system_id.unwrap_or(30_000_142))),
                        PyRep::Integer(a.agent_id),
                    ]
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
            let rows: Vec<Vec<PyRep>> = Vec::new();
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetEdencomFortressSystems") => {
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("map", "GetEdencomMinorVictorySystems") => {
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
            let desc = DBRowDescriptor::new([
                ("solarSystemID", DbColumnType::I4),
                ("value1", DbColumnType::I4),
                ("value2", DbColumnType::I4),
                ("value3", DbColumnType::I4),
            ]);
            let rows = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .map(|ch| {
                        vec![
                            PyRep::Integer(i64::from(ch.solar_system_id)),
                            PyRep::Integer(1),
                            PyRep::Integer(0),
                            PyRep::Integer(0),
                        ]
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetIncursionGlobalReport") => {
            Some(Ok(PyRep::List(Vec::new())))
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
            Some(Ok(PyRep::List(Vec::new())))
        }

        ("map", "GetTriglavianMinorVictorySystems") => {
            Some(Ok(PyRep::List(Vec::new())))
        }

        _ => None,
    }
}
