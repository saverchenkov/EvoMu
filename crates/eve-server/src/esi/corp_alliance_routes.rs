//! `esi::corp_alliance_routes` — `ESI-5` routes for `Alliance` and `Corporation` (`ESI-5`).

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, paginate_slice, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::InventoryFlag;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

fn parse_path_i64(
    path_params: &HashMap<String, String>,
    name: &str,
) -> Result<i64, RouteExecutionResult> {
    let raw = path_params.get(name).ok_or_else(|| {
        RouteExecutionResult::error(400, format!("Missing path parameter '{name}'"))
    })?;
    let val = raw.parse::<i64>().map_err(|_| {
        RouteExecutionResult::error(
            400,
            format!("Invalid integer path parameter '{name}': '{raw}'"),
        )
    })?;
    if val <= 0 {
        return Err(RouteExecutionResult::error(
            400,
            format!("Path parameter '{name}' must be a positive integer"),
        ));
    }
    Ok(val)
}

fn map_blueprint_flag(flag: InventoryFlag) -> &'static str {
    match flag {
        InventoryFlag::Hangar => "CorpSAG1",
        InventoryFlag::Cargo => "Cargo",
        InventoryFlag::CorpDeliveries | InventoryFlag::Deliveries => "CorpDeliveries",
        InventoryFlag::Locked => "Locked",
        InventoryFlag::CorpSAG2 => "CorpSAG2",
        InventoryFlag::CorpSAG3 => "CorpSAG3",
        InventoryFlag::CorpSAG4 => "CorpSAG4",
        InventoryFlag::CorpSAG5 => "CorpSAG5",
        InventoryFlag::CorpSAG6 => "CorpSAG6",
        InventoryFlag::CorpSAG7 => "CorpSAG7",
        _ => "CorpSAG1",
    }
}

fn map_upwell_state(state: &str) -> &'static str {
    match state {
        "AnchorVulnerable" => "anchor_vulnerable",
        "Anchoring" => "anchoring",
        "NoCore" | "FittingInvulnerable" => "fitting_invulnerable",
        "Onlining" | "OnliningVulnerable" => "onlining_vulnerable",
        "ShieldVulnerable" => "shield_vulnerable",
        "ArmorReinforced" => "armor_reinforce",
        "ArmorVulnerable" => "armor_vulnerable",
        "HullReinforced" => "hull_reinforce",
        "HullVulnerable" => "hull_vulnerable",
        "Unanchored" => "unanchored",
        _ => "shield_vulnerable",
    }
}

#[allow(clippy::too_many_lines)]
pub fn dispatch_corp_alliance_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
) -> Option<RouteExecutionResult> {
    let page = query_params
        .get("page")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(1);

    match (method, path) {
        // 1. Alliance Routes (ESI-5)
        ("GET", "/alliances/{alliance_id}/corporations") => {
            let alliance_id = match parse_path_i64(path_params, "alliance_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(al) = snap.alliances.get(&alliance_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Alliance {alliance_id} not found"),
                    );
                };
                let mut corps: Vec<i64> = al.member_corp_ids.iter().copied().collect();
                if !corps.contains(&al.executor_corp_id) {
                    corps.push(al.executor_corp_id);
                }
                corps.sort_unstable();
                RouteExecutionResult::ok_json(json!(corps), 3600)
            }))
        }

        ("GET", "/alliances/{alliance_id}/icons") => {
            let alliance_id = match parse_path_i64(path_params, "alliance_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.alliances.contains_key(&alliance_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Alliance {alliance_id} not found"),
                    );
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "px64x64": format!("https://images.evetech.net/alliances/{alliance_id}/logo?tenant=tranquility&size=64"),
                        "px128x128": format!("https://images.evetech.net/alliances/{alliance_id}/logo?tenant=tranquility&size=128"),
                    }),
                    3600,
                )
            }))
        }

        // 2. Corporation Routes (ESI-5)
        ("GET", "/corporations/{corporation_id}") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    // DERIVED: CorporationRecord does not store a separate creator_id; use ceo_id.
                    let mut obj = json!({
                        "name": corp.name,
                        "ticker": corp.ticker,
                        "ceo_id": corp.ceo_id,
                        "creator_id": corp.ceo_id,
                        "member_count": corp.members.len() as i64,
                        "tax_rate": corp.tax_rate,
                    });
                    if let Some(aid) = corp.alliance_id {
                        obj["alliance_id"] = json!(aid);
                    }
                    if let Some(hq) = corp.hq_station_id {
                        obj["home_station_id"] = json!(hq);
                    }
                    if !corp.description.is_empty() {
                        obj["description"] = json!(corp.description);
                    }
                    if !corp.url.is_empty() {
                        obj["url"] = json!(corp.url);
                    }
                    return RouteExecutionResult::ok_json(obj, 3600);
                }
                if let Some(npc) = ctx.sde.npc_corporations.get(&corp_id) {
                    // DERIVED: NpcCorporationInfo does not persist a separate ceo_id or creator_id;
                    // use the lowest SDE agent_id belonging to the NPC corporation (or corp_id if none),
                    // and count characters currently in the corporation for member_count.
                    let ceo_id = ctx
                        .sde
                        .agents
                        .values()
                        .filter(|a| a.corporation_id == corp_id)
                        .map(|a| a.agent_id)
                        .min()
                        .unwrap_or(corp_id);
                    let member_count = snap
                        .characters
                        .values()
                        .filter(|c| c.corporation_id == corp_id)
                        .count() as i64;
                    let mut obj = json!({
                        "name": npc.name,
                        "ticker": npc.ticker_name,
                        "ceo_id": ceo_id,
                        "creator_id": ceo_id,
                        "member_count": member_count,
                        "tax_rate": npc.tax_rate,
                    });
                    if let Some(fid) = npc.faction_id {
                        obj["faction_id"] = json!(fid as i64);
                    }
                    if let Some(sid) = npc.station_id {
                        obj["home_station_id"] = json!(sid);
                    }
                    return RouteExecutionResult::ok_json(obj, 3600);
                }
                RouteExecutionResult::error(404, format!("Corporation {corp_id} not found"))
            }))
        }

        ("GET", "/corporations/{corporation_id}/blueprints") => {
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
                let mut bps: Vec<_> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == corp_id
                            && (ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                .is_some_and(|t| t.category_id == 9)
                                || ctx.sde.blueprints.contains_key(&i.type_id))
                    })
                    .collect();
                bps.sort_by_key(|i| i.item_id);
                let out: Vec<Value> = bps
                    .into_iter()
                    .map(|item| {
                        let (qty, runs) = if item.is_blueprint_copy {
                            (-2, item.blueprint_runs_remaining.unwrap_or(0))
                        } else if item.singleton {
                            (-1, -1)
                        } else {
                            (item.quantity, -1)
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
                        let eff_loc_id = if matches!(item.flag as u16, 116..=122) {
                            crate::services::industry_pi::industry_rigs::resolve_corp_office_at_facility(
                                snap,
                                true,
                                corp_id,
                                item.location_id,
                            )
                            .unwrap_or(item.location_id)
                        } else {
                            item.location_id
                        };
                        json!({
                            "item_id": item.item_id,
                            "type_id": item.type_id as i64,
                            "location_id": eff_loc_id,
                            "location_flag": map_blueprint_flag(item.flag),
                            "quantity": qty,
                            "time_efficiency": te,
                            "material_efficiency": me,
                            "runs": runs,
                        })
                    })
                    .collect();
                match paginate_slice(&out, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated_json(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/facilities") => {
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
                let mut facilities: Vec<Value> = snap
                    .structures
                    .values()
                    .filter(|s| {
                        s.owner_corp_id == corp_id
                            && s.state != "Destroyed"
                            && snap.items.values().any(|i| {
                                i.location_id == s.structure_id
                                    && matches!(
                                        i.flag,
                                        InventoryFlag::ServiceSlot0
                                            | InventoryFlag::ServiceSlot1
                                            | InventoryFlag::ServiceSlot2
                                            | InventoryFlag::ServiceSlot3
                                            | InventoryFlag::ServiceSlot4
                                            | InventoryFlag::ServiceSlot5
                                            | InventoryFlag::ServiceSlot6
                                            | InventoryFlag::ServiceSlot7
                                    )
                                    && snap
                                        .module_online_states
                                        .get(&i.item_id)
                                        .copied()
                                        .unwrap_or(false)
                            })
                    })
                    .map(|s| {
                        json!({
                            "facility_id": s.structure_id,
                            "type_id": s.type_id as i64,
                            "system_id": s.solar_system_id as i64,
                        })
                    })
                    .collect();
                facilities.sort_by_key(|f| f["facility_id"].as_i64().unwrap_or(0));
                RouteExecutionResult::ok_json(json!(facilities), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/icons") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id)
                    && !ctx.sde.npc_corporations.contains_key(&corp_id)
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "px64x64": format!("https://images.evetech.net/corporations/{corp_id}/logo?tenant=tranquility&size=64"),
                        "px128x128": format!("https://images.evetech.net/corporations/{corp_id}/logo?tenant=tranquility&size=128"),
                        "px256x256": format!("https://images.evetech.net/corporations/{corp_id}/logo?tenant=tranquility&size=256"),
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/members") => {
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
                let mut members: Vec<i64> = corp.members.iter().copied().collect();
                if !members.contains(&corp.ceo_id) {
                    members.push(corp.ceo_id);
                }
                members.sort_unstable();
                RouteExecutionResult::ok_json(json!(members), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/membertracking") => {
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
                let mut members: Vec<i64> = corp.members.iter().copied().collect();
                if !members.contains(&corp.ceo_id) {
                    members.push(corp.ceo_id);
                }
                members.sort_unstable();
                let out: Vec<Value> = members
                    .into_iter()
                    .map(|cid| {
                        let mut obj = json!({ "character_id": cid });
                        if let Some(ch) = snap.characters.get(&cid) {
                            let loc_id = ch
                                .station_id
                                .or(ch.structure_id)
                                .unwrap_or(ch.solar_system_id as i64);
                            obj["location_id"] = json!(loc_id);
                            if let Some(ship) = snap.items.get(&ch.active_ship_id) {
                                obj["ship_type_id"] = json!(ship.type_id as i64);
                            }
                        }
                        if let Some(hq) = corp.hq_station_id {
                            obj["base_id"] = json!(hq);
                        }
                        if let Some(stats) = snap.character_login_stats.get(&cid) {
                            if stats.last_login_unix > 0 {
                                obj["logon_date"] = json!(format_iso8601_utc(stats.last_login_unix));
                            }
                            if stats.last_logout_unix > 0 {
                                obj["logoff_date"] =
                                    json!(format_iso8601_utc(stats.last_logout_unix));
                            }
                        }
                        obj
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/roles") => {
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
                let mut member_ids: Vec<i64> = corp.members.iter().copied().collect();
                if !member_ids.contains(&corp.ceo_id) {
                    member_ids.push(corp.ceo_id);
                }
                member_ids.sort_unstable();
                let mut out = Vec::new();
                for cid in member_ids {
                    let assigned = snap.corp_role_assignments.get(&(corp_id, cid));
                    let roles_mask = assigned.map_or(0, |a| a.roles)
                        | corp.member_roles.get(&cid).copied().unwrap_or(0);
                    let grantable_mask = assigned.map(|a| a.grantable_roles).unwrap_or(0);
                    let hq_mask = assigned.map(|a| a.roles_at_hq).unwrap_or(0);
                    let grantable_hq_mask = assigned.map(|a| a.grantable_roles_at_hq).unwrap_or(0);
                    let base_mask = assigned.map(|a| a.roles_at_base).unwrap_or(0);
                    let grantable_base_mask =
                        assigned.map(|a| a.grantable_roles_at_base).unwrap_or(0);
                    let other_mask = assigned.map(|a| a.roles_at_other).unwrap_or(0);
                    let grantable_other_mask =
                        assigned.map(|a| a.grantable_roles_at_other).unwrap_or(0);

                    let roles = super::corp_governance_routes::decode_corp_roles(roles_mask);
                    let grantable =
                        super::corp_governance_routes::decode_corp_roles(grantable_mask);
                    let at_hq = super::corp_governance_routes::decode_corp_roles(hq_mask);
                    let grantable_hq =
                        super::corp_governance_routes::decode_corp_roles(grantable_hq_mask);
                    let at_base = super::corp_governance_routes::decode_corp_roles(base_mask);
                    let grantable_base =
                        super::corp_governance_routes::decode_corp_roles(grantable_base_mask);
                    let at_other = super::corp_governance_routes::decode_corp_roles(other_mask);
                    let grantable_other =
                        super::corp_governance_routes::decode_corp_roles(grantable_other_mask);

                    if !roles.is_empty()
                        || !grantable.is_empty()
                        || !at_hq.is_empty()
                        || !grantable_hq.is_empty()
                        || !at_base.is_empty()
                        || !grantable_base.is_empty()
                        || !at_other.is_empty()
                        || !grantable_other.is_empty()
                    {
                        let mut obj = json!({ "character_id": cid });
                        if !roles.is_empty() {
                            obj["roles"] = json!(roles);
                        }
                        if !grantable.is_empty() {
                            obj["grantable_roles"] = json!(grantable);
                        }
                        if !at_hq.is_empty() {
                            obj["roles_at_hq"] = json!(at_hq);
                        }
                        if !grantable_hq.is_empty() {
                            obj["grantable_roles_at_hq"] = json!(grantable_hq);
                        }
                        if !at_base.is_empty() {
                            obj["roles_at_base"] = json!(at_base);
                        }
                        if !grantable_base.is_empty() {
                            obj["grantable_roles_at_base"] = json!(grantable_base);
                        }
                        if !at_other.is_empty() {
                            obj["roles_at_other"] = json!(at_other);
                        }
                        if !grantable_other.is_empty() {
                            obj["grantable_roles_at_other"] = json!(grantable_other);
                        }
                        out.push(obj);
                    }
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/standings") => {
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
                let mut by_from: BTreeMap<i64, f64> = BTreeMap::new();
                for tx in snap.standing_transactions.values().flatten() {
                    if tx.to_id == corp_id {
                        let entry = by_from.entry(tx.from_id).or_insert(0.0);
                        *entry = (*entry + tx.modification).clamp(-10.0, 10.0);
                    }
                }
                let out: Vec<Value> = by_from
                    .into_iter()
                    .filter_map(|(from_id, standing)| {
                        let from_type = if ctx.sde.agents.contains_key(&from_id) {
                            "agent"
                        } else if ctx.sde.npc_corporations.contains_key(&from_id) {
                            "npc_corp"
                        } else if u32::try_from(from_id)
                            .ok()
                            .is_some_and(|fid| ctx.sde.factions.contains_key(&fid))
                        {
                            "faction"
                        } else {
                            return None;
                        };
                        Some(json!({
                            "from_id": from_id,
                            "from_type": from_type,
                            "standing": (standing * 100.0).round() / 100.0,
                        }))
                    })
                    .collect();
                match paginate_slice(&out, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated_json(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/starbases") => {
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
                // Filter strictly by SDE group 365 (Control Tower), not all of category 23 (Starbase modules).
                let mut towers: Vec<_> = snap
                    .items
                    .values()
                    .filter_map(|it| {
                        if it.owner_id != corp_id {
                            return None;
                        }
                        if !ctx
                            .sde
                            .types
                            .get(&it.type_id)
                            .is_some_and(|t| t.group_id == 365)
                        {
                            return None;
                        }
                        let sys_id = u32::try_from(it.location_id)
                            .ok()
                            .filter(|sid| ctx.sde.solar_systems.contains_key(sid))?;
                        let is_online = snap
                            .module_online_states
                            .get(&it.item_id)
                            .copied()
                            .unwrap_or(false);
                        Some((
                            it.item_id,
                            json!({
                                "starbase_id": it.item_id,
                                "type_id": it.type_id as i64,
                                "system_id": sys_id as i64,
                                "state": if is_online { "online" } else { "offline" },
                            }),
                        ))
                    })
                    .collect();
                towers.sort_by_key(|(id, _)| *id);
                let out: Vec<Value> = towers.into_iter().map(|(_, v)| v).collect();
                match paginate_slice(&out, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated_json(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/structures") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let has_live_anchored = ctx.db.with_snapshot(|snap| {
                let ext =
                    crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                snap.structures.values().any(|s| {
                    s.owner_corp_id == corp_id
                        && ext
                            .lifecycle
                            .get(&s.structure_id)
                            .and_then(|l| l.anchoring_duration_secs)
                            .is_some()
                })
            });
            if has_live_anchored {
                crate::services::station_structure::structure_lifecycle::sync_all_structures(ctx);
            }
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let fuel_groups =
                    crate::services::station_structure::structure_lifecycle::service_fuel_groups(
                        ctx,
                    );
                let ext =
                    crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                let mut structs: Vec<_> = snap
                    .structures
                    .values()
                    .filter(|s| s.owner_corp_id == corp_id && s.state != "Destroyed")
                    .collect();
                structs.sort_by_key(|s| s.structure_id);
                let out: Vec<Value> = structs
                    .into_iter()
                    .map(|s| {
                        let mut fitted_services: Vec<(i64, u32, bool, Value)> = snap
                            .items
                            .values()
                            .filter(|i| {
                                i.location_id == s.structure_id
                                    && matches!(
                                        i.flag,
                                        InventoryFlag::ServiceSlot0
                                            | InventoryFlag::ServiceSlot1
                                            | InventoryFlag::ServiceSlot2
                                            | InventoryFlag::ServiceSlot3
                                            | InventoryFlag::ServiceSlot4
                                            | InventoryFlag::ServiceSlot5
                                            | InventoryFlag::ServiceSlot6
                                            | InventoryFlag::ServiceSlot7
                                    )
                            })
                            .filter_map(|i| {
                                let svc_name = ctx.sde.types.get(&i.type_id)?.name.clone();
                                let is_online = snap
                                    .module_online_states
                                    .get(&i.item_id)
                                    .copied()
                                    .unwrap_or(false);
                                Some((
                                    i.item_id,
                                    i.type_id,
                                    is_online,
                                    json!({
                                        "name": svc_name,
                                        "state": if is_online { "online" } else { "offline" },
                                    }),
                                ))
                            })
                            .collect();
                        fitted_services.sort_by_key(|(id, _, _, _)| *id);
                        let services: Vec<Value> =
                            fitted_services.iter().map(|(_, _, _, v)| v.clone()).collect();

                        let timer_meta = snap.structure_esi_timer_meta.get(&s.structure_id);
                        let profile_id = ext
                            .structure_profile_id
                            .get(&s.structure_id)
                            .copied()
                            .or_else(|| timer_meta.map(|m| m.profile_id))
                            .unwrap_or(0);
                        let mut obj = json!({
                            "structure_id": s.structure_id,
                            "type_id": s.type_id as i64,
                            "corporation_id": s.owner_corp_id,
                            "system_id": s.solar_system_id as i64,
                            "profile_id": profile_id,
                            "name": s.name,
                            "state": map_upwell_state(&s.state),
                            "services": services,
                        });

                        let life = ext.lifecycle.get(&s.structure_id);
                        let avail_fuel = if s.fuel_blocks > 0 {
                            s.fuel_blocks as u64
                        } else {
                            snap.items
                                .values()
                                .filter(|i| {
                                    i.location_id == s.structure_id
                                        && matches!(
                                            i.flag,
                                            InventoryFlag::StructureFuel
                                                | InventoryFlag::SpecializedFuelBay
                                        )
                                        && ctx
                                            .sde
                                            .types
                                            .get(&i.type_id)
                                            .is_some_and(|t| fuel_groups.contains(&t.group_id))
                                })
                                .map(|i| i.quantity.max(0) as u64)
                                .sum()
                        };
                        let derived_fuel = if let Some(l) = life {
                            if avail_fuel > 0 && !l.online_modules.is_empty() {
                                let mut burn_per_hour = 0u64;
                                for &(_, mod_tid, is_online, _) in &fitted_services {
                                    if !is_online {
                                        continue;
                                    }
                                    let per_hour = ctx
                                        .sde
                                        .types
                                        .get(&mod_tid)
                                        .and_then(|t| {
                                            t.default_attributes
                                                .get(
                                                    &crate::services::station_structure::structure_lifecycle::ATTR_SERVICE_FUEL_PER_HOUR,
                                                )
                                                .copied()
                                        })
                                        .unwrap_or(5.0);
                                    let mult = crate::services::structure_acl_defense_rules::service_module_fuel_multiplier(
                                        ctx,
                                        s.type_id,
                                        mod_tid,
                                    );
                                    burn_per_hour = burn_per_hour
                                        .saturating_add((per_hour * mult).round().max(1.0) as u64);
                                }
                                if burn_per_hour > 0 {
                                    let last_tick =
                                        l.last_fuel_tick_unix.unwrap_or_else(|| ctx.now_unix());
                                    let hours = avail_fuel / burn_per_hour;
                                    Some(last_tick.saturating_add(hours * 3600))
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some(fe) = timer_meta
                            .and_then(|m| m.fuel_expires_unix)
                            .or(derived_fuel)
                            .filter(|&t| t > 0)
                        {
                            obj["fuel_expires"] = json!(format_iso8601_utc(fe));
                        }
                        let deploy_timer_start = if s.reinforced_until_unix > 0 {
                            match s.state.as_str() {
                                "AnchorVulnerable" => Some(
                                    s.reinforced_until_unix.saturating_sub(
                                        crate::services::station_structure::structure_lifecycle::ANCHOR_VULNERABLE_SECS,
                                    ),
                                ),
                                "Anchoring" => {
                                    let dur = life
                                        .and_then(|l| l.anchoring_duration_secs)
                                        .unwrap_or(
                                            crate::services::station_structure::structure_lifecycle::ANCHORING_SECS,
                                        );
                                    Some(s.reinforced_until_unix.saturating_sub(dur))
                                }
                                "Onlining" | "OnliningVulnerable" => Some(
                                    s.reinforced_until_unix.saturating_sub(
                                        crate::services::station_structure::structure_lifecycle::ONLINING_SECS,
                                    ),
                                ),
                                _ => None,
                            }
                        } else {
                            None
                        };
                        if let Some(ts) = timer_meta
                            .and_then(|m| m.state_timer_start_unix)
                            .or_else(|| {
                                life.and_then(|l| {
                                    l.fight_started_unix.or(l.shield_fight_started_unix)
                                })
                            })
                            .or_else(|| (s.last_damage_unix > 0).then_some(s.last_damage_unix))
                            .or(deploy_timer_start)
                            .filter(|&t| t > 0)
                        {
                            obj["state_timer_start"] = json!(format_iso8601_utc(ts));
                        }
                        if let Some(te) = timer_meta
                            .and_then(|m| m.state_timer_end_unix)
                            .or_else(|| life.and_then(|l| l.repair_until_unix))
                            .or_else(|| {
                                (s.reinforced_until_unix > 0).then_some(s.reinforced_until_unix)
                            })
                            .or_else(|| ext.unanchoring_until_unix.get(&s.structure_id).copied())
                            .filter(|&t| t > 0)
                        {
                            obj["state_timer_end"] = json!(format_iso8601_utc(te));
                        }
                        if let Some(ua) = ext
                            .unanchoring_until_unix
                            .get(&s.structure_id)
                            .copied()
                            .or_else(|| timer_meta.and_then(|m| m.unanchors_at_unix))
                            .filter(|&t| t > 0)
                        {
                            obj["unanchors_at"] = json!(format_iso8601_utc(ua));
                        }
                        if let Some((ws, we)) = snap
                            .structure_vulnerability_windows
                            .get(&s.structure_id)
                            .copied()
                            .or_else(|| {
                                snap.corp_governance
                                    .get(&s.owner_corp_id)
                                    .map(|g| (u64::from(g.reinforce_hour), 0u64))
                            })
                        {
                            let reinforce_hour = if ws < 24 {
                                ws as i64
                            } else {
                                ((ws / 3600) % 24) as i64
                            };
                            obj["reinforce_hour"] = json!(reinforce_hour);
                            if we >= 1_000_000 {
                                let next_reinforce_hour = timer_meta
                                    .and_then(|m| m.next_reinforce_hour)
                                    .unwrap_or_else(|| {
                                        if we < 24 {
                                            we as i64
                                        } else {
                                            ((we / 3600) % 24) as i64
                                        }
                                    });
                                let next_apply = timer_meta
                                    .and_then(|m| m.next_reinforce_apply_unix)
                                    .unwrap_or(we);
                                obj["next_reinforce_hour"] = json!(next_reinforce_hour);
                                obj["next_reinforce_apply"] = json!(format_iso8601_utc(next_apply));
                            }
                        }
                        if let Some(&(next_hour, _next_day, next_apply)) =
                            ext.pending_reinforce_schedule.get(&s.structure_id)
                        {
                            obj["next_reinforce_hour"] = json!(next_hour as i64);
                            obj["next_reinforce_apply"] = json!(format_iso8601_utc(next_apply));
                        }
                        if let Some(nrh) = timer_meta.and_then(|m| m.next_reinforce_hour) {
                            obj["next_reinforce_hour"] = json!(nrh);
                        }
                        if let Some(nra) = timer_meta
                            .and_then(|m| m.next_reinforce_apply_unix)
                            .filter(|&t| t > 0)
                        {
                            obj["next_reinforce_apply"] = json!(format_iso8601_utc(nra));
                        }
                        obj
                    })
                    .collect();
                match paginate_slice(&out, page, 250) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated_json(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        _ => super::corp_governance_routes::dispatch_corp_governance_route(
            ctx,
            method,
            path,
            path_params,
            query_params,
        ),
    }
}

#[cfg(test)]
#[path = "corp_alliance_routes_unit_tests.rs"]
mod corp_alliance_routes_unit_tests;

