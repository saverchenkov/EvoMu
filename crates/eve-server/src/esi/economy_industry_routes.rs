//! `esi::economy_industry_routes` — Industry, Mining and Planetary Interaction routes split out of `esi::economy_routes` (`ESI-6`).

#![forbid(unsafe_code)]

// parity: esi:GET /industry/facilities, esi:GET /industry/systems
// parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /characters/{character_id}/mining
// parity: esi:GET /corporations/{corporation_id}/industry/jobs
// parity: esi:GET /corporation/{corporation_id}/mining/extractions
// parity: esi:GET /corporation/{corporation_id}/mining/observers
// parity: esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
// parity: esi:GET /characters/{character_id}/planets, esi:GET /characters/{character_id}/planets/{planet_id}
// parity: esi:GET /corporations/{corporation_id}/customs_offices, esi:GET /universe/schematics/{schematic_id}
// parity: feat:esi.industry.facilities, feat:esi.industry.systems
// parity: feat:esi.industry.char-jobs, feat:esi.industry.char-mining
// parity: feat:esi.industry.corp-jobs, feat:esi.industry.corp-extractions, feat:esi.industry.corp-observers
// parity: feat:esi.planetary-interaction.colonies, feat:esi.planetary-interaction.layout
// parity: feat:esi.planetary-interaction.customs, feat:esi.planetary-interaction.schematics
// parity: plan:ESI-6

use super::economy_routes::parse_path_i64;
use crate::esi::sample_routes::{format_iso8601_utc, paginate_slice, RouteExecutionResult};
use crate::router::ServerContext;
use eve_db::SsoTokenClaims;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

fn map_job_status(status: &str) -> &'static str {
    match status {
        "Active" | "active" => "active",
        "Delivered" | "delivered" => "delivered",
        "Cancelled" | "cancelled" => "cancelled",
        "Paused" | "paused" => "paused",
        "Ready" | "ready" => "ready",
        "Reverted" | "reverted" => "reverted",
        _ => "active",
    }
}

fn format_date_utc(unix_ts: u64) -> String {
    let iso = format_iso8601_utc(unix_ts);
    iso[..10].to_string()
}

fn resolve_planet_type_name(
    ctx: &ServerContext,
    solar_system_id: u32,
    planet_id: i64,
) -> &'static str {
    let type_id = ctx
        .sde
        .celestials_in_system(solar_system_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)
        .map(|c| c.type_id);
    let Some(tid) = type_id else {
        return "barren";
    };
    let Some(inv_type) = ctx.sde.types.get(&tid) else {
        return "barren";
    };
    let name_lower = inv_type.name.to_ascii_lowercase();
    if name_lower.contains("temperate") {
        "temperate"
    } else if name_lower.contains("oceanic") {
        "oceanic"
    } else if name_lower.contains("ice") {
        "ice"
    } else if name_lower.contains("gas") {
        "gas"
    } else if name_lower.contains("lava") {
        "lava"
    } else if name_lower.contains("storm") {
        "storm"
    } else if name_lower.contains("plasma") {
        "plasma"
    } else if name_lower.contains("shattered") {
        "shattered"
    } else {
        "barren"
    }
}

#[allow(clippy::too_many_lines, clippy::too_many_arguments, unused_variables)]
pub fn dispatch_economy_industry_route(
    ctx: &ServerContext,
    method: &str,
    path: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    page: usize,
) -> Option<RouteExecutionResult> {
    let now = ctx.now_unix();
    match (method, path) {
        ("GET", "/industry/facilities") => Some(ctx.db.with_snapshot(|snap| {
            let mut by_id: BTreeMap<i64, Value> = BTreeMap::new();
            for s in snap.structures.values().filter(|s| s.state != "Destroyed") {
                let Some(sys) = ctx.sde.solar_systems.get(&s.solar_system_id) else {
                    continue;
                };
                let mut obj = json!({
                    "facility_id": s.structure_id,
                    "owner_id": s.owner_corp_id,
                    "region_id": sys.region_id as i64,
                    "solar_system_id": s.solar_system_id as i64,
                    "type_id": s.type_id as i64,
                });
                if let Some(corp) = snap.corporations.get(&s.owner_corp_id) {
                    obj["tax"] = json!(corp.tax_rate);
                }
                by_id.insert(s.structure_id, obj);
            }
            for st in ctx.sde.stations.values() {
                if by_id.contains_key(&st.station_id) {
                    continue;
                }
                let has_industry_service = ctx
                    .sde
                    .station_operations
                    .get(&st.operation_id)
                    .map(|op| op.services.iter().any(|&sid| sid == 14 || sid == 15))
                    .unwrap_or(false);
                if !has_industry_service {
                    continue;
                }
                by_id.insert(
                    st.station_id,
                    json!({
                        "facility_id": st.station_id,
                        "owner_id": st.owner_corporation_id,
                        "region_id": st.region_id as i64,
                        "solar_system_id": st.solar_system_id as i64,
                        "type_id": st.station_type_id as i64,
                        "tax": 0.0025,
                    }),
                );
            }
            for job in snap.industry_jobs.values() {
                if by_id.contains_key(&job.facility_id) {
                    continue;
                }
                if let Some(st) = ctx.sde.stations.get(&job.facility_id) {
                    by_id.insert(
                        st.station_id,
                        json!({
                            "facility_id": st.station_id,
                            "owner_id": st.owner_corporation_id,
                            "region_id": st.region_id as i64,
                            "solar_system_id": st.solar_system_id as i64,
                            "type_id": st.station_type_id as i64,
                            "tax": 0.0025,
                        }),
                    );
                }
            }
            let facilities: Vec<Value> = by_id.into_values().collect();
            RouteExecutionResult::ok_json(json!(facilities), 3600)
        })),

        ("GET", "/industry/systems") => Some(ctx.db.with_snapshot(|snap| {
            let now_unix = ctx.now_unix();
            let cutoff = now_unix.saturating_sub(28 * 86_400);
            let mut sys_work: BTreeMap<u32, BTreeMap<&'static str, u64>> = BTreeMap::new();
            let mut total_activity_work: BTreeMap<&'static str, u64> = BTreeMap::new();
            for j in snap.industry_jobs.values() {
                if j.status.eq_ignore_ascii_case("Cancelled")
                    || j.status.eq_ignore_ascii_case("Reverted")
                {
                    continue;
                }
                if j.start_unix > 0 && j.start_unix < cutoff {
                    continue;
                }
                let Some(sid) = ctx
                    .sde
                    .stations
                    .get(&j.facility_id)
                    .map(|st| st.solar_system_id)
                    .or_else(|| snap.structures.get(&j.facility_id).map(|s| s.solar_system_id))
                else {
                    continue;
                };
                let activity = match j.activity_id {
                    1 => "manufacturing",
                    3 => "researching_time_efficiency",
                    4 => "researching_material_efficiency",
                    5 => "copying",
                    8 => "invention",
                    9 | 11 => "reaction",
                    _ => continue,
                };
                // Canonical System Cost Index (`compute_canonical_system_cost_index`): aggregates job duration_seconds over 28d.
                let work = j.duration_seconds;
                if work == 0 {
                    continue;
                }
                *sys_work.entry(sid).or_default().entry(activity).or_insert(0) += work;
                *total_activity_work.entry(activity).or_insert(0) += work;
            }
            let all_sids: std::collections::BTreeSet<u32> = ctx
                .sde
                .solar_systems
                .keys()
                .copied()
                .chain(sys_work.keys().copied())
                .collect();
            let activities = [
                "manufacturing",
                "researching_time_efficiency",
                "researching_material_efficiency",
                "copying",
                "invention",
                "reaction",
            ];
            let out: Vec<Value> = all_sids
                .into_iter()
                .map(|sid| {
                    let acts = sys_work.get(&sid);
                    let cost_indices: Vec<Value> = activities
                        .iter()
                        .map(|&activity| {
                            let total = total_activity_work.get(activity).copied().unwrap_or(0);
                            let work = acts.and_then(|m| m.get(activity).copied()).unwrap_or(0);
                            // Matches `compute_canonical_system_cost_index` (`sqrt(system_work / universe_work).clamp(0.001, 0.50)`, `0.01` when idle).
                            let cost_index = if total > 0 && work > 0 {
                                ((work as f64) / (total as f64)).sqrt().clamp(0.001, 0.50)
                            } else {
                                0.01
                            };
                            json!({
                                "activity": activity,
                                "cost_index": cost_index,
                            })
                        })
                        .collect();
                    json!({
                        "solar_system_id": sid as i64,
                        "cost_indices": cost_indices,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        })),

        ("GET", "/characters/{character_id}/industry/jobs") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let include_completed = query_params
                .get("include_completed")
                .map(String::as_str)
                != Some("false");
            Some(ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut jobs: Vec<_> = snap
                    .industry_jobs
                    .values()
                    .filter(|j| {
                        j.installer_id == char_id
                            && (include_completed || j.status.eq_ignore_ascii_case("active"))
                    })
                    .collect();
                jobs.sort_by_key(|j| j.job_id);
                let out: Vec<Value> = jobs
                    .into_iter()
                    .map(|j| format_industry_job_json(snap, j, false))
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 300)
            }))
        }

        ("GET", "/characters/{character_id}/mining") => {
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
                // Personal mining ledger covers the last 30 days (`feat:esi.industry.char-mining`)
                let cutoff_unix = now.saturating_sub(30 * 86_400);
                let mut agg: BTreeMap<(String, u32, u32), i64> = BTreeMap::new();
                for entry in snap.mining_ledger.iter().filter(|e| {
                    e.character_id == char_id
                        && e.quantity > 0
                        && e.timestamp_unix >= cutoff_unix
                }) {
                    let date_str = if entry.date.is_empty() {
                        format_date_utc(entry.timestamp_unix)
                    } else {
                        entry.date.clone()
                    };
                    *agg.entry((date_str, entry.solar_system_id, entry.type_id))
                        .or_insert(0) += entry.quantity;
                }
                let rows: Vec<Value> = agg
                    .into_iter()
                    .map(|((date, solar_system_id, type_id), quantity)| {
                        json!({
                            "date": date,
                            "solar_system_id": solar_system_id as i64,
                            "type_id": type_id as i64,
                            "quantity": quantity,
                        })
                    })
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 600, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporations/{corporation_id}/industry/jobs") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let include_completed = query_params
                .get("include_completed")
                .map(String::as_str)
                != Some("false");
            Some(ctx.db.with_snapshot(|snap| {
                let Some(corp) = snap.corporations.get(&corp_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                };
                let mut jobs: Vec<_> = snap
                    .industry_jobs
                    .values()
                    .filter(|j| {
                        let is_corp_job = if j.installer_id == corp_id {
                            true
                        } else if let Some(m) = crate::services::industry_pi::load_job_cost_meta(snap, j.job_id) {
                            m.use_corp && m.owner_id == corp_id
                        } else {
                            corp.members.contains(&j.installer_id) || j.installer_id == corp.ceo_id
                        };
                        is_corp_job && (include_completed || j.status.eq_ignore_ascii_case("active"))
                    })
                    .collect();
                jobs.sort_by_key(|j| j.job_id);
                let rows: Vec<Value> = jobs
                    .into_iter()
                    .map(|j| format_industry_job_json(snap, j, true))
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporation/{corporation_id}/mining/extractions") => {
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
                let mut extractions: Vec<_> = snap
                    .moon_extractions
                    .values()
                    .filter(|ext| {
                        ext.status != "Cancelled"
                            && snap
                                .structures
                                .get(&ext.structure_id)
                                .is_some_and(|s| s.owner_corp_id == corp_id)
                    })
                    .collect();
                extractions.sort_by_key(|e| (e.structure_id, e.extraction_start_unix));
                let rows: Vec<Value> = extractions
                    .into_iter()
                    .map(|ext| {
                        json!({
                            "structure_id": ext.structure_id,
                            "moon_id": ext.moon_id,
                            "extraction_start_time": format_iso8601_utc(ext.extraction_start_unix),
                            "chunk_arrival_time": format_iso8601_utc(ext.chunk_arrival_unix),
                            "natural_decay_time": format_iso8601_utc(ext.natural_decay_unix),
                        })
                    })
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 1800, total_pages)
                    }
                    Err(e) => e,
                }
            }))
        }

        ("GET", "/corporation/{corporation_id}/mining/observers") => {
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
                let mut observers: BTreeMap<i64, String> = BTreeMap::new();
                for (&obs_id, entries) in &snap.observer_mining_ledger {
                    let belongs_to_corp = snap
                        .structures
                        .get(&obs_id)
                        .is_some_and(|s| s.owner_corp_id == corp_id && s.state != "Destroyed")
                        || entries.iter().any(|e| e.recorded_corporation_id == corp_id);
                    if !belongs_to_corp {
                        continue;
                    }
                    let Some(last_updated) = entries
                        .iter()
                        .map(|e| {
                            if e.last_updated_date.is_empty() {
                                format_date_utc(e.timestamp_unix)
                            } else {
                                e.last_updated_date.clone()
                            }
                        })
                        .max()
                    else {
                        continue;
                    };
                    observers.insert(obs_id, last_updated);
                }
                let rows: Vec<Value> = observers
                    .into_iter()
                    .map(|(obs_id, last_updated)| {
                        // DERIVED: Upwell moon-mining observers in EVE are always observer_type "structure"
                        json!({
                            "observer_id": obs_id,
                            "observer_type": "structure",
                            "last_updated": last_updated,
                        })
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

        ("GET", "/corporation/{corporation_id}/mining/observers/{observer_id}") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let observer_id = match parse_path_i64(path_params, "observer_id") {
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
                let Some(entries) = snap.observer_mining_ledger.get(&observer_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mining observer {observer_id} not found"),
                    );
                };
                let belongs_to_corp = snap
                    .structures
                    .get(&observer_id)
                    .is_some_and(|s| s.owner_corp_id == corp_id && s.state != "Destroyed")
                    || entries.iter().any(|e| e.recorded_corporation_id == corp_id);
                if !belongs_to_corp {
                    return RouteExecutionResult::error(
                        404,
                        format!("Mining observer {observer_id} not found for corporation {corp_id}"),
                    );
                }
                let mut sorted = entries.clone();
                sorted.sort_by(|a, b| {
                    (
                        a.last_updated_date.as_str(),
                        a.character_id,
                        a.type_id,
                    )
                        .cmp(&(
                            b.last_updated_date.as_str(),
                            b.character_id,
                            b.type_id,
                        ))
                });
                let rows: Vec<Value> = sorted
                    .into_iter()
                    .map(|e| {
                        let last_updated = if e.last_updated_date.is_empty() {
                            format_date_utc(e.timestamp_unix)
                        } else {
                            e.last_updated_date
                        };
                        json!({
                            "character_id": e.character_id,
                            "recorded_corporation_id": e.recorded_corporation_id,
                            "type_id": e.type_id as i64,
                            "quantity": e.quantity,
                            "last_updated": last_updated,
                        })
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

        // Planetary Interaction Routes (ESI-6)
        ("GET", "/characters/{character_id}/planets") => {
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
                let mut colonies: Vec<_> = snap
                    .planetary_colonies
                    .values()
                    .filter(|c| c.owner_id == char_id)
                    .collect();
                colonies.sort_by_key(|c| c.planet_id);
                let out: Vec<Value> = colonies
                    .into_iter()
                    .map(|c| {
                        let planet_type =
                            resolve_planet_type_name(ctx, c.solar_system_id, c.planet_id);
                        json!({
                            "solar_system_id": c.solar_system_id as i64,
                            "planet_id": c.planet_id,
                            "owner_id": c.owner_id,
                            "upgrade_level": c.command_center_level as i64,
                            "num_pins": c.pins_count as i64,
                            "last_update": format_iso8601_utc(c.last_extracted_unix),
                            "planet_type": planet_type,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 600)
            }))
        }

        ("GET", "/characters/{character_id}/planets/{planet_id}") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return Some(e),
            };
            let planet_id = match parse_path_i64(path_params, "planet_id") {
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
                if !snap.planetary_colonies.contains_key(&(char_id, planet_id)) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Colony on planet {planet_id} not found for character {char_id}"),
                    );
                }
                let layout = snap
                    .planetary_colony_layouts
                    .get(&(char_id, planet_id))
                    .cloned()
                    .unwrap_or_default();
                let pins: Vec<Value> = layout
                    .pins
                    .into_iter()
                    .map(|p| {
                        let mut obj = json!({
                            "pin_id": p.pin_id,
                            "type_id": p.type_id as i64,
                            "latitude": p.latitude,
                            "longitude": p.longitude,
                        });
                        if let Some(sid) = p.schematic_id {
                            obj["schematic_id"] = json!(sid);
                        }
                        if let Some(ts) = p.install_time_unix {
                            obj["install_time"] = json!(format_iso8601_utc(ts));
                        }
                        if let Some(ts) = p.expiry_time_unix {
                            obj["expiry_time"] = json!(format_iso8601_utc(ts));
                        }
                        if let Some(ts) = p.last_cycle_start_unix {
                            obj["last_cycle_start"] = json!(format_iso8601_utc(ts));
                        }
                        if !p.contents.is_empty() {
                            let contents: Vec<Value> = p
                                .contents
                                .into_iter()
                                .map(|c| {
                                    json!({
                                        "type_id": c.type_id as i64,
                                        "amount": c.amount,
                                    })
                                })
                                .collect();
                            obj["contents"] = json!(contents);
                        }
                        obj
                    })
                    .collect();
                let links: Vec<Value> = layout
                    .links
                    .into_iter()
                    .map(|l| {
                        json!({
                            "source_pin_id": l.source_pin_id,
                            "destination_pin_id": l.destination_pin_id,
                            "link_level": l.link_level,
                        })
                    })
                    .collect();
                let routes: Vec<Value> = layout
                    .routes
                    .into_iter()
                    .map(|r| {
                        let mut obj = json!({
                            "route_id": r.route_id,
                            "source_pin_id": r.source_pin_id,
                            "destination_pin_id": r.destination_pin_id,
                            "content_type_id": r.content_type_id as i64,
                            "quantity": r.quantity as f64,
                        });
                        if !r.waypoints.is_empty() {
                            obj["waypoints"] = json!(r.waypoints);
                        }
                        obj
                    })
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "pins": pins,
                        "links": links,
                        "routes": routes,
                    }),
                    600,
                )
            }))
        }

        ("GET", "/corporations/{corporation_id}/customs_offices") => {
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
                let mut offices = snap
                    .corp_customs_offices
                    .get(&corp_id)
                    .cloned()
                    .unwrap_or_default();
                offices.sort_by_key(|o| o.office_id);
                let rows: Vec<Value> = offices
                    .into_iter()
                    .map(|o| {
                        let mut obj = json!({
                            "office_id": o.office_id,
                            "system_id": o.system_id as i64,
                            "reinforce_exit_start": o.reinforce_exit_start,
                            "reinforce_exit_end": o.reinforce_exit_end,
                            "allow_alliance_access": o.allow_alliance_access,
                            "allow_access_with_standings": o.allow_access_with_standings,
                        });
                        if let Some(lvl) = o.standing_level {
                            obj["standing_level"] = json!(lvl);
                        }
                        if let Some(r) = o.corporation_tax_rate {
                            obj["corporation_tax_rate"] = json!(r);
                        }
                        if let Some(r) = o.alliance_tax_rate {
                            obj["alliance_tax_rate"] = json!(r);
                        }
                        if let Some(r) = o.excellent_standing_tax_rate {
                            obj["excellent_standing_tax_rate"] = json!(r);
                        }
                        if let Some(r) = o.good_standing_tax_rate {
                            obj["good_standing_tax_rate"] = json!(r);
                        }
                        if let Some(r) = o.neutral_standing_tax_rate {
                            obj["neutral_standing_tax_rate"] = json!(r);
                        }
                        if let Some(r) = o.bad_standing_tax_rate {
                            obj["bad_standing_tax_rate"] = json!(r);
                        }
                        if let Some(r) = o.terrible_standing_tax_rate {
                            obj["terrible_standing_tax_rate"] = json!(r);
                        }
                        obj
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

        _ => None,
    }
}

fn format_industry_job_json(
    snap: &eve_db::WorldStateSnapshot,
    j: &eve_db::IndustryJobRecord,
    is_corp: bool,
) -> Value {
    let start_ts = j.start_unix;
    let end_ts = start_ts + j.duration_seconds;
    let meta = crate::services::industry_pi::load_job_cost_meta(snap, j.job_id);
    let bp_loc = meta
        .as_ref()
        .map(|m| m.blueprint_location_id)
        .filter(|&id| id > 0)
        .or_else(|| snap.items.get(&j.blueprint_item_id).map(|it| it.location_id))
        .unwrap_or(j.facility_id);
    let out_loc = meta
        .as_ref()
        .map(|m| m.output_location_id)
        .filter(|&id| id > 0)
        .unwrap_or(j.facility_id);
    let mut obj = json!({
        "job_id": j.job_id,
        "installer_id": j.installer_id,
        "facility_id": j.facility_id,
        "activity_id": j.activity_id as i64,
        "blueprint_id": j.blueprint_item_id,
        "blueprint_type_id": j.blueprint_type_id as i64,
        "blueprint_location_id": bp_loc,
        "output_location_id": out_loc,
        "runs": j.runs as i64,
        "product_type_id": j.product_type_id as i64,
        "status": map_job_status(&j.status),
        "duration": j.duration_seconds as i64,
        "start_date": format_iso8601_utc(start_ts),
        "end_date": format_iso8601_utc(end_ts),
    });
    if is_corp {
        let loc_id = meta
            .as_ref()
            .map(|m| m.location_id)
            .filter(|&id| id > 0)
            .unwrap_or(j.facility_id);
        obj["location_id"] = json!(loc_id);
    } else {
        obj["station_id"] = json!(j.facility_id);
    }
    if let Some(ref m) = meta {
        if m.total_cost_isk > 0.0 {
            obj["cost"] = json!(m.total_cost_isk);
        }
        if m.invented_runs > 0 {
            obj["licensed_runs"] = json!(m.invented_runs);
        } else if j.activity_id == 5 {
            obj["licensed_runs"] = json!(j.runs as i64);
        }
        if m.inv_prob > 0.0 {
            obj["probability"] = json!(m.inv_prob);
        }
        if let Some(p) = m.paused_at_unix {
            obj["pause_date"] = json!(format_iso8601_utc(p));
        }
    }
    if j.status.eq_ignore_ascii_case("delivered") {
        let comp_ts = meta.as_ref().and_then(|m| m.completed_unix).unwrap_or(end_ts);
        let comp_char = meta
            .as_ref()
            .and_then(|m| m.completed_character_id)
            .unwrap_or(j.installer_id);
        obj["completed_date"] = json!(format_iso8601_utc(comp_ts));
        obj["completed_character_id"] = json!(comp_char);
        obj["successful_runs"] = json!(j.runs as i64);
    }
    obj
}
