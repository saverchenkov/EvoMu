//! `esi::sample_routes` — Real SDE- and `WorldRepository`-backed ESI route dispatcher across all 31
//! OpenAPI tags (`ESI-2`, `ESI-3`, `ESI-4`).
//!
//! Dispatches matched OpenAPI operations against live `ServerContext` state (`ctx.sde` and `ctx.db`),
//! returning typed payloads, cache TTLs, and pagination metadata (`X-Pages`), or `501 Not Implemented`
//! for operations whose backing tables are scheduled for later tasks.

use crate::router::ServerContext;
use eve_db::{InventoryFlag, SsoTokenClaims};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::HashMap;

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

    pub fn created_json(body: Value) -> Self {
        Self {
            status_code: 201,
            body: Some(body),
            max_age_secs: 0,
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

    pub fn ok_paginated_json(body: Value, max_age_secs: u64, total_pages: usize) -> Self {
        Self::ok_paginated(body, max_age_secs, total_pages)
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

impl eve_db::FromWalError for RouteExecutionResult {
    fn from_wal_error(err: String) -> Self {
        Self::error(500, format!("Durable persistence failure: {err}"))
    }
}

/// Opens the on-disk SQLite SDE database (`ctx.sde.db_path`) read-only with `?immutable=1` for celestials/stargates/dogma queries.
pub(crate) fn open_sde_sqlite(ctx: &ServerContext) -> Option<Connection> {
    let path = ctx
        .sde
        .db_path
        .clone()
        .unwrap_or_else(eve_sde::SdeRegistry::default_db_path);
    let path_str = path.to_string_lossy();
    // Strip Windows extended path prefix for SQLite URI compatibility
    let clean_path = if cfg!(windows) && path_str.starts_with(r"\\?\") {
        &path_str[4..]
    } else {
        &*path_str
    };
    let uri = format!("file:{}?immutable=1", clean_path);
    Connection::open_with_flags(
        &uri,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
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

pub(crate) fn asset_location_flag_str(flag: InventoryFlag) -> Option<&'static str> {
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
        InventoryFlag::Unlocked => Some("Unlocked"),
        InventoryFlag::DroneBay => Some("DroneBay"),
        InventoryFlag::Implant => Some("Implant"),
        InventoryFlag::RigSlot0 => Some("RigSlot0"),
        InventoryFlag::RigSlot1 => Some("RigSlot1"),
        InventoryFlag::RigSlot2 => Some("RigSlot2"),
        InventoryFlag::CorpSAG1 => Some("CorpSAG1"),
        InventoryFlag::CorpSAG2 => Some("CorpSAG2"),
        InventoryFlag::CorpSAG3 => Some("CorpSAG3"),
        InventoryFlag::CorpSAG4 => Some("CorpSAG4"),
        InventoryFlag::CorpSAG5 => Some("CorpSAG5"),
        InventoryFlag::CorpSAG6 => Some("CorpSAG6"),
        InventoryFlag::CorpSAG7 => Some("CorpSAG7"),
        InventoryFlag::SubSystemSlot0 => Some("SubSystemSlot0"),
        InventoryFlag::SubSystemSlot1 => Some("SubSystemSlot1"),
        InventoryFlag::SubSystemSlot2 => Some("SubSystemSlot2"),
        InventoryFlag::SubSystemSlot3 => Some("SubSystemSlot3"),
        InventoryFlag::SubSystemSlot4 => Some("SubSystemSlot4"),
        InventoryFlag::FighterBay => Some("FighterBay"),
        InventoryFlag::Deliveries => Some("Deliveries"),
        InventoryFlag::CorpDeliveries => Some("CorpDeliveries"),
        InventoryFlag::StructureDeedBay => Some("QuantumCoreRoom"),
        InventoryFlag::SpecializedFuelBay => Some("SpecializedFuelBay"),
        InventoryFlag::StructureFuel => Some("StructureFuel"),
        InventoryFlag::SpecializedOreHold => Some("SpecializedOreHold"),
        InventoryFlag::SpecializedGasHold => Some("SpecializedGasHold"),
        InventoryFlag::SpecializedMineralHold => Some("SpecializedMineralHold"),
        InventoryFlag::SpecializedSalvageHold => Some("SpecializedSalvageHold"),
        InventoryFlag::SpecializedShipHold => Some("SpecializedShipHold"),
        InventoryFlag::SpecializedAmmoHold => Some("SpecializedAmmoHold"),
        InventoryFlag::SpecializedCommandCenterHold => Some("SpecializedCommandCenterHold"),
        InventoryFlag::SpecializedPlanetaryCommoditiesHold => Some("SpecializedPlanetaryCommoditiesHold"),
        InventoryFlag::SpecializedMaterialBay => Some("SpecializedMaterialBay"),
        InventoryFlag::SpecializedIceHold => Some("SpecializedIceHold"),
        InventoryFlag::SpecializedAsteroidHold => Some("SpecializedAsteroidHold"),
        InventoryFlag::SpecializedMobileDepotHold => Some("MobileDepotHold"),
        InventoryFlag::SpecializedInfrastructureHold => Some("InfrastructureHold"),
        InventoryFlag::ShipHangar => Some("ShipHangar"),
        InventoryFlag::FleetHangar => Some("FleetHangar"),
        InventoryFlag::ServiceSlot0 => Some("ServiceSlot0"),
        InventoryFlag::ServiceSlot1 => Some("ServiceSlot1"),
        InventoryFlag::ServiceSlot2 => Some("ServiceSlot2"),
        InventoryFlag::ServiceSlot3 => Some("ServiceSlot3"),
        InventoryFlag::ServiceSlot4 => Some("ServiceSlot4"),
        InventoryFlag::ServiceSlot5 => Some("ServiceSlot5"),
        InventoryFlag::ServiceSlot6 => Some("ServiceSlot6"),
        InventoryFlag::ServiceSlot7 => Some("ServiceSlot7"),
        InventoryFlag::None
        | InventoryFlag::Capsule
        | InventoryFlag::Pilot
        | InventoryFlag::Booster
        | InventoryFlag::Impounded => None,
    }
}

pub(crate) fn fitting_flag_str(flag: InventoryFlag) -> &'static str {
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
        InventoryFlag::SubSystemSlot1 => "SubSystemSlot1",
        InventoryFlag::SubSystemSlot2 => "SubSystemSlot2",
        InventoryFlag::SubSystemSlot3 => "SubSystemSlot3",
        InventoryFlag::ServiceSlot0 => "ServiceSlot0",
        InventoryFlag::ServiceSlot1 => "ServiceSlot1",
        InventoryFlag::ServiceSlot2 => "ServiceSlot2",
        InventoryFlag::ServiceSlot3 => "ServiceSlot3",
        InventoryFlag::ServiceSlot4 => "ServiceSlot4",
        InventoryFlag::ServiceSlot5 => "ServiceSlot5",
        InventoryFlag::ServiceSlot6 => "ServiceSlot6",
        InventoryFlag::ServiceSlot7 => "ServiceSlot7",
        // Every other flag (including `SubSystemSlot4`) is outside the pinned ESI fitting-item
        // `flag` enum, which names it `Invalid`.
        _ => "Invalid",
    }
}

/// Every inventory flag the pinned ESI fitting-item `flag` enum can name (all enum values except
/// `Invalid`). `parse_fitting_flag` is the exact inverse of `fitting_flag_str` over this set.
const ESI_FITTING_FLAGS: [InventoryFlag; 42] = [
    InventoryFlag::Cargo,
    InventoryFlag::DroneBay,
    InventoryFlag::FighterBay,
    InventoryFlag::HiSlot0,
    InventoryFlag::HiSlot1,
    InventoryFlag::HiSlot2,
    InventoryFlag::HiSlot3,
    InventoryFlag::HiSlot4,
    InventoryFlag::HiSlot5,
    InventoryFlag::HiSlot6,
    InventoryFlag::HiSlot7,
    InventoryFlag::LoSlot0,
    InventoryFlag::LoSlot1,
    InventoryFlag::LoSlot2,
    InventoryFlag::LoSlot3,
    InventoryFlag::LoSlot4,
    InventoryFlag::LoSlot5,
    InventoryFlag::LoSlot6,
    InventoryFlag::LoSlot7,
    InventoryFlag::MedSlot0,
    InventoryFlag::MedSlot1,
    InventoryFlag::MedSlot2,
    InventoryFlag::MedSlot3,
    InventoryFlag::MedSlot4,
    InventoryFlag::MedSlot5,
    InventoryFlag::MedSlot6,
    InventoryFlag::MedSlot7,
    InventoryFlag::RigSlot0,
    InventoryFlag::RigSlot1,
    InventoryFlag::RigSlot2,
    InventoryFlag::ServiceSlot0,
    InventoryFlag::ServiceSlot1,
    InventoryFlag::ServiceSlot2,
    InventoryFlag::ServiceSlot3,
    InventoryFlag::ServiceSlot4,
    InventoryFlag::ServiceSlot5,
    InventoryFlag::ServiceSlot6,
    InventoryFlag::ServiceSlot7,
    InventoryFlag::SubSystemSlot0,
    InventoryFlag::SubSystemSlot1,
    InventoryFlag::SubSystemSlot2,
    InventoryFlag::SubSystemSlot3,
];

/// Parses a pinned-enum ESI fitting-item `flag`. Returns `None` for `Invalid` and for any string
/// outside the enum; the POST handler discards `Invalid` entries and rejects unknown strings.
pub(crate) fn parse_fitting_flag(flag_str: &str) -> Option<InventoryFlag> {
    ESI_FITTING_FLAGS
        .iter()
        .copied()
        .find(|f| fitting_flag_str(*f) == flag_str)
}

pub fn compute_killmail_hash(
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

/// Returns `true` if `(method, path_pattern)` is fully implemented (`2xx` on valid input).
pub fn is_operation_implemented(method: &str, path_pattern: &str) -> bool {
    if super::fleet_routes::is_fleet_operation_implemented(method, path_pattern) {
        return true;
    }
    matches!(
        (method, path_pattern),
        ("GET", "/status")
            | ("GET", "/meta/compatibility-dates")
            | ("GET", "/alliances")
            | ("GET", "/corporations/npccorps")
            | ("POST", "/characters/affiliation")
            | ("GET", "/characters/{character_id}/portrait")
            | ("GET", "/characters/{character_id}/standings")
            | ("GET", "/characters/{character_id}/fatigue")
            | ("GET", "/characters/{character_id}/blueprints")
            | ("GET", "/characters/{character_id}/wallet")
            | ("GET", "/corporations/{corporation_id}/wallets")
            | ("GET", "/characters/{character_id}/location")
            | ("GET", "/characters/{character_id}/online")
            | ("GET", "/characters/{character_id}/ship")
            | ("GET", "/characters/{character_id}/skills")
            | ("GET", "/characters/{character_id}/attributes")
            | ("GET", "/characters/{character_id}/skillqueue")
            | ("GET", "/characters/{character_id}/assets")
            | ("POST", "/characters/{character_id}/assets/locations")
            | ("POST", "/characters/{character_id}/assets/names")
            | ("GET", "/corporations/{corporation_id}/assets")
            | ("POST", "/corporations/{corporation_id}/assets/locations")
            | ("POST", "/corporations/{corporation_id}/assets/names")
            | ("GET", "/characters/{character_id}/clones")
            | ("GET", "/characters/{character_id}/implants")
            | ("GET", "/dogma/attributes")
            | ("GET", "/dogma/attributes/{attribute_id}")
            | ("GET", "/dogma/effects")
            | ("GET", "/dogma/effects/{effect_id}")
            | ("GET", "/dogma/dynamic/items/{type_id}/{item_id}")
            | ("GET", "/characters/{character_id}/fittings")
            | ("POST", "/characters/{character_id}/fittings")
            | ("DELETE", "/characters/{character_id}/fittings/{fitting_id}")
            | ("GET", "/insurance/prices")
            | ("GET", "/characters/{character_id}/killmails/recent")
            | ("GET", "/corporations/{corporation_id}/killmails/recent")
            | ("GET", "/characters/{character_id}/loyalty/points")
            | ("GET", "/characters/{character_id}/mail")
            | ("POST", "/characters/{character_id}/mail")
            | ("GET", "/characters/{character_id}/mail/{mail_id}")
            | ("DELETE", "/characters/{character_id}/mail/{mail_id}")
            | ("GET", "/markets/groups")
            | ("GET", "/universe/schematics/{schematic_id}")
            | ("GET", "/route/{origin}/{destination}")
            | ("GET", "/characters/{character_id}/search")
            | ("GET", "/sovereignty/map")
            | ("GET", "/sovereignty/structures")
            | ("GET", "/universe/categories")
            | ("GET", "/universe/categories/{category_id}")
            | ("GET", "/universe/groups")
            | ("GET", "/universe/groups/{group_id}")
            | ("GET", "/universe/types")
            | ("GET", "/universe/types/{type_id}")
            | ("GET", "/universe/regions")
            | ("GET", "/universe/regions/{region_id}")
            | ("GET", "/universe/constellations")
            | ("GET", "/universe/constellations/{constellation_id}")
            | ("GET", "/universe/systems")
            | ("GET", "/universe/systems/{system_id}")
            | ("GET", "/universe/stars/{star_id}")
            | ("GET", "/universe/planets/{planet_id}")
            | ("GET", "/universe/moons/{moon_id}")
            | ("GET", "/universe/asteroid_belts/{asteroid_belt_id}")
            | ("GET", "/universe/stargates/{stargate_id}")
            | ("GET", "/universe/structures")
            | ("GET", "/universe/structures/{structure_id}")
            | ("GET", "/universe/factions")
            | ("GET", "/universe/bloodlines")
            | ("GET", "/universe/ancestries")
            | ("POST", "/universe/names")
            | ("POST", "/universe/ids")
            | ("GET", "/wars")
            | ("GET", "/wars/{war_id}")
            | ("GET", "/wars/{war_id}/killmails")
            | ("GET", "/industry/facilities")
            | ("GET", "/industry/systems")
            | ("GET", "/characters/{character_id}/industry/jobs")
            | ("GET", "/corporations/{corporation_id}/industry/jobs")
            | ("GET", "/markets/{region_id}/orders")
            | ("GET", "/markets/{region_id}/history")
            | ("GET", "/markets/prices")
            | ("GET", "/markets/{region_id}/types")
            | ("GET", "/markets/structures/{structure_id}")
            | ("GET", "/characters/{character_id}/orders")
            | ("GET", "/characters/{character_id}/orders/history")
            | ("GET", "/corporations/{corporation_id}/orders")
            | ("GET", "/corporations/{corporation_id}/orders/history")
            | ("GET", "/characters/{character_id}/wallet/journal")
            | ("GET", "/characters/{character_id}/wallet/transactions")
            | ("GET", "/characters/{character_id}/contracts")
            | ("GET", "/characters/{character_id}/contracts/{contract_id}/items")
            | ("GET", "/characters/{character_id}/contracts/{contract_id}/bids")
            | ("GET", "/contracts/public/{region_id}")
            | ("GET", "/contracts/public/items/{contract_id}")
            | ("GET", "/contracts/public/bids/{contract_id}")
            | ("GET", "/corporations/{corporation_id}/contracts")
            | ("GET", "/corporations/{corporation_id}/contracts/{contract_id}/items")
            | ("GET", "/corporations/{corporation_id}/contracts/{contract_id}/bids")
            | ("GET", "/corporations/{corporation_id}")
            | ("GET", "/corporations/{corporation_id}/members")
            | ("GET", "/corporations/{corporation_id}/roles")
            | ("GET", "/alliances/{alliance_id}")
            | ("GET", "/alliances/{alliance_id}/corporations")
            | ("GET", "/alliances/{alliance_id}/icons")
            | ("GET", "/characters/{character_id}/bookmarks")
            | ("GET", "/characters/{character_id}/bookmarks/folders")
            | ("GET", "/incursions")
            | ("GET", "/meta/changelog")
            | ("GET", "/sovereignty/campaigns")
            | ("GET", "/killmails/{killmail_id}/{killmail_hash}")
            | ("GET", "/universe/graphics")
            | ("GET", "/universe/graphics/{graphic_id}")
            | ("GET", "/universe/races")
            | ("GET", "/universe/stations/{station_id}")
            | ("GET", "/universe/system_jumps")
            | ("GET", "/universe/system_kills")
            | ("GET", "/fw/wars")
            | ("GET", "/fw/stats")
            | ("GET", "/fw/systems")
            | ("GET", "/fw/leaderboards")
            | ("GET", "/fw/leaderboards/characters")
            | ("GET", "/fw/leaderboards/corporations")
            | ("GET", "/characters/{character_id}/fw/stats")
            | ("GET", "/corporations/{corporation_id}/fw/stats")
            | ("GET", "/opportunities/tasks")
            | ("GET", "/opportunities/groups")
            | ("GET", "/characters/{character_id}/opportunities")
            | ("GET", "/characters/{character_id}/planets")
            | ("GET", "/characters/{character_id}/planets/{planet_id}")
    )
}

/// Dispatches a validated OpenAPI operation against `ServerContext` and returns its execution result.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn dispatch_operation(
    ctx: &ServerContext,
    started_at_unix: u64,
    method: &str,
    path_pattern: &str,
    path_params: &HashMap<String, String>,
    query_params: &HashMap<String, String>,
    claims: Option<&SsoTokenClaims>,
    page: usize,
    req_body: &[u8],
) -> RouteExecutionResult {
    if let Some(res) = super::character_routes::dispatch_character_routes(
        ctx,
        method,
        path_pattern,
        path_params,
        query_params,
        claims,
        page,
        req_body,
    ) {
        return res;
    }

    if let Some(res) = super::corp_alliance_routes::dispatch_corp_alliance_route(
        ctx,
        method,
        path_pattern,
        path_params,
        query_params,
    ) {
        return res;
    }

    if let Some(res) = super::char_social_routes::dispatch_char_social_route(
        ctx,
        method,
        path_pattern,
        path_params,
        query_params,
        req_body,
    ) {
        return res;
    }

    if let Some(res) = super::fleet_routes::dispatch_fleet_routes(
        ctx,
        method,
        path_pattern,
        path_params,
        query_params,
        claims,
        req_body,
    ) {
        return res;
    }

    if let Some(res) = super::sov_fw_routes::dispatch_sov_route(ctx, method, path_pattern) {
        return res;
    }

    if let Some(res) = super::fw_universe_routes::dispatch_fw_universe_route(
        ctx,
        method,
        path_pattern,
        path_params,
        query_params,
    ) {
        return res;
    }

    if let Some(res) = super::economy_routes::dispatch_economy_route(
        ctx,
        method,
        path_pattern,
        path_params,
        query_params,
        claims,
        page,
    ) {
        return res;
    }

    match (method, path_pattern) {
        // 1. Status tag
        ("GET", "/status") => {
            let user_ids: Vec<u64> = ctx
                .db
                .with_snapshot(|snap| snap.accounts.values().map(|acc| acc.user_id).collect());
            let online_players = user_ids
                .into_iter()
                .filter(|&uid| ctx.db.get_online_session(uid).is_some())
                .count();
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

        // 5. Contracts tag (ESI-6)
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
                    "Per-line contract item inclusion records are not yet active (ESI-6)",
                )
            })
        }

        // 6. Dogma tag (ESI-3)
        ("GET", "/dogma/attributes") => {
            let mut ids: Vec<i64> = ctx.sde.attributes.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/dogma/attributes/{attribute_id}") => {
            let aid = match parse_path_i64(path_params, "attribute_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(aid_u16) = u16::try_from(aid) else {
                return RouteExecutionResult::error(
                    404,
                    format!("Dogma attribute {aid} not found"),
                );
            };
            let Some(attr) = ctx.sde.attributes.get(&aid_u16) else {
                return RouteExecutionResult::error(
                    404,
                    format!("Dogma attribute {aid} not found"),
                );
            };
            let published = open_sde_sqlite(ctx)
                .and_then(|conn| {
                    conn.query_row(
                        "SELECT published FROM sde_dogma_attributes WHERE attribute_id = ?1",
                        [aid],
                        |r| r.get::<_, i64>(0),
                    )
                    .ok()
                })
                .map(|v| v != 0)
                .unwrap_or(true);
            RouteExecutionResult::ok_json(
                json!({
                    "attribute_id": attr.attribute_id as i64,
                    "name": attr.name,
                    "default_value": attr.default_value,
                    "high_is_good": attr.high_is_good,
                    "stackable": attr.stackable,
                    "published": published,
                }),
                3600,
            )
        }
        ("GET", "/dogma/effects") => {
            let mut ids: Vec<i64> = ctx.sde.effects.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }
        ("GET", "/dogma/effects/{effect_id}") => {
            let eid = match parse_path_i64(path_params, "effect_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(eid_u16) = u16::try_from(eid) else {
                return RouteExecutionResult::error(404, format!("Dogma effect {eid} not found"));
            };
            let Some(eff) = ctx.sde.effects.get(&eid_u16) else {
                return RouteExecutionResult::error(404, format!("Dogma effect {eid} not found"));
            };
            let modifiers: Vec<Value> = eff
                .modifiers
                .iter()
                .map(|m| {
                    json!({
                        "domain": m.domain,
                        "func": m.func,
                        "modified_attribute_id": m.modified_attribute_id as i64,
                        "modifying_attribute_id": m.modifying_attribute_id as i64,
                        "operator": m.operator as i64,
                    })
                })
                .collect();
            let mut obj = json!({
                "effect_id": eff.effect_id as i64,
                "name": eff.name,
                "effect_category": eff.effect_category as i64,
                "is_offensive": eff.is_offensive,
                "is_assistance": eff.is_assistance,
                "modifiers": modifiers,
            });
            if let Some(conn) = open_sde_sqlite(ctx) {
                if let Ok((warp_safe, disallow_repeat, elec_chance, range_chance, published)) = conn
                    .query_row(
                        "SELECT is_warp_safe, disallow_auto_repeat, electronic_chance, range_chance, published FROM sde_dogma_effects WHERE effect_id = ?1",
                        [eid],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)? != 0,
                                r.get::<_, i64>(1)? != 0,
                                r.get::<_, i64>(2)? != 0,
                                r.get::<_, i64>(3)? != 0,
                                r.get::<_, i64>(4)? != 0,
                            ))
                        },
                    )
                {
                    obj["is_warp_safe"] = json!(warp_safe);
                    obj["disallow_auto_repeat"] = json!(disallow_repeat);
                    obj["electronic_chance"] = json!(elec_chance);
                    obj["range_chance"] = json!(range_chance);
                    obj["published"] = json!(published);
                }
            }
            RouteExecutionResult::ok_json(obj, 3600)
        }
        ("GET", "/dogma/dynamic/items/{type_id}/{item_id}") => {
            let tid = match parse_path_i64(path_params, "type_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let iid = match parse_path_i64(path_params, "item_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            let Ok(tid_u32) = u32::try_from(tid) else {
                return RouteExecutionResult::error(404, format!("Dynamic item {iid} not found"));
            };
            ctx.db.with_snapshot(|snap| {
                let Some(item) = snap.items.get(&iid) else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Dynamic item {iid} not found"),
                    );
                };
                if item.type_id != tid_u32 {
                    return RouteExecutionResult::error(
                        404,
                        format!("Item {iid} does not match type_id {tid}"),
                    );
                }
                let Some(muta_id) = item.source_mutaplasmid_id else {
                    return RouteExecutionResult::error(
                        404,
                        format!("Item {iid} is not a mutated dynamic item"),
                    );
                };
                let source_type_id = ctx
                    .sde
                    .dynamic_item_attributes
                    .get(&muta_id)
                    .and_then(|m| m.applicable_type_ids.first().copied())
                    .unwrap_or(item.type_id) as i64;
                let mut merged_attrs: HashMap<u16, f64> = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .map(|t| t.default_attributes.clone())
                    .unwrap_or_default();
                for (&k, &v) in &item.dynamic_attributes {
                    merged_attrs.insert(k, v);
                }
                let mut attr_keys: Vec<_> = merged_attrs.keys().copied().collect();
                attr_keys.sort_unstable();
                let dogma_attributes: Vec<Value> = attr_keys
                    .into_iter()
                    .map(|aid| {
                        json!({
                            "attribute_id": aid as i64,
                            "value": merged_attrs[&aid],
                        })
                    })
                    .collect();
                let mut eff_ids = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .map(|t| t.effect_ids.clone())
                    .unwrap_or_default();
                eff_ids.sort_unstable();
                eff_ids.dedup();
                let default_eid = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .and_then(|t| t.default_effect_id);
                let dogma_effects: Vec<Value> = eff_ids
                    .into_iter()
                    .map(|eid| {
                        json!({
                            "effect_id": eid as i64,
                            "is_default": Some(eid) == default_eid,
                        })
                    })
                    .collect();
                RouteExecutionResult::ok_json(
                    json!({
                        "created_by": item.owner_id,
                        "mutator_type_id": muta_id as i64,
                        "source_type_id": source_type_id,
                        "dogma_attributes": dogma_attributes,
                        "dogma_effects": dogma_effects,
                    }),
                    3600,
                )
            })
        }

        // 7. Faction Warfare tag (ESI-3)
        ("GET", "/fw/wars") => RouteExecutionResult::error(
            501,
            "Faction warfare war table is not yet active (ESI-3)",
        ),

        // 9. Incursions tag (ESI-3)
        ("GET", "/incursions") => RouteExecutionResult::error(
            501,
            "Active incursion table is not yet active (ESI-3)",
        ),

        // 10. Industry tag (ESI-6)
        ("GET", "/industry/facilities") => RouteExecutionResult::error(
            501,
            "Industry facility service table is not yet active (ESI-6)",
        ),

        // 11. Insurance tag (ESI-3)
        // parity: esi:GET /insurance/prices, feat:esi.insurance.prices
        ("GET", "/insurance/prices") => {
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
                    let ref_val =
                        crate::formulas::compute_ship_insurance_reference_value(&ctx.sde, t.type_id);
                    let levels: Vec<Value> = crate::formulas::compute_insurance_levels(ref_val)
                        .into_iter()
                        .map(|(name, cost, payout)| {
                            json!({
                                "name": name,
                                "cost": cost,
                                "payout": payout,
                            })
                        })
                        .collect();
                    json!({
                        "type_id": t.type_id as i64,
                        "levels": levels,
                    })
                })
                .collect();
            RouteExecutionResult::ok_json(json!(out), 3600)
        }

        // 12. Killmails tag (ESI-3)
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
        ("GET", "/corporations/{corporation_id}/killmails/recent") => {
            let corp_id = match parse_path_i64(path_params, "corporation_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                if !snap.corporations.contains_key(&corp_id) {
                    return RouteExecutionResult::error(
                        404,
                        format!("Corporation {corp_id} not found"),
                    );
                }
                let mut kms: Vec<_> = snap
                    .killmails
                    .values()
                    .filter(|k| {
                        let v_corp = snap
                            .characters
                            .get(&k.victim_character_id)
                            .map(|c| c.corporation_id);
                        let a_corp = snap
                            .characters
                            .get(&k.attacker_character_id)
                            .map(|c| c.corporation_id);
                        v_corp == Some(corp_id) || a_corp == Some(corp_id)
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

        // 13. Market tag (ESI-6)
        ("GET", "/markets/groups") => {
            let mut ids: Vec<i64> = ctx.sde.market_groups.keys().map(|&id| id as i64).collect();
            ids.sort_unstable();
            RouteExecutionResult::ok_json(json!(ids), 3600)
        }

        // 14. Planetary Interaction tag (ESI-6)
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

        // 15. Wars tag (ESI-3)
        ("GET", "/wars") => {
            let max_war_id = if let Some(raw_max) = query_params.get("max_war_id") {
                match raw_max.parse::<i64>() {
                    Ok(v) if v > 0 => Some(v),
                    _ => {
                        return RouteExecutionResult::error(
                            400,
                            format!("Invalid max_war_id '{raw_max}' (expected positive integer)"),
                        );
                    }
                }
            } else {
                None
            };
            let mut war_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.wars
                    .keys()
                    .copied()
                    .filter(|&wid| max_war_id.is_none_or(|m| wid < m))
                    .collect()
            });
            war_ids.sort_by(|a, b| b.cmp(a));
            war_ids.truncate(2000);
            RouteExecutionResult::ok_json(json!(war_ids), 3600)
        }
        ("GET", "/wars/{war_id}") => {
            let war_id = match parse_path_i64(path_params, "war_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                let Some(w) = snap.wars.get(&war_id) else {
                    return RouteExecutionResult::error(404, format!("War {war_id} not found"));
                };
                let mut aggr_ships_killed = 0i64;
                let mut aggr_isk_destroyed = 0.0f64;
                let mut def_ships_killed = 0i64;
                let mut def_isk_destroyed = 0.0f64;
                for km in snap.killmails.values() {
                    let v_corp = snap
                        .characters
                        .get(&km.victim_character_id)
                        .map(|c| c.corporation_id);
                    let a_corp = snap
                        .characters
                        .get(&km.attacker_character_id)
                        .map(|c| c.corporation_id);
                    if a_corp == Some(w.aggressor_corp_id) && v_corp == Some(w.defender_corp_id) {
                        aggr_ships_killed += 1;
                        aggr_isk_destroyed += km.total_value_isk;
                    } else if a_corp == Some(w.defender_corp_id)
                        && v_corp == Some(w.aggressor_corp_id)
                    {
                        def_ships_killed += 1;
                        def_isk_destroyed += km.total_value_isk;
                    }
                }
                let meta = snap.war_lifecycle_meta.get(&war_id);
                let declared_unix = meta.and_then(|m| m.declared_unix).unwrap_or(w.started_unix);
                let started_unix = meta.and_then(|m| m.started_unix).unwrap_or(w.started_unix);
                let open_for_allies = meta
                    .and_then(|m| m.open_for_allies)
                    .unwrap_or(!w.mutual && w.active);
                let mut allies = ctx
                    .esi_state
                    .war_allies
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&war_id)
                    .cloned()
                    .unwrap_or_default();
                if let Some(m) = meta {
                    for ally in &m.allies {
                        let mut entry = json!({});
                        if let Some(cid) = ally.corporation_id {
                            entry["corporation_id"] = json!(cid);
                        }
                        if let Some(aid) = ally.alliance_id {
                            entry["alliance_id"] = json!(aid);
                        }
                        if !allies.contains(&entry) {
                            allies.push(entry);
                        }
                    }
                }
                let mut body = json!({
                    "id": w.war_id,
                    "declared": format_iso8601_utc(declared_unix),
                    "started": format_iso8601_utc(started_unix),
                    "mutual": w.mutual,
                    "open_for_allies": open_for_allies,
                    "allies": allies,
                    "aggressor": {
                        "corporation_id": w.aggressor_corp_id,
                        "ships_killed": aggr_ships_killed,
                        "isk_destroyed": aggr_isk_destroyed,
                    },
                    "defender": {
                        "corporation_id": w.defender_corp_id,
                        "ships_killed": def_ships_killed,
                        "isk_destroyed": def_isk_destroyed,
                    },
                });
                if let Some(retracted_unix) = meta.and_then(|m| m.retracted_unix) {
                    body["retracted"] = json!(format_iso8601_utc(retracted_unix));
                }
                if let Some(finished_unix) = meta.and_then(|m| m.finished_unix) {
                    body["finished"] = json!(format_iso8601_utc(finished_unix));
                }
                RouteExecutionResult::ok_json(body, 3600)
            })
        }
        ("GET", "/wars/{war_id}/killmails") => {
            let war_id = match parse_path_i64(path_params, "war_id") {
                Ok(v) => v,
                Err(e) => return e,
            };
            ctx.db.with_snapshot(|snap| {
                let Some(w) = snap.wars.get(&war_id) else {
                    return RouteExecutionResult::error(404, format!("War {war_id} not found"));
                };
                // War window: kills from `started` up to `finished` (open-ended while running).
                let meta = snap.war_lifecycle_meta.get(&war_id);
                let started = meta.and_then(|m| m.started_unix).unwrap_or(w.started_unix);
                let finished = meta.and_then(|m| m.finished_unix);
                // Allies fight on the defender's side: persisted lifecycle allies plus allies
                // accepted at runtime through `/wars/{war_id}` ally state.
                let mut allies: Vec<(Option<i64>, Option<i64>)> = meta
                    .map(|m| m.allies.iter().map(|a| (a.corporation_id, a.alliance_id)).collect())
                    .unwrap_or_default();
                if let Some(extra) = ctx
                    .esi_state
                    .war_allies
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&war_id)
                {
                    allies.extend(extra.iter().map(|a| {
                        (a["corporation_id"].as_i64(), a["alliance_id"].as_i64())
                    }));
                }
                // A war party id may name a corporation or an alliance.
                let is_party = |party: i64, corp: i64, ally: Option<i64>| {
                    party != 0 && (corp == party || ally == Some(party))
                };
                let aggressor_side =
                    |corp: i64, ally: Option<i64>| is_party(w.aggressor_corp_id, corp, ally);
                let defender_side = |corp: i64, ally: Option<i64>| {
                    is_party(w.defender_corp_id, corp, ally)
                        || allies.iter().any(|(ac, aa)| {
                            ac.is_some_and(|c| c != 0 && c == corp)
                                || aa.is_some_and(|a| a != 0 && Some(a) == ally)
                        })
                };
                // Affiliations are those stamped on the killmail at kill time, never the
                // pilots' current corporations.
                let mut kms: Vec<_> = snap
                    .killmails
                    .values()
                    .filter(|km| {
                        let in_window = km.killed_unix != 0
                            && km.killed_unix >= started
                            && finished.is_none_or(|f| km.killed_unix <= f);
                        let (vc, va) = (km.victim_corp_id, km.victim_alliance_id);
                        let (ac, aa) = (km.attacker_corp_id, km.attacker_alliance_id);
                        in_window
                            && ((aggressor_side(ac, aa) && defender_side(vc, va))
                                || (defender_side(ac, aa) && aggressor_side(vc, va)))
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
                        RouteExecutionResult::ok_paginated(json!(page_rows), 3600, total_pages)
                    }
                    Err(e) => e,
                }
            })
        }

        // Delegate Universe, Routes, Search, Sovereignty, and User Interface tags
        _ => super::universe_routes::dispatch_universe_and_nav(
            ctx,
            method,
            path_pattern,
            path_params,
            query_params,
            claims,
            page,
            req_body,
        ),
    }
}
