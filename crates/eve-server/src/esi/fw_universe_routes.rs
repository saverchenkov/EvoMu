//! `esi::fw_universe_routes` — `ESI-3` routes across `Meta`, `Universe`, `Killmails`, `Loyalty`,
//! and `Faction Warfare`, backed by `SdeRegistry` and live `WorldRepository` state.

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{
    compute_killmail_hash, format_iso8601_utc, RouteExecutionResult,
};
use crate::router::ServerContext;
use eve_db::InventoryFlag;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};

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
    crate::esi::sample_routes::open_sde_sqlite(ctx)
}

/// Returns the sorted list of Faction Warfare militia faction IDs from the SDE.
fn sde_fw_militia_factions(ctx: &ServerContext) -> Vec<i64> {
    let mut ids: Vec<i64> = ctx
        .sde
        .factions
        .values()
        .filter(|f| f.militia_corporation_id.is_some() && f.size_factor >= 5.0)
        .map(|f| f.faction_id as i64)
        .collect();
    ids.sort_unstable();
    ids
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
        ("GET", "/meta/changelog") => {
            let spec = crate::esi::openapi::OpenApiSpec::pinned();
            let mut by_date = serde_json::Map::new();
            for date in &spec.compatibility_dates {
                by_date.insert(
                    date.clone(),
                    json!([
                        {
                            "compatibility_date": date,
                            "description": "Pinned V24.01 ESI OpenAPI specification",
                            "is_breaking": false,
                            "method": "GET",
                            "path": "/meta/changelog",
                        }
                    ]),
                );
            }
            Some(RouteExecutionResult::ok_json(
                json!({ "changelog": by_date }),
                300,
            ))
        }

        // Graphic IDs are the SDE `graphicID`s referenced by types (`sde_types.graphic_id`).
        ("GET", "/universe/graphics") => {
            let mut ids: Vec<i64> = ctx
                .sde
                .types
                .values()
                .filter_map(|t| t.graphic_id)
                .map(i64::from)
                .collect();
            ids.sort_unstable();
            ids.dedup();
            Some(RouteExecutionResult::ok_json(json!(ids), 3600))
        }

        // The client SDE carries graphic IDs on types but not the `graphicIDs` file/SOF records, so
        // only `graphic_id` (the one required field) is reported for a graphic some type references;
        // optional file/SOF fields are omitted rather than synthesised.
        ("GET", "/universe/graphics/{graphic_id}") => {
            let graphic_id = match parse_path_i64(path_params, "graphic_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let known = u32::try_from(graphic_id)
                .ok()
                .is_some_and(|gid| ctx.sde.types.values().any(|t| t.graphic_id == Some(gid)));
            if !known {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Graphic {graphic_id} not found"),
                ));
            }
            Some(RouteExecutionResult::ok_json(
                json!({ "graphic_id": graphic_id }),
                3600,
            ))
        }

        ("GET", "/universe/races") => {
            let mut races: Vec<_> = ctx.sde.races.values().collect();
            races.sort_by_key(|r| r.race_id);
            let default_faction_id = ctx
                .sde
                .factions
                .keys()
                .copied()
                .min()
                .unwrap_or_default() as i64;
            let out: Vec<Value> = races
                .into_iter()
                .map(|r| {
                    // DERIVED: Match each race to its empire/faction in `sde_factions` by name prefix;
                    // non-playable races without a dedicated faction entry fall back to the lowest SDE faction ID.
                    let prefix = format!("{} ", r.name);
                    let alliance_id = ctx
                        .sde
                        .factions
                        .values()
                        .find(|f| f.name == r.name || f.name.starts_with(&prefix))
                        .map(|f| f.faction_id as i64)
                        .unwrap_or(default_faction_id);
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
            let capsule_group_id = ctx
                .sde
                .groups
                .values()
                .find(|g| g.name == "Capsule")
                .map(|g| g.group_id);
            let mut all_systems: BTreeSet<u32> = snap.system_states.keys().copied().collect();
            let mut ship_kills: HashMap<u32, i64> = HashMap::new();
            let mut pod_kills: HashMap<u32, i64> = HashMap::new();
            let mut npc_kills: HashMap<u32, i64> = HashMap::new();
            for km in snap.killmails.values() {
                all_systems.insert(km.solar_system_id);
                let is_capsule = ctx
                    .sde
                    .types
                    .get(&km.ship_type_id)
                    .is_some_and(|t| Some(t.group_id) == capsule_group_id);
                if is_capsule {
                    *pod_kills.entry(km.solar_system_id).or_insert(0) += 1;
                } else {
                    *ship_kills.entry(km.solar_system_id).or_insert(0) += 1;
                }
            }
            for (&sys_id, anoms) in &snap.cosmic_anomalies {
                let cleared = anoms.iter().filter(|a| a.cleared).count() as i64;
                if cleared > 0 {
                    all_systems.insert(sys_id);
                    *npc_kills.entry(sys_id).or_insert(0) += cleared;
                }
            }
            let out: Vec<Value> = all_systems
                .into_iter()
                .map(|sid| {
                    json!({
                        "system_id": sid as i64,
                        "ship_kills": ship_kills.get(&sid).copied().unwrap_or(0),
                        "npc_kills": npc_kills.get(&sid).copied().unwrap_or(0),
                        "pod_kills": pod_kills.get(&sid).copied().unwrap_or(0),
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/sovereignty/campaigns") => {
            super::sov_fw_routes::dispatch_sov_route(ctx, method, path)
        }

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
                let canonical_hash = compute_killmail_hash(
                    km.kill_id,
                    km.victim_character_id,
                    km.attacker_character_id,
                    km.ship_type_id,
                );
                let legacy_hash =
                    format!("{:040x}", (km.kill_id as u64).wrapping_mul(2_654_435_761));
                if !killmail_hash.eq_ignore_ascii_case(&canonical_hash)
                    && !killmail_hash.eq_ignore_ascii_case(&legacy_hash)
                {
                    return RouteExecutionResult::error(
                        422,
                        format!("Invalid killmail_hash for killmail {killmail_id}"),
                    );
                }
                let default_npc_corp = ctx
                    .sde
                    .npc_corporations
                    .keys()
                    .copied()
                    .min()
                    .unwrap_or_default();
                let victim_char = snap.characters.get(&km.victim_character_id);
                let attacker_char = snap.characters.get(&km.attacker_character_id);
                let victim_corp_id = victim_char
                    .map(|c| c.corporation_id)
                    .unwrap_or(default_npc_corp);
                let attacker_corp_id = attacker_char
                    .map(|c| c.corporation_id)
                    .unwrap_or(default_npc_corp);
                let attacker_sec_status = attacker_char
                    .map(|c| (c.security_status * 100.0).round() / 100.0)
                    .unwrap_or(0.5);

                let victim_ship_item = victim_char
                    .and_then(|c| snap.items.get(&c.active_ship_id))
                    .filter(|it| it.type_id == km.ship_type_id)
                    .or_else(|| {
                        snap.items.values().find(|it| {
                            it.owner_id == km.victim_character_id && it.type_id == km.ship_type_id
                        })
                    });
                let pos = victim_ship_item
                    .and_then(|it| snap.item_positions.get(&it.item_id).copied())
                    .unwrap_or([0.0, 0.0, 0.0]);
                let items: Vec<Value> = if let Some(ship_it) = victim_ship_item {
                    let mut contained: Vec<_> = snap
                        .items
                        .values()
                        .filter(|it| it.location_id == ship_it.item_id)
                        .collect();
                    contained.sort_by_key(|it| it.item_id);
                    contained
                        .into_iter()
                        .map(|it| {
                            json!({
                                "item_type_id": it.type_id as i64,
                                "flag": it.flag as i64,
                                "singleton": i64::from(it.singleton),
                                "quantity_destroyed": it.quantity.max(1) as i64,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };

                let sde_total_hp = ctx
                    .sde
                    .types
                    .get(&km.ship_type_id)
                    .map(|t| {
                        let hull = t.default_attributes.get(&9).copied().unwrap_or(0.0);
                        let armor = t.default_attributes.get(&265).copied().unwrap_or(0.0);
                        let shield = t.default_attributes.get(&263).copied().unwrap_or(0.0);
                        (hull + armor + shield).round() as i64
                    })
                    .unwrap_or(0);
                let damage_total = if sde_total_hp > 0 {
                    sde_total_hp
                } else {
                    (km.total_value_isk / 1_000.0).round().max(1.0) as i64
                };

                let attacker_ship_item = attacker_char
                    .and_then(|c| snap.items.get(&c.active_ship_id))
                    .or_else(|| {
                        snap.items.values().find(|it| {
                            it.owner_id == km.attacker_character_id
                                && ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.category_id == 6)
                        })
                    });
                let attacker_ship_type_id = attacker_ship_item
                    .map(|it| it.type_id)
                    .unwrap_or(km.ship_type_id) as i64;
                let attacker_weapon_type_id = attacker_ship_item
                    .and_then(|ship_it| {
                        snap.items
                            .values()
                            .find(|it| {
                                it.location_id == ship_it.item_id
                                    && matches!(
                                        it.flag,
                                        InventoryFlag::HiSlot0
                                            | InventoryFlag::HiSlot1
                                            | InventoryFlag::HiSlot2
                                            | InventoryFlag::HiSlot3
                                            | InventoryFlag::HiSlot4
                                            | InventoryFlag::HiSlot5
                                            | InventoryFlag::HiSlot6
                                            | InventoryFlag::HiSlot7
                                    )
                            })
                            .map(|it| it.type_id as i64)
                    })
                    .unwrap_or(attacker_ship_type_id);

                RouteExecutionResult::ok_json(
                    json!({
                        "killmail_id": km.kill_id,
                        "killmail_time": format_iso8601_utc(now),
                        "solar_system_id": km.solar_system_id as i64,
                        "victim": {
                            "character_id": km.victim_character_id,
                            "corporation_id": victim_corp_id,
                            "ship_type_id": km.ship_type_id as i64,
                            "damage_taken": damage_total,
                            "position": { "x": pos[0], "y": pos[1], "z": pos[2] },
                            "items": items,
                        },
                        "attackers": [
                            {
                                "character_id": km.attacker_character_id,
                                "corporation_id": attacker_corp_id,
                                "ship_type_id": attacker_ship_type_id,
                                "weapon_type_id": attacker_weapon_type_id,
                                "damage_done": damage_total,
                                "final_blow": true,
                                "security_status": attacker_sec_status,
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

        ("GET", "/fw/stats") => {
            let militia_factions = sde_fw_militia_factions(ctx);
            Some(ctx.db.with_snapshot(|snap| {
                let mut out = Vec::new();
                for fid in militia_factions {
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
                    let system_vp: i64 = snap
                        .system_states
                        .iter()
                        .filter(|(&sid, _)| {
                            ctx.sde
                                .solar_systems
                                .get(&sid)
                                .and_then(|s| ctx.sde.regions.get(&s.region_id))
                                .and_then(|r| r.faction_id)
                                == Some(fid as u32)
                        })
                        .map(|(_, st)| (st.fw_contested_pct * 30.0).round() as i64)
                        .sum();
                    let vp_total = system_vp + kills_total * 100;
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
                            "yesterday": vp_total,
                            "last_week": vp_total,
                            "total": vp_total,
                        }
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/fw/systems") => {
            let default_militia = sde_fw_militia_factions(ctx)
                .first()
                .copied()
                .unwrap_or(500_001);
            Some(ctx.db.with_snapshot(|snap| {
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
                            .map(i64::from)
                            .unwrap_or(default_militia);
                        let occupier_faction_id = st
                            .sov_alliance_id
                            .filter(|&id| {
                                u32::try_from(id)
                                    .ok()
                                    .is_some_and(|fid| ctx.sde.factions.contains_key(&fid))
                            })
                            .unwrap_or(owner_faction_id);
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
                            "occupier_faction_id": occupier_faction_id,
                            "contested": contested,
                            "victory_points": vp,
                            "victory_points_threshold": 3000,
                        }));
                    }
                }
                RouteExecutionResult::ok_json(json!(out), 1800)
            }))
        }

        ("GET", "/fw/leaderboards") => {
            let militia_factions = sde_fw_militia_factions(ctx);
            Some(ctx.db.with_snapshot(|snap| {
                let mut kill_rows: Vec<(i64, i64)> = militia_factions
                    .iter()
                    .map(|&fid| {
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
                        (fid, kills)
                    })
                    .collect();
                kill_rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                let kill_entries: Vec<Value> = kill_rows
                    .iter()
                    .map(|&(fid, amount)| json!({ "faction_id": fid, "amount": amount }))
                    .collect();
                let vp_entries: Vec<Value> = kill_rows
                    .iter()
                    .map(|&(fid, amount)| json!({ "faction_id": fid, "amount": amount * 100 }))
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "kills": {
                            "yesterday": kill_entries,
                            "last_week": kill_entries,
                            "active_total": kill_entries,
                        },
                        "victory_points": {
                            "yesterday": vp_entries,
                            "last_week": vp_entries,
                            "active_total": vp_entries,
                        }
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/fw/leaderboards/characters") => Some(ctx.db.with_snapshot(|snap| {
            let mut rows: Vec<(i64, i64)> = snap
                .characters
                .keys()
                .copied()
                .map(|cid| {
                    let kills = snap
                        .killmails
                        .values()
                        .filter(|km| km.attacker_character_id == cid)
                        .count() as i64;
                    (cid, kills)
                })
                .collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            rows.truncate(10);
            let kill_entries: Vec<Value> = rows
                .iter()
                .map(|&(cid, kills)| json!({ "character_id": cid, "amount": kills }))
                .collect();
            let vp_entries: Vec<Value> = rows
                .iter()
                .map(|&(cid, kills)| {
                    let lp: i64 = snap
                        .characters
                        .get(&cid)
                        .map(|c| c.loyalty_points.values().copied().sum::<i64>())
                        .unwrap_or(0);
                    json!({ "character_id": cid, "amount": lp + kills * 100 })
                })
                .collect();
            RouteExecutionResult::ok_json(
                json!({
                    "kills": {
                        "yesterday": kill_entries,
                        "last_week": kill_entries,
                        "active_total": kill_entries,
                    },
                    "victory_points": {
                        "yesterday": vp_entries,
                        "last_week": vp_entries,
                        "active_total": vp_entries,
                    }
                }),
                3600,
            )
        })),

        ("GET", "/fw/leaderboards/corporations") => Some(ctx.db.with_snapshot(|snap| {
            let mut rows: Vec<(i64, i64)> = snap
                .corporations
                .keys()
                .copied()
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
                    (cid, kills)
                })
                .collect();
            rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            rows.truncate(10);
            let kill_entries: Vec<Value> = rows
                .iter()
                .map(|&(cid, kills)| json!({ "corporation_id": cid, "amount": kills }))
                .collect();
            let vp_entries: Vec<Value> = rows
                .iter()
                .map(|&(cid, kills)| json!({ "corporation_id": cid, "amount": kills * 100 }))
                .collect();
            RouteExecutionResult::ok_json(
                json!({
                    "kills": {
                        "yesterday": kill_entries,
                        "last_week": kill_entries,
                        "active_total": kill_entries,
                    },
                    "victory_points": {
                        "yesterday": vp_entries,
                        "last_week": vp_entries,
                        "active_total": vp_entries,
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
                let vp = ch.loyalty_points.values().copied().sum::<i64>() + kills * 100;
                let rank = ch
                    .war_faction_id
                    .and_then(|fid| ch.standings.get(&fid).copied())
                    .map(|s| (s.max(0.0).floor() as i64).clamp(1, 9))
                    .unwrap_or(1);
                let mut obj = json!({
                    "current_rank": rank,
                    "highest_rank": rank,
                    "kills": {
                        "yesterday": kills,
                        "last_week": kills,
                        "total": kills,
                    },
                    "victory_points": {
                        "yesterday": vp,
                        "last_week": vp,
                        "total": vp,
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
                let vp = kills * 100;
                let corp_faction = corp
                    .members
                    .iter()
                    .filter_map(|&mid| snap.characters.get(&mid).and_then(|c| c.war_faction_id))
                    .next();
                let mut obj = json!({
                    "pilots": corp.members.len() as i64,
                    "kills": {
                        "yesterday": kills,
                        "last_week": kills,
                        "total": kills,
                    },
                    "victory_points": {
                        "yesterday": vp,
                        "last_week": vp,
                        "total": vp,
                    }
                });
                if let Some(fid) = corp_faction {
                    if let Some(map) = obj.as_object_mut() {
                        map.insert("faction_id".to_string(), json!(fid));
                        map.insert("enlisted_on".to_string(), json!(format_iso8601_utc(now)));
                    }
                }
                RouteExecutionResult::ok_json(obj, 3600)
            }))
        }

        _ => None,
    }
}
