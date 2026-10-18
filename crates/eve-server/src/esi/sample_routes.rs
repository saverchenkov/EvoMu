//! `esi::sample_routes` — Real SDE- and `WorldRepository`-backed ESI route handlers across all 31
//! OpenAPI tags (`ESI-2`).
//!
//! Dispatches matched OpenAPI operations against live `ServerContext` state (`ctx.sde` and `ctx.db`),
//! returning typed payloads, cache TTLs, and pagination metadata (`X-Pages`), or `501 Not Implemented`
//! for operations scheduled for `ESI-3`..`ESI-7`.

use crate::router::ServerContext;
use eve_db::{InventoryFlag, SsoTokenClaims};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::RwLock;

/// Outcome of dispatching a single ESI OpenAPI operation.
#[derive(Debug, Clone)]
pub struct RouteExecutionResult {
    pub status_code: u16,
    pub body: Option<Value>,
    pub max_age_secs: u64,
    pub total_pages: Option<usize>,
}

impl RouteExecutionResult {
    pub fn ok_json(body: Value, max_age_secs: u64) -> Self {
        Self {
            status_code: 200,
            body: Some(body),
            max_age_secs,
            total_pages: None,
        }
    }

    pub fn ok_paginated(body: Value, max_age_secs: u64, total_pages: usize) -> Self {
        Self {
            status_code: 200,
            body: Some(body),
            max_age_secs,
            total_pages: Some(total_pages.max(1)),
        }
    }

    pub fn no_content(max_age_secs: u64) -> Self {
        Self {
            status_code: 204,
            body: None,
            max_age_secs,
            total_pages: None,
        }
    }

    pub fn error(status_code: u16, msg: impl Into<String>) -> Self {
        let m = msg.into();
        Self {
            status_code,
            body: Some(json!({
                "error": m,
                "status": status_code,
            })),
            max_age_secs: 0,
            total_pages: None,
        }
    }
}

/// Paginates a slice of items (`1`-indexed `page`, `page_size` items per page).
/// Returns `(slice_vec, total_pages)` or `Err(RouteExecutionResult)` on out-of-range page.
pub fn paginate_slice<T: Clone>(
    items: &[T],
    page: usize,
    page_size: usize,
) -> Result<(Vec<T>, usize), RouteExecutionResult> {
    if page == 0 {
        return Err(RouteExecutionResult::error(
            400,
            "page parameter must be a positive integer >= 1",
        ));
    }
    let total_pages = items.len().div_ceil(page_size.max(1)).max(1);
    if page > total_pages {
        return Err(RouteExecutionResult::error(
            404,
            format!("Requested page {page} exceeds total pages {total_pages}"),
        ));
    }
    let start = (page - 1) * page_size;
    let end = (start + page_size).min(items.len());
    let slice = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    Ok((slice, total_pages))
}

pub(crate) fn parse_path_i64(
    path_params: &HashMap<String, String>,
    name: &str,
) -> Result<i64, RouteExecutionResult> {
    let raw = path_params.get(name).ok_or_else(|| {
        RouteExecutionResult::error(400, format!("Missing path parameter '{name}'"))
    })?;
    raw.parse::<i64>().map_err(|_| {
        RouteExecutionResult::error(
            400,
            format!("Invalid integer path parameter '{name}': '{raw}'"),
        )
    })
}

/// Formats a Unix epoch timestamp (seconds) as an ISO-8601 / RFC 3339 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_iso8601_utc(unix_secs: u64) -> String {
    let (year, month, day, hour, min, sec) = civil_from_unix(unix_secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Converts Unix epoch seconds into `(year, month, day, hour, minute, second)` in UTC.
pub fn civil_from_unix(unix_secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let secs_of_day = (unix_secs % 86_400) as u32;
    let hour = secs_of_day / 3_600;
    let min = (secs_of_day % 3_600) / 60;
    let sec = secs_of_day % 60;

    // Howard Hinnant's civil_from_days algorithm
    let z = (unix_secs / 86_400) as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = (y + if m <= 2 { 1 } else { 0 }) as i32;
    (year, m, d, hour, min, sec)
}

fn asset_location_flag_str(flag: InventoryFlag) -> Option<&'static str> {
    match flag {
        InventoryFlag::Hangar => Some("Hangar"),
        InventoryFlag::Cargo => Some("Cargo"),
        InventoryFlag::Skill => Some("Skill"),
        InventoryFlag::LoSlot0 => Some("LoSlot0"),
        InventoryFlag::LoSlot1 => Some("LoSlot1"),
        InventoryFlag::LoSlot2 => Some("LoSlot2"),
        InventoryFlag::LoSlot3 => Some("LoSlot3"),
        InventoryFlag::LoSlot4 => Some("LoSlot4"),
        InventoryFlag::LoSlot5 => Some("LoSlot5"),
        InventoryFlag::LoSlot6 => Some("LoSlot6"),
        InventoryFlag::LoSlot7 => Some("LoSlot7"),
        InventoryFlag::MedSlot0 => Some("MedSlot0"),
        InventoryFlag::MedSlot1 => Some("MedSlot1"),
        InventoryFlag::MedSlot2 => Some("MedSlot2"),
        InventoryFlag::MedSlot3 => Some("MedSlot3"),
        InventoryFlag::MedSlot4 => Some("MedSlot4"),
        InventoryFlag::MedSlot5 => Some("MedSlot5"),
        InventoryFlag::MedSlot6 => Some("MedSlot6"),
        InventoryFlag::MedSlot7 => Some("MedSlot7"),
        InventoryFlag::HiSlot0 => Some("HiSlot0"),
        InventoryFlag::HiSlot1 => Some("HiSlot1"),
        InventoryFlag::HiSlot2 => Some("HiSlot2"),
        InventoryFlag::HiSlot3 => Some("HiSlot3"),
        InventoryFlag::HiSlot4 => Some("HiSlot4"),
        InventoryFlag::HiSlot5 => Some("HiSlot5"),
        InventoryFlag::HiSlot6 => Some("HiSlot6"),
        InventoryFlag::HiSlot7 => Some("HiSlot7"),
        InventoryFlag::Locked => Some("Locked"),
        InventoryFlag::DroneBay => Some("DroneBay"),
        InventoryFlag::Implant => Some("Implant"),
        InventoryFlag::RigSlot0 => Some("RigSlot0"),
        InventoryFlag::RigSlot1 => Some("RigSlot1"),
        InventoryFlag::RigSlot2 => Some("RigSlot2"),
        InventoryFlag::SubSystemSlot0 => Some("SubSystemSlot0"),
        InventoryFlag::FighterBay => Some("FighterBay"),
        InventoryFlag::Deliveries => Some("Deliveries"),
        InventoryFlag::None
        | InventoryFlag::Capsule
        | InventoryFlag::Pilot
        | InventoryFlag::Booster => None,
    }
}

fn fitting_flag_str(flag: InventoryFlag) -> &'static str {
    match flag {
        InventoryFlag::Cargo => "Cargo",
        InventoryFlag::DroneBay => "DroneBay",
        InventoryFlag::FighterBay => "FighterBay",
        InventoryFlag::LoSlot0 => "LoSlot0",
        InventoryFlag::LoSlot1 => "LoSlot1",
        InventoryFlag::LoSlot2 => "LoSlot2",
        InventoryFlag::LoSlot3 => "LoSlot3",
        InventoryFlag::LoSlot4 => "LoSlot4",
        InventoryFlag::LoSlot5 => "LoSlot5",
        InventoryFlag::LoSlot6 => "LoSlot6",
        InventoryFlag::LoSlot7 => "LoSlot7",
        InventoryFlag::MedSlot0 => "MedSlot0",
        InventoryFlag::MedSlot1 => "MedSlot1",
        InventoryFlag::MedSlot2 => "MedSlot2",
        InventoryFlag::MedSlot3 => "MedSlot3",
        InventoryFlag::MedSlot4 => "MedSlot4",
        InventoryFlag::MedSlot5 => "MedSlot5",
        InventoryFlag::MedSlot6 => "MedSlot6",
        InventoryFlag::MedSlot7 => "MedSlot7",
        InventoryFlag::HiSlot0 => "HiSlot0",
        InventoryFlag::HiSlot1 => "HiSlot1",
        InventoryFlag::HiSlot2 => "HiSlot2",
        InventoryFlag::HiSlot3 => "HiSlot3",
        InventoryFlag::HiSlot4 => "HiSlot4",
        InventoryFlag::HiSlot5 => "HiSlot5",
        InventoryFlag::HiSlot6 => "HiSlot6",
        InventoryFlag::HiSlot7 => "HiSlot7",
        InventoryFlag::RigSlot0 => "RigSlot0",
        InventoryFlag::RigSlot1 => "RigSlot1",
        InventoryFlag::RigSlot2 => "RigSlot2",
        InventoryFlag::SubSystemSlot0 => "SubSystemSlot0",
        _ => "Invalid",
    }
}

fn compute_killmail_hash(
    kill_id: i64,
    victim_id: i64,
    attacker_id: i64,
    ship_type_id: u32,
) -> String {
    let material = format!("evomu-killmail-v1:{kill_id}:{victim_id}:{attacker_id}:{ship_type_id}");
    let mut h1: u64 = 0xcbf2_9ce4_8422_2325;
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for (i, b) in material.bytes().enumerate() {
        h1 ^= b as u64;
        h1 = h1.wrapping_mul(0x0000_0100_0000_01b3);
        h2 ^= (b as u64).wrapping_add(i as u64);
        h2 = h2.wrapping_mul(0x0000_0100_0000_01b3).rotate_left(5);
    }
    format!("{h1:016x}{h2:016x}{:08x}", (h1 ^ h2) as u32)
}

/// Returns `true` if `(method, path_pattern)` is implemented in `ESI-2` (returns `2xx` on valid input).
pub fn is_operation_implemented(method: &str, path_pattern: &str) -> bool {
    matches!(
        (method, path_pattern),
        ("GET", "/status")
            | ("GET", "/meta/compatibility-dates")
            | ("GET", "/alliances")
            | ("GET", "/corporations/npccorps")
            | ("GET", "/characters/{character_id}/standings")
            | ("GET", "/characters/{character_id}/wallet")
            | ("GET", "/characters/{character_id}/location")
            | ("GET", "/characters/{character_id}/skills")
            | ("GET", "/characters/{character_id}/attributes")
            | ("GET", "/characters/{character_id}/assets")
            | ("GET", "/characters/{character_id}/clones")
            | ("GET", "/dogma/attributes")
            | ("GET", "/dogma/effects")
            | ("GET", "/characters/{character_id}/fittings")
            | ("GET", "/characters/{character_id}/fleet")
            | ("GET", "/insurance/prices")
            | ("GET", "/characters/{character_id}/killmails/recent")
            | ("GET", "/characters/{character_id}/loyalty/points")
            | ("GET", "/characters/{character_id}/mail")
            | ("GET", "/markets/groups")
            | ("GET", "/universe/schematics/{schematic_id}")
            | ("GET", "/route/{origin}/{destination}")
            | ("GET", "/characters/{character_id}/search")
            | ("GET", "/sovereignty/map")
            | ("GET", "/universe/categories")
            | ("GET", "/universe/categories/{category_id}")
            | ("GET", "/universe/groups")
            | ("GET", "/universe/groups/{group_id}")
            | ("GET", "/universe/types")
            | ("GET", "/universe/regions")
            | ("GET", "/universe/regions/{region_id}")
            | ("GET", "/universe/constellations")
            | ("GET", "/universe/constellations/{constellation_id}")
            | ("GET", "/universe/systems")
            | ("GET", "/universe/systems/{system_id}")
            | ("GET", "/wars")
    )
}

/// Dispatches a validated OpenAPI operation against `ServerContext` and returns its execution result.
#[allow(clippy::too_many_lines)]
pub fn dispatch_operation(
    ctx: &ServerContext,
    started_at_unix: u64,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    page: usize,
    autopilot_waypoints: &RwLock<HashMap<i64, Vec<i64>>>,
) -> RouteExecutionResult {
    match (method, path_pattern) {
        // 1. Status tag
        ("GET", "/status") => {
            let online_players = ctx.db.with_snapshot(|snap| {
                snap.accounts
                    .values()
                    .filter(|acc| ctx.db.get_online_session(acc.user_id).is_some())
                    .count()
            });
            RouteExecutionResult::ok_json(
                json!({
                    "players": online_players as i64,
                    "server_version": eve_machonet::VersionExchange::default().build_version.to_string(),
                    "start_time": format_iso8601_utc(started_at_unix),
                    "vip": false,
                }),
                30,
            )
        }

        // 2. Meta tag
        ("GET", "/meta/compatibility-dates") => RouteExecutionResult::ok_json(
            json!({
                "compatibility_dates": crate::esi::openapi::OpenApiSpec::pinned().compatibility_dates,
            }),
            3600,
        ),

        // 3. Alliance tag
        ("GET", "/alliances") => {
            let mut ids: Vec<i64> = ctx
                .db
                .with_snapshot(|snap| snap.alliances.keys().copied().collect());
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }

        // 4. Corporation tag
        ("GET", "/corporations/npccorps") => {
            let mut ids: Vec<i64> = ctx.sde.npc_corporations.keys().copied().collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }

        // 5. Character tag
        ("GET", "/characters/{character_id}/standings") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
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
            })
        }

        // 6. Wallet tag
        ("GET", "/characters/{character_id}/wallet") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                RouteExecutionResult::ok_json(json!(ch.balance.to_wire_f64()), 120)
            })
        }

        // 7. Location tag
        ("GET", "/characters/{character_id}/location") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                let Some(ch) = snap.characters.get(&char_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                };
                let mut obj = json!({
                    "solar_system_id": ch.solar_system_id as i64,
                });
                if let Some(sid) = ch.station_id {
                    obj["station_id"] = json!(sid);
                }
                if let Some(stid) = ch.structure_id {
                    obj["structure_id"] = json!(stid);
                }
                RouteExecutionResult::ok_json(obj, 5)
            })
        }

        // 8. Skills tag
        ("GET", "/characters/{character_id}/skills") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
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
            })
        }
        ("GET", "/characters/{character_id}/attributes") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
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
            })
        }

        // 9. Assets tag
        ("GET", "/characters/{character_id}/assets") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut items: Vec<_> = snap
                    .items
                    .values()
                    .filter(|i| i.owner_id == char_id && i.quantity > 0)
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
            })
        }

        // 10. Calendar tag
        ("GET", "/characters/{character_id}/calendar") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return RouteExecutionResult::error(404, format!("Character {char_id} not found"));
            }
            RouteExecutionResult::error(
                501,
                "Character calendar event table is not yet active (ESI-7)",
            )
        }

        // 11. Clones tag
        ("GET", "/characters/{character_id}/clones") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
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
            })
        }

        // 12. Contacts tag
        ("GET", "/characters/{character_id}/contacts") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let exists = ctx
                .db
                .with_snapshot(|snap| snap.characters.contains_key(&char_id));
            if !exists {
                return RouteExecutionResult::error(404, format!("Character {char_id} not found"));
            }
            RouteExecutionResult::error(
                501,
                "Character contacts table is not yet active (ESI-6)",
            )
        }

        // 13. Contracts tag
        ("GET", "/characters/{character_id}/contracts/{contract_id}/items") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let contract_id = match parse_path_i64(path_params, "contract_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let Some(contract) = snap.contracts.get(&contract_id) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Contract {contract_id} not found"),
                    );
                };
                if contract.issuer_id != char_id && contract.acceptor_id != Some(char_id) {
                    return RouteExecutionResult::error(
                        403,
                        format!("Character {char_id} is not a party to contract {contract_id}"),
                    );
                }
                RouteExecutionResult::error(
                    501,
                    "Per-line contract item inclusion records are not yet active (ESI-5)",
                )
            })
        }

        // 14. Dogma tag
        ("GET", "/dogma/attributes") => {
            let mut ids: Vec<i64> = ctx.sde.attributes.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/dogma/effects") => {
            let mut ids: Vec<i64> = ctx.sde.effects.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }

        // 15. Faction Warfare tag
        ("GET", "/fw/wars") => RouteExecutionResult::error(
            501,
            "Faction warfare war table is not yet active (ESI-6)",
        ),

        // 16. Fittings tag
        ("GET", "/characters/{character_id}/fittings") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
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
            })
        }

        // 17. Fleets tag
        ("GET", "/characters/{character_id}/fleet") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let fleet = snap
                    .fleets
                    .values()
                    .find(|f| f.boss_id == char_id || f.members.contains(&char_id));
                let Some(f) = fleet else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} is not in a fleet"),
                    );
                };
                if f.boss_id != char_id {
                    return RouteExecutionResult::error(
                        501,
                        "Per-member fleet wing/squad hierarchy is not yet active (ESI-6)",
                    );
                }
                RouteExecutionResult::ok_json(
                    json!({
                        "fleet_id": f.fleet_id,
                        "fleet_boss_id": f.boss_id,
                        "wing_id": -1,
                        "squad_id": -1,
                        "role": "fleet_commander",
                    }),
                    60,
                )
            })
        }

        // 18. Incursions tag
        ("GET", "/incursions") => RouteExecutionResult::error(
            501,
            "Active incursion table is not yet active (ESI-5)",
        ),

        // 19. Industry tag
        ("GET", "/industry/facilities") => RouteExecutionResult::error(
            501,
            "Industry facility service table is not yet active (ESI-5)",
        ),

        // 20. Insurance tag
        ("GET", "/insurance/prices") => {
            // Six wiki insurance tiers (Basic 5%/50%, Standard 10%/60%, Bronze 15%/70%,
            // Silver 20%/80%, Gold 25%/90%, Platinum 30%/100%; eve-wiki/articles/Insurance.md:83-88),
            // with ship reference value derived from SDE `base_price`.
            let mut ship_types: Vec<_> = ctx
                .sde
                .types
                .values()
                .filter(|t| t.category_id == 6 && t.published && t.base_price > 0.0)
                .collect();
            ship_types.sort_by_key(|t| t.type_id);
            let out: Vec<Value> = ship_types
                .into_iter()
                .map(|t| {
                    let base = t.base_price;
                    json!({
                        "type_id": t.type_id as i64,
                        "levels": [
                            { "name": "Basic", "cost": base * 0.05, "payout": base * 0.50 },
                            { "name": "Standard", "cost": base * 0.10, "payout": base * 0.60 },
                            { "name": "Bronze", "cost": base * 0.15, "payout": base * 0.70 },
                            { "name": "Silver", "cost": base * 0.20, "payout": base * 0.80 },
                            { "name": "Gold", "cost": base * 0.25, "payout": base * 0.90 },
                            { "name": "Platinum", "cost": base * 0.30, "payout": base * 1.00 },
                        ]
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        }

        // 21. Killmails tag
        ("GET", "/characters/{character_id}/killmails/recent") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut kms: Vec<_> = snap
                    .killmails
                    .values()
                    .filter(|k| {
                        k.victim_character_id == char_id || k.attacker_character_id == char_id
                    })
                    .collect();
                kms.sort_by_key(|k| std::cmp::Reverse(k.kill_id));
                let rows: Vec<Value> = kms
                    .into_iter()
                    .map(|k| {
                        json!({
                            "killmail_id": k.kill_id,
                            "killmail_hash": compute_killmail_hash(
                                k.kill_id,
                                k.victim_character_id,
                                k.attacker_character_id,
                                k.ship_type_id,
                            ),
                        })
                    })
                    .collect();
                match paginate_slice(&rows, page, 1000) {
                    Ok((page_rows, total_pages)) => {
                        RouteExecutionResult::ok_paginated(json!(page_rows), 300, total_pages)
                    }
                    Err(e) => e,
                }
            })
        }

        // 22. Loyalty tag
        ("GET", "/characters/{character_id}/loyalty/points") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
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
            })
        }

        // 23. Mail tag
        ("GET", "/characters/{character_id}/mail") => {
            let char_id = match parse_path_i64(path_params, "character_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Character {char_id} not found"),
                    );
                }
                let mut mails: Vec<_> = snap
                    .mails
                    .iter()
                    .filter(|m| m.recipient_id == char_id || m.sender_id == char_id)
                    .collect();
                mails.sort_by_key(|m| std::cmp::Reverse(m.message_id));
                let out: Vec<Value> = mails
                    .into_iter()
                    .take(50)
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
                            "mail_id": m.message_id,
                            "from": m.sender_id,
                            "subject": m.subject,
                            "recipients": [{
                                "recipient_id": m.recipient_id,
                                "recipient_type": recipient_type,
                            }],
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(json!(out), 30)
            })
        }

        // 24. Market tag
        ("GET", "/markets/groups") => {
            let mut ids: Vec<i64> = ctx.sde.market_groups.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }

        // 25. Planetary Interaction tag
        ("GET", "/universe/schematics/{schematic_id}") => {
            let schematic_id = match parse_path_i64(path_params, "schematic_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(schem_u32) = u32::try_from(schematic_id) else {
                return RouteExecutionResult::error(
                    404,
                    format!("Schematic {schematic_id} not found"),
                );
            };
            let Some(schem) = ctx.sde.planet_schematics.get(&schem_u32) else {
                return RouteExecutionResult::error(
                    404,
                    format!("Schematic {schematic_id} not found"),
                );
            };
            RouteExecutionResult::ok_json(
                json!({
                    "schematic_name": schem.name,
                    "cycle_time": schem.cycle_time as i64,
                }),
                3600,
            )
        }

        // 26..31. Routes, Search, Sovereignty, Universe, User Interface, and Wars tags
        _ => super::universe_routes::dispatch_universe_and_nav(
            ctx,
            method,
            path_pattern,
            path_params,
            query_params,
            claims,
            page,
            autopilot_waypoints,
        ),
    }
}
