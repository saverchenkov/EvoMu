//! `esi::corp_alliance_routes` — `ESI-5` routes for `Alliance` and `Corporation` (`ESI-5`).

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::InventoryFlag;
use serde_json::{json, Value};
use std::collections::HashMap;

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
        InventoryFlag::Deliveries => "CorpDeliveries",
        InventoryFlag::Locked => "Locked",
        _ => "CorpSAG1",
    }
}

fn map_upwell_state(state: &str) -> &'static str {
    match state {
        "Anchoring" => "anchoring",
        "OnliningVulnerable" => "onlining_vulnerable",
        "ShieldVulnerable" => "shield_vulnerable",
        "ArmorReinforced" => "armor_reinforce",
        "HullReinforced" => "hull_reinforce",
        _ => "shield_vulnerable",
    }
}

pub fn dispatch_corp_alliance_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    _query_params: &HashMap<String, String>,
) -> Option<RouteExecutionResult> {
    let now = ctx.now_unix();

    match (method, path) {
        // 1. Alliance Routes (ESI-5)
        ("GET", "/alliances/{alliance_id}") => {
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
                let creator_id = snap
                    .corporations
                    .get(&al.executor_corp_id)
                    .map(|c| c.ceo_id)
                    .unwrap_or(90_000_001);
                RouteExecutionResult::ok_json(
                    json!({
                        "name": al.name,
                        "ticker": al.short_name,
                        "creator_id": creator_id,
                        "creator_corporation_id": al.executor_corp_id,
                        "executor_corporation_id": al.executor_corp_id,
                        "date_founded": "2024-01-01T00:00:00Z",
                    }),
                    3600,
                )
            }))
        }

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
                    let mut obj = json!({
                        "name": corp.name,
                        "ticker": corp.ticker,
                        "ceo_id": corp.ceo_id,
                        "creator_id": corp.ceo_id,
                        "member_count": corp.members.len().max(1) as i64,
                        "tax_rate": corp.tax_rate,
                        "date_founded": "2024-01-01T00:00:00Z",
                        "description": format!("{} ({})", corp.name, corp.ticker),
                        "home_station_id": 60_003_760,
                        "shares": 1000,
                        "url": "https://evomu.local",
                        "war_eligible": true,
                    });
                    if let Some(aid) = corp.alliance_id {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("alliance_id".to_string(), json!(aid));
                    }
                    return RouteExecutionResult::ok_json(obj, 3600);
                }
                if let Some(npc) = ctx.sde.npc_corporations.get(&corp_id) {
                    let mut obj = json!({
                        "name": npc.name,
                        "ticker": npc.ticker_name,
                        "ceo_id": 1,
                        "creator_id": 1,
                        "member_count": 100,
                        "tax_rate": npc.tax_rate,
                        "date_founded": "2003-05-06T00:00:00Z",
                        "description": npc.name,
                        "shares": 1000,
                        "war_eligible": false,
                    });
                    if let Some(fid) = npc.faction_id {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("faction_id".to_string(), json!(fid as i64));
                    }
                    if let Some(sid) = npc.station_id {
                        obj.as_object_mut()
                            .unwrap()
                            .insert("home_station_id".to_string(), json!(sid));
                    }
                    return RouteExecutionResult::ok_json(obj, 3600);
                }
                RouteExecutionResult::error(404, format!("Corporation {corp_id} not found"))
            }))
        }

        ("GET", "/corporations/{corporation_id}/alliancehistory") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                if let Some(corp) = snap.corporations.get(&corp_id) {
                    let mut entry = json!({
                        "record_id": 1,
                        "start_date": "2024-01-01T00:00:00Z",
                    });
                    if let Some(aid) = corp.alliance_id {
                        entry
                            .as_object_mut()
                            .unwrap()
                            .insert("alliance_id".to_string(), json!(aid));
                    }
                    return RouteExecutionResult::ok_json(json!([entry]), 3600);
                }
                if ctx.sde.npc_corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::ok_json(
                        json!([{ "record_id": 1, "start_date": "2003-05-06T00:00:00Z" }]),
                        3600,
                    );
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
                                .map(|t| t.category_id == 9)
                                .unwrap_or(false)
                                || ctx.sde.blueprints.contains_key(&i.type_id))
                    })
                    .collect();
                bps.sort_by_key(|i| i.item_id);
                let out: Vec<Value> = bps
                    .into_iter()
                    .map(|item| {
                        let (qty, runs) = if item.is_blueprint_copy {
                            (-2, item.blueprint_runs_remaining.unwrap_or(1).max(1))
                        } else if item.singleton {
                            (-1, -1)
                        } else {
                            (item.quantity.max(1), -1)
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
                        json!({
                            "item_id": item.item_id,
                            "type_id": item.type_id as i64,
                            "location_id": item.location_id,
                            "location_flag": map_blueprint_flag(item.flag),
                            "quantity": qty,
                            "time_efficiency": te,
                            "material_efficiency": me,
                            "runs": runs,
                        })
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
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let out = vec![json!({
                    "logged_at": format_iso8601_utc(now),
                    "container_id": 10_000_000_001_i64,
                    "container_type_id": 17_366,
                    "character_id": corp.ceo_id,
                    "location_id": 60_003_760,
                    "location_flag": "CorpSAG1",
                    "action": "assemble",
                })];
                RouteExecutionResult::ok_json(json!(out), 600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/divisions") => {
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
                let hangar: Vec<Value> = (1..=7)
                    .map(|d| json!({ "division": d, "name": format!("Hangar Division {d}") }))
                    .collect();
                let wallet: Vec<Value> = (1..=7)
                    .map(|d| {
                        json!({
                            "division": d,
                            "name": if d == 1 { "Master Wallet".to_string() } else { format!("Wallet Division {d}") }
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
                    .filter(|s| s.owner_corp_id == corp_id)
                    .map(|s| {
                        json!({
                            "facility_id": s.structure_id,
                            "type_id": s.type_id as i64,
                            "system_id": s.solar_system_id as i64,
                        })
                    })
                    .collect();
                if facilities.is_empty() {
                    facilities.push(json!({
                        "facility_id": 60_003_760,
                        "type_id": 52_678,
                        "system_id": 30_000_142,
                    }));
                }
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

        ("GET", "/corporations/{corporation_id}/medals") => {
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
                let out = vec![json!({
                    "medal_id": 1,
                    "title": "Corporation Service Medal",
                    "description": "Awarded for dedicated service to the corporation.",
                    "creator_id": corp.ceo_id,
                    "created_at": "2025-01-01T00:00:00Z",
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/medals/issued") => {
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
                let out = vec![json!({
                    "medal_id": 1,
                    "character_id": corp.ceo_id,
                    "reason": "Founding leadership",
                    "status": "public",
                    "issuer_id": corp.ceo_id,
                    "issued_at": "2025-01-01T00:00:00Z",
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
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

        ("GET", "/corporations/{corporation_id}/members/limit") => {
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
                RouteExecutionResult::ok_json(json!(12600), 3600)
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
                members.sort_unstable();
                let out: Vec<Value> = members
                    .into_iter()
                    .map(|cid| json!({ "character_id": cid, "titles": [1] }))
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 3600)
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
                members.sort_unstable();
                let out: Vec<Value> = members
                    .into_iter()
                    .map(|cid| {
                        let ch = snap.characters.get(&cid);
                        let loc_id = ch
                            .and_then(|c| c.station_id.or(c.structure_id))
                            .or_else(|| ch.map(|c| c.solar_system_id as i64))
                            .unwrap_or(60_003_760);
                        let ship_type_id = ch
                            .and_then(|c| snap.items.get(&c.active_ship_id))
                            .map(|i| i.type_id as i64)
                            .unwrap_or(670);
                        json!({
                            "character_id": cid,
                            "location_id": loc_id,
                            "ship_type_id": ship_type_id,
                            "start_date": "2024-01-01T00:00:00Z",
                            "logon_date": format_iso8601_utc(now),
                            "logoff_date": format_iso8601_utc(now.saturating_sub(3600)),
                            "base_id": 60_003_760,
                        })
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
                let roles = vec![
                    "Director",
                    "Accountant",
                    "Station_Manager",
                    "Personnel_Manager",
                    "Factory_Manager",
                ];
                let out = vec![json!({
                    "character_id": corp.ceo_id,
                    "roles": roles,
                    "grantable_roles": roles,
                    "roles_at_hq": roles,
                    "grantable_roles_at_hq": roles,
                    "roles_at_base": roles,
                    "grantable_roles_at_base": roles,
                    "roles_at_other": roles,
                    "grantable_roles_at_other": roles,
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/roles/history") => {
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
                let out = vec![json!({
                    "character_id": corp.ceo_id,
                    "changed_at": "2024-01-01T00:00:00Z",
                    "issuer_id": corp.ceo_id,
                    "role_type": "roles",
                    "old_roles": [],
                    "new_roles": ["Director"],
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/shareholders") => {
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
                let out = vec![json!({
                    "shareholder_id": corp.ceo_id,
                    "shareholder_type": "character",
                    "share_count": 1000,
                })];
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
                let out = vec![json!({
                    "from_id": 500_001,
                    "from_type": "faction",
                    "standing": 1.5,
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
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
                let out = vec![json!({
                    "starbase_id": 10_000_000_100_i64,
                    "type_id": 12_235,
                    "system_id": 30_000_142,
                    "moon_id": 40_009_082,
                    "state": "online",
                    "onlined_since": "2025-01-01T00:00:00Z",
                })];
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/corporations/{corporation_id}/starbases/{starbase_id}") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let _starbase_id = match parse_path_i64(path_params, "starbase_id") {
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
                RouteExecutionResult::ok_json(
                    json!({
                        "fuel_bay_view": "corporation_member",
                        "fuel_bay_take": "config_starbase_equipment_role",
                        "anchor": "config_starbase_equipment_role",
                        "unanchor": "config_starbase_equipment_role",
                        "online": "config_starbase_equipment_role",
                        "offline": "config_starbase_equipment_role",
                        "allow_corporation_members": true,
                        "allow_alliance_members": true,
                        "use_alliance_standings": false,
                        "attack_if_other_security_status_dropping": false,
                        "attack_if_at_war": true,
                        "fuels": [
                            { "type_id": 4247, "quantity": 1200 }
                        ]
                    }),
                    300,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/structures") => {
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
                let mut structs: Vec<_> = snap
                    .structures
                    .values()
                    .filter(|s| s.owner_corp_id == corp_id && s.state != "Destroyed")
                    .collect();
                structs.sort_by_key(|s| s.structure_id);
                let out: Vec<Value> = structs
                    .into_iter()
                    .map(|s| {
                        json!({
                            "structure_id": s.structure_id,
                            "type_id": s.type_id as i64,
                            "corporation_id": s.owner_corp_id,
                            "system_id": s.solar_system_id as i64,
                            "profile_id": 1,
                            "name": s.name,
                            "state": map_upwell_state(&s.state),
                            "reinforce_hour": 18,
                            "fuel_expires": format_iso8601_utc(now + (s.fuel_blocks as u64) * 3600),
                            "services": [
                                { "name": "Clone Bay", "state": "online" },
                                { "name": "Market", "state": "online" }
                            ]
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
                let roles = vec!["Director", "Station_Manager"];
                let out = vec![json!({
                    "title_id": 1,
                    "name": "Chief Executive Officer",
                    "roles": roles,
                    "grantable_roles": roles,
                    "roles_at_hq": roles,
                    "grantable_roles_at_hq": roles,
                    "roles_at_base": roles,
                    "grantable_roles_at_base": roles,
                    "roles_at_other": roles,
                    "grantable_roles_at_other": roles,
                })];
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        // 3. Fleets Routes (ESI-6)
        ("GET", "/fleets/{fleet_id}") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found"));
                };
                let motd = ctx
                    .esi_state
                    .fleet_motd
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&fleet_id)
                    .cloned()
                    .unwrap_or_else(|| "Fleet operational briefing".to_string());
                RouteExecutionResult::ok_json(
                    json!({
                        "motd": motd,
                        "is_free_move": f.free_move,
                        "is_registered": f.is_registered,
                        "is_voice_enabled": false,
                    }),
                    5,
                )
            }))
        }

        ("GET", "/fleets/{fleet_id}/members") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found"));
                };
                let mut member_ids = f.members.clone();
                if !member_ids.contains(&f.boss_id) {
                    member_ids.insert(0, f.boss_id);
                }
                let mut out = Vec::new();
                for cid in member_ids {
                    let ch = snap.characters.get(&cid);
                    let is_boss = cid == f.boss_id;
                    let role = if is_boss { "fleet_commander" } else { "squad_member" };
                    let role_name = if is_boss { "Fleet Commander" } else { "Squad Member" };
                    let ship_type_id = ch
                        .and_then(|c| snap.items.get(&c.active_ship_id))
                        .map(|i| i.type_id as i64)
                        .unwrap_or(587);
                    let solar_system_id = ch.map(|c| c.solar_system_id as i64).unwrap_or(30000142);
                    let mut obj = json!({
                        "character_id": cid,
                        "ship_type_id": ship_type_id,
                        "wing_id": -1,
                        "squad_id": -1,
                        "role": role,
                        "role_name": role_name,
                        "join_time": format_iso8601_utc(now.saturating_sub(1800)),
                        "takes_fleet_warp": true,
                        "solar_system_id": solar_system_id,
                    });
                    if let Some(c) = ch {
                        if let Some(station_id) = c.station_id {
                            obj.as_object_mut().unwrap().insert("station_id".to_string(), json!(station_id));
                        }
                    }
                    out.push(obj);
                }
                RouteExecutionResult::ok_json(json!(out), 5)
            }))
        }

        ("GET", "/fleets/{fleet_id}/wings") => {
            let fleet_id = match parse_path_i64(path_params, "fleet_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(f) = snap.fleets.get(&fleet_id) else {
                    return RouteExecutionResult::error(404, format!("Fleet {fleet_id} not found"));
                };
                let mut wings = Vec::new();
                if f.wings.is_empty() {
                    wings.push(json!({
                        "id": 1,
                        "name": "Wing 1",
                        "squads": [
                            {
                                "id": 1,
                                "name": "Squad 1"
                            }
                        ]
                    }));
                } else {
                    for &wid in &f.wings {
                        let w_name = ctx
                            .esi_state
                            .fleet_wing_names
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&(fleet_id, wid))
                            .cloned()
                            .unwrap_or_else(|| format!("Wing {wid}"));
                        let mut squads = Vec::new();
                        for &sid in &f.squads {
                            squads.push(json!({
                                "id": sid,
                                "name": format!("Squad {sid}"),
                            }));
                        }
                        if squads.is_empty() {
                            squads.push(json!({
                                "id": 1,
                                "name": "Squad 1",
                            }));
                        }
                        wings.push(json!({
                            "id": wid,
                            "name": w_name,
                            "squads": squads,
                        }));
                    }
                }
                RouteExecutionResult::ok_json(json!(wings), 5)
            }))
        }

        _ => None,
    }
}
