//! `esi::universe_routes` — SDE- and `WorldRepository`-backed ESI route handlers for Routes,
//! Search, Sovereignty, Universe, User Interface, and Wars tags (`ESI-2`).

use crate::esi::sample_routes::{paginate_slice, parse_path_i64, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::SsoTokenClaims;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;

/// Dispatches Routes, Search, Sovereignty, Universe, User Interface, and Wars OpenAPI operations.
#[allow(clippy::too_many_lines)]
pub fn dispatch_universe_and_nav(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    _claims: Option<&SsoTokenClaims>,
    page: usize,
    _autopilot_waypoints: &RwLock<HashMap<i64, Vec<i64>>>,
) -> RouteExecutionResult {
    match (method, path_pattern) {
        // 26. Routes tag
        ("GET", "/route/{origin}/{destination}") => {
            let origin = match parse_path_i64(path_params, "origin") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let destination = match parse_path_i64(path_params, "destination") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let (Ok(orig_u32), Ok(dest_u32)) = (u32::try_from(origin), u32::try_from(destination))
            else {
                return RouteExecutionResult::error(404, "Origin or destination system not found");
            };
            if !ctx.sde.solar_systems.contains_key(&orig_u32)
                || !ctx.sde.solar_systems.contains_key(&dest_u32)
            {
                return RouteExecutionResult::error(404, "Origin or destination system not found");
            }
            let flag = query_params
                .get("flag")
                .map(|s| s.as_str())
                .unwrap_or("shortest");
            if !matches!(flag, "shortest" | "secure" | "insecure") {
                return RouteExecutionResult::error(
                    400,
                    format!("Invalid route flag '{flag}' (expected shortest, secure, or insecure)"),
                );
            }
            let mut avoid_set: HashSet<u32> = HashSet::new();
            if let Some(avoid_raw) = query_params.get("avoid") {
                for tok in avoid_raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let Ok(sys_id) = tok.parse::<u32>() else {
                        return RouteExecutionResult::error(
                            400,
                            format!("Invalid system ID '{tok}' in avoid parameter"),
                        );
                    };
                    avoid_set.insert(sys_id);
                }
            }
            if orig_u32 == dest_u32 {
                if avoid_set.contains(&orig_u32) {
                    return RouteExecutionResult::error(404, "Origin system is in avoid list");
                }
                return RouteExecutionResult::ok_json(json!([origin]), 86_400);
            }

            let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
            for j in ctx.sde.jumps.values() {
                adj.entry(j.from_solar_system_id)
                    .or_default()
                    .push(j.to_solar_system_id);
            }
            if let Some(conn_raw) = query_params.get("connections") {
                for pair in conn_raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let Some((a_str, b_str)) = pair.split_once('|') else {
                        return RouteExecutionResult::error(
                            400,
                            format!("Invalid connection pair '{pair}' (expected from|to)"),
                        );
                    };
                    let (Ok(a_id), Ok(b_id)) = (a_str.parse::<u32>(), b_str.parse::<u32>()) else {
                        return RouteExecutionResult::error(
                            400,
                            format!("Invalid system ID in connection pair '{pair}'"),
                        );
                    };
                    adj.entry(a_id).or_default().push(b_id);
                }
            }
            for neighbors in adj.values_mut() {
                neighbors.sort_unstable();
                neighbors.dedup();
            }

            // Dijkstra shortest path weighted by flag ("shortest" | "secure" | "insecure").
            let mut dist: HashMap<u32, u64> = HashMap::new();
            let mut parent: HashMap<u32, u32> = HashMap::new();
            let mut heap: std::collections::BinaryHeap<std::cmp::Reverse<(u64, u32)>> =
                std::collections::BinaryHeap::new();
            if !avoid_set.contains(&orig_u32) {
                dist.insert(orig_u32, 0);
                heap.push(std::cmp::Reverse((0, orig_u32)));
            }

            let mut found = false;
            while let Some(std::cmp::Reverse((d, curr))) = heap.pop() {
                if curr == dest_u32 {
                    found = true;
                    break;
                }
                if d > *dist.get(&curr).unwrap_or(&u64::MAX) {
                    continue;
                }
                if let Some(neighbors) = adj.get(&curr) {
                    for &nxt in neighbors {
                        if avoid_set.contains(&nxt) {
                            continue;
                        }
                        let sec = ctx
                            .sde
                            .solar_systems
                            .get(&nxt)
                            .map(|s| s.security_status)
                            .unwrap_or(0.0);
                        let step_cost: u64 = match flag {
                            "secure" => {
                                if sec >= 0.45 {
                                    1
                                } else {
                                    100
                                }
                            }
                            "insecure" => {
                                if sec < 0.45 {
                                    1
                                } else {
                                    100
                                }
                            }
                            _ => 1,
                        };
                        let next_d = d.saturating_add(step_cost);
                        if next_d < *dist.get(&nxt).unwrap_or(&u64::MAX) {
                            dist.insert(nxt, next_d);
                            parent.insert(nxt, curr);
                            heap.push(std::cmp::Reverse((next_d, nxt)));
                        }
                    }
                }
            }

            if !found {
                return RouteExecutionResult::error(
                    404,
                    format!("No stargate route found between {origin} and {destination}"),
                );
            }

            let mut path = vec![dest_u32 as i64];
            let mut cur = dest_u32;
            while let Some(&prev) = parent.get(&cur) {
                path.push(prev as i64);
                if prev == orig_u32 {
                    break;
                }
                cur = prev;
            }
            path.reverse();
            RouteExecutionResult::ok_json(json!(path), 86_400)
        }

        // 27. Search tag
        ("GET", "/characters/{character_id}/search") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let has_char = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !has_char {
                return RouteExecutionResult::error(404, format!("Character {char_id} not found"));
            }
            let Some(search_raw) = query_params.get("search") else {
                return RouteExecutionResult::error(400, "Missing required query param 'search'");
            };
            let Some(cats_raw) = query_params.get("categories") else {
                return RouteExecutionResult::error(
                    400,
                    "Missing required query param 'categories'",
                );
            };
            let query_lc = search_raw.trim().to_ascii_lowercase();
            if query_lc.is_empty() {
                return RouteExecutionResult::error(400, "Query param 'search' must not be empty");
            }
            let strict = query_params
                .get("strict")
                .map(|s| s == "true")
                .unwrap_or(false);
            let matches_name = |candidate: &str| -> bool {
                let cand_lc = candidate.to_ascii_lowercase();
                if strict {
                    cand_lc == query_lc
                } else {
                    cand_lc.contains(&query_lc)
                }
            };

            let mut result_obj = serde_json::Map::new();
            for cat in cats_raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                match cat {
                    "solar_system" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .solar_systems
                            .values()
                            .filter(|s| matches_name(&s.name))
                            .map(|s| s.solar_system_id as i64)
                            .collect();
                        ids.sort_unstable();
                        result_obj.insert("solar_system".into(), json!(ids));
                    }
                    "region" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .regions
                            .values()
                            .filter(|r| matches_name(&r.name))
                            .map(|r| r.region_id as i64)
                            .collect();
                        ids.sort_unstable();
                        result_obj.insert("region".into(), json!(ids));
                    }
                    "constellation" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .constellations
                            .values()
                            .filter(|c| matches_name(&c.name))
                            .map(|c| c.constellation_id as i64)
                            .collect();
                        ids.sort_unstable();
                        result_obj.insert("constellation".into(), json!(ids));
                    }
                    "inventory_type" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .types
                            .values()
                            .filter(|t| t.published && matches_name(&t.name))
                            .map(|t| t.type_id as i64)
                            .collect();
                        ids.sort_unstable();
                        ids.truncate(100);
                        result_obj.insert("inventory_type".into(), json!(ids));
                    }
                    "station" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .solar_systems
                            .values()
                            .flat_map(|s| s.stations.iter())
                            .filter(|st| matches_name(&st.name))
                            .map(|st| st.station_id)
                            .collect();
                        ids.sort_unstable();
                        ids.truncate(100);
                        result_obj.insert("station".into(), json!(ids));
                    }
                    "structure" => {
                        let mut ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                            snap.structures
                                .values()
                                .filter(|st| matches_name(&st.name))
                                .map(|st| st.structure_id)
                                .collect()
                        });
                        ids.sort_unstable();
                        result_obj.insert("structure".into(), json!(ids));
                    }
                    "character" => {
                        let mut ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                            snap.characters
                                .values()
                                .filter(|ch| matches_name(&ch.name))
                                .map(|ch| ch.character_id)
                                .collect()
                        });
                        ids.sort_unstable();
                        result_obj.insert("character".into(), json!(ids));
                    }
                    "corporation" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .npc_corporations
                            .values()
                            .filter(|c| matches_name(&c.name))
                            .map(|c| c.corporation_id)
                            .collect();
                        ctx.db.with_snapshot(|snap| {
                            for c in snap.corporations.values() {
                                if matches_name(&c.name) {
                                    ids.push(c.corporation_id);
                                }
                            }
                        });
                        ids.sort_unstable();
                        ids.dedup();
                        result_obj.insert("corporation".into(), json!(ids));
                    }
                    "alliance" => {
                        let mut ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                            snap.alliances
                                .values()
                                .filter(|a| matches_name(&a.name))
                                .map(|a| a.alliance_id)
                                .collect()
                        });
                        ids.sort_unstable();
                        result_obj.insert("alliance".into(), json!(ids));
                    }
                    "faction" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .factions
                            .values()
                            .filter(|f| matches_name(&f.name))
                            .map(|f| f.faction_id as i64)
                            .collect();
                        ids.sort_unstable();
                        result_obj.insert("faction".into(), json!(ids));
                    }
                    "agent" => {
                        let mut ids: Vec<i64> = ctx
                            .sde
                            .agents
                            .values()
                            .filter(|a| matches_name(&a.name))
                            .map(|a| a.agent_id)
                            .collect();
                        ids.sort_unstable();
                        ids.truncate(100);
                        result_obj.insert("agent".into(), json!(ids));
                    }
                    other => {
                        return RouteExecutionResult::error(
                            400,
                            format!("Unsupported search category '{other}'"),
                        );
                    }
                }
            }
            RouteExecutionResult::ok_json(Value::Object(result_obj), 3600)
        }

        // 28. Sovereignty tag
        ("GET", "/sovereignty/map") => ctx.db.with_snapshot(|snap| {
            let mut sys_ids: Vec<_> = ctx.sde.solar_systems.keys().copied().collect();
            sys_ids.sort_unstable();
            let mut out = Vec::with_capacity(sys_ids.len());
            for sid in sys_ids {
                let mut entry = json!({
                    "system_id": sid as i64,
                });
                let sov_alliance = snap
                    .system_states
                    .get(&sid)
                    .and_then(|st| st.sov_alliance_id);
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
        }),

        // 29. Universe tag
        ("GET", "/universe/categories") => {
            let mut ids: Vec<i64> = ctx.sde.categories.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/universe/categories/{category_id}") => {
            let cat_id = match parse_path_i64(path_params, "category_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(cat_u32) = u32::try_from(cat_id) else {
                return RouteExecutionResult::error(404, format!("Category {cat_id} not found"));
            };
            let Some(cat) = ctx.sde.categories.get(&cat_u32) else {
                return RouteExecutionResult::error(404, format!("Category {cat_id} not found"));
            };
            let mut groups: Vec<i64> = ctx
                .sde
                .groups
                .values()
                .filter(|g| g.category_id == cat_u32)
                .map(|g| g.group_id as i64)
                .collect();
            groups.sort_unstable();
            RouteExecutionResult::ok_json(
                json!({
                    "category_id": cat.category_id as i64,
                    "name": cat.name,
                    "published": cat.published,
                    "groups": groups,
                }),
                3600,
            )
        }
        ("GET", "/universe/groups") => {
            let mut ids: Vec<i64> = ctx.sde.groups.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            match paginate_slice(&ids, page, 1000) {
                Ok((page_ids, total_pages)) => {
                    RouteExecutionResult::ok_paginated(json!(page_ids), 3600, total_pages)
                }
                Err(e) => e,
            }
        }
        ("GET", "/universe/groups/{group_id}") => {
            let grp_id = match parse_path_i64(path_params, "group_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(grp_u32) = u32::try_from(grp_id) else {
                return RouteExecutionResult::error(404, format!("Group {grp_id} not found"));
            };
            let Some(grp) = ctx.sde.groups.get(&grp_u32) else {
                return RouteExecutionResult::error(404, format!("Group {grp_id} not found"));
            };
            let mut types: Vec<i64> = ctx
                .sde
                .types
                .values()
                .filter(|t| t.group_id == grp_u32)
                .map(|t| t.type_id as i64)
                .collect();
            types.sort_unstable();
            RouteExecutionResult::ok_json(
                json!({
                    "group_id": grp.group_id as i64,
                    "category_id": grp.category_id as i64,
                    "name": grp.name,
                    "published": grp.published,
                    "types": types,
                }),
                3600,
            )
        }
        ("GET", "/universe/types") => {
            let mut ids: Vec<i64> = ctx.sde.types.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            match paginate_slice(&ids, page, 1000) {
                Ok((page_ids, total_pages)) => {
                    RouteExecutionResult::ok_paginated(json!(page_ids), 3600, total_pages)
                }
                Err(e) => e,
            }
        }
        ("GET", "/universe/regions") => {
            let mut ids: Vec<i64> = ctx.sde.regions.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/universe/regions/{region_id}") => {
            let region_id = match parse_path_i64(path_params, "region_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(reg_u32) = u32::try_from(region_id) else {
                return RouteExecutionResult::error(404, format!("Region {region_id} not found"));
            };
            let Some(reg) = ctx.sde.regions.get(&reg_u32) else {
                return RouteExecutionResult::error(404, format!("Region {region_id} not found"));
            };
            let mut constellations: Vec<i64> = ctx
                .sde
                .constellations
                .values()
                .filter(|c| c.region_id == reg_u32)
                .map(|c| c.constellation_id as i64)
                .collect();
            constellations.sort_unstable();
            RouteExecutionResult::ok_json(
                json!({
                    "region_id": reg.region_id as i64,
                    "name": reg.name,
                    "constellations": constellations,
                }),
                3600,
            )
        }
        ("GET", "/universe/constellations") => {
            let mut ids: Vec<i64> = ctx.sde.constellations.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/universe/constellations/{constellation_id}") => {
            let const_id = match parse_path_i64(path_params, "constellation_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(const_u32) = u32::try_from(const_id) else {
                return RouteExecutionResult::error(
                    404,
                    format!("Constellation {const_id} not found"),
                );
            };
            let Some(con) = ctx.sde.constellations.get(&const_u32) else {
                return RouteExecutionResult::error(
                    404,
                    format!("Constellation {const_id} not found"),
                );
            };
            let mut systems: Vec<i64> = ctx
                .sde
                .solar_systems
                .values()
                .filter(|s| s.constellation_id == const_u32)
                .map(|s| s.solar_system_id as i64)
                .collect();
            systems.sort_unstable();
            RouteExecutionResult::ok_json(
                json!({
                    "constellation_id": con.constellation_id as i64,
                    "region_id": con.region_id as i64,
                    "name": con.name,
                    "position": {
                        "x": con.center[0],
                        "y": con.center[1],
                        "z": con.center[2],
                    },
                    "systems": systems,
                }),
                3600,
            )
        }
        ("GET", "/universe/systems") => {
            let mut ids: Vec<i64> = ctx.sde.solar_systems.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/universe/systems/{system_id}") => {
            let sys_id = match parse_path_i64(path_params, "system_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(sys_u32) = u32::try_from(sys_id) else {
                return RouteExecutionResult::error(404, format!("Solar system {sys_id} not found"));
            };
            let Some(sys) = ctx.sde.solar_systems.get(&sys_u32) else {
                return RouteExecutionResult::error(404, format!("Solar system {sys_id} not found"));
            };
            let mut stargates: Vec<i64> = ctx
                .sde
                .jumps
                .values()
                .filter(|j| j.from_solar_system_id == sys_u32)
                .map(|j| j.stargate_id)
                .collect();
            stargates.sort_unstable();
            let mut stations: Vec<i64> = sys.stations.iter().map(|s| s.station_id).collect();
            stations.sort_unstable();
            let planets: Vec<Value> = sys
                .planet_ids
                .iter()
                .map(|&pid| json!({ "planet_id": pid }))
                .collect();
            const METERS_PER_LIGHT_YEAR: f64 = 9_460_730_472_580_800.0;
            RouteExecutionResult::ok_json(
                json!({
                    "system_id": sys.solar_system_id as i64,
                    "constellation_id": sys.constellation_id as i64,
                    "name": sys.name,
                    "security_status": sys.security_status,
                    "position": {
                        "x": sys.galactic_pos_ly[0] * METERS_PER_LIGHT_YEAR,
                        "y": sys.galactic_pos_ly[1] * METERS_PER_LIGHT_YEAR,
                        "z": sys.galactic_pos_ly[2] * METERS_PER_LIGHT_YEAR,
                    },
                    "planets": planets,
                    "stargates": stargates,
                    "stations": stations,
                }),
                3600,
            )
        }

        // 30. User Interface tag
        ("POST", "/ui/autopilot/waypoint") => {
            let Some(dest_str) = query_params.get("destination_id") else {
                return RouteExecutionResult::error(
                    400,
                    "Missing required query parameter 'destination_id'",
                );
            };
            let Ok(destination_id) = dest_str.parse::<i64>() else {
                return RouteExecutionResult::error(
                    400,
                    format!("Invalid integer destination_id '{dest_str}'"),
                );
            };
            let _add_to_beginning = match query_params.get("add_to_beginning").map(|s| s.as_str()) {
                Some("true") => true,
                Some("false") => false,
                _ => {
                    return RouteExecutionResult::error(
                        400,
                        "Missing or invalid boolean query parameter 'add_to_beginning'",
                    );
                }
            };
            let _clear_other_waypoints =
                match query_params.get("clear_other_waypoints").map(|s| s.as_str()) {
                    Some("true") => true,
                    Some("false") => false,
                    _ => {
                        return RouteExecutionResult::error(
                            400,
                            "Missing or invalid boolean query parameter 'clear_other_waypoints'",
                        );
                    }
                };
            let is_known_destination = u32::try_from(destination_id)
                .ok()
                .is_some_and(|sid| ctx.sde.solar_systems.contains_key(&sid))
                || ctx
                    .sde
                    .solar_systems
                    .values()
                    .any(|s| s.stations.iter().any(|st| st.station_id == destination_id))
                || ctx
                    .db
                    .with_snapshot(|snap| snap.structures.contains_key(&destination_id));
            if !is_known_destination {
                return RouteExecutionResult::error(
                    404,
                    format!("Destination {destination_id} is not a known solar system, station, or structure"),
                );
            }
            RouteExecutionResult::error(
                501,
                "Live client UI session routing for /ui/autopilot/waypoint is not active (ESI-7)",
            )
        }

        // 31. Wars tag
        ("GET", "/wars") => {
            let mut war_ids: Vec<i64> = ctx
                .db
                .with_snapshot(|snap| snap.wars.keys().copied().collect());
            war_ids.sort_by(|a, b| b.cmp(a));
            RouteExecutionResult::ok_json(json!(war_ids), 3600)
        }

        // Any other defined OpenAPI operation returns 501 Not Implemented until ESI-3..ESI-7.
        _ => RouteExecutionResult::error(
            501,
            format!("ESI operation {method} {path_pattern} is not yet implemented"),
        ),
    }
}
