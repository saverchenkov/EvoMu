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

    let now_unix = ctx.now_unix();

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
            let rows = vec![
                vec![
                    PyRep::Integer(1001),
                    PyRep::Integer(30_000_142),
                    PyRep::Integer(3),
                ],
                vec![
                    PyRep::Integer(1002),
                    PyRep::Integer(30_000_144),
                    PyRep::Integer(4),
                ],
            ];
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetEdencomFortressSystems") => {
            // EDENCOM fortress systems
            let fortress_systems = vec![
                PyRep::Integer(30000142),
                PyRep::Integer(30000144),
            ];
            Some(Ok(PyRep::List(fortress_systems)))
        }

        ("map", "GetEdencomMinorVictorySystems") => {
            let minor_victory_systems = vec![
                PyRep::Integer(30000143),
                PyRep::Integer(30000145),
            ];
            Some(Ok(PyRep::List(minor_victory_systems)))
        }

        ("map", "GetFacWarZoneInfo") => {
            let _faction_id = args.first().and_then(|a| a.as_i64()).unwrap_or(500_001);
            Some(Ok(PyRep::keyval([
                ("factionPoints", PyRep::Integer(15_000)),
                ("maxWarZonePoints", PyRep::Integer(100_000)),
            ])))
        }

        ("map", "GetHistory") => {
            // (statType, timeFrame) -> CRowset of (solarSystemID, value1, value2, value3)
            let desc = DBRowDescriptor::new([
                ("solarSystemID", DbColumnType::I4),
                ("value1", DbColumnType::I4),
                ("value2", DbColumnType::I4),
                ("value3", DbColumnType::I4),
            ]);
            let rows = vec![
                vec![
                    PyRep::Integer(30000142),
                    PyRep::Integer(12),
                    PyRep::Integer(5),
                    PyRep::Integer(1),
                ],
                vec![
                    PyRep::Integer(30000144),
                    PyRep::Integer(8),
                    PyRep::Integer(2),
                    PyRep::Integer(0),
                ],
            ];
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetIncursionGlobalReport") => {
            let report = vec![PyRep::keyval([
                ("stagingSolarSystemID", PyRep::Integer(30000142)),
                ("influence", PyRep::Real(0.75)),
                ("lastUpdated", PyRep::Integer(now_unix as i64)),
                ("graceTime", PyRep::Integer(86400)),
                ("decayRate", PyRep::Real(0.05)),
                ("constellationID", PyRep::Integer(20000020)),
                ("factionID", PyRep::Integer(500001)),
                ("rewardGroupID", PyRep::Integer(101)),
                ("rewardQuantity", PyRep::Integer(5000)),
                ("loyaltyPoints", PyRep::Integer(10_000)),
            ])];
            Some(Ok(PyRep::List(report)))
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
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let desc = DBRowDescriptor::new([
                ("agentID", DbColumnType::I4),
                ("standing", DbColumnType::R8),
            ]);
            let rows = vec![vec![PyRep::Integer(3011394), PyRep::Real(5.0)]];
            Some(Ok(desc.build_crowset(rows)))
        }

        ("map", "GetRecentSovActivity") => {
            let mut list = ctx.db.with_snapshot(|snap| {
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
            if list.is_empty() {
                list.push(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(30_000_142)),
                    ("ownerID", PyRep::Integer(99_000_001)),
                    ("oldOwnerID", PyRep::None),
                    ("stationID", PyRep::None),
                ]));
            }
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
            if visits.is_empty() {
                visits.push((PyRep::Integer(30_000_142), PyRep::Integer(1)));
            }
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
            let list = vec![PyRep::keyval([
                ("locationID", PyRep::Integer(30_000_142)),
                ("sceneType", PyRep::Integer(1)),
            ])];
            Some(Ok(PyRep::List(list)))
        }

        ("map", "GetTriglavianMinorVictorySystems") => {
            let trig_systems = vec![
                PyRep::Integer(30000146),
                PyRep::Integer(30000147),
            ];
            Some(Ok(PyRep::List(trig_systems)))
        }

        _ => None,
    }
}
