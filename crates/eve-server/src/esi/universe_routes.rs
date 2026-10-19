//! `esi::universe_routes` — SDE- and `WorldRepository`-backed ESI route handlers for Routes,
//! Search, Sovereignty, Universe, and User Interface tags (`ESI-3`, `ESI-7`).

use crate::esi::sample_routes::{
    open_sde_sqlite, paginate_slice, parse_path_i64, RouteExecutionResult,
};
use crate::router::ServerContext;
use eve_db::{SsoTokenClaims, UpwellStructureRecord, WorldStateSnapshot};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

type JumpAdjacencyMap = HashMap<u32, Vec<u32>>;
static JUMP_ADJ_CACHE: OnceLock<Mutex<HashMap<usize, Arc<JumpAdjacencyMap>>>> = OnceLock::new();

fn cached_base_jump_adjacency(ctx: &ServerContext) -> Arc<JumpAdjacencyMap> {
    let key = ctx.sde.jumps.len();
    let cache = JUMP_ADJ_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(existing) = guard.get(&key) {
            return Arc::clone(existing);
        }
    }
    let mut adj: JumpAdjacencyMap = HashMap::new();
    for j in ctx.sde.jumps.values() {
        adj.entry(j.from_solar_system_id)
            .or_default()
            .push(j.to_solar_system_id);
    }
    for neighbors in adj.values_mut() {
        neighbors.sort_unstable();
        neighbors.dedup();
    }
    let arc_adj = Arc::new(adj);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, Arc::clone(&arc_adj));
    }
    arc_adj
}

/// Evaluates whether `char_id` is authorized by ACL to view or dock at `st`.
/// Denies destroyed structures, pilots with negative standing toward `st.owner_corp_id`,
/// or pilots whose corporation is in an active war with `st.owner_corp_id`.
pub fn character_has_structure_acl_access(
    snap: &WorldStateSnapshot,
    char_id: i64,
    st: &UpwellStructureRecord,
) -> bool {
    if st.state == "Destroyed" {
        return false;
    }
    let Some(ch) = snap.characters.get(&char_id) else {
        return false;
    };
    if ch.corporation_id == st.owner_corp_id {
        return true;
    }
    if ch.alliance_id.is_some()
        && snap
            .corporations
            .get(&st.owner_corp_id)
            .and_then(|c| c.alliance_id)
            == ch.alliance_id
    {
        return true;
    }
    let standing = ch.standings.get(&st.owner_corp_id).copied().unwrap_or(0.0);
    if standing < 0.0 {
        return false;
    }
    let at_war = snap.wars.values().any(|w| {
        w.active
            && ((w.aggressor_corp_id == ch.corporation_id && w.defender_corp_id == st.owner_corp_id)
                || (w.aggressor_corp_id == st.owner_corp_id
                    && w.defender_corp_id == ch.corporation_id))
    });
    !at_war
}

/// Dispatches Routes, Search, Sovereignty, Universe, and User Interface OpenAPI operations.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn dispatch_universe_and_nav(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    page: usize,
    req_body: &[u8],
) -> RouteExecutionResult {
    match (method, path_pattern) {
        // 1. Routes tag
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

            let base_adj = cached_base_jump_adjacency(ctx);
            let custom_adj: Option<JumpAdjacencyMap> = if let Some(conn_raw) =
                query_params.get("connections")
            {
                let mut adj = (*base_adj).clone();
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
                for neighbors in adj.values_mut() {
                    neighbors.sort_unstable();
                    neighbors.dedup();
                }
                Some(adj)
            } else {
                None
            };
            let adj_ref: &JumpAdjacencyMap = custom_adj.as_ref().unwrap_or(&base_adj);

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
                if let Some(neighbors) = adj_ref.get(&curr) {
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

        // 2. Search tag
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
                                .filter(|st| {
                                    character_has_structure_acl_access(snap, char_id, st)
                                        && matches_name(&st.name)
                                })
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

        // 3. Sovereignty tag
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
        // parity: esi:GET /sovereignty/structures, feat:esi.sovereignty.structures
        ("GET", "/sovereignty/structures") => {
            const MIL_IND_ADM_BONUS: [f64; 6] = [0.0, 0.6, 1.2, 1.7, 2.1, 2.5];
            const STRAT_ADM_BONUS: [f64; 6] = [0.0, 0.4, 0.6, 0.8, 0.9, 1.0];
            ctx.db.with_snapshot(|snap| {
                let mut structs: Vec<_> = snap
                    .structures
                    .values()
                    .filter(|st| {
                        st.state != "Destroyed"
                            && (st.type_id == 32458 || st.type_id == 35841)
                            && ctx
                                .sde
                                .solar_systems
                                .get(&st.solar_system_id)
                                .map(|sys| sys.security_status <= 0.0)
                                .unwrap_or(false)
                    })
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
                            let mil = MIL_IND_ADM_BONUS[usize::from(s.military_index.min(5))];
                            let ind = MIL_IND_ADM_BONUS[usize::from(s.industrial_index.min(5))];
                            let strat = STRAT_ADM_BONUS[usize::from(s.strategic_index.min(5))];
                            ((1.0 + mil + ind + strat).clamp(1.0, 6.0) * 10.0).round() / 10.0
                        })
                        .unwrap_or(1.0);
                    let stored_window = if st.state == "CommandNodeEvent" {
                        None
                    } else if let Some(&(v_start, v_end)) =
                        snap.structure_vulnerability_windows.get(&st.structure_id)
                    {
                        if v_start > 0 && v_end > v_start {
                            Some((v_start, v_end))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let mut entry = json!({
                        "alliance_id": aid,
                        "solar_system_id": st.solar_system_id as i64,
                        "structure_id": st.structure_id,
                        "structure_type_id": st.type_id as i64,
                    });
                    if let Some((vuln_start, vuln_end)) = stored_window {
                        entry["vulnerability_occupancy_level"] = json!(adm);
                        entry["vulnerable_start_time"] =
                            json!(crate::esi::sample_routes::format_iso8601_utc(vuln_start));
                        entry["vulnerable_end_time"] =
                            json!(crate::esi::sample_routes::format_iso8601_utc(vuln_end));
                    }
                    out.push(entry);
                }
                RouteExecutionResult::ok_json(json!(out), 120)
            })
        }

        // 4. Universe tag
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
        ("GET", "/universe/types/{type_id}") => {
            let tid = match parse_path_i64(path_params, "type_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(tid_u32) = u32::try_from(tid) else {
                return RouteExecutionResult::error(404, format!("Type {tid} not found"));
            };
            let Some(t) = ctx.sde.types.get(&tid_u32) else {
                return RouteExecutionResult::error(404, format!("Type {tid} not found"));
            };
            let mut attr_keys: Vec<_> = t.default_attributes.keys().copied().collect();
            attr_keys.sort_unstable();
            let dogma_attributes: Vec<Value> = attr_keys
                .into_iter()
                .map(|aid| {
                    json!({
                        "attribute_id": aid as i64,
                        "value": t.default_attributes[&aid],
                    })
                })
                .collect();
            let mut eff_ids = t.effect_ids.clone();
            eff_ids.sort_unstable();
            eff_ids.dedup();
            let default_eid = t.default_effect_id;
            let dogma_effects: Vec<Value> = eff_ids
                .into_iter()
                .map(|eid| {
                    json!({
                        "effect_id": eid as i64,
                        "is_default": Some(eid) == default_eid,
                    })
                })
                .collect();
            let mut obj = json!({
                "type_id": t.type_id as i64,
                "name": t.name,
                "description": t.description,
                "published": t.published,
                "group_id": t.group_id as i64,
                "mass": t.mass,
                "volume": t.volume,
                "capacity": t.capacity,
                "portion_size": t.portion_size as i64,
                "radius": t.radius,
                "dogma_attributes": dogma_attributes,
                "dogma_effects": dogma_effects,
            });
            if let Some(mg) = t.market_group_id {
                obj["market_group_id"] = json!(mg as i64);
            }
            RouteExecutionResult::ok_json(obj, 3600)
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
            let celestials = ctx.sde.celestials_in_system(sys_u32);
            let star_id = celestials
                .iter()
                .find(|c| c.group_id == 6)
                .map(|c| c.celestial_id);
            let planets: Vec<Value> = sys
                .planet_ids
                .iter()
                .map(|&pid| {
                    let moons: Vec<i64> = celestials
                        .iter()
                        .filter(|c| c.group_id == 8 && c.orbit_id == Some(pid))
                        .map(|c| c.celestial_id)
                        .collect();
                    let belts: Vec<i64> = celestials
                        .iter()
                        .filter(|c| c.group_id == 9 && c.orbit_id == Some(pid))
                        .map(|c| c.celestial_id)
                        .collect();
                    let mut p_obj = json!({ "planet_id": pid });
                    if !moons.is_empty() {
                        p_obj["moons"] = json!(moons);
                    }
                    if !belts.is_empty() {
                        p_obj["asteroid_belts"] = json!(belts);
                    }
                    p_obj
                })
                .collect();
            const METERS_PER_LIGHT_YEAR: f64 = 9_460_730_472_580_800.0;
            let mut obj = json!({
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
            });
            if let Some(sid) = star_id {
                obj["star_id"] = json!(sid);
            }
            if let Some(conn) = open_sde_sqlite(ctx) {
                if let Ok(Some(sec_class)) = conn.query_row(
                    "SELECT security_class FROM sde_solar_systems WHERE system_id = ?1",
                    [sys_id],
                    |r| r.get::<_, Option<String>>(0),
                ) {
                    if !sec_class.is_empty() {
                        obj["security_class"] = json!(sec_class);
                    }
                }
            }
            RouteExecutionResult::ok_json(obj, 3600)
        }
        ("GET", "/universe/planets/{planet_id}") => {
            let pid = match parse_path_i64(path_params, "planet_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Some(conn) = open_sde_sqlite(ctx) else {
                return RouteExecutionResult::error(404, format!("Planet {pid} not found"));
            };
            let row = conn.query_row(
                "SELECT celestial_id, system_id, type_id, name, x, y, z FROM sde_celestials WHERE celestial_id = ?1 AND group_id = 7",
                [pid],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, f64>(4)?,
                        r.get::<_, f64>(5)?,
                        r.get::<_, f64>(6)?,
                    ))
                },
            );
            match row {
                Ok((planet_id, system_id, type_id, name, x, y, z)) => {
                    RouteExecutionResult::ok_json(
                        json!({
                            "planet_id": planet_id,
                            "system_id": system_id,
                            "type_id": type_id,
                            "name": name,
                            "position": { "x": x, "y": y, "z": z },
                        }),
                        3600,
                    )
                }
                Err(_) => RouteExecutionResult::error(404, format!("Planet {pid} not found")),
            }
        }
        ("GET", "/universe/moons/{moon_id}") => {
            let mid = match parse_path_i64(path_params, "moon_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Some(conn) = open_sde_sqlite(ctx) else {
                return RouteExecutionResult::error(404, format!("Moon {mid} not found"));
            };
            let row = conn.query_row(
                "SELECT celestial_id, system_id, name, x, y, z FROM sde_celestials WHERE celestial_id = ?1 AND group_id = 8",
                [mid],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, f64>(3)?,
                        r.get::<_, f64>(4)?,
                        r.get::<_, f64>(5)?,
                    ))
                },
            );
            match row {
                Ok((moon_id, system_id, name, x, y, z)) => RouteExecutionResult::ok_json(
                    json!({
                        "moon_id": moon_id,
                        "system_id": system_id,
                        "name": name,
                        "position": { "x": x, "y": y, "z": z },
                    }),
                    3600,
                ),
                Err(_) => RouteExecutionResult::error(404, format!("Moon {mid} not found")),
            }
        }
        ("GET", "/universe/asteroid_belts/{asteroid_belt_id}") => {
            let bid = match parse_path_i64(path_params, "asteroid_belt_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Some(conn) = open_sde_sqlite(ctx) else {
                return RouteExecutionResult::error(404, format!("Asteroid belt {bid} not found"));
            };
            let row = conn.query_row(
                "SELECT system_id, name, x, y, z FROM sde_celestials WHERE celestial_id = ?1 AND group_id = 9",
                [bid],
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, f64>(2)?,
                        r.get::<_, f64>(3)?,
                        r.get::<_, f64>(4)?,
                    ))
                },
            );
            match row {
                Ok((system_id, name, x, y, z)) => RouteExecutionResult::ok_json(
                    json!({
                        "system_id": system_id,
                        "name": name,
                        "position": { "x": x, "y": y, "z": z },
                    }),
                    3600,
                ),
                Err(_) => {
                    RouteExecutionResult::error(404, format!("Asteroid belt {bid} not found"))
                }
            }
        }
        ("GET", "/universe/stargates/{stargate_id}") => {
            let gid = match parse_path_i64(path_params, "stargate_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            if let Some(conn) = open_sde_sqlite(ctx) {
                let row = conn.query_row(
                    "SELECT stargate_id, system_id, destination_gate_id, destination_system_id, type_id, name, x, y, z FROM sde_stargates WHERE stargate_id = ?1",
                    [gid],
                    |r| {
                        Ok((
                            r.get::<_, i64>(0)?,
                            r.get::<_, i64>(1)?,
                            r.get::<_, i64>(2)?,
                            r.get::<_, i64>(3)?,
                            r.get::<_, i64>(4)?,
                            r.get::<_, String>(5)?,
                            r.get::<_, f64>(6)?,
                            r.get::<_, f64>(7)?,
                            r.get::<_, f64>(8)?,
                        ))
                    },
                );
                if let Ok((
                    stargate_id,
                    system_id,
                    dest_gate_id,
                    dest_sys_id,
                    type_id,
                    name,
                    x,
                    y,
                    z,
                )) = row
                {
                    return RouteExecutionResult::ok_json(
                        json!({
                            "stargate_id": stargate_id,
                            "system_id": system_id,
                            "type_id": type_id,
                            "name": name,
                            "position": { "x": x, "y": y, "z": z },
                            "destination": {
                                "stargate_id": dest_gate_id,
                                "system_id": dest_sys_id,
                            },
                        }),
                        3600,
                    );
                }
            }
            let Some(j) = ctx.sde.jumps.get(&gid) else {
                return RouteExecutionResult::error(404, format!("Stargate {gid} not found"));
            };
            let dest_name = ctx
                .sde
                .solar_systems
                .get(&j.to_solar_system_id)
                .map(|s| format!("Stargate ({})", s.name))
                .unwrap_or_else(|| format!("Stargate ({})", j.to_solar_system_id));
            RouteExecutionResult::ok_json(
                json!({
                    "stargate_id": j.stargate_id,
                    "system_id": j.from_solar_system_id as i64,
                    "type_id": 16,
                    "name": dest_name,
                    "position": {
                        "x": j.position.x,
                        "y": j.position.y,
                        "z": j.position.z,
                    },
                    "destination": {
                        "stargate_id": j.destination_stargate_id,
                        "system_id": j.to_solar_system_id as i64,
                    },
                }),
                3600,
            )
        }
        ("GET", "/universe/structures") => {
            let require_active_service = if let Some(filter) = query_params.get("filter") {
                if !matches!(filter.as_str(), "market" | "manufacturing_basic") {
                    return RouteExecutionResult::error(
                        400,
                        format!("Invalid structure filter '{filter}'"),
                    );
                }
                true
            } else {
                false
            };
            ctx.db.with_snapshot(|snap| {
                let mut ids: Vec<i64> = snap
                    .structures
                    .values()
                    .filter(|st| {
                        st.state != "Destroyed"
                            && (!require_active_service
                                || (st.quantum_core_installed && st.fuel_blocks > 0))
                    })
                    .map(|st| st.structure_id)
                    .collect();
                ids.sort_unstable();
                RouteExecutionResult::ok_json(json!(ids), 3600)
            })
        }
        ("GET", "/universe/structures/{structure_id}") => {
            let sid = match parse_path_i64(path_params, "structure_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let caller_char_id = claims.map(|c| c.character_id).unwrap_or(0);
            ctx.db.with_snapshot(|snap| {
                let Some(st) = snap.structures.get(&sid) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Structure {sid} not found"),
                    );
                };
                if !character_has_structure_acl_access(snap, caller_char_id, st) {
                    return RouteExecutionResult::error(
                        403,
                        format!("Character {caller_char_id} is not on the ACL for structure {sid}"),
                    );
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "name": st.name,
                        "owner_id": st.owner_corp_id,
                        "solar_system_id": st.solar_system_id as i64,
                        "type_id": st.type_id as i64,
                        "position": {
                            "x": st.position[0],
                            "y": st.position[1],
                            "z": st.position[2],
                        },
                    }),
                    3600,
                )
            })
        }
        ("GET", "/universe/factions") => {
            let mut station_counts: HashMap<u32, (i64, HashSet<u32>)> = HashMap::new();
            for sys in ctx.sde.solar_systems.values() {
                for st in &sys.stations {
                    let fid_opt = st
                        .owner_faction_id
                        .and_then(|fid| u32::try_from(fid).ok())
                        .or_else(|| {
                            ctx.sde
                                .npc_corporations
                                .get(&st.owner_corporation_id)
                                .and_then(|c| c.faction_id)
                        });
                    if let Some(fid) = fid_opt {
                        let entry = station_counts.entry(fid).or_insert((0, HashSet::new()));
                        entry.0 += 1;
                        entry.1.insert(sys.solar_system_id);
                    }
                }
            }
            let mut facs: Vec<_> = ctx.sde.factions.values().collect();
            facs.sort_by_key(|f| f.faction_id);
            let out: Vec<Value> = facs
                .into_iter()
                .map(|f| {
                    let (st_cnt, sys_set) = station_counts
                        .get(&f.faction_id)
                        .map(|(c, s)| (*c, s.len() as i64))
                        .unwrap_or((0, 0));
                    let mut obj = json!({
                        "faction_id": f.faction_id as i64,
                        "name": f.name,
                        "description": f.description,
                        "size_factor": f.size_factor,
                        "station_count": st_cnt,
                        "station_system_count": sys_set,
                        "is_unique": true,
                    });
                    if let Some(cid) = f.corporation_id {
                        obj["corporation_id"] = json!(cid);
                    }
                    if let Some(mcid) = f.militia_corporation_id {
                        obj["militia_corporation_id"] = json!(mcid);
                    }
                    if let Some(sid) = f.solar_system_id {
                        obj["solar_system_id"] = json!(sid as i64);
                    }
                    obj
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        }
        ("GET", "/universe/bloodlines") => {
            let mut bls: Vec<_> = ctx.sde.bloodlines.values().collect();
            bls.sort_by_key(|b| b.bloodline_id);
            let out: Vec<Value> = bls
                .into_iter()
                .filter_map(|b| {
                    let ship_type_id = b
                        .ship_type_id
                        .or_else(|| {
                            ctx.sde
                                .races
                                .get(&b.race_id)
                                .and_then(|r| r.corvette_type_id)
                        })? as i64;
                    Some(json!({
                        "bloodline_id": b.bloodline_id as i64,
                        "name": b.name,
                        "description": b.description,
                        "race_id": b.race_id as i64,
                        "ship_type_id": ship_type_id,
                        "corporation_id": b.corporation_id,
                        "charisma": b.charisma,
                        "intelligence": b.intelligence,
                        "memory": b.memory,
                        "perception": b.perception,
                        "willpower": b.willpower,
                    }))
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        }
        ("GET", "/universe/ancestries") => {
            let mut ancs: Vec<_> = ctx.sde.ancestries.values().collect();
            ancs.sort_by_key(|a| a.ancestry_id);
            let out: Vec<Value> = ancs
                .into_iter()
                .map(|a| {
                    let mut obj = json!({
                        "id": a.ancestry_id as i64,
                        "name": a.name,
                        "bloodline_id": a.bloodline_id as i64,
                        "description": a.description,
                    });
                    if !a.short_description.is_empty() {
                        obj["short_description"] = json!(a.short_description);
                    }
                    obj
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        }
        ("POST", "/universe/names") => {
            let ids: Vec<i64> = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return RouteExecutionResult::error(
                        400,
                        format!("Invalid JSON integer array body: {e}"),
                    );
                }
            };
            if ids.is_empty() || ids.len() > 1000 {
                return RouteExecutionResult::error(
                    400,
                    format!("ID array length {} must be between 1 and 1000", ids.len()),
                );
            }
            let mut seen = HashSet::with_capacity(ids.len());
            for &id in &ids {
                if !seen.insert(id) {
                    return RouteExecutionResult::error(
                        400,
                        format!("Duplicate ID {id} in /universe/names request"),
                    );
                }
            }
            ctx.db.with_snapshot(|snap| {
                let mut out = Vec::with_capacity(ids.len());
                for id in ids {
                    let resolved: Option<(&str, String)> = if let Some(al) = snap.alliances.get(&id)
                    {
                        Some(("alliance", al.name.clone()))
                    } else if let Some(ch) = snap.characters.get(&id) {
                        Some(("character", ch.name.clone()))
                    } else if let Some(co) = snap.corporations.get(&id) {
                        Some(("corporation", co.name.clone()))
                    } else if let Some(npc) = ctx.sde.npc_corporations.get(&id) {
                        Some(("corporation", npc.name.clone()))
                    } else if let Some(u) = u32::try_from(id).ok() {
                        if let Some(fac) = ctx.sde.factions.get(&u) {
                            Some(("faction", fac.name.clone()))
                        } else if let Some(reg) = ctx.sde.regions.get(&u) {
                            Some(("region", reg.name.clone()))
                        } else if let Some(con) = ctx.sde.constellations.get(&u) {
                            Some(("constellation", con.name.clone()))
                        } else if let Some(sys) = ctx.sde.solar_systems.get(&u) {
                            Some(("solar_system", sys.name.clone()))
                        } else if let Some(t) = ctx.sde.types.get(&u) {
                            Some(("inventory_type", t.name.clone()))
                        } else {
                            ctx.sde
                                .stations
                                .get(&id)
                                .map(|st| ("station", st.name.clone()))
                        }
                    } else {
                        ctx.sde
                            .stations
                            .get(&id)
                            .map(|st| ("station", st.name.clone()))
                    };
                    let Some((category, name)) = resolved else {
                        return RouteExecutionResult::error(
                            404,
                            format!("Ensure all IDs are valid before resolving; unknown ID {id}"),
                        );
                    };
                    out.push(json!({
                        "id": id,
                        "name": name,
                        "category": category,
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            })
        }
        ("POST", "/universe/ids") => {
            let names: Vec<String> = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return RouteExecutionResult::error(
                        400,
                        format!("Invalid JSON string array body: {e}"),
                    );
                }
            };
            if names.is_empty() || names.len() > 500 {
                return RouteExecutionResult::error(
                    400,
                    format!("Name array length {} must be between 1 and 500", names.len()),
                );
            }
            let mut seen = HashSet::with_capacity(names.len());
            for n in &names {
                let trimmed = n.trim();
                if trimmed.is_empty() {
                    return RouteExecutionResult::error(400, "Name entries must not be empty");
                }
                if !seen.insert(trimmed.to_ascii_lowercase()) {
                    return RouteExecutionResult::error(
                        400,
                        format!("Duplicate name '{trimmed}' in /universe/ids request"),
                    );
                }
            }
            ctx.db.with_snapshot(|snap| {
                let mut agents = Vec::new();
                let mut alliances = Vec::new();
                let mut characters = Vec::new();
                let mut constellations = Vec::new();
                let mut corporations = Vec::new();
                let mut factions = Vec::new();
                let mut inventory_types = Vec::new();
                let mut regions = Vec::new();
                let mut stations = Vec::new();
                let mut systems = Vec::new();

                for raw_name in &names {
                    let target_lc = raw_name.trim().to_ascii_lowercase();
                    for a in snap.alliances.values() {
                        if a.name.to_ascii_lowercase() == target_lc {
                            alliances.push(json!({ "id": a.alliance_id, "name": a.name }));
                        }
                    }
                    for ch in snap.characters.values() {
                        if ch.name.to_ascii_lowercase() == target_lc {
                            characters.push(json!({ "id": ch.character_id, "name": ch.name }));
                        }
                    }
                    for co in snap.corporations.values() {
                        if co.name.to_ascii_lowercase() == target_lc {
                            corporations.push(json!({ "id": co.corporation_id, "name": co.name }));
                        }
                    }
                    for npc in ctx.sde.npc_corporations.values() {
                        if npc.name.to_ascii_lowercase() == target_lc {
                            corporations
                                .push(json!({ "id": npc.corporation_id, "name": npc.name }));
                        }
                    }
                    for ag in ctx.sde.agents.values() {
                        if ag.name.to_ascii_lowercase() == target_lc {
                            agents.push(json!({ "id": ag.agent_id, "name": ag.name }));
                        }
                    }
                    for fac in ctx.sde.factions.values() {
                        if fac.name.to_ascii_lowercase() == target_lc {
                            factions.push(json!({ "id": fac.faction_id as i64, "name": fac.name }));
                        }
                    }
                    for reg in ctx.sde.regions.values() {
                        if reg.name.to_ascii_lowercase() == target_lc {
                            regions.push(json!({ "id": reg.region_id as i64, "name": reg.name }));
                        }
                    }
                    for con in ctx.sde.constellations.values() {
                        if con.name.to_ascii_lowercase() == target_lc {
                            constellations.push(
                                json!({ "id": con.constellation_id as i64, "name": con.name }),
                            );
                        }
                    }
                    for sys in ctx.sde.solar_systems.values() {
                        if sys.name.to_ascii_lowercase() == target_lc {
                            systems
                                .push(json!({ "id": sys.solar_system_id as i64, "name": sys.name }));
                        }
                        for st in &sys.stations {
                            if st.name.to_ascii_lowercase() == target_lc {
                                stations.push(json!({ "id": st.station_id, "name": st.name }));
                            }
                        }
                    }
                    for t in ctx.sde.types.values() {
                        if t.name.to_ascii_lowercase() == target_lc {
                            inventory_types.push(json!({ "id": t.type_id as i64, "name": t.name }));
                        }
                    }
                }

                let mut obj = serde_json::Map::new();
                for (k, v) in [
                    ("agents", agents),
                    ("alliances", alliances),
                    ("characters", characters),
                    ("constellations", constellations),
                    ("corporations", corporations),
                    ("factions", factions),
                    ("inventory_types", inventory_types),
                    ("regions", regions),
                    ("stations", stations),
                    ("systems", systems),
                ] {
                    if !v.is_empty() {
                        obj.insert(k.into(), json!(v));
                    }
                }
                RouteExecutionResult::ok_json(Value::Object(obj), 3600)
            })
        }

        // 5. User Interface tag (ESI-7)
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
                || ctx.sde.stations.contains_key(&destination_id)
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

        // Any other defined OpenAPI operation returns 501 Not Implemented until ESI-5..ESI-7.
        _ => RouteExecutionResult::error(
            501,
            format!("ESI operation {method} {path_pattern} is not yet implemented"),
        ),
    }
}
