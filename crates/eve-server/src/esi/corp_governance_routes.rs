//! `esi::corp_governance_routes` — `ESI-5` governance, history, divisions, medals, titles,
//! role assignments/history, shareholders, starbase detail, and corporation projects routes.

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, paginate_slice, RouteExecutionResult};
use crate::router::ServerContext;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const CORP_ROLE_TABLE: &[(u64, &str)] = &[
    (eve_db::corp_roles::ROLE_DIRECTOR, "Director"),
    (eve_db::corp_roles::ROLE_ACCOUNTANT, "Accountant"),
    (eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT, "Junior_Accountant"),
    (eve_db::corp_roles::ROLE_CONFIG_EQUIPMENT, "Config_Equipment"),
    (eve_db::corp_roles::ROLE_DIPLOMAT, "Diplomat"),
    (eve_db::corp_roles::ROLE_FACTORY_MANAGER, "Factory_Manager"),
    (eve_db::corp_roles::ROLE_PERSONNEL_MANAGER, "Personnel_Manager"),
    (eve_db::corp_roles::ROLE_SECURITY_OFFICER, "Security_Officer"),
    (eve_db::corp_roles::ROLE_STATION_MANAGER, "Station_Manager"),
    (eve_db::corp_roles::ROLE_AUDITOR, "Auditor"),
];

pub(crate) fn decode_corp_roles(mask: u64) -> Vec<&'static str> {
    CORP_ROLE_TABLE
        .iter()
        .filter_map(|&(bit, name)| ((mask & bit) != 0).then_some(name))
        .collect()
}

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

#[allow(clippy::too_many_lines)]
pub fn dispatch_corp_governance_route(
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
        ("GET", "/alliances/{alliance_id}") => {
            let alliance_id = match parse_path_i64(path_params, "alliance_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let (Some(al), Some(meta)) = (
                    snap.alliances.get(&alliance_id),
                    snap.alliance_lifecycle_meta.get(&alliance_id),
                ) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Alliance {alliance_id} not found"),
                    );
                };
                let mut obj = json!({
                    "creator_corporation_id": meta.creator_corporation_id,
                    "creator_id": meta.creator_id,
                    "date_founded": format_iso8601_utc(meta.date_founded_unix),
                    "name": al.name,
                    "ticker": al.short_name,
                });
                if al.executor_corp_id > 0 {
                    obj["executor_corporation_id"] = json!(al.executor_corp_id);
                }
                if let Some(faction_id) = meta.faction_id {
                    obj["faction_id"] = json!(faction_id);
                }
                RouteExecutionResult::ok_json(obj, 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/alliancehistory") => {
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
                let mut history = snap
                    .corp_alliance_history
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                history.sort_by_key(|e| std::cmp::Reverse(e.record_id));
                let out: Vec<Value> = history
                    .into_iter()
                    .map(|e| {
                        let mut obj = json!({
                            "record_id": e.record_id,
                            "start_date": format_iso8601_utc(e.start_date_unix),
                        });
                        if let Some(aid) = e.alliance_id {
                            obj["alliance_id"] = json!(aid);
                        }
                        if let Some(deleted) = e.is_deleted {
                            obj["is_deleted"] = json!(deleted);
                        }
                        obj
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/containers/logs") => {
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
                let mut logs = snap
                    .corp_container_logs
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                logs.sort_by_key(|e| (std::cmp::Reverse(e.logged_at_unix), e.container_id));
                let out: Vec<Value> = logs
                    .into_iter()
                    .map(|e| {
                        let mut obj = json!({
                            "logged_at": format_iso8601_utc(e.logged_at_unix),
                            "container_id": e.container_id,
                            "container_type_id": i64::from(e.container_type_id),
                            "character_id": e.character_id,
                            "location_id": e.location_id,
                            "location_flag": e.location_flag,
                            "action": e.action,
                        });
                        if let Some(pt) = e.password_type {
                            obj["password_type"] = json!(pt);
                        }
                        if let Some(tid) = e.type_id {
                            obj["type_id"] = json!(i64::from(tid));
                        }
                        if let Some(qty) = e.quantity {
                            obj["quantity"] = json!(i64::from(qty));
                        }
                        if let Some(old_cfg) = e.old_config_bitmask {
                            obj["old_config_bitmask"] = json!(i64::from(old_cfg));
                        }
                        if let Some(new_cfg) = e.new_config_bitmask {
                            obj["new_config_bitmask"] = json!(i64::from(new_cfg));
                        }
                        obj
                    })
                    .collect();
                match paginate_slice(&out, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated_json(json!(page_rows), 600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/divisions") => {
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
                let mut wallet_divs: Vec<_> = corp
                    .wallet_divisions
                    .iter()
                    .filter(|d| (1..=7).contains(&d.division_id))
                    .collect();
                wallet_divs.sort_by_key(|d| d.division_id);
                let wallet: Vec<Value> = wallet_divs
                    .into_iter()
                    .map(|d| {
                        json!({
                            "division": i64::from(d.division_id),
                            "name": d.name,
                        })
                    })
                    .collect();

                let mut hangar_entries: Vec<(u8, String)> = snap
                    .corp_hangar_divisions
                    .get(&corp_id)
                    .map(|m| {
                        m.iter()
                            .filter(|&(&div_id, _)| (1..=7).contains(&div_id))
                            .map(|(&div_id, name)| (div_id, name.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                hangar_entries.sort_by_key(|(div_id, _)| *div_id);
                let hangar: Vec<Value> = hangar_entries
                    .into_iter()
                    .map(|(div_id, name)| {
                        json!({
                            "division": i64::from(div_id),
                            "name": name,
                        })
                    })
                    .collect();

                RouteExecutionResult::ok_json(
                    json!({
                        "hangar": hangar,
                        "wallet": wallet,
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/medals") => {
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
                let mut medals = snap
                    .corp_medals
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                medals.sort_by_key(|m| m.medal_id);
                let out: Vec<Value> = medals
                    .into_iter()
                    .map(|m| {
                        json!({
                            "medal_id": m.medal_id,
                            "title": m.title,
                            "description": m.description,
                            "creator_id": m.creator_id,
                            "created_at": format_iso8601_utc(m.created_at_unix),
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

        ("GET", "/corporations/{corporation_id}/medals/issued") => {
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
                let mut issued = snap
                    .corp_issued_medals
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                issued.sort_by_key(|m| (m.medal_id, m.character_id));
                let out: Vec<Value> = issued
                    .into_iter()
                    .map(|m| {
                        json!({
                            "medal_id": m.medal_id,
                            "character_id": m.character_id,
                            "reason": m.reason,
                            "status": m.status,
                            "issuer_id": m.issuer_id,
                            "issued_at": format_iso8601_utc(m.issued_at_unix),
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

        ("GET", "/corporations/{corporation_id}/members/limit") => {
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
                if let Some(&stored_limit) = snap.corp_member_limits.get(&corp_id) {
                    return RouteExecutionResult::ok_json(json!(stored_limit), 3600);
                }
                // DERIVED: sum SDE dogma attribute 191 (maxCorporationMembersBonus) across CEO's trained skills.
                let mut limit: i64 = 0;
                if let Some(ceo_skills) = snap.skills.get(&corp.ceo_id) {
                    for skill in ceo_skills.values() {
                        if let Some(bonus) = ctx
                            .sde
                            .types
                            .get(&skill.type_id)
                            .and_then(|t| t.default_attributes.get(&191))
                            .copied()
                        {
                            limit += (bonus.round() as i64) * i64::from(skill.level);
                        }
                    }
                }
                RouteExecutionResult::ok_json(json!(limit), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/members/titles") => {
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
                        let mut titles = snap
                            .corp_member_titles
                            .get(&(corp_id, cid))
                            .cloned()
                            .unwrap_or_default();
                        titles.sort_unstable();
                        json!({
                            "character_id": cid,
                            "titles": titles,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/titles") => {
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
                let mut titles = snap
                    .corp_titles
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                titles.sort_by_key(|t| t.title_id);
                let out: Vec<Value> = titles
                    .into_iter()
                    .map(|t| {
                        json!({
                            "title_id": t.title_id,
                            "name": t.name,
                            "roles": t.roles,
                            "grantable_roles": t.grantable_roles,
                            "roles_at_hq": t.roles_at_hq,
                            "grantable_roles_at_hq": t.grantable_roles_at_hq,
                            "roles_at_base": t.roles_at_base,
                            "grantable_roles_at_base": t.grantable_roles_at_base,
                            "roles_at_other": t.roles_at_other,
                            "grantable_roles_at_other": t.grantable_roles_at_other,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/roles/history") => {
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
                let mut history = snap
                    .corp_role_history
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                history.sort_by_key(|h| (std::cmp::Reverse(h.changed_at_unix), h.character_id));
                let out: Vec<Value> = history
                    .into_iter()
                    .map(|h| {
                        json!({
                            "character_id": h.character_id,
                            "changed_at": format_iso8601_utc(h.changed_at_unix),
                            "issuer_id": h.issuer_id,
                            "role_type": h.role_type,
                            "old_roles": h.old_roles,
                            "new_roles": h.new_roles,
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

        ("GET", "/corporations/{corporation_id}/shareholders") => {
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
                let mut holders = snap
                    .corp_shareholders
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                holders.sort_by_key(|s| s.shareholder_id);
                let out: Vec<Value> = holders
                    .into_iter()
                    .map(|s| {
                        json!({
                            "shareholder_id": s.shareholder_id,
                            "shareholder_type": s.shareholder_type,
                            "share_count": s.share_count,
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

        ("GET", "/corporations/{corporation_id}/starbases/{starbase_id}") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let starbase_id = match parse_path_i64(path_params, "starbase_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some(raw_sys) = query_params.get("system_id") else {
                return Some(RouteExecutionResult::error(
                    400,
                    "Missing required query parameter 'system_id'",
                ));
            };
            let Ok(system_id) = raw_sys.parse::<i64>() else {
                return Some(RouteExecutionResult::error(
                    400,
                    format!("Invalid integer query parameter 'system_id': '{raw_sys}'"),
                ));
            };
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let Some(tower) = snap.items.get(&starbase_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Starbase {starbase_id} not found"),
                    );
                };
                let is_control_tower = ctx
                    .sde
                    .types
                    .get(&tower.type_id)
                    .is_some_and(|t| t.group_id == 365);
                if tower.owner_id != corp_id || !is_control_tower || tower.location_id != system_id
                {
                    return RouteExecutionResult::error(
                        404,
                        format!("Starbase {starbase_id} not found in system {system_id}"),
                    );
                }
                let Some(settings) = snap.starbase_settings.get(&starbase_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Starbase {starbase_id} configuration not found"),
                    );
                };
                let mut fuel_totals: BTreeMap<u32, i64> = BTreeMap::new();
                for item in snap.items.values() {
                    if item.location_id == starbase_id && item.quantity > 0 {
                        *fuel_totals.entry(item.type_id).or_insert(0) += i64::from(item.quantity);
                    }
                }
                let fuels: Vec<Value> = fuel_totals
                    .into_iter()
                    .map(|(tid, qty)| json!({ "type_id": i64::from(tid), "quantity": qty }))
                    .collect();

                let mut obj = json!({
                    "allow_corporation_members": settings.allow_corporation_members,
                    "allow_alliance_members": settings.allow_alliance_members,
                    "use_alliance_standings": settings.use_alliance_standings,
                    "attack_if_other_security_status_dropping": settings.attack_if_other_security_status_dropping,
                    "attack_if_at_war": settings.attack_if_at_war,
                    "fuel_bay_view": settings.fuel_bay_view,
                    "fuel_bay_take": settings.fuel_bay_take,
                    "anchor": settings.anchor,
                    "unanchor": settings.unanchor,
                    "online": settings.online,
                    "offline": settings.offline,
                    "fuels": fuels,
                });
                if let Some(th) = settings.attack_standing_threshold {
                    obj["attack_standing_threshold"] = json!(th);
                }
                if let Some(sec) = settings.attack_security_status_threshold {
                    obj["attack_security_status_threshold"] = json!(sec);
                }
                RouteExecutionResult::ok_json(obj, 300)
            }))
        }

        ("GET", "/corporations/{corporation_id}/projects") => {
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
                let mut projs: Vec<_> = snap
                    .corp_projects
                    .values()
                    .filter(|p| p.corporation_id == corp_id)
                    .collect();
                projs.sort_by_key(|p| p.project_id);
                let out: Vec<Value> = projs
                    .into_iter()
                    .map(|p| {
                        let contributors: Vec<Value> = snap
                            .corp_project_contributors
                            .get(&p.project_id)
                            .map(|list| {
                                let mut sorted = list.clone();
                                sorted.sort_by_key(|c| c.character_id);
                                sorted
                                    .into_iter()
                                    .map(|c| {
                                        json!({
                                            "character_id": c.character_id,
                                            "contributed_quantity": c.contributed_quantity,
                                        })
                                    })
                                    .collect()
                            })
                            .unwrap_or_default();
                        json!({
                            "project_id": p.project_id,
                            "corporation_id": p.corporation_id,
                            "name": p.name,
                            "target_quantity": p.target_quantity,
                            "current_quantity": p.current_quantity,
                            "isk_per_unit": p.isk_per_unit.to_wire_f64(),
                            "escrow_remaining": p.escrow_remaining.to_wire_f64(),
                            "contributors": contributors,
                        })
                    })
                    .collect();
                match paginate_slice(&out, page, 250) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated_json(json!(page_rows), 60, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        _ => None,
    }
}
