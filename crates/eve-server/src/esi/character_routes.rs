//! `esi::character_routes` — SDE- and `WorldRepository`-backed ESI route handlers for Character,
//! Clones, Skills, Location, Fittings, Assets, Wallet, Loyalty, Calendar, Contacts, and Mail
//! tags (`ESI-4`).

use crate::esi::sample_routes::{
    asset_location_flag_str, fitting_flag_str, format_iso8601_utc, paginate_slice,
    parse_fitting_flag, parse_path_i64, RouteExecutionResult,
};
use crate::formulas::{skill_training_time_seconds, sp_for_skill_level};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, SavedFittingRecord, SsoTokenClaims};
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
    item: &eve_db::InventoryItemRecord,
) -> [f64; 3] {
    let mut root = item;
    for _ in 0..8 {
        if let Some(parent) = snap.items.get(&root.location_id) {
            root = parent;
        } else {
            break;
        }
    }
    // Items docked inside a station or structure hangar always have (0.0, 0.0, 0.0) in ESI /assets/locations
    if (60_000_000..64_000_000).contains(&root.location_id) {
        return [0.0, 0.0, 0.0];
    }
    if snap.structures.contains_key(&root.location_id) && root.flag != InventoryFlag::None {
        return [0.0, 0.0, 0.0];
    }
    if let Some(&pos) = snap.item_positions.get(&item.item_id) {
        return pos;
    }
    if let Some(&pos) = snap.item_positions.get(&root.item_id) {
        return pos;
    }
    if (30_000_000..33_000_000).contains(&root.location_id) {
        let solar_system_id = root.location_id as u32;
        if let Some(pos) = ctx
            .systems
            .get_ball_position_sync(solar_system_id, item.item_id)
            .or_else(|| ctx.systems.get_ball_position_sync(solar_system_id, root.item_id))
        {
            return [pos.x, pos.y, pos.z];
        }
    }
    if let Some(pos) = ctx
        .systems
        .find_ball_position_sync(item.item_id)
        .or_else(|| ctx.systems.find_ball_position_sync(root.item_id))
    {
        return [pos.x, pos.y, pos.z];
    }
    if let Some(st) = snap.structures.get(&item.item_id) {
        return st.position;
    }
    if let Some(st) = snap.structures.get(&root.location_id) {
        if root.flag == InventoryFlag::None {
            return st.position;
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
        let pos = resolve_item_coordinates(ctx, snap, item);
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
        let name = snap
            .item_custom_names
            .get(&item.item_id)
            .cloned()
            .or_else(|| ctx.esi_state.get_custom_item_name(item.item_id))
            .or_else(|| {
                snap.owner_notes
                    .values()
                    .find(|n| {
                        n.item_id == item.item_id
                            && n.character_id == owner_id
                            && !n.label.is_empty()
                    })
                    .map(|n| n.label.clone())
            })
            .or_else(|| ctx.sde.types.get(&item.type_id).map(|t| t.name.clone()))
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
    query_params: &HashMap<String, String>,
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
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let base_url = std::env::var("EVOMU_IMAGE_SERVER_URL")
                    .unwrap_or_else(|_| format!("http://127.0.0.1:{}/images", ctx.esi_port));
                let base_url = base_url.trim_end_matches('/');
                let doll_query = snap
                    .character_doll_info
                    .get(&char_id)
                    .map(|(dna, _)| {
                        let mut encoded = String::with_capacity(dna.len());
                        for b in dna.bytes() {
                            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~')
                            {
                                encoded.push(b as char);
                            } else {
                                encoded.push_str(&format!("%{b:02X}"));
                            }
                        }
                        format!("&dna={encoded}")
                    })
                    .unwrap_or_default();
                RouteExecutionResult::ok_json(
                    json!({
                        "px64x64": format!("{base_url}/characters/{char_id}/portrait?size=64&bloodline_id={}{doll_query}", ch.bloodline_id),
                        "px128x128": format!("{base_url}/characters/{char_id}/portrait?size=128&bloodline_id={}{doll_query}", ch.bloodline_id),
                        "px256x256": format!("{base_url}/characters/{char_id}/portrait?size=256&bloodline_id={}{doll_query}", ch.bloodline_id),
                        "px512x512": format!("{base_url}/characters/{char_id}/portrait?size=512&bloodline_id={}{doll_query}", ch.bloodline_id),
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
                let last_jump_unix = snap
                    .character_last_jump_unix
                    .get(&char_id)
                    .copied()
                    .unwrap_or(0);
                if last_jump_unix > 0 {
                    let ts = format_iso8601_utc(last_jump_unix);
                    obj.insert("last_jump_date".into(), json!(ts.clone()));
                    obj.insert("last_update_date".into(), json!(ts));
                }
                let fatigue_until = if ch.jump_fatigue_until_unix > 0 {
                    Some(ch.jump_fatigue_until_unix)
                } else if ch.jump_fatigue_minutes > 0.0 {
                    let anchor = if last_jump_unix > 0 {
                        last_jump_unix
                    } else {
                        now
                    };
                    Some(anchor.saturating_add((ch.jump_fatigue_minutes * 60.0).round() as u64))
                } else {
                    None
                };
                if let Some(fat_unix) = fatigue_until {
                    if fat_unix > now {
                        obj.insert(
                            "jump_fatigue_expire_date".into(),
                            json!(format_iso8601_utc(fat_unix)),
                        );
                    }
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
                            (-2i64, item.blueprint_runs_remaining.unwrap_or(1) as i64)
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
                        let active_level = if ch.clone_grade <= 1 {
                            let alpha_cap = ctx
                                .sde
                                .clone_grades
                                .get(&ch.race_id)
                                .and_then(|cg| cg.alpha_skill_caps.get(&s.type_id).copied())
                                .or_else(|| {
                                    ctx.sde
                                        .clone_grades
                                        .values()
                                        .filter_map(|cg| cg.alpha_skill_caps.get(&s.type_id).copied())
                                        .max()
                                })
                                .unwrap_or(0);
                            s.level.min(alpha_cap)
                        } else {
                            s.level
                        };
                        skills_vec.push(json!({
                            "skill_id": s.type_id as i64,
                            "trained_skill_level": s.level as i64,
                            "active_skill_level": active_level as i64,
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
                let mut obj = serde_json::Map::new();
                obj.insert("charisma".into(), json!(ch.neural.charisma as i64));
                obj.insert("intelligence".into(), json!(ch.neural.intelligence as i64));
                obj.insert("memory".into(), json!(ch.neural.memory as i64));
                obj.insert("perception".into(), json!(ch.neural.perception as i64));
                obj.insert("willpower".into(), json!(ch.neural.willpower as i64));
                obj.insert("bonus_remaps".into(), json!(ch.neural.bonus_remaps as i64));
                if let Some(last) = ch.neural.last_remap_unix {
                    obj.insert("last_remap_date".into(), json!(format_iso8601_utc(last)));
                }
                if let Some(cooldown) = ch.neural.remap_cooldown_until_unix {
                    obj.insert(
                        "accrued_remap_cooldown_date".into(),
                        json!(format_iso8601_utc(cooldown)),
                    );
                }
                RouteExecutionResult::ok_json(Value::Object(obj), 120)
            }))
        }
        ("GET", "/characters/{character_id}/skillqueue") => {
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
                let is_paused = snap.paused_skill_queues.contains(&char_id);
                let clone_mult = if ch.clone_grade >= 2 { 1.0 } else { 0.5 };
                let mut out = Vec::new();
                if let Some(queue) = snap.skill_queues.get(&char_id) {
                    let queue_anchor = queue
                        .first()
                        .and_then(|e| e.started_at_unix)
                        .unwrap_or(now);
                    let mut cursor_unix = queue_anchor;
                    for (idx, entry) in queue.iter().enumerate() {
                        let skill_type = ctx.sde.types.get(&entry.type_id);
                        let rank = skill_type
                            .and_then(|t| t.default_attributes.get(&275).copied())
                            .unwrap_or(1.0)
                            .max(1.0);
                        let level_start_sp =
                            sp_for_skill_level(rank, entry.target_level.saturating_sub(1)).round()
                                as i64;
                        let level_end_sp = if entry.end_sp > 0.0 {
                            entry.end_sp.round() as i64
                        } else {
                            sp_for_skill_level(rank, entry.target_level).round() as i64
                        };
                        let training_start_sp = entry.start_sp.round() as i64;
                        let mut item = json!({
                            "queue_position": idx as i64,
                            "skill_id": entry.type_id as i64,
                            "finished_level": entry.target_level as i64,
                            "training_start_sp": training_start_sp,
                            "level_start_sp": level_start_sp,
                            "level_end_sp": level_end_sp,
                        });
                        if !is_paused {
                            let start_unix = cursor_unix;
                            let dur = if entry.duration_seconds > 0.0 {
                                entry.duration_seconds
                            } else {
                                let primary_attr_id = skill_type
                                    .and_then(|t| t.default_attributes.get(&180).copied())
                                    .map(|v| v as u32)
                                    .unwrap_or(164);
                                let secondary_attr_id = skill_type
                                    .and_then(|t| t.default_attributes.get(&181).copied())
                                    .map(|v| v as u32)
                                    .unwrap_or(165);
                                let primary = ch.neural.get_attribute_by_id(primary_attr_id);
                                let secondary = ch.neural.get_attribute_by_id(secondary_attr_id);
                                skill_training_time_seconds(
                                    entry.start_sp,
                                    level_end_sp as f64,
                                    primary,
                                    secondary,
                                    clone_mult,
                                )
                            };
                            let finish_unix =
                                start_unix.saturating_add((dur.round() as u64).max(1));
                            item["start_date"] = json!(format_iso8601_utc(start_unix));
                            item["finish_date"] = json!(format_iso8601_utc(finish_unix));
                            cursor_unix = finish_unix;
                        }
                        out.push(item);
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
            let char_info = ctx.db.with_snapshot(|snap| {
                snap.characters.get(&char_id).map(|c| {
                    let stats = snap
                        .character_login_stats
                        .get(&char_id)
                        .cloned()
                        .unwrap_or_default();
                    (c.user_id, stats)
                })
            });
            let Some((uid, stats)) = char_info else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Character {char_id} not found"),
                ));
            };
            let session = ctx
                .db
                .get_online_session(uid)
                .filter(|s| s.character_id == Some(char_id));
            let online = session.is_some();
            let mut obj = serde_json::Map::new();
            obj.insert("online".into(), json!(online));
            let logins = if stats.logins_count > 0 {
                stats.logins_count
            } else if online {
                1
            } else {
                0
            };
            if logins > 0 {
                obj.insert("logins".into(), json!(logins as i64));
            }
            let last_login_unix = if stats.last_login_unix > 0 {
                stats.last_login_unix
            } else {
                session.as_ref().map(|s| s.connected_at_unix).unwrap_or(0)
            };
            if last_login_unix > 0 {
                obj.insert(
                    "last_login".into(),
                    json!(format_iso8601_utc(last_login_unix)),
                );
            }
            if stats.last_logout_unix > 0 {
                obj.insert(
                    "last_logout".into(),
                    json!(format_iso8601_utc(stats.last_logout_unix)),
                );
            }
            Some(RouteExecutionResult::ok_json(Value::Object(obj), 60))
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
                let ship_name = snap
                    .item_custom_names
                    .get(&ship.item_id)
                    .cloned()
                    .or_else(|| ctx.esi_state.get_custom_item_name(ship.item_id))
                    .or_else(|| {
                        snap.owner_notes
                            .values()
                            .find(|n| {
                                n.character_id == char_id
                                    && n.item_id == ship.item_id
                                    && !n.label.is_empty()
                            })
                            .map(|n| n.label.clone())
                    })
                    .or_else(|| ctx.sde.types.get(&ship.type_id).map(|t| t.name.clone()))
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
                // Pinned OpenAPI: "Entries placed in 'Invalid' will be discarded."
                if flag_str == "Invalid" {
                    continue;
                }
                let Some(flag) = parse_fitting_flag(flag_str) else {
                    return Some(RouteExecutionResult::error(
                        400,
                        format!("Invalid fitting item flag '{flag_str}'"),
                    ));
                };
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
            // Pinned OpenAPI: discarding 'Invalid' entries that leaves nothing is an error.
            if parsed_items.is_empty() {
                return Some(RouteExecutionResult::error(
                    400,
                    "Fitting has no items left after discarding 'Invalid' entries",
                ));
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
                    // Same notification the in-game `charFittingMgr.SaveFitting` path sends, so a
                    // logged-in client refreshes its fitting window from the shared state.
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![eve_proto::PyRep::Integer(char_id)],
                    );
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
                Ok(()) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnFittingsUpdated",
                        "charid",
                        vec![eve_proto::PyRep::Integer(char_id)],
                    );
                    RouteExecutionResult::no_content(0)
                }
                Err(e) => e,
            })
        }

        // 6. Assets tag
        ("GET", "/characters/{character_id}/assets") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
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

        _ => super::character_mail_routes::dispatch_character_mail_routes(
            ctx,
            method,
            path_pattern,
            path_params,
            query_params,
            _claims,
            page,
            req_body,
        ),
    }
}
