//! `esi::economy_industry_routes` — Industry, Mining and Planetary Interaction routes split out of `esi::economy_routes` (`ESI-6`).

#![forbid(unsafe_code)]

// parity: esi:GET /characters/{character_id}/industry/jobs, esi:GET /characters/{character_id}/mining
// parity: esi:GET /corporations/{corporation_id}/industry/jobs
// parity: esi:GET /corporation/{corporation_id}/mining/extractions
// parity: esi:GET /corporation/{corporation_id}/mining/observers
// parity: esi:GET /corporation/{corporation_id}/mining/observers/{observer_id}
// parity: esi:GET /characters/{character_id}/planets, esi:GET /universe/schematics/{schematic_id}
// parity: feat:esi.industry.char-jobs, feat:esi.industry.char-mining
// parity: feat:esi.industry.corp-jobs, feat:esi.industry.corp-extractions, feat:esi.industry.corp-observers
// parity: feat:esi.planetary-interaction.colonies, feat:esi.planetary-interaction.schematics

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
                    .map(|j| {
                        let start_ts = j.start_unix;
                        let end_ts = start_ts + j.duration_seconds;
                        // DERIVED: IndustryJobRecord stores facility_id as the job's station/blueprint/output location
                        json!({
                            "job_id": j.job_id,
                            "installer_id": j.installer_id,
                            "facility_id": j.facility_id,
                            "station_id": j.facility_id,
                            "activity_id": j.activity_id as i64,
                            "blueprint_id": j.blueprint_item_id,
                            "blueprint_type_id": j.blueprint_type_id as i64,
                            "blueprint_location_id": j.facility_id,
                            "output_location_id": j.facility_id,
                            "runs": j.runs as i64,
                            "product_type_id": j.product_type_id as i64,
                            "status": map_job_status(&j.status),
                            "duration": j.duration_seconds as i64,
                            "start_date": format_iso8601_utc(start_ts),
                            "end_date": format_iso8601_utc(end_ts),
                        })
                    })
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
                        (j.installer_id == corp_id
                            || corp.members.contains(&j.installer_id)
                            || j.installer_id == corp.ceo_id)
                            && (include_completed || j.status.eq_ignore_ascii_case("active"))
                    })
                    .collect();
                jobs.sort_by_key(|j| j.job_id);
                let rows: Vec<Value> = jobs
                    .into_iter()
                    .map(|j| {
                        let start_ts = j.start_unix;
                        let end_ts = start_ts + j.duration_seconds;
                        // DERIVED: IndustryJobRecord stores facility_id as the job's location/blueprint/output location
                        json!({
                            "job_id": j.job_id,
                            "installer_id": j.installer_id,
                            "facility_id": j.facility_id,
                            "location_id": j.facility_id,
                            "activity_id": j.activity_id as i64,
                            "blueprint_id": j.blueprint_item_id,
                            "blueprint_type_id": j.blueprint_type_id as i64,
                            "blueprint_location_id": j.facility_id,
                            "output_location_id": j.facility_id,
                            "runs": j.runs as i64,
                            "product_type_id": j.product_type_id as i64,
                            "status": map_job_status(&j.status),
                            "duration": j.duration_seconds as i64,
                            "start_date": format_iso8601_utc(start_ts),
                            "end_date": format_iso8601_utc(end_ts),
                        })
                    })
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

        _ => None,
    }
}
