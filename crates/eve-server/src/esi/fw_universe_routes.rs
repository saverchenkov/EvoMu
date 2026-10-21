//! `esi::fw_universe_routes` — ESI endpoints for Meta Changelog, Universe Races/Graphics/Stations,
//! System Jumps/Kills, Single Killmail, and Faction Warfare (`ESI-3`), backed by `SdeRegistry`
//! and live `WorldRepository` state.

#![forbid(unsafe_code)]

use crate::esi::sample_routes::{
    compute_killmail_hash, format_iso8601_utc, parse_path_i64, RouteExecutionResult,
};
use crate::router::ServerContext;
use eve_db::{FwWindowedStat, InventoryFlag, WorldStateSnapshot};
use eve_sde::{SdeRegistry, StationInfo};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};

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

/// Maps an SDE `sde_station_services.name` string to the pinned OpenAPI `services` enum slug.
pub fn station_service_to_esi_slug(sde_name: &str) -> String {
    let raw = sde_name.trim().to_ascii_lowercase().replace(' ', "-");
    match raw.as_str() {
        "assassination-missions" => "assasination-missions".to_string(),
        "laboratory" => "labratory".to_string(),
        "security-office" => "security-offices".to_string(),
        _ => raw,
    }
}

/// Resolves the OpenAPI `services` list for a station's SDE `operation_id`.
pub fn station_services_for_operation(sde: &SdeRegistry, operation_id: u32) -> Vec<String> {
    let Some(op) = sde.station_operations.get(&operation_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for &svc_id in &op.services {
        if let Some(svc) = sde.station_services.get(&svc_id) {
            let slug = station_service_to_esi_slug(&svc.name);
            if !slug.is_empty() && !out.contains(&slug) {
                out.push(slug);
            }
        }
    }
    out
}

fn find_sde_station(sde: &SdeRegistry, station_id: i64) -> Option<(u32, &StationInfo)> {
    for (&sys_id, sys) in &sde.solar_systems {
        if let Some(st) = sys.stations.iter().find(|s| s.station_id == station_id) {
            return Some((sys_id, st));
        }
    }
    None
}

fn char_fw_stats(snap: &WorldStateSnapshot, char_id: i64) -> (FwWindowedStat, FwWindowedStat) {
    if let Some(st) = snap.fw_character_stats.get(&char_id) {
        return (st.kills.clone(), st.victory_points.clone());
    }
    (FwWindowedStat::default(), FwWindowedStat::default())
}

fn char_fw_faction(snap: &WorldStateSnapshot, char_id: i64) -> Option<i64> {
    snap.fw_character_stats
        .get(&char_id)
        .and_then(|s| s.faction_id)
        .or_else(|| snap.characters.get(&char_id).and_then(|c| c.war_faction_id))
}

fn rank_entries<F>(
    ids: impl Iterator<Item = i64>,
    key_name: &str,
    stat_fn: F,
    limit: Option<usize>,
) -> Value
where
    F: Fn(i64) -> FwWindowedStat,
{
    let all: Vec<(i64, FwWindowedStat)> = ids.map(|id| (id, stat_fn(id))).collect();
    let build_window = |extract: fn(&FwWindowedStat) -> i64| -> Vec<Value> {
        let mut rows: Vec<(i64, i64)> = all.iter().map(|(id, st)| (*id, extract(st))).collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        if let Some(lim) = limit {
            rows.truncate(lim);
        }
        rows.into_iter()
            .map(|(id, amount)| json!({ key_name: id, "amount": amount }))
            .collect()
    };
    json!({
        "yesterday": build_window(|s| s.yesterday),
        "last_week": build_window(|s| s.last_week),
        "active_total": build_window(|s| s.total),
    })
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
                json!({ "changelog": Value::Object(by_date) }),
                3600,
            ))
        }

        // `graphicIDs.fsdbinary` is a developer-only authoring file not shipped in the V24 client
        // install; expose the graphic_ids referenced by `sde_types` with only the schema-required field.
        ("GET", "/universe/graphics") => {
            let ids: BTreeSet<i64> = ctx
                .sde
                .types
                .values()
                .filter_map(|t| t.graphic_id.map(|g| g as i64))
                .collect();
            Some(RouteExecutionResult::ok_json(
                json!(ids.into_iter().collect::<Vec<_>>()),
                3600,
            ))
        }

        ("GET", "/universe/graphics/{graphic_id}") => {
            let graphic_id = match parse_path_i64(path_params, "graphic_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let exists = u32::try_from(graphic_id).ok().is_some_and(|gid| {
                ctx.sde
                    .types
                    .values()
                    .any(|t| t.graphic_id == Some(gid))
            });
            if !exists {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Graphic {graphic_id} not found"),
                ));
            }
            Some(RouteExecutionResult::ok_json(
                json!({ "graphic_id": graphic_id }),
                86400,
            ))
        }

        ("GET", "/universe/races") => {
            let mut list: Vec<_> = ctx.sde.races.values().collect();
            list.sort_by_key(|r| r.race_id);
            let out: Vec<Value> = list
                .into_iter()
                .map(|r| {
                    // DERIVED: `races.fsdbinary` does not carry `factionID`; match each race
                    // to its faction in `sde_factions` by name prefix/substring, falling back to
                    // the SDE `Unknown` faction (`500021`) so required `alliance_id` is always populated.
                    let race_prefix = r
                        .name
                        .split_whitespace()
                        .next()
                        .unwrap_or(r.name.as_str());
                    let faction_id = ctx
                        .sde
                        .factions
                        .values()
                        .find(|f| f.name.starts_with(race_prefix) || f.name.contains(race_prefix))
                        .or_else(|| ctx.sde.factions.values().find(|f| f.name == "Unknown"))
                        .map(|f| f.faction_id as i64)
                        .unwrap_or(500_021);
                    json!({
                        "race_id": r.race_id as i64,
                        "name": r.name,
                        "description": r.description,
                        "alliance_id": faction_id,
                    })
                })
                .collect();
            Some(RouteExecutionResult::ok_json(json!(out), 86400))
        }

        ("GET", "/universe/stations/{station_id}") => {
            let station_id = match parse_path_i64(path_params, "station_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let Some((system_id, st)) = find_sde_station(&ctx.sde, station_id) else {
                return Some(RouteExecutionResult::error(
                    404,
                    format!("Station {station_id} not found"),
                ));
            };
            let station_type = ctx.sde.types.get(&st.station_type_id);
            let race_id = station_type.and_then(|t| t.race_id);
            // DERIVED: Station `max_dockable_ship_volume` comes from station type attribute 9 (`hp`),
            // falling back to `base_price` when attribute 9 is absent on the station type.
            let max_dockable_ship_volume = station_type
                .and_then(|t| {
                    t.default_attributes
                        .get(&9)
                        .copied()
                        .filter(|&v| v > 0.0)
                        .or(Some(t.base_price).filter(|&v| v > 0.0))
                })
                .unwrap_or(50_000_000.0);
            let services = station_services_for_operation(&ctx.sde, st.operation_id);
            let office_rental_cost = ctx.db.with_snapshot(|snap| {
                // DERIVED: Dynamic station office rental cost from `snap.station_office_rental_costs`,
                // defaulting to the station type's SDE `base_price`.
                snap.station_office_rental_costs
                    .get(&station_id)
                    .copied()
                    .or_else(|| station_type.map(|t| t.base_price).filter(|&v| v > 0.0))
                    .unwrap_or(10_000.0)
            });
            let mut obj = json!({
                "station_id": station_id,
                "name": st.name,
                "type_id": st.station_type_id as i64,
                "system_id": system_id as i64,
                "owner": st.owner_corporation_id,
                "position": {
                    "x": st.position.x,
                    "y": st.position.y,
                    "z": st.position.z,
                },
                "reprocessing_efficiency": st.reprocessing_efficiency,
                "reprocessing_stations_take": st.reprocessing_stations_take,
                "max_dockable_ship_volume": max_dockable_ship_volume,
                "office_rental_cost": office_rental_cost,
                "services": services,
            });
            if let Some(rid) = race_id {
                obj["race_id"] = json!(rid as i64);
            }
            Some(RouteExecutionResult::ok_json(obj, 3600))
        }

        // Hourly stargate jump counts aggregated over `[now - 3600, now]` from `snap.system_jump_events`.
        ("GET", "/universe/system_jumps") => Some(ctx.db.with_snapshot(|snap| {
            let window_start = now.saturating_sub(3600);
            let mut jump_counts: BTreeMap<u32, i64> = BTreeMap::new();
            for ev in &snap.system_jump_events {
                if ev.timestamp_unix >= window_start && ev.timestamp_unix <= now {
                    *jump_counts.entry(ev.solar_system_id).or_insert(0) += 1;
                }
            }
            let out: Vec<Value> = jump_counts
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

        // Hourly ship, NPC, and pod kill counts aggregated over `[now - 3600, now]` from
        // `snap.system_kill_events` and timestamped `snap.killmail_details`.
        ("GET", "/universe/system_kills") => Some(ctx.db.with_snapshot(|snap| {
            let window_start = now.saturating_sub(3600);
            let capsule_group_id = ctx
                .sde
                .groups
                .values()
                .find(|g| g.name == "Capsule")
                .map(|g| g.group_id);
            let mut totals: BTreeMap<u32, (i64, i64, i64)> = BTreeMap::new();
            for ev in &snap.system_kill_events {
                if ev.timestamp_unix >= window_start && ev.timestamp_unix <= now {
                    let entry = totals.entry(ev.solar_system_id).or_insert((0, 0, 0));
                    entry.0 += ev.ship_kills as i64;
                    entry.1 += ev.npc_kills as i64;
                    entry.2 += ev.pod_kills as i64;
                }
            }
            for km in snap.killmails.values() {
                let Some(detail) = snap.killmail_details.get(&km.kill_id) else {
                    continue;
                };
                if detail.killmail_time_unix < window_start || detail.killmail_time_unix > now {
                    continue;
                }
                let entry = totals.entry(km.solar_system_id).or_insert((0, 0, 0));
                let is_capsule = capsule_group_id.is_some()
                    && ctx
                        .sde
                        .types
                        .get(&km.ship_type_id)
                        .map(|t| t.group_id)
                        == capsule_group_id;
                if is_capsule {
                    entry.2 += 1;
                } else {
                    entry.0 += 1;
                }
            }
            let out: Vec<Value> = totals
                .into_iter()
                .filter(|(_, (sk, nk, pk))| *sk > 0 || *nk > 0 || *pk > 0)
                .map(|(sid, (ship_kills, npc_kills, pod_kills))| {
                    json!({
                        "system_id": sid as i64,
                        "ship_kills": ship_kills,
                        "npc_kills": npc_kills,
                        "pod_kills": pod_kills,
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
                if !killmail_hash.eq_ignore_ascii_case(&canonical_hash) {
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

                let detail = snap.killmail_details.get(&killmail_id);
                let killmail_time_unix = detail.map(|d| d.killmail_time_unix).unwrap_or(1_700_000_000);
                let victim_ship_item = victim_char
                    .and_then(|c| snap.items.get(&c.active_ship_id))
                    .filter(|it| it.type_id == km.ship_type_id)
                    .or_else(|| {
                        snap.items.values().find(|it| {
                            it.owner_id == km.victim_character_id && it.type_id == km.ship_type_id
                        })
                    });
                let pos = detail
                    .and_then(|d| d.victim_position)
                    .or_else(|| {
                        victim_ship_item
                            .and_then(|it| snap.item_positions.get(&it.item_id).copied())
                    })
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

                let damage_taken = detail.map(|d| d.damage_taken).unwrap_or(0);
                let attackers: Vec<Value> = if let Some(d) = detail.filter(|d| !d.attackers.is_empty()) {
                    d.attackers
                        .iter()
                        .map(|a| {
                            let mut obj = json!({
                                "character_id": a.character_id,
                                "corporation_id": a.corporation_id,
                                "ship_type_id": a.ship_type_id as i64,
                                "weapon_type_id": a.weapon_type_id as i64,
                                "damage_done": a.damage_done,
                                "final_blow": a.final_blow,
                                "security_status": a.security_status,
                            });
                            if let Some(aid) = a.alliance_id {
                                obj["alliance_id"] = json!(aid);
                            }
                            obj
                        })
                        .collect()
                } else {
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
                    vec![json!({
                        "character_id": km.attacker_character_id,
                        "corporation_id": attacker_corp_id,
                        "ship_type_id": attacker_ship_type_id,
                        "weapon_type_id": attacker_weapon_type_id,
                        "damage_done": damage_taken,
                        "final_blow": true,
                        "security_status": attacker_sec_status,
                    })]
                };

                RouteExecutionResult::ok_json(
                    json!({
                        "killmail_id": km.kill_id,
                        "killmail_time": format_iso8601_utc(killmail_time_unix),
                        "solar_system_id": km.solar_system_id as i64,
                        "victim": {
                            "character_id": km.victim_character_id,
                            "corporation_id": victim_corp_id,
                            "ship_type_id": km.ship_type_id as i64,
                            "damage_taken": damage_taken,
                            "position": { "x": pos[0], "y": pos[1], "z": pos[2] },
                            "items": items,
                        },
                        "attackers": attackers,
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/fw/stats") => {
            let militia_factions = sde_fw_militia_factions(ctx);
            Some(ctx.db.with_snapshot(|snap| {
                let mut out = Vec::new();
                for fid in militia_factions {
                    let mut pilots = 0_i64;
                    let mut kills = FwWindowedStat::default();
                    let mut vp = FwWindowedStat::default();
                    for &cid in snap.characters.keys() {
                        if char_fw_faction(snap, cid) == Some(fid) {
                            pilots += 1;
                            let (ck, cv) = char_fw_stats(snap, cid);
                            kills.yesterday += ck.yesterday;
                            kills.last_week += ck.last_week;
                            kills.total += ck.total;
                            vp.yesterday += cv.yesterday;
                            vp.last_week += cv.last_week;
                            vp.total += cv.total;
                        }
                    }
                    let systems_controlled = snap
                        .fw_system_occupations
                        .values()
                        .filter(|s| s.occupier_faction_id == fid)
                        .count() as i64;
                    out.push(json!({
                        "faction_id": fid,
                        "pilots": pilots,
                        "systems_controlled": systems_controlled,
                        "kills": {
                            "yesterday": kills.yesterday,
                            "last_week": kills.last_week,
                            "total": kills.total,
                        },
                        "victory_points": {
                            "yesterday": vp.yesterday,
                            "last_week": vp.last_week,
                            "total": vp.total,
                        }
                    }));
                }
                RouteExecutionResult::ok_json(json!(out), 3600)
            }))
        }

        ("GET", "/fw/systems") => Some(ctx.db.with_snapshot(|snap| {
            let mut list: Vec<_> = snap.fw_system_occupations.values().collect();
            list.sort_by_key(|s| s.solar_system_id);
            let out: Vec<Value> = list
                .into_iter()
                .map(|occ| {
                    let contested = if occ.victory_points >= occ.victory_points_threshold
                        && occ.victory_points_threshold > 0
                    {
                        "vulnerable"
                    } else if occ.victory_points > 0 {
                        "contested"
                    } else {
                        "uncontested"
                    };
                    json!({
                        "solar_system_id": occ.solar_system_id as i64,
                        "owner_faction_id": occ.owner_faction_id,
                        "occupier_faction_id": occ.occupier_faction_id,
                        "contested": contested,
                        "victory_points": occ.victory_points,
                        "victory_points_threshold": occ.victory_points_threshold,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 1800)
        })),

        ("GET", "/fw/leaderboards") => {
            let militia_factions = sde_fw_militia_factions(ctx);
            Some(ctx.db.with_snapshot(|snap| {
                let kills = rank_entries(
                    militia_factions.iter().copied(),
                    "faction_id",
                    |fid| {
                        let mut acc = FwWindowedStat::default();
                        for &cid in snap.characters.keys() {
                            if char_fw_faction(snap, cid) == Some(fid) {
                                let (ck, _) = char_fw_stats(snap, cid);
                                acc.yesterday += ck.yesterday;
                                acc.last_week += ck.last_week;
                                acc.total += ck.total;
                            }
                        }
                        acc
                    },
                    None,
                );
                let victory_points = rank_entries(
                    militia_factions.iter().copied(),
                    "faction_id",
                    |fid| {
                        let mut acc = FwWindowedStat::default();
                        for &cid in snap.characters.keys() {
                            if char_fw_faction(snap, cid) == Some(fid) {
                                let (_, cv) = char_fw_stats(snap, cid);
                                acc.yesterday += cv.yesterday;
                                acc.last_week += cv.last_week;
                                acc.total += cv.total;
                            }
                        }
                        acc
                    },
                    None,
                );
                RouteExecutionResult::ok_json(
                    json!({
                        "kills": kills,
                        "victory_points": victory_points,
                    }),
                    3600,
                )
            }))
        }

        ("GET", "/fw/leaderboards/characters") => Some(ctx.db.with_snapshot(|snap| {
            let kills = rank_entries(
                snap.characters.keys().copied(),
                "character_id",
                |cid| char_fw_stats(snap, cid).0,
                Some(10),
            );
            let victory_points = rank_entries(
                snap.characters.keys().copied(),
                "character_id",
                |cid| char_fw_stats(snap, cid).1,
                Some(10),
            );
            RouteExecutionResult::ok_json(
                json!({
                    "kills": kills,
                    "victory_points": victory_points,
                }),
                3600,
            )
        })),

        ("GET", "/fw/leaderboards/corporations") => Some(ctx.db.with_snapshot(|snap| {
            let kills = rank_entries(
                snap.corporations.keys().copied(),
                "corporation_id",
                |corp_id| {
                    let mut acc = FwWindowedStat::default();
                    if let Some(corp) = snap.corporations.get(&corp_id) {
                        for &mid in &corp.members {
                            let (ck, _) = char_fw_stats(snap, mid);
                            acc.yesterday += ck.yesterday;
                            acc.last_week += ck.last_week;
                            acc.total += ck.total;
                        }
                    }
                    acc
                },
                Some(10),
            );
            let victory_points = rank_entries(
                snap.corporations.keys().copied(),
                "corporation_id",
                |corp_id| {
                    let mut acc = FwWindowedStat::default();
                    if let Some(corp) = snap.corporations.get(&corp_id) {
                        for &mid in &corp.members {
                            let (_, cv) = char_fw_stats(snap, mid);
                            acc.yesterday += cv.yesterday;
                            acc.last_week += cv.last_week;
                            acc.total += cv.total;
                        }
                    }
                    acc
                },
                Some(10),
            );
            RouteExecutionResult::ok_json(
                json!({
                    "kills": kills,
                    "victory_points": victory_points,
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
                let rec = snap.fw_character_stats.get(&char_id);
                let (kills, vp) = char_fw_stats(snap, char_id);
                let current_rank = rec.map(|r| r.current_rank).unwrap_or(0);
                let highest_rank = rec.map(|r| r.highest_rank).unwrap_or(0);
                let mut obj = json!({
                    "current_rank": current_rank,
                    "highest_rank": highest_rank,
                    "kills": {
                        "yesterday": kills.yesterday,
                        "last_week": kills.last_week,
                        "total": kills.total,
                    },
                    "victory_points": {
                        "yesterday": vp.yesterday,
                        "last_week": vp.last_week,
                        "total": vp.total,
                    }
                });
                if let Some(fid) = rec.and_then(|r| r.faction_id).or(ch.war_faction_id) {
                    obj["faction_id"] = json!(fid);
                }
                if let Some(enlisted_unix) = rec.and_then(|r| r.enlisted_on_unix) {
                    obj["enlisted_on"] = json!(format_iso8601_utc(enlisted_unix));
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
                let mut pilots = 0_i64;
                let mut kills = FwWindowedStat::default();
                let mut vp = FwWindowedStat::default();
                let mut corp_faction = None;
                let mut member_ids: Vec<i64> = corp.members.iter().copied().collect();
                member_ids.sort_unstable();
                for mid in member_ids {
                    if let Some(fid) = char_fw_faction(snap, mid) {
                        pilots += 1;
                        if corp_faction.is_none() {
                            corp_faction = Some(fid);
                        }
                    }
                    let (ck, cv) = char_fw_stats(snap, mid);
                    kills.yesterday += ck.yesterday;
                    kills.last_week += ck.last_week;
                    kills.total += ck.total;
                    vp.yesterday += cv.yesterday;
                    vp.last_week += cv.last_week;
                    vp.total += cv.total;
                }
                let mut obj = json!({
                    "pilots": pilots,
                    "kills": {
                        "yesterday": kills.yesterday,
                        "last_week": kills.last_week,
                        "total": kills.total,
                    },
                    "victory_points": {
                        "yesterday": vp.yesterday,
                        "last_week": vp.last_week,
                        "total": vp.total,
                    }
                });
                if let Some(fid) = corp_faction {
                    obj["faction_id"] = json!(fid);
                }
                if let Some(&enlisted_unix) = snap.fw_corp_enlisted_on.get(&corp_id) {
                    obj["enlisted_on"] = json!(format_iso8601_utc(enlisted_unix));
                }
                RouteExecutionResult::ok_json(obj, 3600)
            }))
        }

        _ => None,
    }
}
