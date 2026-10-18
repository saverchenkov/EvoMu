//! `esi::character_routes` — SDE- and `WorldRepository`-backed ESI route handlers for Character,
//! Clones, Skills, Location, Fittings, Assets, Wallet, Loyalty, Calendar, Contacts, and Mail
//! tags (`ESI-4`).

use crate::esi::sample_routes::{
    asset_location_flag_str, fitting_flag_str, format_iso8601_utc, paginate_slice,
    parse_fitting_flag, parse_path_i64, RouteExecutionResult,
};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, MailRecord, SavedFittingRecord, SsoTokenClaims};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

fn parse_unique_id_array(req_body: &[u8], max_items: usize) -> Result<Vec<i64>, RouteExecutionResult> {
    let ids: Vec<i64> = serde_json::from_slice(req_body).map_err(|e| {
        RouteExecutionResult::error(400, format!("Invalid JSON integer array body: {e}"))
    })?;
    if ids.is_empty() || ids.len() > max_items {
        return Err(RouteExecutionResult::error(
            400,
            format!("ID array length {} must be between 1 and {max_items}", ids.len()),
        ));
    }
    let mut seen = HashSet::with_capacity(ids.len());
    for &id in &ids {
        if !seen.insert(id) {
            return Err(RouteExecutionResult::error(
                400,
                format!("Duplicate ID {id} in request body"),
            ));
        }
    }
    Ok(ids)
}

fn resolve_item_coordinates(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    location_id: i64,
) -> [f64; 3] {
    let resolved_loc = snap
        .items
        .get(&location_id)
        .map(|parent| parent.location_id)
        .unwrap_or(location_id);
    if let Some(st) = snap.structures.get(&resolved_loc) {
        return st.position;
    }
    for sys in ctx.sde.solar_systems.values() {
        for st in &sys.stations {
            if st.station_id == resolved_loc {
                return [st.position.x, st.position.y, st.position.z];
            }
        }
    }
    [0.0, 0.0, 0.0]
}

fn build_owner_assets(
    snap: &eve_db::WorldStateSnapshot,
    owner_id: i64,
    page: usize,
) -> RouteExecutionResult {
    let mut items: Vec<_> = snap
        .items
        .values()
        .filter(|i| i.owner_id == owner_id && i.quantity > 0)
        .collect();
    items.sort_by_key(|i| i.item_id);

    let rows: Vec<Value> = items
        .into_iter()
        .filter_map(|item| {
            let flag_str = asset_location_flag_str(item.flag)?;
            let loc_type = if (60_000_000..64_000_000).contains(&item.location_id) {
                "station"
            } else if (30_000_000..33_000_000).contains(&item.location_id) {
                "solar_system"
            } else if snap.items.contains_key(&item.location_id) {
                "item"
            } else {
                "other"
            };
            let mut obj = json!({
                "item_id": item.item_id,
                "type_id": item.type_id as i64,
                "quantity": item.quantity as i64,
                "location_id": item.location_id,
                "location_type": loc_type,
                "location_flag": flag_str,
                "is_singleton": item.singleton,
            });
            if item.is_blueprint_copy {
                obj["is_blueprint_copy"] = json!(true);
            }
            Some(obj)
        })
        .collect();

    match paginate_slice(&rows, page, 1000) {
        Ok((page_rows, total_pages)) => {
            RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
        }
        Err(e) => e,
    }
}

fn build_owner_asset_locations(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    owner_id: i64,
    req_body: &[u8],
) -> RouteExecutionResult {
    let ids = match parse_unique_id_array(req_body, 1000) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut out = Vec::with_capacity(ids.len());
    for item_id in ids {
        let Some(item) = snap
            .items
            .get(&item_id)
            .filter(|i| i.owner_id == owner_id && i.quantity > 0)
        else {
            return RouteExecutionResult::error(
                404,
                format!("Asset item {item_id} not found for owner {owner_id}"),
            );
        };
        let pos = resolve_item_coordinates(ctx, snap, item.location_id);
        out.push(json!({
            "item_id": item.item_id,
            "position": {
                "x": pos[0],
                "y": pos[1],
                "z": pos[2],
            }
        }));
    }
    RouteExecutionResult::ok_json(json!(out), 0)
}

fn build_owner_asset_names(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    owner_id: i64,
    req_body: &[u8],
) -> RouteExecutionResult {
    let ids = match parse_unique_id_array(req_body, 1000) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut out = Vec::with_capacity(ids.len());
    for item_id in ids {
        let Some(item) = snap
            .items
            .get(&item_id)
            .filter(|i| i.owner_id == owner_id && i.quantity > 0)
        else {
            return RouteExecutionResult::error(
                404,
                format!("Asset item {item_id} not found for owner {owner_id}"),
            );
        };
        let name = ctx
            .sde
            .types
            .get(&item.type_id)
            .map(|t| t.name.clone())
            .unwrap_or_else(|| format!("Type {}", item.type_id));
        out.push(json!({
            "item_id": item.item_id,
            "name": name,
        }));
    }
    RouteExecutionResult::ok_json(json!(out), 0)
}

/// Dispatches Character, Clones, Skills, Location, Fittings, Assets, Wallet, Loyalty, Calendar,
/// Contacts, and Mail OpenAPI operations (`ESI-4`).
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn dispatch_character_routes(
    ctx: &ServerContext,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    _query_params: &HashMap<String, String>,
    _claims: Option<&SsoTokenClaims>,
    page: usize,
    req_body: &[u8],
) -> Option<RouteExecutionResult> {
    match (method, path_pattern) {
        // 1. Character tag
        ("POST", "/characters/affiliation") => {
            let ids = match parse_unique_id_array(req_body, 1000) {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let mut out = Vec::with_capacity(ids.len());
                for cid in ids {
                    let Some(ch) = snap.characters.get(&cid) else {
                        return RouteExecutionResult::error(
                            404,
                            format!("Character {cid} not found"),
                        );
                    };
                    let mut entry = json!({
                        "character_id": ch.character_id,
                        "corporation_id": ch.corporation_id,
                    });
                    if let Some(aid) = ch.alliance_id {
                        entry["alliance_id"] = json!(aid);
                    }
                    let faction_id = ch.war_faction_id.or_else(|| {
                        ctx.sde
                            .npc_corporations
                            .get(&ch.corporation_id)
                            .and_then(|c| c.faction_id)
                            .map(i64::from)
                    });
                    if let Some(fid) = faction_id {
                        entry["faction_id"] = json!(fid);
                    }
                    out.push(entry);
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }
        ("GET", "/characters/{character_id}/portrait") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "px64x64": format!("https://images.evetech.net/characters/{char_id}/portrait?tenant=tranquility&size=64"),
                        "px128x128": format!("https://images.evetech.net/characters/{char_id}/portrait?tenant=tranquility&size=128"),
                        "px256x256": format!("https://images.evetech.net/characters/{char_id}/portrait?tenant=tranquility&size=256"),
                        "px512x512": format!("https://images.evetech.net/characters/{char_id}/portrait?tenant=tranquility&size=512"),
                    }),
                    3600,
                )
            }))
        }
        ("GET", "/characters/{character_id}/standings") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut entries: Vec<_> = ch.standings.iter().collect();
                entries.sort_by_key(|(&id, _)| id);
                let mut out = Vec::new();
                for (&from_id, &standing) in entries {
                    let from_type = if u32::try_from(from_id)
                        .map(|fid| ctx.sde.factions.contains_key(&fid))
                        .unwrap_or(false)
                    {
                        "faction"
                    } else if ctx.sde.npc_corporations.contains_key(&from_id) {
                        "npc_corp"
                    } else if ctx.sde.agents.contains_key(&from_id) {
                        "agent"
                    } else {
                        continue;
                    };
                    out.push(json!({
                        "from_id": from_id,
                        "from_type": from_type,
                        "standing": standing,
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }
        ("GET", "/characters/{character_id}/fatigue") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let now = ctx.now_unix();
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut obj = serde_json::Map::new();
                if ch.jump_cooldown_until_unix > 0 {
                    obj.insert(
                        "jump_fatigue_expire_date".into(),
                        json!(format_iso8601_utc(ch.jump_cooldown_until_unix)),
                    );
                } else if ch.jump_fatigue_minutes > 0.0 {
                    let exp = now.saturating_add((ch.jump_fatigue_minutes * 60.0).round() as u64);
                    obj.insert(
                        "jump_fatigue_expire_date".into(),
                        json!(format_iso8601_utc(exp)),
                    );
                }
                if ch.last_clone_jump_unix > 0 {
                    let ts = format_iso8601_utc(ch.last_clone_jump_unix);
                    obj.insert("last_jump_date".into(), json!(ts));
                    obj.insert("last_update_date".into(), json!(ts));
                }
                RouteExecutionResult::ok_json(Value::Object(obj), 300)
            }))
        }
        ("GET", "/characters/{character_id}/blueprints") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut items: Vec<_> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.quantity > 0
                            && (ctx.sde.blueprints.contains_key(&i.type_id)
                                || ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .is_some_and(|t| t.category_id == 9))
                    })
                    .collect();
                items.sort_by_key(|i| i.item_id);

                let rows: Vec<Value> = items
                    .into_iter()
                    .filter_map(|item| {
                        let flag_str = asset_location_flag_str(item.flag)?;
                        let (qty, runs) = if item.is_blueprint_copy {
                            (-2i64, item.blueprint_runs_remaining.unwrap_or(1).max(1) as i64)
                        } else if item.singleton {
                            (-1i64, -1i64)
                        } else {
                            (item.quantity as i64, -1i64)
                        };
                        let me = item
                            .dynamic_attributes
                            .get(&1112)
                            .copied()
                            .unwrap_or(0.0)
                            .round()
                            .clamp(0.0, 10.0) as i64;
                        let te = item
                            .dynamic_attributes
                            .get(&1113)
                            .copied()
                            .unwrap_or(0.0)
                            .round()
                            .clamp(0.0, 20.0) as i64;
                        Some(json!({
                            "item_id": item.item_id,
                            "type_id": item.type_id as i64,
                            "location_id": item.location_id,
                            "location_flag": flag_str,
                            "quantity": qty,
                            "runs": runs,
                            "material_efficiency": me,
                            "time_efficiency": te,
                        }))
                    })
                    .collect();

                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        // 2. Clones tag
        ("GET", "/characters/{character_id}/clones") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut jump_clones = Vec::new();
                if let Some(clones) = snap.jump_clones.get(&char_id) {
                    for jc in clones {
                        let loc_type = if (60_000_000..64_000_000).contains(&jc.location_id) {
                            "station"
                        } else {
                            "structure"
                        };
                        let implants: Vec<i64> = jc.implants.iter().map(|&t| t as i64).collect();
                        jump_clones.push(json!({
                            "jump_clone_id": jc.jump_clone_id,
                            "location_id": jc.location_id,
                            "location_type": loc_type,
                            "implants": implants,
                        }));
                    }
                }
                let home_loc_type = if (60_000_000..64_000_000).contains(&ch.home_station_id) {
                    "station"
                } else {
                    "structure"
                };
                let mut obj = json!({
                    "home_location": {
                        "location_id": ch.home_station_id,
                        "location_type": home_loc_type,
                    },
                    "jump_clones": jump_clones,
                });
                if ch.last_clone_jump_unix > 0 {
                    obj["last_clone_jump_date"] =
                        json!(format_iso8601_utc(ch.last_clone_jump_unix));
                }
                RouteExecutionResult::ok_json(obj, 120)
            }))
        }
        ("GET", "/characters/{character_id}/implants") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut implants: Vec<i64> = snap
                    .active_implants
                    .get(&char_id)
                    .map(|v| v.iter().map(|&tid| tid as i64).collect())
                    .unwrap_or_default();
                for item in snap.items.values() {
                    if item.owner_id == char_id
                        && item.location_id == char_id
                        && item.flag == InventoryFlag::Implant
                        && item.quantity > 0
                    {
                        implants.push(item.type_id as i64);
                    }
                }
                implants.sort_unstable();
                implants.dedup();
                RouteExecutionResult::ok_json(json!(implants), 120)
            }))
        }

        // 3. Skills tag
        ("GET", "/characters/{character_id}/skills") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut skills_vec = Vec::new();
                if let Some(skill_map) = snap.skills.get(&char_id) {
                    let mut sorted: Vec<_> = skill_map.values().collect();
                    sorted.sort_by_key(|s| s.type_id);
                    for s in sorted {
                        skills_vec.push(json!({
                            "skill_id": s.type_id as i64,
                            "trained_skill_level": s.level as i64,
                            "active_skill_level": s.level as i64,
                            "skillpoints_in_skill": s.skill_points.round() as i64,
                        }));
                    }
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "skills": skills_vec,
                        "total_sp": ch.skill_points.round() as i64,
                        "unallocated_sp": ch.free_skill_points.round() as i64,
                    }),
                    120,
                )
            }))
        }
        ("GET", "/characters/{character_id}/attributes") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                RouteExecutionResult::ok_json(
                    json!({
                        "charisma": ch.neural.charisma as i64,
                        "intelligence": ch.neural.intelligence as i64,
                        "memory": ch.neural.memory as i64,
                        "perception": ch.neural.perception as i64,
                        "willpower": ch.neural.willpower as i64,
                        "bonus_remaps": ch.neural.bonus_remaps as i64,
                    }),
                    120,
                )
            }))
        }
        ("GET", "/characters/{character_id}/skillqueue") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut out = Vec::new();
                if let Some(queue) = snap.skill_queues.get(&char_id) {
                    for (idx, entry) in queue.iter().enumerate() {
                        out.push(json!({
                            "queue_position": idx as i64,
                            "skill_id": entry.type_id as i64,
                            "finished_level": entry.target_level as i64,
                            "training_start_sp": entry.start_sp.round() as i64,
                            "level_start_sp": entry.start_sp.round() as i64,
                            "level_end_sp": entry.end_sp.round() as i64,
                        }));
                    }
                }
                RouteExecutionResult::ok_json(json!(out), 120)
            }))
        }

        // 4. Location tag
        ("GET", "/characters/{character_id}/location") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut obj = json!({
                    "solar_system_id": ch.solar_system_id as i64,
                });
                if let Some(stid) = ch.structure_id {
                    obj["structure_id"] = json!(stid);
                } else if let Some(sid) = ch.station_id {
                    if sid >= 100_000_000 {
                        obj["structure_id"] = json!(sid);
                    } else {
                        obj["station_id"] = json!(sid);
                    }
                }
                RouteExecutionResult::ok_json(obj, 5)
            }))
        }
        ("GET", "/characters/{character_id}/online") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let user_id = ctx
                .db
                .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id));
            let Some(uid) = user_id else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            };
            let online = ctx
                .db
                .get_online_session(uid)
                .is_some_and(|s| s.character_id == Some(char_id) || s.character_id.is_none());
            Some(RouteExecutionResult::ok_json(json!({ "online": online }), 60))
        }
        ("GET", "/characters/{character_id}/ship") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let Some(ship) = snap
                    .items
                    .get(&ch.active_ship_id)
                    .filter(|i| i.owner_id == char_id)
                else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Active ship {} not found for character {char_id}", ch.active_ship_id),
                    );
                };
                let ship_name = ctx
                    .sde
                    .types
                    .get(&ship.type_id)
                    .map(|t| t.name.clone())
                    .unwrap_or_else(|| format!("Ship {}", ship.type_id));
                RouteExecutionResult::ok_json(
                    json!({
                        "ship_item_id": ship.item_id,
                        "ship_type_id": ship.type_id as i64,
                        "ship_name": ship_name,
                    }),
                    5,
                )
            }))
        }

        // 5. Fittings tag
        ("GET", "/characters/{character_id}/fittings") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut fits: Vec<_> = snap
                    .saved_fittings
                    .values()
                    .filter(|f| f.owner_id == char_id)
                    .collect();
                fits.sort_by_key(|f| f.fitting_id);
                let out: Vec<Value> = fits
                    .into_iter()
                    .map(|f| {
                        let items: Vec<Value> = f
                            .items
                            .iter()
                            .map(|(tid, flag, qty)| {
                                json!({
                                    "type_id": *tid as i64,
                                    "flag": fitting_flag_str(*flag),
                                    "quantity": *qty as i64,
                                })
                            })
                            .collect();
                        json!({
                            "fitting_id": f.fitting_id,
                            "name": f.name,
                            "description": f.description,
                            "ship_type_id": f.ship_type_id as i64,
                            "items": items,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }
        ("POST", "/characters/{character_id}/fittings") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid fitting JSON body: {e}"),
                    ));
                }
            };
            let Some(name) = payload.get("name").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing fitting 'name'"));
            };
            if name.is_empty() || name.len() > 50 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Fitting 'name' must be 1..=50 characters",
                ));
            }
            let Some(description) = payload.get("description").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing fitting 'description'",
                ));
            };
            if description.len() > 500 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Fitting 'description' exceeds 500 characters",
                ));
            }
            let Some(ship_tid_i64) = payload.get("ship_type_id").and_then(Value::as_i64) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing fitting 'ship_type_id'",
                ));
            };
            let Ok(ship_type_id) = u32::try_from(ship_tid_i64) else {
                return Some(RouteExecutionResult::error(400, "Invalid ship_type_id"));
            };
            if !ctx.sde.types.contains_key(&ship_type_id) {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Unknown ship_type_id {ship_type_id}"),
                ));
            }
            let Some(raw_items) = payload.get("items").and_then(Value::as_array) else {
                return Some(RouteExecutionResult::error(400, "Missing fitting 'items'"));
            };
            if raw_items.is_empty() || raw_items.len() > 512 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Fitting 'items' must contain 1..=512 entries",
                ));
            }
            let mut parsed_items = Vec::with_capacity(raw_items.len());
            for entry in raw_items {
                let Some(tid_i64) = entry.get("type_id").and_then(Value::as_i64) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Fitting item missing 'type_id'",
                    ));
                };
                let Ok(tid) = u32::try_from(tid_i64) else {
                    return Some(RouteExecutionResult::error(400, "Invalid item type_id"));
                };
                if !ctx.sde.types.contains_key(&tid) {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Unknown item type_id {tid}"),
                    ));
                }
                let Some(flag_str) = entry.get("flag").and_then(Value::as_str) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Fitting item missing 'flag'",
                    ));
                };
                let Some(flag) = parse_fitting_flag(flag_str) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid fitting item flag '{flag_str}'"),
                    ));
                }
                ;
                let Some(qty_i64) = entry.get("quantity").and_then(Value::as_i64) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Fitting item missing 'quantity'",
                    ));
                };
                let Ok(qty) = i32::try_from(qty_i64) else {
                    return Some(RouteExecutionResult::error(400, "Invalid item quantity"));
                };
                if qty <= 0 {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Fitting item quantity must be > 0",
                    ));
                }
                parsed_items.push((tid, flag, qty));
            }

            let res: Result<i64, RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    ));
                }
                snap.next_entity_id += 1;
                let fitting_id = snap.next_entity_id;
                snap.saved_fittings.insert(
                    fitting_id,
                    SavedFittingRecord {
                        fitting_id,
                        owner_id: char_id,
                        name: name.to_string(),
                        ship_type_id,
                        description: description.to_string(),
                        items: parsed_items,
                    },
                );
                Ok(fitting_id)
            });
            Some(match res {
                Ok(fitting_id) => {
                    RouteExecutionResult::created_json(json!({ "fitting_id": fitting_id }))
                }
                Err(e) => e,
            })
        }
        ("DELETE", "/characters/{character_id}/fittings/{fitting_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let fitting_id = match parse_path_i64(path_params, "fitting_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    ));
                }
                let Some(fit) = snap.saved_fittings.get(&fitting_id) else {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Fitting {fitting_id} not found"),
                    ));
                };
                if fit.owner_id != char_id {
                    return Err(RouteExecutionResult::error(
                        403,
                        format!("Fitting {fitting_id} is not owned by character {char_id}"),
                    ));
                }
                snap.saved_fittings.remove(&fitting_id);
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        // 6. Assets tag
        ("GET", "/characters/{character_id}/assets") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                build_owner_assets(snap, char_id, page)
            }))
        }
        ("POST", "/characters/{character_id}/assets/locations") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                build_owner_asset_locations(ctx, snap, char_id, req_body)
            }))
        }
        ("POST", "/characters/{character_id}/assets/names") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                build_owner_asset_names(ctx, snap, char_id, req_body)
            }))
        }
        ("GET", "/corporations/{corporation_id}/assets") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                build_owner_assets(snap, corp_id, page)
            }))
        }
        ("POST", "/corporations/{corporation_id}/assets/locations") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                build_owner_asset_locations(ctx, snap, corp_id, req_body)
            }))
        }
        ("POST", "/corporations/{corporation_id}/assets/names") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                build_owner_asset_names(ctx, snap, corp_id, req_body)
            }))
        }

        // 7. Wallet tag
        ("GET", "/characters/{character_id}/wallet") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                RouteExecutionResult::ok_json(json!(ch.balance.to_wire_f64()), 120)
            }))
        }
        ("GET", "/corporations/{corporation_id}/wallets") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let mut divisions = Vec::with_capacity(7);
                for div in 1..=7 {
                    let bal = if div == 1 {
                        corp.master_wallet.to_wire_f64()
                    } else {
                        0.0
                    };
                    divisions.push(json!({
                        "division": div,
                        "balance": bal,
                    }));
                }
                RouteExecutionResult::ok_json(json!(divisions), 300)
            }))
        }

        // 8. Loyalty tag
        ("GET", "/characters/{character_id}/loyalty/points") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut lps: Vec<_> = ch.loyalty_points.iter().collect();
                lps.sort_by_key(|(&cid, _)| cid);
                let out: Vec<Value> = lps
                    .into_iter()
                    .map(|(&corp_id, &pts)| {
                        json!({
                            "corporation_id": corp_id,
                            "loyalty_points": pts,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        // 9. Mail tag
        ("GET", "/characters/{character_id}/mail") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut msg_ids: Vec<i64> = snap
                    .mails
                    .iter()
                    .filter(|m| m.recipient_id == char_id || m.sender_id == char_id)
                    .map(|m| m.message_id)
                    .collect();
                msg_ids.sort_by(|a, b| b.cmp(a));
                msg_ids.dedup();
                let out: Vec<Value> = msg_ids
                    .into_iter()
                    .take(50)
                    .filter_map(|mid| {
                        let rows: Vec<_> =
                            snap.mails.iter().filter(|m| m.message_id == mid).collect();
                        let first = rows.first()?;
                        let recipients: Vec<Value> = rows
                            .iter()
                            .map(|m| {
                                let recipient_type = if snap.alliances.contains_key(&m.recipient_id)
                                {
                                    "alliance"
                                } else if snap.corporations.contains_key(&m.recipient_id)
                                    || ctx.sde.npc_corporations.contains_key(&m.recipient_id)
                                {
                                    "corporation"
                                } else {
                                    "character"
                                };
                                json!({
                                    "recipient_id": m.recipient_id,
                                    "recipient_type": recipient_type,
                                })
                            })
                            .collect();
                        Some(json!({
                            "mail_id": first.message_id,
                            "from": first.sender_id,
                            "subject": first.subject,
                            "recipients": recipients,
                        }))
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 30)
            }))
        }
        ("POST", "/characters/{character_id}/mail") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let payload: Value = match serde_json::from_slice(req_body) {
                Ok(v) => v,
                Err(e) => {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid mail JSON body: {e}"),
                    ));
                }
            };
            let Some(subject) = payload.get("subject").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing mail 'subject'"));
            };
            if subject.is_empty() || subject.len() > 1000 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail 'subject' must be 1..=1000 characters",
                ));
            }
            let Some(body) = payload.get("body").and_then(Value::as_str) else {
                return Some(RouteExecutionResult::error(400, "Missing mail 'body'"));
            };
            if body.len() > 10_000 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail 'body' exceeds 10000 characters",
                ));
            }
            let Some(recipients) = payload.get("recipients").and_then(Value::as_array) else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing mail 'recipients'",
                ));
            };
            if recipients.is_empty() || recipients.len() > 50 {
                return Some(RouteExecutionResult::error(
                    400,
                    "Mail 'recipients' must contain 1..=50 entries",
                ));
            }
            let mut recipient_ids = Vec::with_capacity(recipients.len());
            for r in recipients {
                let Some(rid) = r.get("recipient_id").and_then(Value::as_i64) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Mail recipient missing 'recipient_id'",
                    ));
                };
                let Some(rtype) = r.get("recipient_type").and_then(Value::as_str) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        "Mail recipient missing 'recipient_type'",
                    ));
                };
                if !matches!(
                    rtype,
                    "alliance" | "character" | "corporation" | "mailing_list"
                ) {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid recipient_type '{rtype}'"),
                    ));
                }
                if !recipient_ids.contains(&rid) {
                    recipient_ids.push(rid);
                }
            }
            let res: Result<i64, RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    ));
                }
                snap.next_entity_id += 1;
                let mail_id = snap.next_entity_id;
                for rid in recipient_ids {
                    snap.mails.push(MailRecord {
                        message_id: mail_id,
                        sender_id: char_id,
                        recipient_id: rid,
                        subject: subject.to_string(),
                        body: body.to_string(),
                    });
                }
                Ok(mail_id)
            });
            Some(match res {
                Ok(mail_id) => RouteExecutionResult::created_json(json!(mail_id)),
                Err(e) => e,
            })
        }
        ("GET", "/characters/{character_id}/mail/{mail_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let mail_id = match parse_path_i64(path_params, "mail_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let rows: Vec<_> = snap
                    .mails
                    .iter()
                    .filter(|m| m.message_id == mail_id)
                    .collect();
                if rows.is_empty()
                    || !rows
                        .iter()
                        .any(|m| m.recipient_id == char_id || m.sender_id == char_id)
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    );
                }
                let first = rows[0];
                let recipients: Vec<Value> = rows
                    .iter()
                    .map(|m| {
                        let recipient_type = if snap.alliances.contains_key(&m.recipient_id) {
                            "alliance"
                        } else if snap.corporations.contains_key(&m.recipient_id)
                            || ctx.sde.npc_corporations.contains_key(&m.recipient_id)
                        {
                            "corporation"
                        } else {
                            "character"
                        };
                        json!({
                            "recipient_id": m.recipient_id,
                            "recipient_type": recipient_type,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "from": first.sender_id,
                        "subject": first.subject,
                        "body": first.body,
                        "recipients": recipients,
                    }),
                    30,
                )
            }))
        }
        ("DELETE", "/characters/{character_id}/mail/{mail_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let mail_id = match parse_path_i64(path_params, "mail_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let res: Result<(), RouteExecutionResult> = ctx.db.try_with_mut_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    ));
                }
                let authorized = snap.mails.iter().any(|m| {
                    m.message_id == mail_id && (m.recipient_id == char_id || m.sender_id == char_id)
                });
                if !authorized {
                    return Err(RouteExecutionResult::error(
                        404,
                        format!("Mail {mail_id} not found for character {char_id}"),
                    ));
                }
                snap.mails.retain(|m| m.message_id != mail_id);
                Ok(())
            });
            Some(match res {
                Ok(()) => RouteExecutionResult::no_content(0),
                Err(e) => e,
            })
        }

        // 10. Calendar & Contacts 501 handlers with parameter/character validation (ESI-4)
        ("GET", "/characters/{character_id}/calendar") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            }
            Some(RouteExecutionResult::error(
                501,
                "Character calendar event table is not yet active (ESI-4)",
            ))
        }
        ("GET", "/characters/{character_id}/contacts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            }
            Some(RouteExecutionResult::error(
                501,
                "Character contacts table is not yet active (ESI-4)",
            ))
        }

        _ => None,
    }
}
