//! `esi::fw_universe_routes` — Remaining `ESI-3` routes across `Meta`, `Universe`, `Sovereignty`,
//! `Killmails`, `Loyalty`, and `Faction Warfare` (`ESI-3`).

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{format_iso8601_utc, RouteExecutionResult};
use crate::router::ServerContext;
use rusqlite::{Connection, OpenFlags};
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

fn open_sde_conn(ctx: &ServerContext) -> Option<Connection> {
    let path = ctx
        .sde
        .db_path
        .clone()
        .unwrap_or_else(eve_sde::SdeRegistry::default_db_path);
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
}

pub fn dispatch_fw_universe_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    _query_params: &HashMap<String, String>,
) -> Option<RouteExecutionResult> {
    let now = ctx.now_unix();

    match (method, path) {
        ("GET", "/meta/changelog") => Some(RouteExecutionResult::ok_json(
            json!({
                "changelog": {
                    "2025-12-16": [
                        {
                            "compatibility_date": "2025-12-16",
                            "description": "Pinned V24.01 ESI OpenAPI 3.1.0 specification",
                            "is_breaking": false,
                            "method": "GET",
                            "path": "/meta/changelog",
                        }
                    ]
                }
            }),
            300,
        )),

        ("GET", "/universe/graphics") => {
            let mut ids: Vec<i64> = ctx
                .sde
                .types
                .keys()
                .filter(|&&tid| ctx.sde.types.get(&tid).map(|t| t.category_id == 6).unwrap_or(false))
                .take(200)
                .map(|&tid| tid as i64)
                .collect();
            if !ids.contains(&587) {
                ids.push(587);
            }
            ids.sort_unstable();
            ids.dedup();
            Some(RouteExecutionResult::ok_json(json!(ids), 3600))
        }

        ("GET", "/universe/graphics/{graphic_id}") => {
            let graphic_id = match parse_path_i64(path_params, "graphic_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Ok(tid) = u32::try_from(graphic_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Graphic {graphic_id} not found"),
                ));
            };
            let Some(inv_type) = ctx.sde.types.get(&tid) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Graphic {graphic_id} not found"),
                ));
            };
            let race_name = inv_type
                .race_id
                .and_then(|rid| ctx.sde.races.get(&rid))
                .map(|r| r.name.to_lowercase())
                .unwrap_or_else(|| "caldari".to_string());
            let hull_name = inv_type
                .name
                .to_lowercase()
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>();
            Some(RouteExecutionResult::ok_json(
                json!({
                    "graphic_id": graphic_id,
                    "graphic_file": format!("res:/dx9/model/ship/{race_name}/{hull_name}.red"),
                    "sof_fation_name": format!("{race_name}base"),
                    "sof_hull_name": hull_name,
                    "sof_race_name": race_name,
                    "sof_dna": format!("{hull_name}:{race_name}base:{race_name}"),
                }),
                3600,
            ))
        }

        ("GET", "/universe/races") => {
            let mut races: Vec<_> = ctx.sde.races.values().collect();
            races.sort_by_key(|r| r.race_id);
            let out: Vec<Value> = races
                .into_iter()
                .map(|r| {
                    let alliance_id = match r.race_id {
                        1 => 500_001_i64, // Caldari State
                        2 => 500_002_i64, // Minmatar Republic
                        4 => 500_003_i64, // Amarr Empire
                        8 => 500_004_i64, // Gallente Federation
                        _ => 500_005_i64, // Jove Empire
                    };
                    json!({
                        "race_id": r.race_id as i64,
                        "name": r.name,
                        "description": r.description,
                        "alliance_id": alliance_id,
                    })
                })
                .collect();
            Some(RouteExecutionResult::ok_json(json!(out), 3600))
        }

        ("GET", "/universe/stars/{star_id}") => {
            let star_id = match parse_path_i64(path_params, "star_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some(conn) = open_sde_conn(ctx) else {
                return Some(RouteExecutionResult::error(500, "SDE connection unavailable"));
            };
            let row: Option<(i64, i64, String, f64)> = conn
                .query_row(
                    "SELECT system_id, type_id, name, radius FROM sde_celestials WHERE celestial_id = ?1 AND group_id = 6",
                    [star_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .ok();
            let Some((system_id, type_id, name, radius)) = row else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Star {star_id} not found"),
                ));
            };
            let luminosity: f64 = conn
                .query_row(
                    "SELECT luminosity FROM sde_solar_systems WHERE system_id = ?1",
                    [system_id],
                    |r| r.get(0),
                )
                .unwrap_or(0.05);
            Some(RouteExecutionResult::ok_json(
                json!({
                    "name": name,
                    "solar_system_id": system_id,
                    "type_id": type_id,
                    "age": 4_600_000_000_i64,
                    "luminosity": luminosity,
                    "radius": radius.round() as i64,
                    "spectral_class": "G2 V",
                    "temperature": 5778_i64,
                }),
                3600,
            ))
        }

        ("GET", "/universe/stations/{station_id}") => {
            let station_id = match parse_path_i64(path_params, "station_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some(conn) = open_sde_conn(ctx) else {
                return Some(RouteExecutionResult::error(500, "SDE connection unavailable"));
            };
            type StationRow = (i64, i64, i64, String, f64, f64, f64, f64, f64);
            let row: Option<StationRow> = conn
                .query_row(
                    "SELECT system_id, owner_id, type_id, name, x, y, z, reprocessing_efficiency, reprocessing_stations_take FROM sde_stations WHERE station_id = ?1",
                    [station_id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                            r.get(7)?,
                            r.get(8)?,
                        ))
                    },
                )
                .ok();
            let Some((
                system_id,
                owner_id,
                type_id,
                name,
                x,
                y,
                z,
                reprocessing_efficiency,
                reprocessing_stations_take,
            )) = row
            else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Station {station_id} not found"),
                ));
            };
            let race_id = ctx
                .sde
                .types
                .get(&(type_id as u32))
                .and_then(|t| t.race_id)
                .unwrap_or(1) as i64;
            Some(RouteExecutionResult::ok_json(
                json!({
                    "station_id": station_id,
                    "name": name,
                    "type_id": type_id,
                    "system_id": system_id,
                    "owner": owner_id,
                    "race_id": race_id,
                    "position": { "x": x, "y": y, "z": z },
                    "reprocessing_efficiency": reprocessing_efficiency,
                    "reprocessing_stations_take": reprocessing_stations_take,
                    "max_dockable_ship_volume": 50_000_000.0,
                    "office_rental_cost": 10_000.0,
                    "services": [
                        "bounty-missions",
                        "courier-missions",
                        "reprocessing-plant",
                        "market",
                        "cloning",
                        "repair-facilities",
                        "factory",
                        "labratory",
                        "fitting",
                        "storage",
                        "insurance",
                        "docking",
                        "office-rental",
                        "jump-clone-facility",
                        "loyalty-point-store"
                    ]
                }),
                3600,
            ))
        }

        ("GET", "/universe/system_jumps") => Some(ctx.db.with_snapshot(|snap| {
            let mut jump_counts: HashMap<u32, i64> = HashMap::new();
            for ch in snap.characters.values() {
                *jump_counts.entry(ch.solar_system_id).or_insert(0) += 1;
            }
            for &sys_id in snap.system_states.keys() {
                jump_counts.entry(sys_id).or_insert(1);
            }
            let mut entries: Vec<(u32, i64)> = jump_counts.into_iter().collect();
            entries.sort_by_key(|(sid, _)| *sid);
            let out: Vec<Value> = entries
                .into_iter()
                .map(|(sid, jumps)| {
                    json!({
                        "system_id": sid as i64,
                        "ship_jumps": jumps,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/universe/system_kills") => Some(ctx.db.with_snapshot(|snap| {
            let mut ship_kills: HashMap<u32, i64> = HashMap::new();
            for km in snap.killmails.values() {
                *ship_kills.entry(km.solar_system_id).or_insert(0) += 1;
            }
            for &sys_id in snap.system_states.keys() {
                ship_kills.entry(sys_id).or_insert(0);
            }
            let mut entries: Vec<(u32, i64)> = ship_kills.into_iter().collect();
            entries.sort_by_key(|(sid, _)| *sid);
            let out: Vec<Value> = entries
                .into_iter()
                .map(|(sid, kills)| {
                    json!({
                        "system_id": sid as i64,
                        "ship_kills": kills,
                        "npc_kills": 0,
                        "pod_kills": 0,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/sovereignty/campaigns") => Some(ctx.db.with_snapshot(|snap| {
            let mut campaigns: Vec<Value> = Vec::new();
            let mut sys_ids: Vec<u32> = snap.system_states.keys().copied().collect();
            sys_ids.sort_unstable();
            for sys_id in sys_ids {
                if let Some(st) = snap.system_states.get(&sys_id) {
                    if let Some(aid) = st.sov_alliance_id {
                        let constellation_id = ctx
                            .sde
                            .solar_systems
                            .get(&sys_id)
                            .map(|s| s.constellation_id as i64)
                            .unwrap_or(20_000_020);
                        campaigns.push(json!({
                            "campaign_id": (sys_id as i64) * 10 + 1,
                            "constellation_id": constellation_id,
                            "solar_system_id": sys_id as i64,
                            "structure_id": (sys_id as i64) * 100 + 1,
                            "event_type": "ihub_defense",
                            "defender_id": aid,
                            "defender_score": 0.6,
                            "attackers_score": 0.4,
                            "start_time": format_iso8601_utc(now),
                        }));
                    }
                }
            }
            RouteExecutionResult::ok_json(json!(campaigns), 5)
        })),

        ("GET", "/killmails/{killmail_id}/{killmail_hash}") => {
            let killmail_id = match parse_path_i64(path_params, "killmail_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some(killmail_hash) = path_params.get("killmail_hash") else {
                return Some(RouteExecutionResult::error(400, "Missing killmail_hash"));
            };
            Some(ctx.db.with_snapshot(|snap| {
                let Some(km) = snap.killmails.get(&killmail_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Killmail {killmail_id} not found"),
                    );
                };
                let expected_hash =
                    format!("{:040x}", (km.kill_id as u64).wrapping_mul(2_654_435_761));
                if !killmail_hash.eq_ignore_ascii_case(&expected_hash) {
                    return RouteExecutionResult::error(
                        422,
                        format!("Invalid killmail_hash for killmail {killmail_id}"),
                    );
                }
                let victim_corp_id = snap
                    .characters
                    .get(&km.victim_character_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009);
                let attacker_corp_id = snap
                    .characters
                    .get(&km.attacker_character_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009);
                RouteExecutionResult::ok_json(
                    json!({
                        "killmail_id": km.kill_id,
                        "killmail_time": format_iso8601_utc(now),
                        "solar_system_id": km.solar_system_id as i64,
                        "victim": {
                            "character_id": km.victim_character_id,
                            "corporation_id": victim_corp_id,
                            "ship_type_id": km.ship_type_id as i64,
                            "damage_taken": 1250,
                            "position": { "x": 0.0, "y": 0.0, "z": 0.0 },
                            "items": [],
                        },
                        "attackers": [
                            {
                                "character_id": km.attacker_character_id,
                                "corporation_id": attacker_corp_id,
                                "ship_type_id": 587,
                                "weapon_type_id": 587,
                                "damage_done": 1250,
                                "final_blow": true,
                                "security_status": 0.5,
                            }
                        ],
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/loyalty/stores/{corporation_id}/offers") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let is_known_corp = ctx.sde.npc_corporations.contains_key(&corp_id)
                || ctx.db.with_snapshot(|snap| snap.corporations.contains_key(&corp_id));
            if !is_known_corp {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Corporation {corp_id} not found"),
                ));
            }
            let offers = vec![
                json!({
                    "offer_id": 1,
                    "type_id": 587,
                    "quantity": 1,
                    "lp_cost": 5000,
                    "isk_cost": 500_000,
                    "required_items": [],
                }),
                json!({
                    "offer_id": 2,
                    "type_id": 34,
                    "quantity": 1000,
                    "lp_cost": 100,
                    "isk_cost": 10_000,
                    "required_items": [],
                }),
            ];
            Some(RouteExecutionResult::ok_json(json!(offers), 3600))
        }

        ("GET", "/fw/stats") => Some(ctx.db.with_snapshot(|snap| {
            let mut out = Vec::new();
            for &fid in &[500_001_i64, 500_002, 500_003, 500_004] {
                let pilots = snap
                    .characters
                    .values()
                    .filter(|c| c.war_faction_id == Some(fid))
                    .count() as i64;
                let kills_total = snap
                    .killmails
                    .values()
                    .filter(|km| {
                        snap.characters
                            .get(&km.attacker_character_id)
                            .and_then(|c| c.war_faction_id)
                            == Some(fid)
                    })
                    .count() as i64;
                let systems_controlled = ctx
                    .sde
                    .regions
                    .values()
                    .filter(|r| r.faction_id == Some(fid as u32))
                    .count() as i64;
                out.push(json!({
                    "faction_id": fid,
                    "pilots": pilots.max(1),
                    "systems_controlled": systems_controlled.max(1),
                    "kills": {
                        "yesterday": kills_total,
                        "last_week": kills_total,
                        "total": kills_total,
                    },
                    "victory_points": {
                        "yesterday": 100,
                        "last_week": 500,
                        "total": 2500,
                    }
                }));
            }
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/fw/systems") => Some(ctx.db.with_snapshot(|snap| {
            let mut sys_ids: Vec<u32> = snap.system_states.keys().copied().collect();
            sys_ids.sort_unstable();
            let mut out = Vec::new();
            for sid in sys_ids {
                if let Some(st) = snap.system_states.get(&sid) {
                    let owner_faction_id = ctx
                        .sde
                        .solar_systems
                        .get(&sid)
                        .and_then(|s| ctx.sde.regions.get(&s.region_id))
                        .and_then(|r| r.faction_id)
                        .unwrap_or(500_001) as i64;
                    let contested = if st.fw_contested_pct >= 100.0 {
                        "vulnerable"
                    } else if st.fw_contested_pct > 0.0 {
                        "contested"
                    } else {
                        "uncontested"
                    };
                    let vp = (st.fw_contested_pct * 30.0).round() as i64;
                    out.push(json!({
                        "solar_system_id": sid as i64,
                        "owner_faction_id": owner_faction_id,
                        "occupier_faction_id": owner_faction_id,
                        "contested": contested,
                        "victory_points": vp,
                        "victory_points_threshold": 3000,
                    }));
                }
            }
            RouteExecutionResult::ok_json(json!(out), 1800)
        })),

        ("GET", "/fw/leaderboards") => Some(ctx.db.with_snapshot(|snap| {
            let entries: Vec<Value> = [500_001_i64, 500_002, 500_003, 500_004]
                .into_iter()
                .map(|fid| {
                    let kills = snap
                        .killmails
                        .values()
                        .filter(|km| {
                            snap.characters
                                .get(&km.attacker_character_id)
                                .and_then(|c| c.war_faction_id)
                                == Some(fid)
                        })
                        .count() as i64;
                    json!({ "faction_id": fid, "amount": kills })
                })
                .collect();
            RouteExecutionResult::ok_json(
                json!({
                    "kills": {
                        "yesterday": entries,
                        "last_week": entries,
                        "active_total": entries,
                    },
                    "victory_points": {
                        "yesterday": entries,
                        "last_week": entries,
                        "active_total": entries,
                    }
                }),
                3600,
            )
        })),

        ("GET", "/fw/leaderboards/characters") => Some(ctx.db.with_snapshot(|snap| {
            let mut char_ids: Vec<i64> = snap.characters.keys().copied().collect();
            char_ids.sort_unstable();
            let entries: Vec<Value> = char_ids
                .into_iter()
                .take(10)
                .map(|cid| {
                    let kills = snap
                        .killmails
                        .values()
                        .filter(|km| km.attacker_character_id == cid)
                        .count() as i64;
                    json!({ "character_id": cid, "amount": kills })
                })
                .collect();
            RouteExecutionResult::ok_json(
                json!({
                    "kills": {
                        "yesterday": entries,
                        "last_week": entries,
                        "active_total": entries,
                    },
                    "victory_points": {
                        "yesterday": entries,
                        "last_week": entries,
                        "active_total": entries,
                    }
                }),
                3600,
            )
        })),

        ("GET", "/fw/leaderboards/corporations") => Some(ctx.db.with_snapshot(|snap| {
            let mut corp_ids: Vec<i64> = snap.corporations.keys().copied().collect();
            corp_ids.sort_unstable();
            let entries: Vec<Value> = corp_ids
                .into_iter()
                .take(10)
                .map(|cid| {
                    let kills = snap
                        .killmails
                        .values()
                        .filter(|km| {
                            snap.characters
                                .get(&km.attacker_character_id)
                                .map(|c| c.corporation_id)
                                == Some(cid)
                        })
                        .count() as i64;
                    json!({ "corporation_id": cid, "amount": kills })
                })
                .collect();
            RouteExecutionResult::ok_json(
                json!({
                    "kills": {
                        "yesterday": entries,
                        "last_week": entries,
                        "active_total": entries,
                    },
                    "victory_points": {
                        "yesterday": entries,
                        "last_week": entries,
                        "active_total": entries,
                    }
                }),
                3600,
            )
        })),

        ("GET", "/characters/{character_id}/fw/stats") => {
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
                let kills = snap
                    .killmails
                    .values()
                    .filter(|km| km.attacker_character_id == char_id)
                    .count() as i64;
                let mut obj = json!({
                    "current_rank": 1,
                    "highest_rank": 1,
                    "kills": {
                        "yesterday": kills,
                        "last_week": kills,
                        "total": kills,
                    },
                    "victory_points": {
                        "yesterday": 0,
                        "last_week": 0,
                        "total": 0,
                    }
                });
                if let Some(fid) = ch.war_faction_id {
                    if let Some(map) = obj.as_object_mut() {
                        map.insert("faction_id".to_string(), json!(fid));
                        map.insert("enlisted_on".to_string(), json!(format_iso8601_utc(now)));
                    }
                }
                RouteExecutionResult::ok_json(obj, 3600)
            }))
        }

        ("GET", "/corporations/{corporation_id}/fw/stats") => {
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
                let kills = snap
                    .killmails
                    .values()
                    .filter(|km| {
                        snap.characters
                            .get(&km.attacker_character_id)
                            .map(|c| c.corporation_id)
                            == Some(corp_id)
                    })
                    .count() as i64;
                RouteExecutionResult::ok_json(
                    json!({
                        "pilots": corp.members.len() as i64,
                        "kills": {
                            "yesterday": kills,
                            "last_week": kills,
                            "total": kills,
                        },
                        "victory_points": {
                            "yesterday": 0,
                            "last_week": 0,
                            "total": 0,
                        }
                    }),
                    3600,
                )
            }))
        }

        _ => None,
    }
}
