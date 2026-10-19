//! `industry_pi` — Domain 8: Manufacturing, ME/TE Research, Invention, Reprocessing, Compression, & PI (`§3.7`).

// parity: plan:E-4, plan:E-6
// parity: feat:industry.blueprint-data, feat:industry.facilities, feat:industry.facility-tax
// parity: feat:industry.install-job, feat:industry.job-cost
// parity: feat:pi.planet-info, feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins, feat:pi.links

#![forbid(unsafe_code)]

use crate::formulas::{
    compute_invention_probability, compute_manufacturing_material_qty,
    compute_manufacturing_time_seconds, compute_pi_link_cost, compute_poco_export_tax,
    compute_reprocessing_yield_rate,
};
use crate::router::ServerContext;
use eve_db::{
    IndustryJobRecord, InventoryFlag, InventoryItemRecord, IskCents, PlanetaryColonyRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

const ATTR_BLUEPRINT_ME: u16 = 1112;
const ATTR_BLUEPRINT_TE: u16 = 1113;
const ATTR_HARVESTER_TYPE: u16 = 709;
const ATTR_PLANET_RESTRICTION: u16 = 1632;
const ATTR_EXPORT_TAX_MULTIPLIER: u16 = 1641;
const ATTR_PIN_EXTRACTION_QUANTITY: u16 = 1642;
const ATTR_PIN_CYCLE_TIME: u16 = 1643;

/// Resolves the raw P0 planetary resource (`harvesterType` attribute `709`), base pin extraction
/// cycle duration (`pinCycleTime` attribute `1643` = `300`s), base pin extraction quantity
/// (`pinExtractionQuantity` attribute `1642` = `1000` units), and R0 export tax base
/// (`exportTaxMultiplier` attribute `1641` = `5.0` ISK per `Planetary_Industry.md:248`) for an
/// SDE planet celestial (`group_id == 7`) via SDE extractor pins (`group_id == 1026`, linked by
/// `planetRestriction` attribute `1632`). Note: pin count (`cc_level + 1`), batch yield scaling
/// (`pinExtractionQuantity * (pins_count - 1)`), `pinCycleTime` cooldown, and combined extraction/POCO
/// export in `Flush` are vertical-slice approximations until Phase 7 `E-5` pin-graph and customs-office
/// state land. Supports selecting any of the planet's 5 SDE raw P0 resources via `requested_resource_or_pin`.
fn resolve_sde_planet_p0_extraction(
    ctx: &ServerContext,
    solar_system_id: u32,
    planet_id: i64,
    requested_resource_or_pin: Option<TypeId>,
) -> Result<(TypeId, u64, u64, f64), BridgeRpcError> {
    let planet_celestial = ctx
        .sde
        .celestials_in_system(solar_system_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)
        .ok_or_else(|| BridgeRpcError {
            code: "PlanetNotInCurrentSystem".into(),
            message: format!(
                "Planet {planet_id} is not a planet celestial in solar system {solar_system_id}"
            ),
        })?;
    let planet_type = ctx
        .sde
        .types
        .get(&planet_celestial.type_id)
        .ok_or_else(|| BridgeRpcError {
            code: "UnknownPlanetType".into(),
            message: format!(
                "Planet type {} not found in SDE types",
                planet_celestial.type_id
            ),
        })?;
    let restriction_type_id = planet_type
        .variation_parent_type_id
        .unwrap_or(planet_type.type_id);
    let mut matching_extractors: Vec<&eve_sde::InvType> = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 1026
                && t.default_attributes
                    .get(&ATTR_PLANET_RESTRICTION)
                    .map(|v| *v as u32)
                    == Some(restriction_type_id)
        })
        .collect();
    if matching_extractors.is_empty() {
        return Err(BridgeRpcError {
            code: "UncolonizablePlanetType".into(),
            message: format!(
                "Planet {planet_id} ({}) has no SDE extractor pins with planetRestriction {}",
                planet_type.name, restriction_type_id
            ),
        });
    }
    matching_extractors.sort_by_key(|t| t.type_id);
    let extractor_type = match requested_resource_or_pin {
        None => matching_extractors[0],
        Some(req_id) => matching_extractors
            .into_iter()
            .find(|t| {
                t.type_id == req_id
                    || t.default_attributes
                        .get(&ATTR_HARVESTER_TYPE)
                        .map(|v| *v as u32)
                        == Some(req_id)
            })
            .ok_or_else(|| BridgeRpcError {
                code: "ClientSchematicOverrideForbidden".into(),
                message: format!(
                    "Requested type {req_id} is not a valid SDE extractor pin or harvesterType (709) raw P0 resource on planet {planet_id} ({})",
                    planet_type.name
                ),
            })?,
    };
    let p0_type_id = extractor_type
        .default_attributes
        .get(&ATTR_HARVESTER_TYPE)
        .map(|v| *v as u32)
        .filter(|&id| id > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "MissingPlanetRawResource".into(),
            message: format!(
                "Extractor pin {} is missing SDE harvesterType (709) attribute",
                extractor_type.type_id
            ),
        })?;
    let p0_qty_per_pin = extractor_type
        .default_attributes
        .get(&ATTR_PIN_EXTRACTION_QUANTITY)
        .map(|v| *v as u64)
        .filter(|&q| q > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "MissingPinExtractionQuantity".into(),
            message: format!(
                "Extractor pin {} is missing SDE pinExtractionQuantity (1642) attribute",
                extractor_type.type_id
            ),
        })?;
    let cycle_secs = extractor_type
        .default_attributes
        .get(&ATTR_PIN_CYCLE_TIME)
        .map(|v| *v as u64)
        .filter(|&c| c > 0)
        .ok_or_else(|| BridgeRpcError {
            code: "MissingPinCycleTime".into(),
            message: format!(
                "Extractor pin {} is missing SDE pinCycleTime (1643) attribute",
                extractor_type.type_id
            ),
        })?;
    let p0_type = ctx.sde.types.get(&p0_type_id).ok_or_else(|| BridgeRpcError {
        code: "MissingPlanetRawResource".into(),
        message: format!("Raw P0 resource {p0_type_id} not found in SDE types"),
    })?;
    let p0_unit_tax_value = p0_type
        .default_attributes
        .get(&ATTR_EXPORT_TAX_MULTIPLIER)
        .copied()
        .filter(|&v| v > 0.0)
        .ok_or_else(|| BridgeRpcError {
            code: "MissingExportTaxMultiplier".into(),
            message: format!(
                "Raw P0 resource {p0_type_id} is missing SDE exportTaxMultiplier (1641) attribute"
            ),
        })?;
    Ok((p0_type_id, cycle_secs, p0_qty_per_pin, p0_unit_tax_value))
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PiPinState {
    pin_id: i64,
    type_id: TypeId,
    latitude: f64,
    longitude: f64,
    schematic_id: Option<u32>,
    cpu_usage: f64,
    pg_usage: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PiLinkState {
    source_pin_id: i64,
    dest_pin_id: i64,
    level: u8,
    length_km: f64,
    cpu_usage: f64,
    pg_usage: f64,
    bandwidth: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PiRouteState {
    route_id: i64,
    path: Vec<i64>,
    commodity_type_id: TypeId,
    quantity: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct PiColonyNetworkState {
    pins: Vec<PiPinState>,
    links: Vec<PiLinkState>,
    routes: Vec<PiRouteState>,
    cpu_used: f64,
    cpu_max: f64,
    pg_used: f64,
    pg_max: f64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
struct IndustryJobCostMeta {
    eiv_isk: f64,
    system_cost_index: f64,
    structure_cost_modifier: f64,
    base_job_cost_isk: f64,
    facility_tax_rate: f64,
    facility_tax_isk: f64,
    scc_surcharge_isk: f64,
    total_cost_isk: f64,
}

/// Command Center level table from V24 `eve/common/script/util/planetCommon.py`
/// (`commandCenterInfoPerLevel`): `(cpu_output, power_output, cumulative_upgrade_cost_isk)`.
/// Per-step upgrade costs (`GetUpgradeCost(currentLevel, desiredLevel)`) are:
/// - 0 -> 1: 580,000 ISK (cumulative 580,000; CPU 7,057 TF, PG 9,000 MW)
/// - 1 -> 2: 930,000 ISK (cumulative 1,510,000; CPU 12,136 TF, PG 12,000 MW)
/// - 2 -> 3: 1,200,000 ISK (cumulative 2,710,000; CPU 17,215 TF, PG 15,000 MW)
/// - 3 -> 4: 1,500,000 ISK (cumulative 4,210,000; CPU 21,315 TF, PG 17,000 MW)
/// - 4 -> 5: 2,100,000 ISK (cumulative 6,310,000; CPU 25,415 TF, PG 19,000 MW)
fn cc_level_info(level: u8) -> (f64, f64, f64) {
    match level {
        0 => (1_675.0, 6_000.0, 0.0),
        1 => (7_057.0, 9_000.0, 580_000.0),
        2 => (12_136.0, 12_000.0, 1_510_000.0),
        3 => (17_215.0, 15_000.0, 2_710_000.0),
        4 => (21_315.0, 17_000.0, 4_210_000.0),
        _ => (25_415.0, 19_000.0, 6_310_000.0),
    }
}

fn cc_level_cpu_pg(level: u8) -> (f64, f64) {
    let (cpu, pg, _) = cc_level_info(level);
    (cpu, pg)
}

fn cc_upgrade_cost_isk(current_level: u8, desired_level: u8) -> f64 {
    if desired_level <= current_level {
        return 0.0;
    }
    let (_, _, cur_cum) = cc_level_info(current_level);
    let (_, _, des_cum) = cc_level_info(desired_level);
    (des_cum - cur_cum).max(0.0)
}

fn is_command_center_type(ctx: &ServerContext, type_id: TypeId) -> bool {
    if type_id == 2254 || (2524..=2551).contains(&type_id) {
        return true;
    }
    ctx.sde
        .types
        .get(&type_id)
        .map(|t| t.group_id == 1027)
        .unwrap_or(false)
}

fn extract_pi_pin_id(rep: Option<&PyRep>) -> i64 {
    match rep {
        Some(PyRep::Integer(id)) => *id,
        Some(PyRep::Tuple(els) | PyRep::List(els)) => els
            .last()
            .and_then(|v| v.as_i64())
            .or_else(|| els.first().and_then(|v| v.as_i64()))
            .unwrap_or(0),
        _ => 0,
    }
}

fn resolve_pi_pin_stats(ctx: &ServerContext, type_id: TypeId) -> (f64, f64, f64) {
    if is_command_center_type(ctx, type_id) {
        return (0.0, 0.0, 0.0);
    }
    if let Some(inv_type) = ctx.sde.types.get(&type_id) {
        let cpu = inv_type
            .default_attributes
            .get(&49)
            .copied()
            .or_else(|| inv_type.default_attributes.get(&50).copied())
            .filter(|&v| v > 0.0);
        let pg = inv_type
            .default_attributes
            .get(&15)
            .copied()
            .or_else(|| inv_type.default_attributes.get(&11).copied())
            .filter(|&v| v > 0.0);
        let base_isk = if inv_type.base_price > 0.0 {
            inv_type.base_price
        } else {
            45_000.0
        };
        let (fallback_cpu, fallback_pg) = match inv_type.group_id {
            1026 => (400.0, 2_600.0), // Extractor Control Unit
            1028 => (200.0, 800.0),   // Processor Facility
            1029 => (500.0, 700.0),   // Storage Facility
            1030 => (3_600.0, 700.0), // Spaceport / Launchpad
            _ => (200.0, 800.0),
        };
        (cpu.unwrap_or(fallback_cpu), pg.unwrap_or(fallback_pg), base_isk)
    } else {
        (200.0, 800.0, 45_000.0)
    }
}

fn pi_network_note_id(char_id: i64, planet_id: i64) -> i64 {
    let mixed = ((char_id.unsigned_abs() & 0xFFFF_FFFF) << 24) ^ (planet_id.unsigned_abs() & 0xFFFF_FFFF);
    -((mixed as i64).max(1) + 3_000_000_000_000)
}

fn load_pi_network(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    cc_level: u8,
) -> PiColonyNetworkState {
    let nid = pi_network_note_id(char_id, planet_id);
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(mut net) = serde_json::from_str::<PiColonyNetworkState>(&note.note) {
            let (c_max, p_max) = cc_level_cpu_pg(cc_level);
            if net.cpu_max < c_max {
                net.cpu_max = c_max;
            }
            if net.pg_max < p_max {
                net.pg_max = p_max;
            }
            return net;
        }
    }
    let (cpu_max, pg_max) = cc_level_cpu_pg(cc_level);
    PiColonyNetworkState {
        pins: Vec::new(),
        links: Vec::new(),
        routes: Vec::new(),
        cpu_used: 0.0,
        cpu_max,
        pg_used: 0.0,
        pg_max,
    }
}

fn save_pi_network(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
    net: &PiColonyNetworkState,
) {
    let nid = pi_network_note_id(char_id, planet_id);
    if let Ok(js) = serde_json::to_string(net) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -1,
                item_id: planet_id,
                label: format!("pi_net:{char_id}:{planet_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

fn facility_tax_note_id(facility_id: i64) -> i64 {
    -(1_000_000_000_000_i64.saturating_add(facility_id.abs()))
}

fn load_facility_taxes(snap: &eve_db::WorldStateSnapshot, facility_id: i64) -> HashMap<u8, f64> {
    let nid = facility_tax_note_id(facility_id);
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(map) = serde_json::from_str::<HashMap<u8, f64>>(&note.note) {
            return map;
        }
    }
    let is_upwell = snap.structures.contains_key(&facility_id)
        || snap
            .items
            .get(&facility_id)
            .map(|it| matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836))
            .unwrap_or(false);
    let default_rate = if is_upwell { 0.05 } else { 0.10 };
    let mut map = HashMap::new();
    for act in [1_u8, 3, 4, 5, 8, 9] {
        map.insert(act, default_rate);
    }
    map
}

fn has_custom_facility_taxes(snap: &eve_db::WorldStateSnapshot, facility_id: i64) -> bool {
    snap.owner_notes.contains_key(&facility_tax_note_id(facility_id))
        || snap.structures.contains_key(&facility_id)
        || snap
            .items
            .get(&facility_id)
            .map(|it| matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836))
            .unwrap_or(false)
}

fn save_facility_taxes(
    snap: &mut eve_db::WorldStateSnapshot,
    facility_id: i64,
    taxes: &HashMap<u8, f64>,
) {
    let nid = facility_tax_note_id(facility_id);
    if let Ok(js) = serde_json::to_string(taxes) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -2,
                item_id: facility_id,
                label: format!("facility_tax:{facility_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

fn job_cost_note_id(job_id: i64) -> i64 {
    -(2_000_000_000_000_i64.saturating_add(job_id.abs()))
}

fn save_job_cost_meta(
    snap: &mut eve_db::WorldStateSnapshot,
    job_id: i64,
    meta: &IndustryJobCostMeta,
) {
    let nid = job_cost_note_id(job_id);
    if let Ok(js) = serde_json::to_string(meta) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -3,
                item_id: job_id,
                label: format!("ind_job_cost:{job_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

fn load_job_cost_meta(snap: &eve_db::WorldStateSnapshot, job_id: i64) -> Option<IndustryJobCostMeta> {
    let nid = job_cost_note_id(job_id);
    snap.owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str::<IndustryJobCostMeta>(&n.note).ok())
}

fn resolve_facility_modifiers_and_system(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<(TypeId, i64, u32, u32, f64, f64, f64)> {
    if let Some(st) = snap.structures.get(&facility_id) {
        if st.state == "Destroyed" {
            return None;
        }
        let region_id = ctx
            .sde
            .solar_systems
            .get(&st.solar_system_id)
            .map(|sys| sys.region_id)
            .unwrap_or(10000002);
        let (mat_mod, time_mod, cost_mod) = match st.type_id {
            35825 => (0.99, 0.85, 0.97), // Raitaru
            35826 => (0.99, 0.80, 0.96), // Azbel
            35827 => (0.99, 0.70, 0.95), // Sotiyo
            35835 => (0.98, 0.80, 0.96), // Athanor
            35836 => (0.98, 0.75, 0.95), // Tatara
            _ => (1.0, 1.0, 1.0),
        };
        return Some((
            st.type_id,
            st.owner_corp_id,
            st.solar_system_id,
            region_id,
            mat_mod,
            time_mod,
            cost_mod,
        ));
    }
    if let Some(it) = snap.items.get(&facility_id) {
        if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836) {
            let sys_id = if (30_000_000..40_000_000).contains(&it.location_id) {
                it.location_id as u32
            } else {
                30_000_142
            };
            let region_id = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|sys| sys.region_id)
                .unwrap_or(10000002);
            let (mat_mod, time_mod, cost_mod) = match it.type_id {
                35825 => (0.99, 0.85, 0.97),
                35826 => (0.99, 0.80, 0.96),
                35827 => (0.99, 0.70, 0.95),
                35835 => (0.98, 0.80, 0.96),
                35836 => (0.98, 0.75, 0.95),
                _ => (1.0, 1.0, 1.0),
            };
            return Some((
                it.type_id,
                it.owner_id,
                sys_id,
                region_id,
                mat_mod,
                time_mod,
                cost_mod,
            ));
        }
    }
    for sys in ctx.sde.solar_systems.values() {
        if let Some(station) = sys.stations.iter().find(|s| s.station_id == facility_id) {
            return Some((
                station.station_type_id,
                station.owner_corporation_id,
                sys.solar_system_id,
                sys.region_id,
                1.0,
                1.0,
                1.0,
            ));
        }
    }
    None
}

fn compute_industry_job_costs(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    bp_type_id: TypeId,
    activity_id: u8,
    runs: u32,
    facility_id: i64,
) -> Result<IndustryJobCostMeta, BridgeRpcError> {
    let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
        return Err(BridgeRpcError {
            code: "UnknownBlueprint".into(),
            message: format!("Blueprint type {bp_type_id} not found in SDE"),
        });
    };
    let eiv_per_run: f64 = bp
        .materials
        .iter()
        .map(|m| {
            let unit_price = ctx
                .sde
                .types
                .get(&m.type_id)
                .map(|t| if t.base_price > 0.0 { t.base_price } else { 10.0 })
                .unwrap_or(10.0);
            (m.quantity as f64) * unit_price
        })
        .sum::<f64>()
        .max(10_000.0);
    let activity_eiv_factor = match activity_id {
        1 => 1.0,
        3 | 4 | 5 | 8 => 0.02,
        _ => 1.0,
    };
    let total_eiv = eiv_per_run * (runs as f64) * activity_eiv_factor;
    let (_, _, sys_id, _, _, _, structure_cost_modifier) =
        resolve_facility_modifiers_and_system(ctx, snap, facility_id).unwrap_or((
            1529, 1000035, 30000142, 10000002, 1.0, 1.0, 1.0,
        ));
    let ind_idx = snap
        .system_states
        .get(&sys_id)
        .map(|s| s.industrial_index)
        .unwrap_or(4);
    let system_cost_index = (0.01 + 0.01 * (ind_idx as f64)).clamp(0.001, 0.50);
    let base_job_cost_isk =
        ((total_eiv * system_cost_index * structure_cost_modifier) * 100.0).round() / 100.0;
    let taxes = load_facility_taxes(snap, facility_id);
    let facility_tax_rate = taxes
        .get(&activity_id)
        .or_else(|| taxes.get(&1))
        .copied()
        .unwrap_or(0.10);
    let facility_tax_isk = ((base_job_cost_isk * facility_tax_rate) * 100.0).round() / 100.0;
    let scc_rate = if matches!(activity_id, 3 | 4) {
        0.01
    } else {
        0.04
    };
    let scc_surcharge_isk = ((base_job_cost_isk * scc_rate) * 100.0).round() / 100.0;
    let total_cost_isk =
        ((base_job_cost_isk + facility_tax_isk + scc_surcharge_isk) * 100.0).round() / 100.0;
    Ok(IndustryJobCostMeta {
        eiv_isk: (eiv_per_run * 100.0).round() / 100.0,
        system_cost_index,
        structure_cost_modifier,
        base_job_cost_isk,
        facility_tax_rate,
        facility_tax_isk,
        scc_surcharge_isk,
        total_cost_isk,
    })
}

fn build_facility_pyrep(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    facility_id: i64,
) -> Option<PyRep> {
    let (type_id, owner_id, sys_id, region_id, mat_mod, time_mod, cost_mod) =
        resolve_facility_modifiers_and_system(ctx, snap, facility_id)?;
    let taxes = load_facility_taxes(snap, facility_id);
    let mfg_tax = taxes.get(&1).copied().unwrap_or(0.10);
    let ind_idx = snap
        .system_states
        .get(&sys_id)
        .map(|s| s.industrial_index)
        .unwrap_or(4);
    let sys_cost_index = (0.01 + 0.01 * (ind_idx as f64)).clamp(0.001, 0.50);
    let tax_kv = PyRep::keyval([
        ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
        ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
        ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
        ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
        ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
        ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
    ]);
    Some(PyRep::keyval([
        ("facilityID", PyRep::Integer(facility_id)),
        ("typeID", PyRep::Integer(type_id as i64)),
        ("ownerID", PyRep::Integer(owner_id)),
        ("solarSystemID", PyRep::Integer(sys_id as i64)),
        ("regionID", PyRep::Integer(region_id as i64)),
        ("tax", PyRep::Real(mfg_tax)),
        ("taxRates", tax_kv),
        ("online", PyRep::Bool(true)),
        ("materialMultiplier", PyRep::Real(mat_mod)),
        ("timeMultiplier", PyRep::Real(time_mod)),
        ("costMultiplier", PyRep::Real(cost_mod)),
        ("materialModifier", PyRep::Real(mat_mod)),
        ("timeModifier", PyRep::Real(time_mod)),
        ("costModifier", PyRep::Real(cost_mod)),
        ("systemCostIndex", PyRep::Real(sys_cost_index)),
        (
            "activities",
            PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::Integer(3),
                PyRep::Integer(4),
                PyRep::Integer(5),
                PyRep::Integer(8),
            ]),
        ),
    ]))
}

fn list_sde_planet_p0_resources(ctx: &ServerContext, solar_system_id: u32, planet_id: i64) -> Vec<TypeId> {
    let Some(planet_cel) = ctx
        .sde
        .celestials_in_system(solar_system_id)
        .into_iter()
        .find(|c| c.celestial_id == planet_id && c.group_id == 7)
    else {
        return Vec::new();
    };
    let Some(planet_type) = ctx.sde.types.get(&planet_cel.type_id) else {
        return Vec::new();
    };
    let restriction_type_id = planet_type
        .variation_parent_type_id
        .unwrap_or(planet_type.type_id);
    let mut resources: Vec<TypeId> = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 1026
                && t.default_attributes
                    .get(&ATTR_PLANET_RESTRICTION)
                    .map(|v| *v as u32)
                    == Some(restriction_type_id)
        })
        .filter_map(|t| {
            t.default_attributes
                .get(&ATTR_HARVESTER_TYPE)
                .map(|v| *v as u32)
                .filter(|&id| id > 0)
        })
        .collect();
    resources.sort_unstable();
    resources.dedup();
    resources
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // parity: plan:S-7
    match (service, method) {
        ("reprocessingSvc", "Reprocess") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Reprocessing requires being docked at a station or structure".into(),
                }));
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Reprocess requires an explicit integer itemID".into(),
                }));
            };

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} does not exist"),
                    });
                };
                if item.owner_id != char_id || item.location_id != station_id || item.quantity <= 0
                {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .or(session.ship_id);
                if active_ship_id == Some(item_id) {
                    return Err(BridgeRpcError {
                        code: "CannotReprocessActiveShip".into(),
                        message: format!(
                            "Item {item_id} is your currently active ship and cannot be reprocessed"
                        ),
                    });
                }
                let Some(inv_type) = ctx.sde.types.get(&item.type_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotReprocessable".into(),
                        message: format!("Unknown item type {}", item.type_id),
                    });
                };
                let Some(materials) = ctx
                    .sde
                    .type_materials
                    .get(&item.type_id)
                    .filter(|m| !m.is_empty())
                else {
                    return Err(BridgeRpcError {
                        code: "ItemNotReprocessable".into(),
                        message: format!(
                            "Item type {} ({}) has no SDE typeMaterials and cannot be reprocessed",
                            item.type_id, inv_type.name
                        ),
                    });
                };
                if item.singleton || snap.items.values().any(|child| child.location_id == item_id) {
                    return Err(BridgeRpcError {
                        code: "ItemMustBeRepackaged".into(),
                        message: format!(
                            "Item {item_id} must be emptied and repackaged before reprocessing"
                        ),
                    });
                }
                if item.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(item_id)
                {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked and cannot be reprocessed"),
                    });
                }

                let portion_size = inv_type.portion_size.max(1) as i32;
                if item.quantity < portion_size {
                    return Err(BridgeRpcError {
                        code: "InsufficientBatchPortion".into(),
                        message: format!(
                            "Need at least {portion_size} units to reprocess type {} (have {})",
                            item.type_id, item.quantity
                        ),
                    });
                }

                let char_skills = snap.skills.get(&char_id);
                let reproc_lvl = char_skills
                    .and_then(|m| m.get(&3385))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let reproc_eff_lvl = char_skills
                    .and_then(|m| m.get(&3389))
                    .map(|s| s.level)
                    .unwrap_or(0);
                let ore_proc_lvl = inv_type
                    .default_attributes
                    .get(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE)
                    .and_then(|&skill_id_f| {
                        let skill_id = skill_id_f as TypeId;
                        char_skills.and_then(|m| m.get(&skill_id)).map(|s| s.level)
                    })
                    .unwrap_or(0);

                // Station tax: 5% base reduced by 0.75% per point of positive NPC station owner standing (0% at >= 6.67 or player structure)
                let station_tax_rate = if station_id >= 1_000_000_000_000 {
                    0.0
                } else {
                    let owner_corp_id = ctx
                        .sde
                        .solar_systems
                        .values()
                        .flat_map(|s| s.stations.iter())
                        .find(|st| st.station_id == station_id)
                        .map(|st| st.owner_corporation_id as i64);
                    let standing = owner_corp_id
                        .and_then(|cid| {
                            snap.characters
                                .get(&char_id)
                                .and_then(|ch| ch.standings.get(&cid).copied())
                        })
                        .unwrap_or(0.0);
                    (0.05_f64 - 0.0075_f64 * standing.max(0.0)).clamp(0.0, 0.05)
                };

                let station_base_yield = if station_id >= 1_000_000_000_000 {
                    snap.structures
                        .get(&station_id)
                        .map(|st| {
                            if st.type_id == 35836 {
                                0.54
                            } else if st.type_id == 35835 {
                                0.52
                            } else {
                                0.50
                            }
                        })
                        .unwrap_or(0.50)
                } else {
                    ctx.sde
                        .solar_systems
                        .values()
                        .flat_map(|s| s.stations.iter())
                        .find(|st| st.station_id == station_id)
                        .map(|st| st.reprocessing_efficiency)
                        .unwrap_or(0.50)
                };

                let implant_bonus = snap
                    .active_implants
                    .get(&char_id)
                    .map(|implants| {
                        let mut b = 0.0;
                        for &imp_id in implants {
                            if let Some(imp_type) = ctx.sde.types.get(&imp_id) {
                                if let Some(&bonus) = imp_type.default_attributes.get(&379) {
                                    b += bonus / 100.0;
                                }
                            }
                        }
                        b
                    })
                    .unwrap_or(0.0);

                let is_ore_or_ice = inv_type.category_id == 25
                    || inv_type.group_id == 465
                    || inv_type
                        .default_attributes
                        .contains_key(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE);

                let yield_rate = if is_ore_or_ice {
                    compute_reprocessing_yield_rate(
                        station_base_yield,
                        reproc_lvl,
                        reproc_eff_lvl,
                        ore_proc_lvl,
                        implant_bonus,
                    )
                } else {
                    let scrap_lvl = char_skills
                        .and_then(|m| m.get(&12196))
                        .map(|s| s.level)
                        .unwrap_or(0);
                    (station_base_yield * (1.0 + 0.02 * (scrap_lvl.min(5) as f64))).clamp(0.0, 1.0)
                };
                let net_yield_rate = yield_rate * (1.0 - station_tax_rate);

                let portions = item.quantity / portion_size;
                let remainder = item.quantity % portion_size;
                if remainder > 0 {
                    if let Some(im) = snap.items.get_mut(&item_id) {
                        im.quantity = remainder;
                        im.version += 1;
                    }
                } else {
                    snap.items.remove(&item_id);
                }

                let mut outputs: Vec<(i64, TypeId, i32)> = Vec::with_capacity(materials.len());
                for mat in materials {
                    let out_qty =
                        ((portions as f64) * (mat.quantity as f64) * net_yield_rate).floor() as i32;
                    if out_qty <= 0 {
                        continue;
                    }
                    snap.next_entity_id += 1;
                    let mid = snap.next_entity_id;
                    snap.items.insert(
                        mid,
                        InventoryItemRecord {
                            item_id: mid,
                            type_id: mat.material_type_id,
                            owner_id: char_id,
                            location_id: station_id,
                            flag: InventoryFlag::Hangar,
                            quantity: out_qty,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    outputs.push((mid, mat.material_type_id, out_qty));
                }
                Ok((outputs, yield_rate, station_tax_rate))
            });

            match res {
                Ok((outputs, yield_rate, station_tax_rate)) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnReprocessingMaterialsChanged",
                        "character",
                        vec![
                            PyRep::Integer(char_id),
                            PyRep::Integer(station_id),
                        ],
                    );
                    let (trit_id, _, trit_qty) = outputs
                        .iter()
                        .copied()
                        .find(|&(_, tid, _)| tid == 34)
                        .or_else(|| outputs.first().copied())
                        .unwrap_or((0, 0, 0));
                    let output_entries: Vec<PyRep> = outputs
                        .into_iter()
                        .map(|(iid, tid, qty)| {
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(iid)),
                                ("typeID", PyRep::Integer(tid as i64)),
                                ("quantity", PyRep::Integer(qty as i64)),
                            ])
                        })
                        .collect();
                    Some(Ok(PyRep::keyval([
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(station_tax_rate)),
                        ("tritaniumItemID", PyRep::Integer(trit_id)),
                        ("tritaniumQuantity", PyRep::Integer(trit_qty as i64)),
                        ("outputs", PyRep::List(output_entries)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("reprocessingSvc", "GetQuotes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Reprocessing quote requires being docked at a station or structure".into(),
                }));
            };
            let item_ids: Vec<i64> = if let Some(first) = args.first() {
                if let PyRep::List(list) = first {
                    list.iter().filter_map(|a| a.as_i64()).collect()
                } else if let Some(iid) = first.as_i64() {
                    vec![iid]
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            if item_ids.is_empty() {
                let station_base_yield = if station_id >= 1_000_000_000_000 {
                    ctx.db.with_snapshot(|snap| {
                        snap.structures.get(&station_id).map(|st| {
                            if st.type_id == 35836 { 0.54 } else if st.type_id == 35835 { 0.52 } else { 0.50 }
                        }).unwrap_or(0.50)
                    })
                } else {
                    ctx.sde
                        .solar_systems
                        .values()
                        .flat_map(|s| s.stations.iter())
                        .find(|st| st.station_id == station_id)
                        .map(|st| st.reprocessing_efficiency)
                        .unwrap_or(0.50)
                };
                let (reproc_lvl, reproc_eff_lvl) = ctx.db.with_snapshot(|snap| {
                    let char_skills = snap.skills.get(&char_id);
                    let r = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
                    let re = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
                    (r, re)
                });
                return Some(Ok(PyRep::keyval([
                    ("stationBaseYield", PyRep::Real(station_base_yield)),
                    ("reprocessingSkillLevel", PyRep::Integer(reproc_lvl as i64)),
                    ("reprocessingEfficiencyLevel", PyRep::Integer(reproc_eff_lvl as i64)),
                ])));
            }
            let res = ctx.db.with_snapshot(|snap| {
                let mut quotes = Vec::new();
                for item_id in item_ids {
                    let Some(item) = snap.items.get(&item_id) else { continue; };
                    let Some(inv_type) = ctx.sde.types.get(&item.type_id) else { continue; };
                    let Some(materials) = ctx.sde.type_materials.get(&item.type_id).filter(|m| !m.is_empty()) else { continue; };
                    let char_skills = snap.skills.get(&char_id);
                    let reproc_lvl = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
                    let reproc_eff_lvl = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
                    let ore_proc_lvl = inv_type.default_attributes.get(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE)
                        .and_then(|&skill_id_f| char_skills.and_then(|m| m.get(&(skill_id_f as TypeId))).map(|s| s.level))
                        .unwrap_or(0);
                    let scrap_lvl = char_skills.and_then(|m| m.get(&12196)).map(|s| s.level).unwrap_or(0);
                    let station_base_yield = if station_id >= 1_000_000_000_000 {
                        snap.structures.get(&station_id).map(|st| {
                            if st.type_id == 35836 { 0.54 } else if st.type_id == 35835 { 0.52 } else { 0.50 }
                        }).unwrap_or(0.50)
                    } else {
                        ctx.sde.solar_systems.values().flat_map(|s| s.stations.iter()).find(|st| st.station_id == station_id).map(|st| st.reprocessing_efficiency).unwrap_or(0.50)
                    };
                    let implant_bonus = snap.active_implants.get(&char_id).map(|implants| {
                        let mut b = 0.0;
                        for &imp_id in implants {
                            if let Some(imp_type) = ctx.sde.types.get(&imp_id) {
                                if let Some(&bonus) = imp_type.default_attributes.get(&379) { b += bonus / 100.0; }
                            }
                        }
                        b
                    }).unwrap_or(0.0);
                    let is_ore_or_ice = inv_type.category_id == 25 || inv_type.group_id == 465 || inv_type.default_attributes.contains_key(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE);
                    let yield_rate = if is_ore_or_ice {
                        compute_reprocessing_yield_rate(station_base_yield, reproc_lvl, reproc_eff_lvl, ore_proc_lvl, implant_bonus)
                    } else {
                        (station_base_yield * (1.0 + 0.02 * (scrap_lvl.min(5) as f64))).clamp(0.0, 1.0)
                    };
                    let station_tax_rate = if station_id >= 1_000_000_000_000 { 0.0 } else {
                        let owner_corp_id = ctx.sde.solar_systems.values().flat_map(|s| s.stations.iter()).find(|st| st.station_id == station_id).map(|st| st.owner_corporation_id as i64);
                        let standing = owner_corp_id.and_then(|cid| snap.characters.get(&char_id).and_then(|ch| ch.standings.get(&cid).copied())).unwrap_or(0.0);
                        (0.05_f64 - 0.0075_f64 * standing.max(0.0)).clamp(0.0, 0.05)
                    };
                    let net_yield_rate = yield_rate * (1.0 - station_tax_rate);
                    let portion_size = inv_type.portion_size.max(1) as i32;
                    let portions = item.quantity / portion_size;
                    let mut est_outputs = Vec::new();
                    for mat in materials {
                        let out_qty = ((portions as f64) * (mat.quantity as f64) * net_yield_rate).floor() as i32;
                        if out_qty > 0 {
                            est_outputs.push(PyRep::keyval([
                                ("typeID", PyRep::Integer(mat.material_type_id as i64)),
                                ("quantity", PyRep::Integer(out_qty as i64)),
                            ]));
                        }
                    }
                    quotes.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(station_tax_rate)),
                        ("netYieldRate", PyRep::Real(net_yield_rate)),
                        ("portionSize", PyRep::Integer(portion_size as i64)),
                        ("outputs", PyRep::List(est_outputs)),
                    ]));
                }
                quotes
            });
            Some(Ok(PyRep::List(res)))
        }

        ("structureCompressionMgr", "CompressItemInStructure")
        | ("inSpaceCompressionMgr", "CompressItemInSpace") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if method == "CompressItemInStructure"
                && session.station_id.is_none()
                && session.structure_id.is_none()
            {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "CompressItemInStructure requires being docked".into(),
                }));
            }
            if method == "CompressItemInSpace"
                && (session.station_id.is_some() || session.structure_id.is_some())
            {
                return Some(Err(BridgeRpcError {
                    code: "MustBeInSpace".into(),
                    message: "CompressItemInSpace requires being undocked in space".into(),
                }));
            }
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "Compression requires an explicit integer itemID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut remainder_record = None;
                let mut compressed_out = None;
                if let Some(item) = snap.items.get_mut(&item_id) {
                    let reachable = (Some(item.location_id) == session.station_id)
                        || (Some(item.location_id) == session.structure_id)
                        || (Some(item.location_id) == session.ship_id);
                    let portion_size = ctx
                        .sde
                        .types
                        .get(&item.type_id)
                        .map(|t| t.portion_size as i32)
                        .unwrap_or(100)
                        .max(1);
                    if item.owner_id == char_id
                        && reachable
                        && item.flag != InventoryFlag::Locked
                        && item.quantity >= portion_size
                    {
                        if let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) {
                            let orig_type = item.type_id;
                            let orig_loc = item.location_id;
                            let orig_flag = item.flag;
                            let rem = item.quantity % portion_size;
                            item.type_id = comp_type;
                            item.quantity /= portion_size;
                            item.version += 1;
                            compressed_out = Some((item.item_id, item.type_id, item.quantity, rem));
                            if rem > 0 {
                                remainder_record = Some((orig_type, orig_loc, orig_flag, rem));
                            }
                        }
                    }
                }
                if let Some((orig_type, orig_loc, orig_flag, rem)) = remainder_record {
                    snap.next_entity_id += 1;
                    let rem_id = snap.next_entity_id;
                    snap.items.insert(
                        rem_id,
                        InventoryItemRecord {
                            item_id: rem_id,
                            type_id: orig_type,
                            owner_id: char_id,
                            location_id: orig_loc,
                            flag: orig_flag,
                            quantity: rem,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                }
                compressed_out.ok_or_else(|| BridgeRpcError {
                    code: "CompressionFailed".into(),
                    message: "Item cannot be compressed (invalid type, <100 units, locked, or unreachable location)".into(),
                })
            });
            match res {
                Ok((iid, ctype, qty, rem)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                    ("remainderUnits", PyRep::Integer(rem as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("industryManager", "InstallJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Installing an industry job requires being docked".into(),
                }));
            };

            // Support V24 `industryManager.InstallJob(jobData)` KeyVal/Dict signature (`industrySvc.pyj`),
            // V24 4+ arg `(blueprintItemID, blueprintTypeID, activityID, runs[, facilityID])`,
            // and legacy 3-arg `(blueprintTypeID, activityID, runs)`.
            let mut is_v24_call = false;
            let mut explicit_bp_item_id: Option<i64> = None;
            let mut explicit_facility_id: Option<i64> = None;
            let (raw_bp_type_id, raw_activity_id, runs_raw) = match args.first() {
                Some(PyRep::KeyVal(kv)) => {
                    is_v24_call = true;
                    explicit_bp_item_id = kv
                        .get("blueprintID")
                        .or_else(|| kv.get("blueprintItemID"))
                        .and_then(|v| v.as_i64())
                        .filter(|&id| id > 0);
                    explicit_facility_id = kv
                        .get("facilityID")
                        .and_then(|v| v.as_i64())
                        .filter(|&id| id > 0);
                    let btid = kv
                        .get("blueprintTypeID")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let act = kv.get("activityID").and_then(|v| v.as_i64()).unwrap_or(1);
                    let r = kv.get("runs").and_then(|v| v.as_i64()).unwrap_or(1);
                    (Some(btid), act, r)
                }
                Some(PyRep::Dict(entries)) => {
                    is_v24_call = true;
                    let find_k = |k: &str| {
                        entries
                            .iter()
                            .find(|(ek, _)| ek.as_str() == Some(k))
                            .map(|(_, ev)| ev)
                    };
                    explicit_bp_item_id = find_k("blueprintID")
                        .or_else(|| find_k("blueprintItemID"))
                        .and_then(|v| v.as_i64())
                        .filter(|&id| id > 0);
                    explicit_facility_id = find_k("facilityID")
                        .and_then(|v| v.as_i64())
                        .filter(|&id| id > 0);
                    let btid = find_k("blueprintTypeID")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    let act = find_k("activityID").and_then(|v| v.as_i64()).unwrap_or(1);
                    let r = find_k("runs").and_then(|v| v.as_i64()).unwrap_or(1);
                    (Some(btid), act, r)
                }
                _ if args.len() >= 4 => {
                    is_v24_call = true;
                    explicit_bp_item_id = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0);
                    explicit_facility_id = args.get(4).and_then(|a| a.as_i64()).filter(|&id| id > 0);
                    (
                        args.get(1).and_then(|a| a.as_i64()),
                        args.get(2).and_then(|a| a.as_i64()).unwrap_or(1),
                        args.get(3).and_then(|a| a.as_i64()).unwrap_or(1),
                    )
                }
                _ => (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(1),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(1),
                ),
            };
            let station_id = explicit_facility_id.unwrap_or(docked_id);
            if station_id != docked_id {
                let current_sys = session.solar_system_id.unwrap_or(30_000_142);
                let same_sys = ctx.db.with_snapshot(|snap| {
                    resolve_facility_modifiers_and_system(ctx, snap, station_id)
                        .map(|(_, _, f_sys, _, _, _, _)| f_sys == current_sys)
                        .unwrap_or(false)
                });
                if !same_sys {
                    return Some(Err(BridgeRpcError {
                        code: "FacilityMismatch".into(),
                        message: format!(
                            "Cannot install job at facility {station_id} while docked at {docked_id}"
                        ),
                    }));
                }
            }

            let resolved_bp_type_id = match raw_bp_type_id.filter(|&id| id > 0) {
                Some(id) => Some(id),
                None => explicit_bp_item_id.and_then(|iid| {
                    ctx.db
                        .with_snapshot(|snap| snap.items.get(&iid).map(|it| it.type_id as i64))
                }),
            };
            let Some(raw_bp_tid) = resolved_bp_type_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "InstallJob requires an explicit blueprint typeID or blueprintItemID"
                        .into(),
                }));
            };
            let (Ok(bp_type_id), Ok(activity_id)) = (
                TypeId::try_from(raw_bp_tid),
                u8::try_from(raw_activity_id),
            ) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Invalid blueprint typeID or activityID".into(),
                }));
            };

            if !(1..=1_000).contains(&runs_raw) || !matches!(activity_id, 1 | 3 | 4 | 5 | 8) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Runs must be in 1..=1000 and activityID in {1, 3, 4, 5, 8}".into(),
                }));
            }
            let (Ok(runs), Ok(runs_i32)) = (u32::try_from(runs_raw), i32::try_from(runs_raw)) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Runs out of 32-bit range".into(),
                }));
            };

            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                }));
            };
            let product_type_id: TypeId = if activity_id == 8 {
                11372 // Wolf Blueprint (T2 Assault Frigate BPC invented from Rifter BPC)
            } else {
                bp.product_type_id
            };
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Read character's actual trained skill levels from snap.skills
                let char_skills = snap.skills.get(&char_id);
                let skill_lvl = |sid: TypeId| -> u8 {
                    char_skills
                        .and_then(|m| m.get(&sid))
                        .map(|s| s.level)
                        .unwrap_or(0)
                };
                let industry_lvl = skill_lvl(3380); // Industry
                let adv_industry_lvl = skill_lvl(3388); // Advanced Industry
                let mass_prod_lvl = skill_lvl(3387); // Mass Production
                let adv_mass_prod_lvl = skill_lvl(24625); // Advanced Mass Production
                let lab_op_lvl = skill_lvl(3406); // Laboratory Operation
                let adv_lab_op_lvl = skill_lvl(24624); // Advanced Laboratory Operation
                let sci1_lvl = skill_lvl(11448); // Minmatar Starship Engineering
                let sci2_lvl = skill_lvl(11452); // Mechanical Engineering
                let enc_lvl = skill_lvl(21791); // Minmatar Encryption Methods

                // Enforce active job slot limits (`feat:industry.install-job`):
                // - Manufacturing (activity 1): `1 + Mass Production (3387) + Advanced Mass Production (24625)`
                // - Research/Science (activities 3, 4, 5, 8): `1 + Laboratory Operation (3406) + Advanced Laboratory Operation (24624)`
                let is_mfg = activity_id == 1;
                let max_slots = if is_mfg {
                    1 + (mass_prod_lvl as usize) + (adv_mass_prod_lvl as usize)
                } else {
                    1 + (lab_op_lvl as usize) + (adv_lab_op_lvl as usize)
                };
                let active_slots_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| {
                        j.installer_id == char_id
                            && j.status == "Active"
                            && ((is_mfg && j.activity_id == 1)
                                || (!is_mfg && matches!(j.activity_id, 3 | 4 | 5 | 8)))
                    })
                    .count();
                if active_slots_used >= max_slots {
                    return Err(BridgeRpcError {
                        code: "IndustrySlotsFull".into(),
                        message: format!(
                            "Character {char_id} has used all {active_slots_used}/{max_slots} {} slots",
                            if is_mfg { "manufacturing" } else { "science/research" }
                        ),
                    });
                }

                // Match an unlocked blueprint item at `station_id` (or docked station in same system) respecting BPO vs BPC rules:
                // - Activity 3 (TE), 4 (ME), 5 (Copying): require a BPO (`!i.is_blueprint_copy`)
                // - Activity 8 (Invention): require a BPC (`i.is_blueprint_copy && runs_remaining >= 1`)
                // - Activity 1 (Manufacturing): allow either a BPO or a BPC with `runs_remaining >= runs`
                let matching_bps: Vec<InventoryItemRecord> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && (i.location_id == station_id || i.location_id == docked_id)
                            && i.type_id == bp_type_id
                            && explicit_bp_item_id.map(|eid| i.item_id == eid).unwrap_or(true)
                            && i.quantity >= 1
                            && i.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .cloned()
                    .collect();
                if matching_bps.is_empty() {
                    return Err(BridgeRpcError {
                        code: "BlueprintNotAvailable".into(),
                        message: format!(
                            "Character {char_id} does not have an unlocked blueprint {bp_type_id} at facility {station_id}"
                        ),
                    });
                }

                let bp_rec = match activity_id {
                    3 | 4 | 5 => {
                        let Some(bpo) = matching_bps.into_iter().find(|i| !i.is_blueprint_copy)
                        else {
                            return Err(BridgeRpcError {
                                code: "CannotResearchOrCopyBPC".into(),
                                message: "ME/TE research and copying require a Blueprint Original (BPO), not a Blueprint Copy (BPC)".into(),
                            });
                        };
                        bpo
                    }
                    8 => {
                        let Some(bpc) = matching_bps.into_iter().find(|i| {
                            i.is_blueprint_copy && i.blueprint_runs_remaining.unwrap_or(0) >= 1
                        }) else {
                            return Err(BridgeRpcError {
                                code: "InventionRequiresBPC".into(),
                                message: "T2 Invention requires a Blueprint Copy (BPC) with at least 1 remaining run; BPOs cannot be used for invention".into(),
                            });
                        };
                        bpc
                    }
                    _ => {
                        let Some(bp_any) = matching_bps.into_iter().find(|i| {
                            !i.is_blueprint_copy
                                || i.blueprint_runs_remaining.unwrap_or(0) >= runs_i32
                        }) else {
                            return Err(BridgeRpcError {
                                code: "InsufficientBlueprintRuns".into(),
                                message: format!(
                                    "No BPO or BPC with at least {runs} remaining runs available at facility {station_id}"
                                ),
                            });
                        };
                        bp_any
                    }
                };
                let bp_iid = bp_rec.item_id;

                // Read ME/TE strictly from the blueprint item's server-side dynamic_attributes (default 0 for unresearched BPO/BPC)
                let me_level = bp_rec
                    .dynamic_attributes
                    .get(&ATTR_BLUEPRINT_ME)
                    .map(|v| (*v as i64).clamp(0, 10) as u8)
                    .unwrap_or(0);
                let te_level = bp_rec
                    .dynamic_attributes
                    .get(&ATTR_BLUEPRINT_TE)
                    .map(|v| (*v as i64).clamp(0, 20) as u8)
                    .unwrap_or(0);

                let (_, _, _, _, mat_mod, time_mod, _) =
                    resolve_facility_modifiers_and_system(ctx, snap, station_id).unwrap_or((
                        1529, 1000035, 30000142, 10000002, 1.0, 1.0, 1.0,
                    ));

                let duration_sec = match activity_id {
                    1 => compute_manufacturing_time_seconds(
                        runs,
                        bp.manufacturing_time_sec,
                        te_level,
                        industry_lvl,
                        adv_industry_lvl,
                        time_mod,
                    ),
                    3 => (((bp.research_te_time_sec.max(1) as f64) * time_mod).round().max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    4 => (((bp.research_me_time_sec.max(1) as f64) * time_mod).round().max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    5 => (((bp.copy_time_sec.max(1) as f64) * time_mod).round().max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    8 => (((bp.invention_time_sec.max(1) as f64) * time_mod).round().max(1.0) as u64)
                        .saturating_mul(runs as u64),
                    _ => 600,
                };
                let trit_req =
                    compute_manufacturing_material_qty(runs, 32_000, me_level, mat_mod, 0.0);
                let inv_prob = compute_invention_probability(
                    bp.invention_base_probability,
                    sci1_lvl,
                    sci2_lvl,
                    enc_lvl,
                    1.0,
                );

                // For manufacturing (activity_id == 1), verify and consume BOM materials at `station_id` (or docked station)
                if activity_id == 1 {
                    if industry_lvl < 1 {
                        return Err(BridgeRpcError {
                            code: "MissingIndustrySkills".into(),
                            message: "Manufacturing requires Industry (3380) >= level 1".into(),
                        });
                    }
                    for mat in &bp.materials {
                        let req_qty = compute_manufacturing_material_qty(
                            runs,
                            mat.quantity,
                            me_level,
                            mat_mod,
                            0.0,
                        ) as i32;
                        let mat_item_id = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == char_id
                                    && (i.location_id == station_id || i.location_id == docked_id)
                                    && i.flag == InventoryFlag::Hangar
                                    && i.type_id == mat.type_id
                                    && i.quantity >= req_qty
                                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
                            })
                            .map(|i| i.item_id);
                        let Some(mid) = mat_item_id else {
                            return Err(BridgeRpcError {
                                code: "InsufficientManufacturingMaterials".into(),
                                message: format!(
                                    "Missing {req_qty}x material type {} in facility {station_id} hangar",
                                    mat.type_id
                                ),
                            });
                        };
                        if let Some(m_item) = snap.items.get_mut(&mid) {
                            if m_item.quantity > req_qty {
                                m_item.quantity -= req_qty;
                                m_item.version += 1;
                            } else {
                                snap.items.remove(&mid);
                            }
                        }
                    }
                } else if activity_id == 8 {
                    if bp.invention_base_probability <= 0.0 {
                        return Err(BridgeRpcError {
                            code: "BlueprintNotInventable".into(),
                            message: format!("Blueprint {bp_type_id} does not support T2 invention"),
                        });
                    }
                    if sci1_lvl < 1 || sci2_lvl < 1 || enc_lvl < 1 {
                        return Err(BridgeRpcError {
                            code: "MissingInventionSkills".into(),
                            message: "Invention requires science skills (11448, 11452) and encryption skill (21791) >= level 1".into(),
                        });
                    }
                    let req_dc = runs as i32;
                    for dc_type in [20424_u32, 20172_u32] {
                        let dc_item_id = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == char_id
                                    && (i.location_id == station_id || i.location_id == docked_id)
                                    && i.flag == InventoryFlag::Hangar
                                    && i.type_id == dc_type
                                    && i.quantity >= req_dc
                                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
                            })
                            .map(|i| i.item_id);
                        let Some(dc_id) = dc_item_id else {
                            return Err(BridgeRpcError {
                                code: "InsufficientInventionDatacores".into(),
                                message: format!(
                                    "Missing {req_dc}x Datacore (type {dc_type}) in facility {station_id} hangar"
                                ),
                            });
                        };
                        if let Some(dc_item) = snap.items.get_mut(&dc_id) {
                            if dc_item.quantity > req_dc {
                                dc_item.quantity -= req_dc;
                                dc_item.version += 1;
                            } else {
                                snap.items.remove(&dc_id);
                            }
                        }
                    }
                }

                let cost_meta =
                    compute_industry_job_costs(ctx, snap, bp_type_id, activity_id, runs, station_id)?;
                let use_full_eiv_debit = is_v24_call || has_custom_facility_taxes(snap, station_id);
                let (charged_facility_tax_isk, total_debit_isk) = if use_full_eiv_debit {
                    (cost_meta.facility_tax_isk, cost_meta.total_cost_isk)
                } else {
                    let legacy_tax = 1_000.0 * (runs as f64);
                    (legacy_tax, legacy_tax)
                };

                let total_debit_cents = IskCents::from_f64_isk(total_debit_isk);
                if let Some(ch) = snap.characters.get(&char_id) {
                    if ch.balance < total_debit_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!(
                                "Insufficient funds for industry job installation ({:.2} ISK required, {:.2} available)",
                                total_debit_isk,
                                ch.balance.to_wire_f64()
                            ),
                        });
                    }
                }

                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;

                if use_full_eiv_debit {
                    let base_cents = IskCents::from_f64_isk(cost_meta.base_job_cost_isk);
                    let tax_cents = IskCents::from_f64_isk(cost_meta.facility_tax_isk);
                    let scc_cents = IskCents::from_f64_isk(cost_meta.scc_surcharge_isk);
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(base_cents).unwrap_or(IskCents(0));
                        let b1 = ch.balance;
                        snap.append_journal(
                            char_id,
                            "IndustryJobInstallationFee",
                            base_cents,
                            b1,
                            format!("Base job cost for industry job {jid}"),
                        );
                    }
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(tax_cents).unwrap_or(IskCents(0));
                        let b2 = ch.balance;
                        snap.append_journal(
                            char_id,
                            "IndustryFacilityTax",
                            tax_cents,
                            b2,
                            format!("Facility tax for industry job {jid}"),
                        );
                    }
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(scc_cents).unwrap_or(IskCents(0));
                        let b3 = ch.balance;
                        snap.append_journal(
                            char_id,
                            "IndustrySccSurcharge",
                            scc_cents,
                            b3,
                            format!("SCC surcharge for industry job {jid}"),
                        );
                    }
                } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch
                        .balance
                        .checked_sub(total_debit_cents)
                        .unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "IndustryFacilityTax",
                        total_debit_cents,
                        bal_after,
                        format!("Facility tax for industry job {jid}"),
                    );
                }

                // Lock blueprint while job is active
                if let Some(bp_mut) = snap.items.get_mut(&bp_iid) {
                    bp_mut.flag = InventoryFlag::Locked;
                    bp_mut.version += 1;
                }

                snap.industry_jobs.insert(
                    jid,
                    IndustryJobRecord {
                        job_id: jid,
                        installer_id: char_id,
                        facility_id: station_id,
                        blueprint_item_id: bp_iid,
                        blueprint_type_id: bp_type_id,
                        activity_id,
                        runs,
                        product_type_id,
                        status: "Active".into(),
                        duration_seconds: duration_sec,
                        start_unix: now_unix,
                    },
                );
                save_job_cost_meta(snap, jid, &cost_meta);
                Ok((
                    jid,
                    bp_iid,
                    duration_sec,
                    trit_req,
                    inv_prob,
                    charged_facility_tax_isk,
                    cost_meta,
                ))
            });

            match res {
                Ok((
                    job_id,
                    bp_iid,
                    duration_sec,
                    trit_req,
                    inv_prob,
                    charged_tax_isk,
                    cost_meta,
                )) => Some(Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("blueprintID", PyRep::Integer(bp_iid)),
                    ("blueprintItemID", PyRep::Integer(bp_iid)),
                    ("blueprintTypeID", PyRep::Integer(bp_type_id as i64)),
                    ("facilityID", PyRep::Integer(station_id)),
                    ("activityID", PyRep::Integer(activity_id as i64)),
                    ("runs", PyRep::Integer(runs as i64)),
                    ("productTypeID", PyRep::Integer(product_type_id as i64)),
                    ("durationSeconds", PyRep::Integer(duration_sec as i64)),
                    ("tritaniumRequired", PyRep::Integer(trit_req as i64)),
                    ("inventionProbability", PyRep::Real(inv_prob)),
                    ("estimatedItemValue", PyRep::Real(cost_meta.eiv_isk)),
                    ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                    ("systemCostIndex", PyRep::Real(cost_meta.system_cost_index)),
                    (
                        "structureCostModifier",
                        PyRep::Real(cost_meta.structure_cost_modifier),
                    ),
                    ("baseJobCost", PyRep::Real(cost_meta.base_job_cost_isk)),
                    ("facilityTaxRate", PyRep::Real(cost_meta.facility_tax_rate)),
                    ("facilityTaxISK", PyRep::Real(charged_tax_isk)),
                    ("sccSurchargeISK", PyRep::Real(cost_meta.scc_surcharge_isk)),
                    ("totalJobCost", PyRep::Real(cost_meta.total_cost_isk)),
                    ("totalJobCostISK", PyRep::Real(cost_meta.total_cost_isk)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("industryManager", "GetJobCounts") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let counts = ctx.db.with_snapshot(|snap| {
                let char_skills = snap.skills.get(&char_id);
                let skill_lvl = |sid: TypeId| -> u8 {
                    char_skills
                        .and_then(|m| m.get(&sid))
                        .map(|s| s.level)
                        .unwrap_or(0)
                };
                let mfg_max = 1 + (skill_lvl(3387) as i64) + (skill_lvl(24625) as i64);
                let sci_max = 1 + (skill_lvl(3406) as i64) + (skill_lvl(24624) as i64);
                let mfg_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| j.installer_id == char_id && j.status == "Active" && j.activity_id == 1)
                    .count() as i64;
                let sci_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| {
                        j.installer_id == char_id
                            && j.status == "Active"
                            && matches!(j.activity_id, 3 | 4 | 5 | 8)
                    })
                    .count() as i64;
                PyRep::keyval([
                    ("manufacturingUsed", PyRep::Integer(mfg_used)),
                    ("manufacturingMax", PyRep::Integer(mfg_max)),
                    ("researchUsed", PyRep::Integer(sci_used)),
                    ("researchMax", PyRep::Integer(sci_max)),
                ])
            });
            Some(Ok(counts))
        }

        ("industryManager", "GetJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(j) = snap.industry_jobs.get(&job_id) else {
                    return Err(BridgeRpcError {
                        code: "JobNotFound".into(),
                        message: format!("Industry job {job_id} not found"),
                    });
                };
                if j.installer_id != char_id {
                    return Err(BridgeRpcError {
                        code: "JobOwnershipViolation".into(),
                        message: format!("Industry job {job_id} does not belong to character {char_id}"),
                    });
                }
                let meta = load_job_cost_meta(snap, j.job_id).unwrap_or_default();
                Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(j.job_id)),
                    ("installerID", PyRep::Integer(j.installer_id)),
                    ("facilityID", PyRep::Integer(j.facility_id)),
                    ("blueprintItemID", PyRep::Integer(j.blueprint_item_id)),
                    ("blueprintTypeID", PyRep::Integer(j.blueprint_type_id as i64)),
                    ("activityID", PyRep::Integer(j.activity_id as i64)),
                    ("runs", PyRep::Integer(j.runs as i64)),
                    ("productTypeID", PyRep::Integer(j.product_type_id as i64)),
                    ("status", PyRep::String(j.status.clone())),
                    ("durationSeconds", PyRep::Integer(j.duration_seconds as i64)),
                    ("startUnix", PyRep::Integer(j.start_unix as i64)),
                    ("estimatedItemValueISK", PyRep::Real(meta.eiv_isk)),
                    ("baseJobCost", PyRep::Real(meta.base_job_cost_isk)),
                    ("facilityTaxISK", PyRep::Real(meta.facility_tax_isk)),
                    ("sccSurchargeISK", PyRep::Real(meta.scc_surcharge_isk)),
                    ("totalJobCostISK", PyRep::Real(meta.total_cost_isk)),
                ]))
            });
            Some(res)
        }

        ("industryManager", "CancelJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(job) = snap.industry_jobs.get(&job_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "JobNotFound".into(),
                        message: format!("Industry job {job_id} does not exist"),
                    });
                };
                if job.installer_id != char_id {
                    return Err(BridgeRpcError {
                        code: "JobOwnershipViolation".into(),
                        message: format!("Industry job {job_id} does not belong to character {char_id}"),
                    });
                }
                if job.status != "Active" {
                    return Err(BridgeRpcError {
                        code: "InvalidJobState".into(),
                        message: format!("Industry job {job_id} is not Active"),
                    });
                }
                if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                    j_mut.status = "Cancelled".into();
                }
                if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                    bp_mut.flag = InventoryFlag::Hangar;
                    bp_mut.version += 1;
                }
                Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("status", PyRep::String("Cancelled".into())),
                ]))
            });
            Some(res)
        }

        ("industryManager", "GetJobsByOwner") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let jobs = ctx.db.with_snapshot(|snap| {
                snap.industry_jobs
                    .values()
                    .filter(|j| j.installer_id == char_id)
                    .map(|j| {
                        let meta = load_job_cost_meta(snap, j.job_id).unwrap_or_default();
                        PyRep::keyval([
                            ("jobID", PyRep::Integer(j.job_id)),
                            ("installerID", PyRep::Integer(j.installer_id)),
                            ("facilityID", PyRep::Integer(j.facility_id)),
                            ("blueprintItemID", PyRep::Integer(j.blueprint_item_id)),
                            ("blueprintTypeID", PyRep::Integer(j.blueprint_type_id as i64)),
                            ("activityID", PyRep::Integer(j.activity_id as i64)),
                            ("runs", PyRep::Integer(j.runs as i64)),
                            ("productTypeID", PyRep::Integer(j.product_type_id as i64)),
                            ("status", PyRep::String(j.status.clone())),
                            ("durationSeconds", PyRep::Integer(j.duration_seconds as i64)),
                            ("startUnix", PyRep::Integer(j.start_unix as i64)),
                            ("estimatedItemValueISK", PyRep::Real(meta.eiv_isk)),
                            ("baseJobCost", PyRep::Real(meta.base_job_cost_isk)),
                            ("facilityTaxISK", PyRep::Real(meta.facility_tax_isk)),
                            ("sccSurchargeISK", PyRep::Real(meta.scc_surcharge_isk)),
                            ("totalJobCostISK", PyRep::Real(meta.total_cost_isk)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(jobs)))
        }

        ("industryManager", "CompleteJob") | ("industryManager", "CompleteManyJobs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = match args.first() {
                Some(PyRep::List(ids) | PyRep::Tuple(ids)) => {
                    ids.first().and_then(|v| v.as_i64()).unwrap_or(0)
                }
                Some(v) => v.as_i64().unwrap_or(0),
                None => 0,
            };
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(job) = snap.industry_jobs.get(&job_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "JobNotFound".into(),
                        message: format!("Industry job {job_id} does not exist"),
                    });
                };
                if job.installer_id != char_id {
                    return Err(BridgeRpcError {
                        code: "JobOwnershipViolation".into(),
                        message: format!(
                            "Industry job {job_id} does not belong to character {char_id}"
                        ),
                    });
                }
                if job.status != "Active" {
                    return Err(BridgeRpcError {
                        code: "InvalidJobState".into(),
                        message: format!(
                            "Industry job {job_id} is in state '{}', not Active",
                            job.status
                        ),
                    });
                }
                let ready_at = job.start_unix.saturating_add(job.duration_seconds);
                if now_unix < ready_at {
                    return Err(BridgeRpcError {
                        code: "JobNotYetComplete".into(),
                        message: format!(
                            "Industry job {job_id} completes at {ready_at} (current unix time {now_unix})"
                        ),
                    });
                }

                // Verify the blueprint item still exists, belongs to installer, and is Locked at job.facility_id (or docked station)
                let docked_loc = session.station_id.or(session.structure_id);
                let bp_valid = snap
                    .items
                    .get(&job.blueprint_item_id)
                    .map(|bp| {
                        bp.owner_id == char_id
                            && (bp.location_id == job.facility_id || Some(bp.location_id) == docked_loc)
                            && bp.flag == InventoryFlag::Locked
                            && (!bp.is_blueprint_copy
                                || !matches!(job.activity_id, 1 | 8)
                                || bp.blueprint_runs_remaining.unwrap_or(0) >= job.runs as i32)
                    })
                    .unwrap_or(false);
                if !bp_valid {
                    return Err(BridgeRpcError {
                        code: "BlueprintMissingOrUnlocked".into(),
                        message: format!(
                            "Blueprint {} for job {job_id} is missing, no longer locked at facility {}, or lacks sufficient runs",
                            job.blueprint_item_id, job.facility_id
                        ),
                    });
                }

                if job.activity_id == 8 {
                    let mut remove_bpc = true;
                    if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                        let rem = bp_mut
                            .blueprint_runs_remaining
                            .unwrap_or(1)
                            .saturating_sub(job.runs as i32);
                        if rem > 0 {
                            bp_mut.blueprint_runs_remaining = Some(rem);
                            bp_mut.flag = InventoryFlag::Hangar;
                            bp_mut.version += 1;
                            remove_bpc = false;
                        }
                    }
                    if remove_bpc {
                        snap.items.remove(&job.blueprint_item_id);
                    }
                    let char_skills = snap.skills.get(&char_id);
                    let skill_lvl = |sid: TypeId| -> u8 {
                        char_skills
                            .and_then(|m| m.get(&sid))
                            .map(|s| s.level)
                            .unwrap_or(0)
                    };
                    let sci1_lvl = skill_lvl(11448);
                    let sci2_lvl = skill_lvl(11452);
                    let enc_lvl = skill_lvl(21791);
                    let base_prob = ctx
                        .sde
                        .blueprints
                        .get(&job.blueprint_type_id)
                        .map(|b| b.invention_base_probability)
                        .unwrap_or(0.30);
                    let inv_prob = compute_invention_probability(
                        base_prob,
                        sci1_lvl,
                        sci2_lvl,
                        enc_lvl,
                        1.0,
                    );
                    let server_roll =
                        ctx.next_invention_roll(job.job_id as u64, char_id as u64, now_unix);
                    if server_roll >= inv_prob {
                        if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                            j_mut.status = "Failed".into();
                        }
                        return Ok(Err(BridgeRpcError {
                            code: "InventionAttemptFailed".into(),
                            message: format!(
                                "Invention roll {server_roll:.4} >= probability {inv_prob:.4}; input BPC run consumed"
                            ),
                        }));
                    }
                    if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                        j_mut.status = "Delivered".into();
                    }
                    snap.next_entity_id += 1;
                    let bpc_item_id = snap.next_entity_id;
                    let mut dyn_attrs = HashMap::new();
                    dyn_attrs.insert(ATTR_BLUEPRINT_ME, 2.0);
                    dyn_attrs.insert(ATTR_BLUEPRINT_TE, 4.0);
                    let t2_bpc_type_id: TypeId = 11372; // Wolf Blueprint
                    snap.items.insert(
                        bpc_item_id,
                        InventoryItemRecord {
                            item_id: bpc_item_id,
                            type_id: t2_bpc_type_id,
                            owner_id: char_id,
                            location_id: job.facility_id,
                            flag: InventoryFlag::Hangar,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: dyn_attrs,
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: true,
                            blueprint_runs_remaining: Some(1),
                        },
                    );
                    return Ok(Ok((bpc_item_id, t2_bpc_type_id)));
                }

                if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                    j_mut.status = "Delivered".into();
                }

                let mut parent_dyn_attrs = HashMap::new();
                let mut remove_consumed_bpc = false;
                let mut bp_location_id = job.facility_id;
                if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                    bp_location_id = bp_mut.location_id;
                    bp_mut.flag = InventoryFlag::Hangar;
                    if job.activity_id == 1 && bp_mut.is_blueprint_copy {
                        let rem = bp_mut
                            .blueprint_runs_remaining
                            .unwrap_or(1)
                            .saturating_sub(job.runs as i32);
                        if rem <= 0 {
                            remove_consumed_bpc = true;
                        } else {
                            bp_mut.blueprint_runs_remaining = Some(rem);
                        }
                    } else if job.activity_id == 3 {
                        let cur_te = bp_mut
                            .dynamic_attributes
                            .get(&ATTR_BLUEPRINT_TE)
                            .copied()
                            .unwrap_or(0.0);
                        bp_mut.dynamic_attributes.insert(
                            ATTR_BLUEPRINT_TE,
                            (cur_te + 2.0 * (job.runs as f64)).min(20.0),
                        );
                    } else if job.activity_id == 4 {
                        let cur_me = bp_mut
                            .dynamic_attributes
                            .get(&ATTR_BLUEPRINT_ME)
                            .copied()
                            .unwrap_or(0.0);
                        bp_mut.dynamic_attributes.insert(
                            ATTR_BLUEPRINT_ME,
                            (cur_me + 1.0 * (job.runs as f64)).min(10.0),
                        );
                    }
                    parent_dyn_attrs = bp_mut.dynamic_attributes.clone();
                    bp_mut.version += 1;
                }
                if remove_consumed_bpc {
                    snap.items.remove(&job.blueprint_item_id);
                }

                if job.activity_id == 1 {
                    snap.next_entity_id += 1;
                    let prod_item_id = snap.next_entity_id;
                    snap.items.insert(
                        prod_item_id,
                        InventoryItemRecord {
                            item_id: prod_item_id,
                            type_id: job.product_type_id,
                            owner_id: char_id,
                            location_id: bp_location_id,
                            flag: InventoryFlag::Hangar,
                            quantity: job.runs as i32,
                            singleton: job.runs == 1,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    Ok(Ok((prod_item_id, job.product_type_id)))
                } else if job.activity_id == 5 {
                    snap.next_entity_id += 1;
                    let copy_item_id = snap.next_entity_id;
                    snap.items.insert(
                        copy_item_id,
                        InventoryItemRecord {
                            item_id: copy_item_id,
                            type_id: job.blueprint_type_id,
                            owner_id: char_id,
                            location_id: bp_location_id,
                            flag: InventoryFlag::Hangar,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: parent_dyn_attrs,
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: true,
                            blueprint_runs_remaining: Some(job.runs as i32),
                        },
                    );
                    Ok(Ok((copy_item_id, job.blueprint_type_id)))
                } else {
                    Ok(Ok((job.blueprint_item_id, job.blueprint_type_id)))
                }
            });

            match res {
                Ok(Ok((prod_item_id, prod_type))) => Some(Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("deliveredItemID", PyRep::Integer(prod_item_id)),
                    ("productTypeID", PyRep::Integer(prod_type as i64)),
                ]))),
                Ok(Err(inv_err)) => Some(Err(inv_err)),
                Err(e) => Some(Err(e)),
            }
        }

        ("blueprintManager", "GetBlueprintData")
        | ("blueprintManager", "GetBlueprintDataByOwner") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if method == "GetBlueprintDataByOwner" {
                let owner_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
                let facility_filter = args.get(1).and_then(|a| a.as_i64()).filter(|&id| id > 0);
                let bps = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .filter(|it| {
                            it.owner_id == owner_id
                                && facility_filter.map(|fid| it.location_id == fid).unwrap_or(true)
                                && ctx.sde.blueprints.contains_key(&it.type_id)
                        })
                        .filter_map(|it| {
                            let bp = ctx.sde.blueprints.get(&it.type_id)?;
                            let me = it
                                .dynamic_attributes
                                .get(&ATTR_BLUEPRINT_ME)
                                .copied()
                                .unwrap_or(0.0) as i64;
                            let te = it
                                .dynamic_attributes
                                .get(&ATTR_BLUEPRINT_TE)
                                .copied()
                                .unwrap_or(0.0) as i64;
                            let cost_meta = compute_industry_job_costs(
                                ctx,
                                snap,
                                it.type_id,
                                1,
                                1,
                                it.location_id,
                            )
                            .unwrap_or_default();
                            let mats_list: Vec<PyRep> = bp
                                .materials
                                .iter()
                                .map(|m| {
                                    PyRep::keyval([
                                        ("typeID", PyRep::Integer(m.type_id as i64)),
                                        ("quantity", PyRep::Integer(m.quantity as i64)),
                                    ])
                                })
                                .collect();
                            let skills_list: Vec<PyRep> = bp
                                .activities
                                .get("manufacturing")
                                .or_else(|| bp.activities.get("1"))
                                .map(|act| {
                                    act.skills
                                        .iter()
                                        .map(|sk| {
                                            PyRep::keyval([
                                                ("typeID", PyRep::Integer(sk.type_id as i64)),
                                                ("level", PyRep::Integer(sk.level as i64)),
                                            ])
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .filter(|v| !v.is_empty())
                                .unwrap_or_else(|| {
                                    vec![PyRep::keyval([
                                        ("typeID", PyRep::Integer(3380)),
                                        ("level", PyRep::Integer(1)),
                                    ])]
                                });
                            Some(PyRep::keyval([
                                ("itemID", PyRep::Integer(it.item_id)),
                                ("blueprintID", PyRep::Integer(it.item_id)),
                                ("blueprintTypeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                                ("productTypeID", PyRep::Integer(bp.product_type_id as i64)),
                                ("facilityID", PyRep::Integer(it.location_id)),
                                ("materialEfficiency", PyRep::Integer(me)),
                                ("timeEfficiency", PyRep::Integer(te)),
                                ("original", PyRep::Bool(!it.is_blueprint_copy)),
                                (
                                    "runsRemaining",
                                    PyRep::Integer(it.blueprint_runs_remaining.unwrap_or(-1) as i64),
                                ),
                                ("materials", PyRep::List(mats_list)),
                                ("skills", PyRep::List(skills_list)),
                                ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                            ]))
                        })
                        .collect::<Vec<_>>()
                });
                return Some(Ok(PyRep::List(bps)));
            }
            let Some(raw_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBlueprintTypeID".into(),
                    message: "GetBlueprintData requires a blueprint typeID or itemID".into(),
                }));
            };
            let (bp_type_id, bp_item_opt) = ctx.db.with_snapshot(|snap| {
                if let Some(it) = snap.items.get(&raw_id) {
                    if ctx.sde.blueprints.contains_key(&it.type_id) {
                        return (it.type_id, Some(it.clone()));
                    }
                }
                (raw_id as TypeId, None)
            });
            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                }));
            };
            let facility_id = bp_item_opt
                .as_ref()
                .map(|i| i.location_id)
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let cost_meta = ctx.db.with_snapshot(|snap| {
                compute_industry_job_costs(ctx, snap, bp_type_id, 1, 1, facility_id)
                    .unwrap_or_default()
            });
            let me_lvl = bp_item_opt
                .as_ref()
                .and_then(|i| i.dynamic_attributes.get(&ATTR_BLUEPRINT_ME).copied())
                .unwrap_or(0.0) as i64;
            let te_lvl = bp_item_opt
                .as_ref()
                .and_then(|i| i.dynamic_attributes.get(&ATTR_BLUEPRINT_TE).copied())
                .unwrap_or(0.0) as i64;
            let mats_rep: Vec<PyRep> = bp
                .materials
                .iter()
                .map(|m| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(m.type_id as i64)),
                        ("quantity", PyRep::Integer(m.quantity as i64)),
                    ])
                })
                .collect();
            let skills_rep: Vec<PyRep> = bp
                .activities
                .get("manufacturing")
                .or_else(|| bp.activities.get("1"))
                .map(|act| {
                    act.skills
                        .iter()
                        .map(|sk| {
                            PyRep::keyval([
                                ("typeID", PyRep::Integer(sk.type_id as i64)),
                                ("level", PyRep::Integer(sk.level as i64)),
                            ])
                        })
                        .collect::<Vec<_>>()
                })
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| {
                    vec![PyRep::keyval([
                        ("typeID", PyRep::Integer(3380)),
                        ("level", PyRep::Integer(1)),
                    ])]
                });
            Some(Ok(PyRep::keyval([
                (
                    "blueprintID",
                    PyRep::Integer(bp_item_opt.as_ref().map(|i| i.item_id).unwrap_or(raw_id)),
                ),
                ("blueprintTypeID", PyRep::Integer(bp.blueprint_type_id as i64)),
                ("productTypeID", PyRep::Integer(bp.product_type_id as i64)),
                ("maxProductionLimit", PyRep::Integer(bp.max_production_limit as i64)),
                ("manufacturingTime", PyRep::Integer(bp.manufacturing_time_sec as i64)),
                ("researchMaterialTime", PyRep::Integer(bp.research_me_time_sec as i64)),
                ("researchTimeTime", PyRep::Integer(bp.research_te_time_sec as i64)),
                ("copyingTime", PyRep::Integer(bp.copy_time_sec as i64)),
                ("inventionTime", PyRep::Integer(bp.invention_time_sec as i64)),
                ("materialEfficiency", PyRep::Integer(me_lvl)),
                ("timeEfficiency", PyRep::Integer(te_lvl)),
                (
                    "original",
                    PyRep::Bool(!bp_item_opt.as_ref().map(|i| i.is_blueprint_copy).unwrap_or(false)),
                ),
                (
                    "runsRemaining",
                    PyRep::Integer(
                        bp_item_opt
                            .as_ref()
                            .and_then(|i| i.blueprint_runs_remaining)
                            .unwrap_or(-1) as i64,
                    ),
                ),
                ("materials", PyRep::List(mats_rep)),
                ("skills", PyRep::List(skills_rep)),
                ("estimatedItemValueISK", PyRep::Real(cost_meta.eiv_isk)),
                ("baseJobCost", PyRep::Real(cost_meta.base_job_cost_isk)),
                ("facilityTaxISK", PyRep::Real(cost_meta.facility_tax_isk)),
                ("sccSurchargeISK", PyRep::Real(cost_meta.scc_surcharge_isk)),
                ("totalJobCostISK", PyRep::Real(cost_meta.total_cost_isk)),
            ])))
        }

        ("blueprintManager", "GetLimits") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let limits = ctx.db.with_snapshot(|snap| {
                let char_skills = snap.skills.get(&char_id);
                let skill_lvl = |sid: TypeId| -> u8 {
                    char_skills
                        .and_then(|m| m.get(&sid))
                        .map(|s| s.level)
                        .unwrap_or(0)
                };
                let mfg_max = 1 + (skill_lvl(3387) as i64) + (skill_lvl(24625) as i64);
                let sci_max = 1 + (skill_lvl(3406) as i64) + (skill_lvl(24624) as i64);
                PyRep::keyval([
                    ("maxManufacturingJobs", PyRep::Integer(mfg_max)),
                    ("maxResearchJobs", PyRep::Integer(sci_max)),
                ])
            });
            Some(Ok(limits))
        }

        ("facilityManager", "GetFacilities")
        | ("facilityManager", "GetFacilitiesByID")
        | ("facilityManager", "GetFacilityLocations") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let explicit_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(ids) | PyRep::Tuple(ids)) => {
                    ids.iter().filter_map(|v| v.as_i64()).collect()
                }
                _ => Vec::new(),
            };
            let current_sys = session.solar_system_id.unwrap_or(30_000_142);
            let facilities = ctx.db.with_snapshot(|snap| {
                let mut list = Vec::new();
                if !explicit_ids.is_empty() {
                    for fid in explicit_ids {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, fid) {
                            list.push(f_rep);
                        }
                    }
                    return list;
                }
                let mut seen = std::collections::HashSet::new();
                for s in snap.structures.values() {
                    if s.state != "Destroyed" && seen.insert(s.structure_id) {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, s.structure_id) {
                            list.push(f_rep);
                        }
                    }
                }
                for it in snap.items.values() {
                    if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836)
                        && seen.insert(it.item_id)
                    {
                        if let Some(f_rep) = build_facility_pyrep(ctx, snap, it.item_id) {
                            list.push(f_rep);
                        }
                    }
                }
                if let Some(sys) = ctx.sde.solar_systems.get(&current_sys) {
                    for st in &sys.stations {
                        if seen.insert(st.station_id) {
                            if let Some(f_rep) = build_facility_pyrep(ctx, snap, st.station_id) {
                                list.push(f_rep);
                            }
                        }
                    }
                }
                if list.is_empty() {
                    if let Some(f_rep) = build_facility_pyrep(ctx, snap, 60_003_760) {
                        list.push(f_rep);
                    }
                }
                list
            });
            Some(Ok(PyRep::List(facilities)))
        }

        ("facilityManager", "GetFacility") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let facility_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let res = ctx.db.with_snapshot(|snap| {
                build_facility_pyrep(ctx, snap, facility_id).ok_or_else(|| BridgeRpcError {
                    code: "FacilityNotFound".into(),
                    message: format!("Industry facility {facility_id} not found"),
                })
            });
            Some(res)
        }

        ("facilityManager", "GetFacilityTaxes") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let facility_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
                .unwrap_or(60_003_760);
            let res = ctx.db.with_snapshot(|snap| {
                if resolve_facility_modifiers_and_system(ctx, snap, facility_id).is_none() {
                    return Err(BridgeRpcError {
                        code: "FacilityNotFound".into(),
                        message: format!("Industry facility {facility_id} not found"),
                    });
                }
                let taxes = load_facility_taxes(snap, facility_id);
                let mfg_tax = taxes.get(&1).copied().unwrap_or(0.10);
                Ok(PyRep::keyval([
                    ("facilityID", PyRep::Integer(facility_id)),
                    ("tax", PyRep::Real(mfg_tax)),
                    ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                    ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                    ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                    ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                    ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ("research_te", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                    ("research_me", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                    ("copying", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                    ("invention", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                ]))
            });
            Some(res)
        }

        ("facilityManager", "SetFacilityTaxes") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(facility_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityID".into(),
                    message: "SetFacilityTaxes requires an explicit integer facilityID".into(),
                }));
            };
            let tax_arg = if args.len() >= 3 {
                args.get(2)
            } else {
                args.get(1)
            };
            let mut updates: Vec<(u8, f64)> = Vec::new();
            let normalize_rate = |raw: f64| -> Result<f64, BridgeRpcError> {
                if !raw.is_finite() || raw < 0.0 {
                    return Err(BridgeRpcError {
                        code: "InvalidFacilityTaxRate".into(),
                        message: format!("Facility tax rate {raw} must be non-negative and finite"),
                    });
                }
                let rate = if raw > 1.0 && raw <= 100.0 {
                    raw / 100.0
                } else if raw > 100.0 {
                    raw / 1_000_000.0
                } else {
                    raw
                };
                if rate > 1.0 {
                    return Err(BridgeRpcError {
                        code: "InvalidFacilityTaxRate".into(),
                        message: format!("Facility tax rate {rate} exceeds 100%"),
                    });
                }
                Ok(rate)
            };
            let parse_act_key = |k: &str| -> Vec<u8> {
                match k {
                    "1" | "manufacturing" => vec![1],
                    "3" | "research_te" => vec![3],
                    "4" | "research_me" => vec![4],
                    "5" | "copying" => vec![5],
                    "8" | "invention" => vec![8],
                    "9" | "reaction" => vec![9],
                    "all" | "tax" => vec![1, 3, 4, 5, 8, 9],
                    _ => Vec::new(),
                }
            };
            match tax_arg {
                Some(PyRep::Real(r)) => {
                    let rate = match normalize_rate(*r) {
                        Ok(v) => v,
                        Err(e) => return Some(Err(e)),
                    };
                    for act in [1_u8, 3, 4, 5, 8, 9] {
                        updates.push((act, rate));
                    }
                }
                Some(PyRep::KeyVal(kv)) => {
                    for (k, v) in kv {
                        if let Some(raw) = v.as_f64() {
                            let rate = match normalize_rate(raw) {
                                Ok(r) => r,
                                Err(e) => return Some(Err(e)),
                            };
                            for act in parse_act_key(k) {
                                updates.push((act, rate));
                            }
                        }
                    }
                }
                Some(PyRep::Dict(entries)) => {
                    for (k, v) in entries {
                        let Some(raw) = v.as_f64() else { continue };
                        let rate = match normalize_rate(raw) {
                            Ok(r) => r,
                            Err(e) => return Some(Err(e)),
                        };
                        if let Some(act_id) = k.as_i64().and_then(|i| u8::try_from(i).ok()) {
                            updates.push((act_id, rate));
                        } else if let Some(ks) = k.as_str() {
                            for act in parse_act_key(ks) {
                                updates.push((act, rate));
                            }
                        }
                    }
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidFacilityTaxRate".into(),
                        message: "SetFacilityTaxes requires a taxRateValues dict/KeyVal or float"
                            .into(),
                    }));
                }
            }
            if updates.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFacilityTaxRate".into(),
                    message: "SetFacilityTaxes requires at least one valid activity tax rate"
                        .into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if resolve_facility_modifiers_and_system(ctx, snap, facility_id).is_none() {
                    return Err(BridgeRpcError {
                        code: "FacilityNotFound".into(),
                        message: format!("Industry facility {facility_id} not found"),
                    });
                }
                if let Some(st) = snap.structures.get(&facility_id) {
                    let char_corp = snap
                        .characters
                        .get(&char_id)
                        .map(|c| c.corporation_id)
                        .unwrap_or(0);
                    if st.owner_corp_id != char_corp && st.owner_corp_id != char_id {
                        return Err(BridgeRpcError {
                            code: "FacilityPermissionDenied".into(),
                            message: format!(
                                "Character {char_id} (corp {char_corp}) does not own structure {facility_id} (owner corp {})",
                                st.owner_corp_id
                            ),
                        });
                    }
                }
                if let Some(it) = snap.items.get(&facility_id) {
                    if matches!(it.type_id, 35825 | 35826 | 35827 | 35835 | 35836) {
                        let char_corp = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.corporation_id)
                            .unwrap_or(0);
                        if it.owner_id != char_corp && it.owner_id != char_id {
                            return Err(BridgeRpcError {
                                code: "FacilityPermissionDenied".into(),
                                message: format!(
                                    "Character {char_id} (corp {char_corp}) does not own structure {facility_id} (owner {})",
                                    it.owner_id
                                ),
                            });
                        }
                    }
                }
                let mut taxes = load_facility_taxes(snap, facility_id);
                for (act, rate) in updates {
                    taxes.insert(act, rate);
                }
                save_facility_taxes(snap, facility_id, &taxes);
                let mfg_tax = taxes.get(&1).copied().unwrap_or(0.10);
                Ok(PyRep::keyval([
                    ("facilityID", PyRep::Integer(facility_id)),
                    ("tax", PyRep::Real(mfg_tax)),
                    ("1", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                    ("3", PyRep::Real(taxes.get(&3).copied().unwrap_or(mfg_tax))),
                    ("4", PyRep::Real(taxes.get(&4).copied().unwrap_or(mfg_tax))),
                    ("5", PyRep::Real(taxes.get(&5).copied().unwrap_or(mfg_tax))),
                    ("8", PyRep::Real(taxes.get(&8).copied().unwrap_or(mfg_tax))),
                    ("manufacturing", PyRep::Real(taxes.get(&1).copied().unwrap_or(mfg_tax))),
                ]))
            });
            Some(res)
        }

        ("facilityManager", "GetMaxActivityModifiers") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::keyval([
                ("materialMultiplier", PyRep::Real(0.99)),
                ("timeMultiplier", PyRep::Real(0.70)),
                ("costMultiplier", PyRep::Real(0.95)),
            ])))
        }

        // Planetary Interaction (`planetMgr`, `E2E-13` / `plan:E-6`)
        ("planetMgr", "GetPlanetInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetPlanetInfo requires an explicit integer planetID".into(),
                }));
            };
            let planet_sys_id = ctx
                .sde
                .solar_systems
                .values()
                .find(|s| s.planet_ids.contains(&planet_id))
                .map(|s| s.solar_system_id);
            let Some(sys_id) = planet_sys_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: format!("Planet {planet_id} not found in SDE"),
                }));
            };
            let planet_type_id = ctx
                .sde
                .celestials_in_system(sys_id)
                .into_iter()
                .find(|c| c.celestial_id == planet_id && c.group_id == 7)
                .map(|c| c.type_id)
                .unwrap_or(11);
            let p0_resources = list_sde_planet_p0_resources(ctx, sys_id, planet_id);
            let res_types_rep: Vec<PyRep> = p0_resources
                .iter()
                .map(|&tid| PyRep::Integer(tid as i64))
                .collect();

            let (colony_opt, net) = ctx.db.with_snapshot(|snap| {
                let col = snap.planetary_colonies.get(&(char_id, planet_id)).cloned();
                let cc_lvl = col.as_ref().map(|c| c.command_center_level).unwrap_or(0);
                let net_state = load_pi_network(snap, char_id, planet_id, cc_lvl);
                (col, net_state)
            });
            let pins_rep: Vec<PyRep> = net
                .pins
                .iter()
                .map(|p| {
                    PyRep::keyval([
                        ("pinID", PyRep::Integer(p.pin_id)),
                        ("typeID", PyRep::Integer(p.type_id as i64)),
                        ("latitude", PyRep::Real(p.latitude)),
                        ("longitude", PyRep::Real(p.longitude)),
                        ("cpuUsage", PyRep::Real(p.cpu_usage)),
                        ("powergridUsage", PyRep::Real(p.pg_usage)),
                    ])
                })
                .collect();
            let links_rep: Vec<PyRep> = net
                .links
                .iter()
                .map(|l| {
                    PyRep::keyval([
                        ("endpoint1", PyRep::Integer(l.source_pin_id)),
                        ("endpoint2", PyRep::Integer(l.dest_pin_id)),
                        ("sourcePinID", PyRep::Integer(l.source_pin_id)),
                        ("destPinID", PyRep::Integer(l.dest_pin_id)),
                        ("level", PyRep::Integer(l.level as i64)),
                        ("lengthKm", PyRep::Real(l.length_km)),
                        ("cpuUsage", PyRep::Real(l.cpu_usage)),
                        ("powergridUsage", PyRep::Real(l.pg_usage)),
                        ("bandwidth", PyRep::Real(l.bandwidth)),
                    ])
                })
                .collect();
            let routes_rep: Vec<PyRep> = net
                .routes
                .iter()
                .map(|r| {
                    PyRep::keyval([
                        ("routeID", PyRep::Integer(r.route_id)),
                        ("commodityTypeID", PyRep::Integer(r.commodity_type_id as i64)),
                        ("quantity", PyRep::Integer(r.quantity as i64)),
                        (
                            "path",
                            PyRep::List(r.path.iter().map(|&id| PyRep::Integer(id)).collect()),
                        ),
                    ])
                })
                .collect();

            let mut res_entries = Vec::with_capacity(p0_resources.len());
            for (idx, &tid) in p0_resources.iter().enumerate() {
                let extracted_qty = colony_opt
                    .as_ref()
                    .and_then(|c| c.extracted_commodities.get(&tid).copied())
                    .unwrap_or(0);
                let depletion = ((extracted_qty as f64) / 100_000.0).min(0.40);
                let quality = (0.85 - (idx as f64) * 0.08 - depletion).clamp(0.25, 1.0);
                res_entries.push(PyRep::keyval([
                    ("resourceTypeID", PyRep::Integer(tid as i64)),
                    ("quality", PyRep::Real(quality)),
                ]));
            }

            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("planetTypeID", PyRep::Integer(planet_type_id as i64)),
                ("radius", PyRep::Real(5_000_000.0)),
                ("hasColony", PyRep::Bool(colony_opt.is_some())),
                (
                    "commandCenterLevel",
                    PyRep::Integer(
                        colony_opt
                            .as_ref()
                            .map(|c| c.command_center_level as i64)
                            .unwrap_or(0),
                    ),
                ),
                (
                    "pinsCount",
                    PyRep::Integer(colony_opt.as_ref().map(|c| c.pins_count as i64).unwrap_or(0)),
                ),
                ("cpuUsed", PyRep::Real(net.cpu_used)),
                ("cpuMax", PyRep::Real(net.cpu_max)),
                ("powergridUsed", PyRep::Real(net.pg_used)),
                ("powergridMax", PyRep::Real(net.pg_max)),
                ("pins", PyRep::List(pins_rep)),
                ("links", PyRep::List(links_rep)),
                ("routes", PyRep::List(routes_rep)),
                ("resourceTypes", PyRep::List(res_types_rep)),
                ("resources", PyRep::List(res_entries)),
            ])))
        }

        ("planetMgr", "GetPlanetResourceInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetPlanetResourceInfo requires an explicit integer planetID".into(),
                }));
            };
            let planet_sys_id = ctx
                .sde
                .solar_systems
                .values()
                .find(|s| s.planet_ids.contains(&planet_id))
                .map(|s| s.solar_system_id);
            let Some(sys_id) = planet_sys_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: format!("Planet {planet_id} not found in SDE"),
                }));
            };
            let p0_resources = list_sde_planet_p0_resources(ctx, sys_id, planet_id);
            if p0_resources.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "UncolonizablePlanetType".into(),
                    message: format!("Planet {planet_id} has no harvestable P0 resources in SDE"),
                }));
            }
            let (remote_sensing_lvl, planetology_lvl, adv_planetology_lvl, extracted_map) =
                ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let rs = sk.and_then(|m| m.get(&13279)).map(|s| s.level).unwrap_or(0);
                    let pl = sk.and_then(|m| m.get(&2406)).map(|s| s.level).unwrap_or(0);
                    let apl = sk.and_then(|m| m.get(&2403)).map(|s| s.level).unwrap_or(0);
                    let ext = snap
                        .planetary_colonies
                        .get(&(char_id, planet_id))
                        .map(|c| c.extracted_commodities.clone())
                        .unwrap_or_default();
                    (rs, pl, apl, ext)
                });
            let num_bands = 2 + (planetology_lvl as i64) + 2 * (adv_planetology_lvl as i64);
            let mut entries = Vec::with_capacity(p0_resources.len());
            for (idx, &tid) in p0_resources.iter().enumerate() {
                let extracted_qty = extracted_map.get(&tid).copied().unwrap_or(0);
                let depletion = ((extracted_qty as f64) / 100_000.0).min(0.40);
                let quality = (0.85 - (idx as f64) * 0.08 - depletion).clamp(0.25, 1.0);
                entries.push(PyRep::keyval([
                    ("resourceTypeID", PyRep::Integer(tid as i64)),
                    ("quality", PyRep::Real(quality)),
                    ("numBands", PyRep::Integer(num_bands)),
                ]));
            }
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("remoteSensingLevel", PyRep::Integer(remote_sensing_lvl as i64)),
                ("planetologyLevel", PyRep::Integer(planetology_lvl as i64)),
                (
                    "advancedPlanetologyLevel",
                    PyRep::Integer(adv_planetology_lvl as i64),
                ),
                ("numBands", PyRep::Integer(num_bands)),
                ("resources", PyRep::List(entries)),
            ])))
        }

        ("planetMgr", "GetResourceData") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (planet_id_opt, res_type_opt) = match args.first() {
                Some(PyRep::KeyVal(kv)) => (
                    kv.get("planetID").and_then(|v| v.as_i64()),
                    kv.get("resourceTypeID").and_then(|v| v.as_i64()),
                ),
                Some(PyRep::Dict(entries)) => {
                    let find_k = |k: &str| {
                        entries
                            .iter()
                            .find(|(ek, _)| ek.as_str() == Some(k))
                            .map(|(_, ev)| ev)
                    };
                    (
                        find_k("planetID").and_then(|v| v.as_i64()),
                        find_k("resourceTypeID").and_then(|v| v.as_i64()),
                    )
                }
                _ => (
                    args.first().and_then(|a| a.as_i64()),
                    args.get(1).and_then(|a| a.as_i64()),
                ),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetResourceData requires planetID".into(),
                }));
            };
            let planet_sys_id = ctx
                .sde
                .solar_systems
                .values()
                .find(|s| s.planet_ids.contains(&planet_id))
                .map(|s| s.solar_system_id);
            let Some(sys_id) = planet_sys_id else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: format!("Planet {planet_id} not found in SDE"),
                }));
            };
            let p0_resources = list_sde_planet_p0_resources(ctx, sys_id, planet_id);
            let req_res_tid = res_type_opt
                .map(|r| r as TypeId)
                .or_else(|| p0_resources.first().copied())
                .unwrap_or(0);
            if !p0_resources.contains(&req_res_tid) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetResource".into(),
                    message: format!(
                        "Resource type {req_res_tid} is not present on planet {planet_id}"
                    ),
                }));
            }
            let (remote_sensing_lvl, planetology_lvl, adv_planetology_lvl) =
                ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let rs = sk.and_then(|m| m.get(&13279)).map(|s| s.level).unwrap_or(0);
                    let pl = sk.and_then(|m| m.get(&2406)).map(|s| s.level).unwrap_or(0);
                    let apl = sk.and_then(|m| m.get(&2403)).map(|s| s.level).unwrap_or(0);
                    (rs, pl, apl)
                });
            if remote_sensing_lvl < 1 {
                return Some(Err(BridgeRpcError {
                    code: "PISkillRequiredToSurveyPlanetResources".into(),
                    message: "GetResourceData requires Remote Sensing (13279) >= level 1".into(),
                }));
            }
            let num_bands = (2 + (planetology_lvl as usize) + 2 * (adv_planetology_lvl as usize)).clamp(2, 16);
            let mut sh_bytes = Vec::with_capacity(num_bands * num_bands * 4);
            for b in 0..(num_bands * num_bands) {
                let sample = ((planet_id as u64)
                    .wrapping_mul(31)
                    .wrapping_add((req_res_tid as u64).wrapping_mul(17))
                    .wrapping_add(b as u64)
                    % 100) as f32
                    / 100.0;
                sh_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("resourceTypeID", PyRep::Integer(req_res_tid as i64)),
                ("numBands", PyRep::Integer(num_bands as i64)),
                ("inRange", PyRep::Bool(true)),
                ("proximity", PyRep::Integer(5)),
                ("data", PyRep::Bytes(sh_bytes)),
            ])))
        }

        ("planetMgr", "GetPlanetsForChar") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.planetary_colonies
                    .values()
                    .filter(|c| c.owner_id == char_id)
                    .map(|c| {
                        vec![
                            PyRep::Integer(c.planet_id),
                            PyRep::Integer(c.solar_system_id as i64),
                            PyRep::Integer(c.command_center_level as i64),
                            PyRep::Integer(c.pins_count as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "planetID",
                    "solarSystemID",
                    "commandCenterLevel",
                    "pinsCount",
                ],
                rows,
            )))
        }

        ("planetMgr", "GetColony")
        | ("planetMgr", "GetFullNetworkForOwner")
        | ("planetMgr", "GetCommandPinsForPlanet")
        | ("planetMgr", "GetExtractorsForPlanet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: format!("{method} requires an explicit integer planetID"),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No planetary colony found for character {char_id} on planet {planet_id}"
                        ),
                    });
                };
                let net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                if method == "GetExtractorsForPlanet" {
                    let ext_rep: Vec<PyRep> = net
                        .pins
                        .iter()
                        .filter(|p| {
                            p.type_id == 2848
                                || ctx
                                    .sde
                                    .types
                                    .get(&p.type_id)
                                    .map(|t| t.group_id == 1026)
                                    .unwrap_or(false)
                        })
                        .map(|p| {
                            PyRep::keyval([
                                ("pinID", PyRep::Integer(p.pin_id)),
                                ("typeID", PyRep::Integer(p.type_id as i64)),
                                ("latitude", PyRep::Real(p.latitude)),
                                ("longitude", PyRep::Real(p.longitude)),
                                ("cpuUsage", PyRep::Real(p.cpu_usage)),
                                ("powergridUsage", PyRep::Real(p.pg_usage)),
                            ])
                        })
                        .collect();
                    return Ok(PyRep::List(ext_rep));
                }
                let pins_rep: Vec<PyRep> = net
                    .pins
                    .iter()
                    .map(|p| {
                        PyRep::keyval([
                            ("pinID", PyRep::Integer(p.pin_id)),
                            ("typeID", PyRep::Integer(p.type_id as i64)),
                            ("latitude", PyRep::Real(p.latitude)),
                            ("longitude", PyRep::Real(p.longitude)),
                            ("cpuUsage", PyRep::Real(p.cpu_usage)),
                            ("powergridUsage", PyRep::Real(p.pg_usage)),
                        ])
                    })
                    .collect();
                let links_rep: Vec<PyRep> = net
                    .links
                    .iter()
                    .map(|l| {
                        PyRep::keyval([
                            ("endpoint1", PyRep::Integer(l.source_pin_id)),
                            ("endpoint2", PyRep::Integer(l.dest_pin_id)),
                            ("sourcePinID", PyRep::Integer(l.source_pin_id)),
                            ("destPinID", PyRep::Integer(l.dest_pin_id)),
                            ("level", PyRep::Integer(l.level as i64)),
                            ("lengthKm", PyRep::Real(l.length_km)),
                            ("cpuUsage", PyRep::Real(l.cpu_usage)),
                            ("powergridUsage", PyRep::Real(l.pg_usage)),
                            ("bandwidth", PyRep::Real(l.bandwidth)),
                        ])
                    })
                    .collect();
                Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("ownerID", PyRep::Integer(char_id)),
                    ("solarSystemID", PyRep::Integer(col.solar_system_id as i64)),
                    (
                        "commandCenterLevel",
                        PyRep::Integer(col.command_center_level as i64),
                    ),
                    ("pinsCount", PyRep::Integer(col.pins_count as i64)),
                    ("cpuUsed", PyRep::Real(net.cpu_used)),
                    ("cpuMax", PyRep::Real(net.cpu_max)),
                    ("powergridUsed", PyRep::Real(net.pg_used)),
                    ("powergridMax", PyRep::Real(net.pg_max)),
                    ("pins", PyRep::List(pins_rep)),
                    ("links", PyRep::List(links_rep)),
                ]))
            });
            Some(res)
        }

        ("planetMgr", "GetCommandCenterLevel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetCommandCenterLevel requires planetID".into(),
                }));
            };
            let lvl = ctx.db.with_snapshot(|snap| {
                snap.planetary_colonies
                    .get(&(char_id, planet_id))
                    .map(|c| c.command_center_level as i64)
                    .unwrap_or(0)
            });
            Some(Ok(PyRep::Integer(lvl)))
        }

        ("planetMgr", "GetPin") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetPin requires planetID".into(),
                }));
            };
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony on planet {planet_id}"),
                    });
                };
                let net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let Some(pin) = net.pins.iter().find(|p| p.pin_id == pin_id) else {
                    return Err(BridgeRpcError {
                        code: "PinNotFound".into(),
                        message: format!("Pin {pin_id} not found on planet {planet_id}"),
                    });
                };
                let schem_id = pin.schematic_id.unwrap_or(0);
                let cycle_secs = if (65..=135).contains(&schem_id) {
                    1800
                } else if schem_id > 0 {
                    3600
                } else {
                    0
                };
                Ok(PyRep::keyval([
                    ("pinID", PyRep::Integer(pin.pin_id)),
                    ("typeID", PyRep::Integer(pin.type_id as i64)),
                    ("latitude", PyRep::Real(pin.latitude)),
                    ("longitude", PyRep::Real(pin.longitude)),
                    ("cpuUsage", PyRep::Real(pin.cpu_usage)),
                    ("powergridUsage", PyRep::Real(pin.pg_usage)),
                    ("schematicID", PyRep::Integer(schem_id as i64)),
                    ("cycleTimeSeconds", PyRep::Integer(cycle_secs)),
                ]))
            });
            Some(res)
        }

        ("planetMgr", "UserAbandonPlanet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserAbandonPlanet requires planetID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.planetary_colonies.remove(&(char_id, planet_id)).is_none() {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony on planet {planet_id} to abandon"),
                    });
                }
                snap.owner_notes.remove(&pi_network_note_id(char_id, planet_id));
                Ok(PyRep::Bool(true))
            });
            Some(res)
        }

        // V24.01 `code.ccp` `eve/client/script/environment/planet/clientPlanet.pyj` & `commandStream.pyj`:
        // `planetMgr.UserUpdateNetwork(planetID, commandStream)` (or `(commandStream)` when bound to a planet)
        // Executes atomic colony command stream:
        // - `COMMAND_CREATEPIN = 1`: `(1, pinID, typeID, latitude, longitude)` (Command Center or Extractor/Processor/Storage/Launchpad)
        // - `COMMAND_REMOVEPIN = 2`: `(2, pinID)`
        // - `COMMAND_CREATELINK = 3`: `(3, srcPinID, dstPinID, level)`
        // - `COMMAND_REMOVELINK = 4`: `(4, srcPinID, dstPinID)`
        // - `COMMAND_SETLINKLEVEL = 5`: `(5, srcPinID, dstPinID, newLevel)`
        // - `COMMAND_CREATEROUTE = 6`: `(6, routeID, path, typeID, qty)`
        // - `COMMAND_REMOVEROUTE = 7`: `(7, routeID)`
        // - `COMMAND_SETSCHEMATIC = 8`: `(8, pinID, schematicID)`
        // - `COMMAND_UPGRADECOMMANDCENTER = 9`: `(9, pinID, newLevel)` (or `(9, newLevel)`)
        ("planetMgr", "UserUpdateNetwork") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if matches!(args.first(), Some(PyRep::Integer(_)))
                && args.get(1).is_some()
                && !matches!(args.get(1), Some(PyRep::List(_) | PyRep::Tuple(_)))
            {
                return Some(Err(BridgeRpcError {
                    code: "ClientSchematicOverrideForbidden".into(),
                    message: "UserUpdateNetwork only accepts (planetID, [commandStream]) and forbids client-supplied overrides".into(),
                }));
            }
            let (planet_id_opt, cmd_list_opt) = match (args.first(), args.get(1)) {
                (Some(PyRep::Integer(pid)), Some(PyRep::List(cmds) | PyRep::Tuple(cmds))) => {
                    (Some(*pid), Some(cmds.as_slice()))
                }
                (Some(PyRep::List(cmds) | PyRep::Tuple(cmds)), maybe_pid) => {
                    (maybe_pid.and_then(|v| v.as_i64()), Some(cmds.as_slice()))
                }
                (Some(PyRep::Integer(pid)), None) => (Some(*pid), None),
                _ => (None, None),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserUpdateNetwork requires planetID and a commandStream list".into(),
                }));
            };
            if !(40_000_000..50_000_000).contains(&planet_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: format!(
                        "Planet ID {planet_id} is outside canonical celestial range 40000000..50000000"
                    ),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let planet_in_current_sys = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.planet_ids.contains(&planet_id))
                .unwrap_or(false);
            if !planet_in_current_sys {
                return Some(Err(BridgeRpcError {
                    code: "PlanetNotInCurrentSystem".into(),
                    message: format!(
                        "Planet {planet_id} does not belong to current solar system {sys_id}"
                    ),
                }));
            }
            let _ = match resolve_sde_planet_p0_extraction(ctx, sys_id, planet_id, None) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(commands) = cmd_list_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandStream".into(),
                    message: "UserUpdateNetwork requires a commandStream list or tuple".into(),
                }));
            };
            if commands.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandStream".into(),
                    message: "UserUpdateNetwork commandStream cannot be empty".into(),
                }));
            }

            let active_ship_id = session.ship_id;
            let is_docked = session.station_id.is_some() || session.structure_id.is_some();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let prev_colony = snap.planetary_colonies.get(&(char_id, planet_id)).cloned();
                let net_nid = pi_network_note_id(char_id, planet_id);
                let prev_note = snap.owner_notes.get(&net_nid).cloned();
                let prev_balance = snap.characters.get(&char_id).map(|c| c.balance);
                let prev_journal_len = snap.wallet_journal.len();
                let mut consumed_cc_items: Vec<InventoryItemRecord> = Vec::new();
                let prev_next_entity_id = snap.next_entity_id;

                let mut apply_cmds = || -> Result<f64, BridgeRpcError> {
                    let mut total_isk_spent = 0.0_f64;
                    let mut client_to_server_pin_id: HashMap<i64, i64> = HashMap::new();

                for cmd_rep in commands {
                    let els: Vec<PyRep> = match cmd_rep {
                        PyRep::Tuple(v) | PyRep::List(v) => v.clone(),
                        PyRep::KeyVal(kv) => {
                            let cid = kv.get("commandID").or_else(|| kv.get("cmd")).and_then(|v| v.as_i64()).unwrap_or(0);
                            match cid {
                                1 => vec![
                                    PyRep::Integer(1),
                                    kv.get("pinID").cloned().unwrap_or(PyRep::Integer(0)),
                                    kv.get("typeID").cloned().unwrap_or(PyRep::Integer(0)),
                                    kv.get("latitude").cloned().unwrap_or(PyRep::Real(0.0)),
                                    kv.get("longitude").cloned().unwrap_or(PyRep::Real(0.0)),
                                ],
                                3 | 5 => vec![
                                    PyRep::Integer(cid),
                                    kv.get("endpoint1").or_else(|| kv.get("sourcePinID")).cloned().unwrap_or(PyRep::Integer(0)),
                                    kv.get("endpoint2").or_else(|| kv.get("destPinID")).cloned().unwrap_or(PyRep::Integer(0)),
                                    kv.get("level").cloned().unwrap_or(PyRep::Integer(0)),
                                ],
                                9 => vec![
                                    PyRep::Integer(9),
                                    kv.get("pinID").cloned().unwrap_or(PyRep::Integer(1)),
                                    kv.get("level").or_else(|| kv.get("targetLevel")).cloned().unwrap_or(PyRep::Integer(1)),
                                ],
                                _ => vec![PyRep::Integer(cid)],
                            }
                        }
                        _ => {
                            return Err(BridgeRpcError {
                                code: "InvalidCommandStream".into(),
                                message: "Each UserUpdateNetwork command must be a tuple, list, or KeyVal".into(),
                            });
                        }
                    };
                    let cmd_id = els.first().and_then(|v| v.as_i64()).unwrap_or(0);

                    match cmd_id {
                        // COMMAND_CREATEPIN = 1: (1, pinID, typeID, latitude, longitude) or (1, typeID, latitude, longitude)
                        1 => {
                            let (pin_id_raw, type_id_raw, lat, lon) = if els.len() >= 5 {
                                (
                                    extract_pi_pin_id(els.get(1)),
                                    els.get(2).and_then(|v| v.as_i64()).unwrap_or(0),
                                    els.get(3).and_then(|v| v.as_f64()).unwrap_or(f64::NAN),
                                    els.get(4).and_then(|v| v.as_f64()).unwrap_or(f64::NAN),
                                )
                            } else {
                                (
                                    0,
                                    els.get(1).and_then(|v| v.as_i64()).unwrap_or(0),
                                    els.get(2).and_then(|v| v.as_f64()).unwrap_or(f64::NAN),
                                    els.get(3).and_then(|v| v.as_f64()).unwrap_or(f64::NAN),
                                )
                            };
                            let Ok(pin_type_id) = TypeId::try_from(type_id_raw) else {
                                return Err(BridgeRpcError {
                                    code: "InvalidPinType".into(),
                                    message: format!("Invalid pin typeID {type_id_raw}"),
                                });
                            };
                            let half_pi = std::f64::consts::FRAC_PI_2 + 1e-6;
                            let full_pi = std::f64::consts::PI + 1e-6;
                            if !lat.is_finite()
                                || !lon.is_finite()
                                || !(-half_pi..=half_pi).contains(&lat)
                                || !(-full_pi..=full_pi).contains(&lon)
                            {
                                return Err(BridgeRpcError {
                                    code: "InvalidPinCoordinates".into(),
                                    message: format!(
                                        "Pin coordinates (lat={lat}, lon={lon}) must be finite radians in [-pi/2..pi/2, -pi..pi]"
                                    ),
                                });
                            }

                            if is_command_center_type(ctx, pin_type_id) {
                                if is_docked {
                                    return Err(BridgeRpcError {
                                        code: "CannotPlaceCommandCenterWhileDocked".into(),
                                        message: "Cannot deploy a Command Center while docked; you must be undocked in space with the Command Center in your active ship's cargo hold".into(),
                                    });
                                }
                                let Some(ship_id) = active_ship_id else {
                                    return Err(BridgeRpcError {
                                        code: "CommandCenterItemRequired".into(),
                                        message: "Deploying a Command Center requires an active ship in space with the Command Center in its cargo hold".into(),
                                    });
                                };
                                if snap.planetary_colonies.contains_key(&(char_id, planet_id)) {
                                    return Err(BridgeRpcError {
                                        code: "ColonyAlreadyExists".into(),
                                        message: format!(
                                            "Character {char_id} already has a Command Center on planet {planet_id}"
                                        ),
                                    });
                                }
                                let char_colony_count = snap
                                    .planetary_colonies
                                    .keys()
                                    .filter(|(cid, _)| *cid == char_id)
                                    .count();
                                let ipc_level = snap
                                    .skills
                                    .get(&char_id)
                                    .and_then(|m| m.get(&2495))
                                    .map(|s| s.level.clamp(0, 5))
                                    .unwrap_or(0);
                                let max_colonies = 1 + (ipc_level as usize);
                                if char_colony_count >= max_colonies {
                                    return Err(BridgeRpcError {
                                        code: "MaxColoniesReached".into(),
                                        message: format!(
                                            "Maximum of {max_colonies} planetary colonies reached (Interplanetary Consolidation level {ipc_level})"
                                        ),
                                    });
                                }
                                if pin_id_raw > 0 && client_to_server_pin_id.contains_key(&pin_id_raw) {
                                    return Err(BridgeRpcError {
                                        code: "DuplicatePinID".into(),
                                        message: format!("Duplicate pin ID {pin_id_raw} in command stream"),
                                    });
                                }
                                let matching_cc_item_id = snap
                                    .items
                                    .values()
                                    .filter(|it| {
                                        it.owner_id == char_id
                                            && it.type_id == pin_type_id
                                            && it.quantity >= 1
                                            && it.location_id == ship_id
                                            && matches!(
                                                it.flag,
                                                InventoryFlag::Cargo
                                                    | InventoryFlag::SpecializedCommandCenterHold
                                            )
                                    })
                                    .map(|it| it.item_id)
                                    .min();
                                let Some(cc_item_id) = matching_cc_item_id else {
                                    return Err(BridgeRpcError {
                                        code: "CommandCenterItemRequired".into(),
                                        message: format!(
                                            "Placing Command Center type {pin_type_id} requires an owned Command Center item in active ship cargo while undocked in space"
                                        ),
                                    });
                                };
                                if let Some(item) = snap.items.get_mut(&cc_item_id) {
                                    consumed_cc_items.push(item.clone());
                                    if item.quantity > 1 {
                                        item.quantity -= 1;
                                        item.version += 1;
                                    } else {
                                        snap.items.remove(&cc_item_id);
                                    }
                                }
                                snap.next_entity_id += 1;
                                let server_pin_id = snap.next_entity_id;
                                if pin_id_raw > 0 {
                                    client_to_server_pin_id.insert(pin_id_raw, server_pin_id);
                                }
                                let (cpu_max, pg_max) = cc_level_cpu_pg(0);
                                let net = PiColonyNetworkState {
                                    pins: vec![PiPinState {
                                        pin_id: server_pin_id,
                                        type_id: pin_type_id,
                                        latitude: lat,
                                        longitude: lon,
                                        schematic_id: None,
                                        cpu_usage: 0.0,
                                        pg_usage: 0.0,
                                    }],
                                    links: Vec::new(),
                                    routes: Vec::new(),
                                    cpu_used: 0.0,
                                    cpu_max,
                                    pg_used: 0.0,
                                    pg_max,
                                };
                                snap.planetary_colonies.insert(
                                    (char_id, planet_id),
                                    PlanetaryColonyRecord {
                                        planet_id,
                                        owner_id: char_id,
                                        solar_system_id: sys_id,
                                        command_center_level: 0,
                                        pins_count: 1,
                                        extracted_commodities: HashMap::new(),
                                        last_extracted_unix: 0,
                                    },
                                );
                                save_pi_network(snap, char_id, planet_id, &net);
                            } else {
                                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                    return Err(BridgeRpcError {
                                        code: "ColonyNotFound".into(),
                                        message: format!(
                                            "Cannot place pin on planet {planet_id} before placing a Command Center"
                                        ),
                                    });
                                };
                                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                if pin_id_raw > 0
                                    && (client_to_server_pin_id.contains_key(&pin_id_raw)
                                        || net.pins.iter().any(|p| p.pin_id == pin_id_raw))
                                {
                                    return Err(BridgeRpcError {
                                        code: "DuplicatePinID".into(),
                                        message: format!("Pin ID {pin_id_raw} already exists on planet {planet_id}"),
                                    });
                                }
                                let (pin_cpu, pin_pg, pin_cost_isk) = resolve_pi_pin_stats(ctx, pin_type_id);
                                if net.cpu_used + pin_cpu > net.cpu_max + 1e-6
                                    || net.pg_used + pin_pg > net.pg_max + 1e-6
                                {
                                    return Err(BridgeRpcError {
                                        code: "InsufficientColonyCpuOrPowergrid".into(),
                                        message: format!(
                                            "Placing pin type {pin_type_id} requires ({pin_cpu:.1} CPU, {pin_pg:.1} PG), exceeding colony budget (CPU {:.1}/{:.1}, PG {:.1}/{:.1})",
                                            net.cpu_used, net.cpu_max, net.pg_used, net.pg_max
                                        ),
                                    });
                                }
                                let cost_cents = IskCents::from_f64_isk(pin_cost_isk);
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    if ch.balance < cost_cents {
                                        return Err(BridgeRpcError {
                                            code: "InsufficientFunds".into(),
                                            message: format!(
                                                "Insufficient ISK to construct planetary pin ({pin_cost_isk:.2} ISK required)"
                                            ),
                                        });
                                    }
                                    ch.balance = ch.balance.checked_sub(cost_cents).unwrap_or(IskCents(0));
                                    let bal_after = ch.balance;
                                    snap.append_journal(
                                        char_id,
                                        "PlanetPinConstruction",
                                        cost_cents,
                                        bal_after,
                                        format!("Constructed planetary pin type {pin_type_id} on planet {planet_id}"),
                                    );
                                }
                                total_isk_spent += pin_cost_isk;
                                snap.next_entity_id += 1;
                                let mut server_pin_id = snap.next_entity_id;
                                while net.pins.iter().any(|p| p.pin_id == server_pin_id) {
                                    snap.next_entity_id += 1;
                                    server_pin_id = snap.next_entity_id;
                                }
                                if pin_id_raw > 0 {
                                    client_to_server_pin_id.insert(pin_id_raw, server_pin_id);
                                }
                                net.cpu_used += pin_cpu;
                                net.pg_used += pin_pg;
                                net.pins.push(PiPinState {
                                    pin_id: server_pin_id,
                                    type_id: pin_type_id,
                                    latitude: lat,
                                    longitude: lon,
                                    schematic_id: None,
                                    cpu_usage: pin_cpu,
                                    pg_usage: pin_pg,
                                });
                                if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                                    col_mut.pins_count = net.pins.len() as u32;
                                }
                                save_pi_network(snap, char_id, planet_id, &net);
                            }
                        }

                        // COMMAND_REMOVEPIN = 2: (2, pinID)
                        2 => {
                            let pin_id_raw = extract_pi_pin_id(els.get(1));
                            let pin_id = client_to_server_pin_id.get(&pin_id_raw).copied().unwrap_or(pin_id_raw);
                            let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "ColonyNotFound".into(),
                                    message: format!("No colony on planet {planet_id}"),
                                });
                            };
                            let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                            let Some(idx) = net.pins.iter().position(|p| p.pin_id == pin_id) else {
                                return Err(BridgeRpcError {
                                    code: "PinNotFound".into(),
                                    message: format!("Pin {pin_id} not found on planet {planet_id}"),
                                });
                            };
                            let removed = net.pins.remove(idx);
                            net.cpu_used = (net.cpu_used - removed.cpu_usage).max(0.0);
                            net.pg_used = (net.pg_used - removed.pg_usage).max(0.0);
                            net.links.retain(|l| {
                                if l.source_pin_id == pin_id || l.dest_pin_id == pin_id {
                                    net.cpu_used = (net.cpu_used - l.cpu_usage).max(0.0);
                                    net.pg_used = (net.pg_used - l.pg_usage).max(0.0);
                                    false
                                } else {
                                    true
                                }
                            });
                            if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                                col_mut.pins_count = net.pins.len().max(1) as u32;
                            }
                            save_pi_network(snap, char_id, planet_id, &net);
                        }

                        // COMMAND_CREATELINK = 3: (3, srcPinID, dstPinID[, level])
                        3 => {
                            let src_raw = extract_pi_pin_id(els.get(1));
                            let dst_raw = extract_pi_pin_id(els.get(2));
                            let src_id = client_to_server_pin_id.get(&src_raw).copied().unwrap_or(src_raw);
                            let dst_id = client_to_server_pin_id.get(&dst_raw).copied().unwrap_or(dst_raw);
                            let level_raw = els.get(3).and_then(|v| v.as_i64()).unwrap_or(0);
                            if src_id <= 0 || dst_id <= 0 || src_id == dst_id || !(0..=10).contains(&level_raw) {
                                return Err(BridgeRpcError {
                                    code: "InvalidLinkParameters".into(),
                                    message: "CreateLink requires distinct positive endpoint pinIDs and level in 0..=10".into(),
                                });
                            }
                            let level = level_raw as u8;
                            let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "ColonyNotFound".into(),
                                    message: format!("No colony on planet {planet_id}"),
                                });
                            };
                            let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                            let Some(p1) = net.pins.iter().find(|p| p.pin_id == src_id).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "PinNotFound".into(),
                                    message: format!("Source pin {src_id} not found on planet {planet_id}"),
                                });
                            };
                            let Some(p2) = net.pins.iter().find(|p| p.pin_id == dst_id).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "PinNotFound".into(),
                                    message: format!("Destination pin {dst_id} not found on planet {planet_id}"),
                                });
                            };
                            if net.links.iter().any(|l| {
                                (l.source_pin_id == src_id && l.dest_pin_id == dst_id)
                                    || (l.source_pin_id == dst_id && l.dest_pin_id == src_id)
                            }) {
                                return Err(BridgeRpcError {
                                    code: "LinkAlreadyExists".into(),
                                    message: format!("Link between pins {src_id} and {dst_id} already exists"),
                                });
                            }
                            let cos_dsigma = (p1.latitude.sin() * p2.latitude.sin()
                                + p1.latitude.cos() * p2.latitude.cos() * (p1.longitude - p2.longitude).cos())
                            .clamp(-1.0, 1.0);
                            let length_km = (5_000.0 * cos_dsigma.acos()).max(1.0);
                            let (link_pg, link_cpu) = compute_pi_link_cost(length_km, level);
                            if net.cpu_used + link_cpu > net.cpu_max + 1e-6
                                || net.pg_used + link_pg > net.pg_max + 1e-6
                            {
                                return Err(BridgeRpcError {
                                    code: "InsufficientColonyCpuOrPowergrid".into(),
                                    message: format!(
                                        "Link ({length_km:.1} km, level {level}) requires ({link_cpu:.1} CPU, {link_pg:.1} PG), exceeding colony budget"
                                    ),
                                });
                            }
                            let link_cost_isk = (2_500.0 + 10.0 * length_km) * 2.0_f64.powi(level as i32);
                            let link_cost_cents = IskCents::from_f64_isk(link_cost_isk);
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                if ch.balance < link_cost_cents {
                                    return Err(BridgeRpcError {
                                        code: "InsufficientFunds".into(),
                                        message: format!("Insufficient ISK to build planetary link ({link_cost_isk:.2} ISK)"),
                                    });
                                }
                                ch.balance = ch.balance.checked_sub(link_cost_cents).unwrap_or(IskCents(0));
                            }
                            total_isk_spent += link_cost_isk;
                            let bandwidth = 1_250.0 * 2.0_f64.powi(level as i32);
                            net.cpu_used += link_cpu;
                            net.pg_used += link_pg;
                            net.links.push(PiLinkState {
                                source_pin_id: src_id,
                                dest_pin_id: dst_id,
                                level,
                                length_km,
                                cpu_usage: link_cpu,
                                pg_usage: link_pg,
                                bandwidth,
                            });
                            save_pi_network(snap, char_id, planet_id, &net);
                        }

                        // COMMAND_REMOVELINK = 4: (4, srcPinID, dstPinID)
                        4 => {
                            let src_raw = extract_pi_pin_id(els.get(1));
                            let dst_raw = extract_pi_pin_id(els.get(2));
                            let src_id = client_to_server_pin_id.get(&src_raw).copied().unwrap_or(src_raw);
                            let dst_id = client_to_server_pin_id.get(&dst_raw).copied().unwrap_or(dst_raw);
                            let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "ColonyNotFound".into(),
                                    message: format!("No colony on planet {planet_id}"),
                                });
                            };
                            let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                            let Some(idx) = net.links.iter().position(|l| {
                                (l.source_pin_id == src_id && l.dest_pin_id == dst_id)
                                    || (l.source_pin_id == dst_id && l.dest_pin_id == src_id)
                            }) else {
                                return Err(BridgeRpcError {
                                    code: "LinkNotFound".into(),
                                    message: format!("Link between {src_id} and {dst_id} not found"),
                                });
                            };
                            let removed = net.links.remove(idx);
                            net.cpu_used = (net.cpu_used - removed.cpu_usage).max(0.0);
                            net.pg_used = (net.pg_used - removed.pg_usage).max(0.0);
                            save_pi_network(snap, char_id, planet_id, &net);
                        }

                        // COMMAND_SETLINKLEVEL = 5: (5, srcPinID, dstPinID, newLevel)
                        5 => {
                            let src_raw = extract_pi_pin_id(els.get(1));
                            let dst_raw = extract_pi_pin_id(els.get(2));
                            let src_id = client_to_server_pin_id.get(&src_raw).copied().unwrap_or(src_raw);
                            let dst_id = client_to_server_pin_id.get(&dst_raw).copied().unwrap_or(dst_raw);
                            let new_lvl_raw = els.get(3).and_then(|v| v.as_i64()).unwrap_or(0);
                            if !(1..=10).contains(&new_lvl_raw) {
                                return Err(BridgeRpcError {
                                    code: "InvalidLinkLevel".into(),
                                    message: format!("Link upgrade level {new_lvl_raw} must be in 1..=10"),
                                });
                            }
                            let new_level = new_lvl_raw as u8;
                            let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "ColonyNotFound".into(),
                                    message: format!("No colony on planet {planet_id}"),
                                });
                            };
                            let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                            let Some(idx) = net.links.iter().position(|l| {
                                (l.source_pin_id == src_id && l.dest_pin_id == dst_id)
                                    || (l.source_pin_id == dst_id && l.dest_pin_id == src_id)
                            }) else {
                                return Err(BridgeRpcError {
                                    code: "LinkNotFound".into(),
                                    message: format!("Link between {src_id} and {dst_id} not found"),
                                });
                            };
                            let old_link = net.links[idx].clone();
                            if new_level <= old_link.level {
                                return Err(BridgeRpcError {
                                    code: "InvalidLinkLevel".into(),
                                    message: format!(
                                        "New link level {new_level} must be greater than current level {}",
                                        old_link.level
                                    ),
                                });
                            }
                            let (new_pg, new_cpu) = compute_pi_link_cost(old_link.length_km, new_level);
                            let delta_cpu = (new_cpu - old_link.cpu_usage).max(0.0);
                            let delta_pg = (new_pg - old_link.pg_usage).max(0.0);
                            if net.cpu_used + delta_cpu > net.cpu_max + 1e-6
                                || net.pg_used + delta_pg > net.pg_max + 1e-6
                            {
                                return Err(BridgeRpcError {
                                    code: "InsufficientColonyCpuOrPowergrid".into(),
                                    message: "Upgrading link exceeds colony CPU or Powergrid capacity".into(),
                                });
                            }
                            let upgrade_cost_isk =
                                (2_500.0 + 10.0 * old_link.length_km) * 2.0_f64.powi(new_level as i32);
                            let upgrade_cents = IskCents::from_f64_isk(upgrade_cost_isk);
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                if ch.balance < upgrade_cents {
                                    return Err(BridgeRpcError {
                                        code: "InsufficientFunds".into(),
                                        message: "Insufficient ISK to upgrade planetary link".into(),
                                    });
                                }
                                ch.balance = ch.balance.checked_sub(upgrade_cents).unwrap_or(IskCents(0));
                            }
                            total_isk_spent += upgrade_cost_isk;
                            net.cpu_used += delta_cpu;
                            net.pg_used += delta_pg;
                            net.links[idx].level = new_level;
                            net.links[idx].cpu_usage = new_cpu;
                            net.links[idx].pg_usage = new_pg;
                            net.links[idx].bandwidth = 1_250.0 * 2.0_f64.powi(new_level as i32);
                            save_pi_network(snap, char_id, planet_id, &net);
                        }

                        // COMMAND_CREATEROUTE = 6, COMMAND_REMOVEROUTE = 7, COMMAND_SETSCHEMATIC = 8
                        6 => {
                            let route_id = els.get(1).and_then(|v| v.as_i64()).unwrap_or(1);
                            let path_ids: Vec<i64> = match els.get(2) {
                                Some(PyRep::List(p) | PyRep::Tuple(p)) => p
                                    .iter()
                                    .filter_map(|v| v.as_i64())
                                    .map(|pid| client_to_server_pin_id.get(&pid).copied().unwrap_or(pid))
                                    .collect(),
                                _ => Vec::new(),
                            };
                            let ctype = els.get(3).and_then(|v| v.as_i64()).unwrap_or(0) as TypeId;
                            let qty = els.get(4).and_then(|v| v.as_i64()).unwrap_or(1).max(1) as u32;
                            let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "ColonyNotFound".into(),
                                    message: format!("No colony on planet {planet_id}"),
                                });
                            };
                            let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                            net.routes.push(PiRouteState {
                                route_id,
                                path: path_ids,
                                commodity_type_id: ctype,
                                quantity: qty,
                            });
                            save_pi_network(snap, char_id, planet_id, &net);
                        }
                        7 => {
                            let route_id = els.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
                            if let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() {
                                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                net.routes.retain(|r| r.route_id != route_id);
                                save_pi_network(snap, char_id, planet_id, &net);
                            }
                        }
                        8 => {
                            let pin_id_raw = extract_pi_pin_id(els.get(1));
                            let pin_id = client_to_server_pin_id.get(&pin_id_raw).copied().unwrap_or(pin_id_raw);
                            let schem_raw = els.get(2).and_then(|v| v.as_i64()).unwrap_or(0);
                            if !(1..=200).contains(&schem_raw) {
                                return Err(BridgeRpcError {
                                    code: "InvalidSchematic".into(),
                                    message: format!("Invalid planetary schematic ID {schem_raw}"),
                                });
                            }
                            let schem_id = schem_raw as u32;
                            if let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() {
                                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                if let Some(p) = net.pins.iter_mut().find(|p| p.pin_id == pin_id) {
                                    p.schematic_id = Some(schem_id);
                                }
                                save_pi_network(snap, char_id, planet_id, &net);
                            }
                        }

                        // COMMAND_UPGRADECOMMANDCENTER = 9: (9, pinID, targetLevel) or (9, targetLevel)
                        9 => {
                            let target_lvl_raw = if els.len() >= 3 {
                                els.get(2).and_then(|v| v.as_i64()).unwrap_or(0)
                            } else {
                                els.get(1).and_then(|v| v.as_i64()).unwrap_or(0)
                            };
                            if !(1..=5).contains(&target_lvl_raw) {
                                return Err(BridgeRpcError {
                                    code: "InvalidCommandCenterLevel".into(),
                                    message: format!("Target Command Center level {target_lvl_raw} must be in 1..=5"),
                                });
                            }
                            let target_level = target_lvl_raw as u8;
                            let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                return Err(BridgeRpcError {
                                    code: "ColonyNotFound".into(),
                                    message: format!("No colony on planet {planet_id}"),
                                });
                            };
                            if target_level <= col.command_center_level {
                                return Err(BridgeRpcError {
                                    code: "InvalidCommandCenterLevel".into(),
                                    message: format!(
                                        "Target level {target_level} must be greater than current level {}",
                                        col.command_center_level
                                    ),
                                });
                            }
                            let cc_skill_lvl = snap
                                .skills
                                .get(&char_id)
                                .and_then(|m| m.get(&2505))
                                .map(|s| s.level)
                                .unwrap_or(0);
                            if cc_skill_lvl < target_level {
                                return Err(BridgeRpcError {
                                    code: "MissingSkillForCommandCenterUpgrade".into(),
                                    message: format!(
                                        "Command Center upgrade to level {target_level} requires Command Center Upgrades skill (2505) >= {target_level} (current: {cc_skill_lvl})"
                                    ),
                                });
                            }
                            let upgrade_cost_isk =
                                cc_upgrade_cost_isk(col.command_center_level, target_level);
                            let upgrade_cost_cents = IskCents::from_f64_isk(upgrade_cost_isk);
                            if let Some(ch) = snap.characters.get_mut(&char_id) {
                                if ch.balance < upgrade_cost_cents {
                                    return Err(BridgeRpcError {
                                        code: "InsufficientFunds".into(),
                                        message: format!(
                                            "Insufficient funds for command center upgrade ({upgrade_cost_isk:.2} ISK required)"
                                        ),
                                    });
                                }
                                ch.balance = ch.balance.checked_sub(upgrade_cost_cents).unwrap_or(IskCents(0));
                                let bal_after = ch.balance;
                                snap.append_journal(
                                    char_id,
                                    "PlanetCommandCenterUpgrade",
                                    upgrade_cost_cents,
                                    bal_after,
                                    format!("Upgraded Command Center on planet {planet_id} to level {target_level}"),
                                );
                            }
                            total_isk_spent += upgrade_cost_isk;
                            let mut net = load_pi_network(snap, char_id, planet_id, target_level);
                            let (new_cpu_max, new_pg_max) = cc_level_cpu_pg(target_level);
                            net.cpu_max = new_cpu_max;
                            net.pg_max = new_pg_max;
                            if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                                col_mut.command_center_level = target_level;
                                col_mut.pins_count = net.pins.len().max((target_level as usize) + 1) as u32;
                            }
                            save_pi_network(snap, char_id, planet_id, &net);
                        }

                        other => {
                            return Err(BridgeRpcError {
                                code: "UnsupportedPlanetCommand".into(),
                                message: format!("Unsupported UserUpdateNetwork command code {other}"),
                            });
                        }
                    }
                }
                    Ok(total_isk_spent)
                };

                let total_isk_spent = match apply_cmds() {
                    Ok(spent) => spent,
                    Err(e) => {
                        match prev_colony {
                            Some(col) => {
                                snap.planetary_colonies.insert((char_id, planet_id), col);
                            }
                            None => {
                                snap.planetary_colonies.remove(&(char_id, planet_id));
                            }
                        }
                        match prev_note {
                            Some(n) => {
                                snap.owner_notes.insert(net_nid, n);
                            }
                            None => {
                                snap.owner_notes.remove(&net_nid);
                            }
                        }
                        if let (Some(bal), Some(ch)) = (prev_balance, snap.characters.get_mut(&char_id)) {
                            ch.balance = bal;
                        }
                        snap.wallet_journal.truncate(prev_journal_len);
                        for restored_item in consumed_cc_items {
                            snap.items.insert(restored_item.item_id, restored_item);
                        }
                        snap.next_entity_id = prev_next_entity_id;
                        return Err(e);
                    }
                };

                let col = snap
                    .planetary_colonies
                    .get(&(char_id, planet_id))
                    .cloned()
                    .ok_or_else(|| BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony on planet {planet_id}"),
                    })?;
                let net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let pins_rep: Vec<PyRep> = net
                    .pins
                    .iter()
                    .map(|p| {
                        PyRep::keyval([
                            ("pinID", PyRep::Integer(p.pin_id)),
                            ("typeID", PyRep::Integer(p.type_id as i64)),
                            ("latitude", PyRep::Real(p.latitude)),
                            ("longitude", PyRep::Real(p.longitude)),
                            ("cpuUsage", PyRep::Real(p.cpu_usage)),
                            ("powergridUsage", PyRep::Real(p.pg_usage)),
                        ])
                    })
                    .collect();
                let links_rep: Vec<PyRep> = net
                    .links
                    .iter()
                    .map(|l| {
                        PyRep::keyval([
                            ("endpoint1", PyRep::Integer(l.source_pin_id)),
                            ("endpoint2", PyRep::Integer(l.dest_pin_id)),
                            ("sourcePinID", PyRep::Integer(l.source_pin_id)),
                            ("destPinID", PyRep::Integer(l.dest_pin_id)),
                            ("level", PyRep::Integer(l.level as i64)),
                            ("lengthKm", PyRep::Real(l.length_km)),
                            ("cpuUsage", PyRep::Real(l.cpu_usage)),
                            ("powergridUsage", PyRep::Real(l.pg_usage)),
                            ("bandwidth", PyRep::Real(l.bandwidth)),
                        ])
                    })
                    .collect();
                Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    (
                        "currentLevel",
                        PyRep::Integer(col.command_center_level as i64),
                    ),
                    (
                        "commandCenterLevel",
                        PyRep::Integer(col.command_center_level as i64),
                    ),
                    ("pinsCount", PyRep::Integer(net.pins.len() as i64)),
                    ("linksCount", PyRep::Integer(net.links.len() as i64)),
                    ("cpuUsed", PyRep::Real(net.cpu_used)),
                    ("cpuMax", PyRep::Real(net.cpu_max)),
                    ("cpuCapacity", PyRep::Integer(net.cpu_max.round() as i64)),
                    ("powerUsed", PyRep::Real(net.pg_used)),
                    ("powergridUsed", PyRep::Real(net.pg_used)),
                    ("powergridMax", PyRep::Real(net.pg_max)),
                    ("powerCapacity", PyRep::Integer(net.pg_max.round() as i64)),
                    ("totalCostISK", PyRep::Real(total_isk_spent)),
                    ("pins", PyRep::List(pins_rep)),
                    ("links", PyRep::List(links_rep)),
                ]))
            });
            Some(res)
        }

        ("planetMgr", "Flush") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.len() > 2 {
                return Some(Err(BridgeRpcError {
                    code: "ClientSchematicOverrideForbidden".into(),
                    message: "Flush accepts at most (planetID, [planetHarvesterOrResourceTypeID])".into(),
                }));
            }
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "Flush requires an explicit integer planetID".into(),
                }));
            };
            let requested_resource_or_pin = match args.get(1) {
                None => None,
                Some(arg) => {
                    let Some(raw_id) = arg
                        .as_i64()
                        .filter(|&id| id > 0 && id <= i64::from(u32::MAX))
                        .map(|id| id as u32)
                        .filter(|id| {
                            ctx.sde
                                .types
                                .get(id)
                                .map(|t| {
                                    t.group_id == 1026 || matches!(t.group_id, 1032 | 1033 | 1035)
                                })
                                .unwrap_or(false)
                        })
                    else {
                        return Some(Err(BridgeRpcError {
                            code: "ClientSchematicOverrideForbidden".into(),
                            message: "Flush optional second argument must be a valid planet extractor pin or raw P0 resource typeID".into(),
                        }));
                    };
                    Some(raw_id)
                }
            };
            let Some((delivery_loc_id, delivery_flag)) = session
                .station_id
                .or(session.structure_id)
                .map(|loc| (loc, InventoryFlag::Hangar))
                .or_else(|| session.ship_id.map(|ship| (ship, InventoryFlag::Cargo)))
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoDeliveryLocation".into(),
                    message: "Flushing planetary colony requires an active station, structure, or ship in the colony system".into(),
                }));
            };
            let current_sys_id = session.solar_system_id;
            let (link_pg, link_cpu) = compute_pi_link_cost(150.0, 1);
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (cc_level, pins_count, col_sys_id) = {
                    let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                        return Err(BridgeRpcError {
                            code: "ColonyNotFound".into(),
                            message: format!(
                                "No planetary colony found for character {char_id} on planet {planet_id}"
                            ),
                        });
                    };
                    if current_sys_id != Some(col.solar_system_id) {
                        return Err(BridgeRpcError {
                            code: "ColonyNotInCurrentSystem".into(),
                            message: format!(
                                "Planetary colony {planet_id} is in solar system {}, not {:?}",
                                col.solar_system_id, current_sys_id
                            ),
                        });
                    }
                    (
                        col.command_center_level,
                        col.pins_count,
                        col.solar_system_id,
                    )
                };

                let (p0_type_id, cycle_secs, p0_qty_per_pin, p0_unit_tax_value) =
                    resolve_sde_planet_p0_extraction(
                        ctx,
                        col_sys_id,
                        planet_id,
                        requested_resource_or_pin,
                    )?;
                let extractor_pins = pins_count.saturating_sub(1).max(1);
                let produced_qty = p0_qty_per_pin.saturating_mul(u64::from(extractor_pins));
                let produced_qty_i32 =
                    i32::try_from(produced_qty).map_err(|_| BridgeRpcError {
                        code: "QuantityOverflow".into(),
                        message: format!(
                            "Extracted quantity {produced_qty} overflows i32 inventory stack"
                        ),
                    })?;

                {
                    let col = snap
                        .planetary_colonies
                        .get(&(char_id, planet_id))
                        .expect("colony checked above");
                    if col.last_extracted_unix > 0
                        && now_unix < col.last_extracted_unix.saturating_add(cycle_secs)
                    {
                        return Err(BridgeRpcError {
                            code: "ExtractorCycleNotReady".into(),
                            message: format!(
                                "Extractor cycle on planet {planet_id} requires {cycle_secs}s between flushes"
                            ),
                        });
                    }
                }

                let tax_base = p0_unit_tax_value * (produced_qty as f64);
                let export_tax = compute_poco_export_tax(tax_base, 0.10, cc_level, 0.05);
                let tax_cents = IskCents::from_f64_isk(export_tax);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    let new_bal =
                        ch.balance
                            .checked_sub(tax_cents)
                            .ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: format!(
                                    "Insufficient ISK to pay POCO export tax ({export_tax:.2} ISK)"
                                ),
                            })?;
                    ch.balance = new_bal;
                }

                let col = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony checked above");
                col.last_extracted_unix = now_unix;
                *col.extracted_commodities.entry(p0_type_id).or_insert(0) += produced_qty;
                let total_stored = col.extracted_commodities[&p0_type_id];

                let existing_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == delivery_loc_id
                            && i.flag == delivery_flag
                            && i.type_id == p0_type_id
                    })
                    .map(|i| i.item_id);
                let delivered_item_id = if let Some(iid) = existing_item_id {
                    if let Some(item_mut) = snap.items.get_mut(&iid) {
                        item_mut.quantity = item_mut.quantity.saturating_add(produced_qty_i32);
                        item_mut.version += 1;
                    }
                    iid
                } else {
                    snap.next_entity_id += 1;
                    let iid = snap.next_entity_id;
                    snap.items.insert(
                        iid,
                        InventoryItemRecord {
                            item_id: iid,
                            type_id: p0_type_id,
                            owner_id: char_id,
                            location_id: delivery_loc_id,
                            flag: delivery_flag,
                            quantity: produced_qty_i32,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    iid
                };
                Ok((
                    p0_type_id,
                    produced_qty,
                    total_stored,
                    delivered_item_id,
                    export_tax,
                    cycle_secs,
                ))
            });

            match res {
                Ok((
                    p0_type_id,
                    produced_qty,
                    total_stored,
                    delivered_item_id,
                    export_tax,
                    cycle_secs,
                )) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("cycleTimeSeconds", PyRep::Integer(cycle_secs as i64)),
                    ("producedTypeID", PyRep::Integer(p0_type_id as i64)),
                    ("producedQuantity", PyRep::Integer(produced_qty as i64)),
                    ("totalStoredQuantity", PyRep::Integer(total_stored as i64)),
                    ("deliveredItemID", PyRep::Integer(delivered_item_id)),
                    ("linkPowergrid", PyRep::Real(link_pg)),
                    ("linkCPU", PyRep::Real(link_cpu)),
                    ("pocoExportTaxISK", PyRep::Real(export_tax)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "UpgradeCommandCenter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UpgradeCommandCenter requires an explicit integer planetID".into(),
                }));
            };
            let Some(target_level_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandCenterLevel".into(),
                    message: "UpgradeCommandCenter requires target upgrade level (1..=5)".into(),
                }));
            };
            if !(1..=5).contains(&target_level_raw) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandCenterLevel".into(),
                    message: format!("Target level {target_level_raw} must be in range 1..=5"),
                }));
            }
            let target_level = target_level_raw as u8;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony found for character {char_id} on planet {planet_id}"),
                    });
                };
                if target_level <= col.command_center_level {
                    return Err(BridgeRpcError {
                        code: "InvalidCommandCenterLevel".into(),
                        message: format!(
                            "Target level {target_level} must be greater than current level {}",
                            col.command_center_level
                        ),
                    });
                }
                let cc_skill_lvl = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&2505))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if cc_skill_lvl < target_level {
                    return Err(BridgeRpcError {
                        code: "MissingSkillForCommandCenterUpgrade".into(),
                        message: format!(
                            "Command Center upgrade to level {target_level} requires Command Center Upgrades skill (2505) >= {target_level} (current: {cc_skill_lvl})"
                        ),
                    });
                }

                let upgrade_cost_isk =
                    cc_upgrade_cost_isk(col.command_center_level, target_level);
                let upgrade_cost_cents = IskCents::from_f64_isk(upgrade_cost_isk);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.balance < upgrade_cost_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!(
                                "Insufficient funds for command center upgrade ({upgrade_cost_isk:.2} ISK required)"
                            ),
                        });
                    }
                    ch.balance = ch.balance.checked_sub(upgrade_cost_cents).unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "PlanetCommandCenterUpgrade",
                        upgrade_cost_cents,
                        bal_after,
                        format!("Upgraded Command Center on planet {planet_id} to level {target_level}"),
                    );
                }

                let mut net = load_pi_network(snap, char_id, planet_id, target_level);
                let (new_cpu_max, new_pg_max) = cc_level_cpu_pg(target_level);
                net.cpu_max = new_cpu_max;
                net.pg_max = new_pg_max;
                save_pi_network(snap, char_id, planet_id, &net);

                let col_mut = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony exists");
                col_mut.command_center_level = target_level;
                col_mut.pins_count = (target_level as u32) + 1;
                let new_pins = col_mut.pins_count;

                Ok((target_level, new_pins, upgrade_cost_isk, new_cpu_max, new_pg_max))
            });

            match res {
                Ok((new_level, new_pins, cost, cpu_max, pg_max)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("commandCenterLevel", PyRep::Integer(new_level as i64)),
                    ("pinsCount", PyRep::Integer(new_pins as i64)),
                    ("upgradeCostISK", PyRep::Real(cost)),
                    ("cpuMax", PyRep::Real(cpu_max)),
                    ("powergridMax", PyRep::Real(pg_max)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "UserTransferCommodities") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserTransferCommodities requires an explicit integer planetID".into(),
                }));
            };
            let Some(input_type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProcessorParameters".into(),
                    message: "UserTransferCommodities requires input commodity typeID".into(),
                }));
            };
            let Some(output_type_id_raw) = args.get(2).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProcessorParameters".into(),
                    message: "UserTransferCommodities requires output commodity typeID".into(),
                }));
            };
            let input_type_id = input_type_id_raw as TypeId;
            let output_type_id = output_type_id_raw as TypeId;
            let cycles = args.get(3).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u64;
            if cycles > 20_000 {
                return Some(Err(BridgeRpcError {
                    code: "PinStorageFull".into(),
                    message: format!("Requested transfer/processing quantity {cycles} exceeds planetary pin storage capacity"),
                }));
            }

            let is_p1_input = ctx
                .sde
                .types
                .get(&input_type_id)
                .map(|t| t.group_id == 1032)
                .unwrap_or(false);
            let (input_per_cycle, output_per_cycle) = if is_p1_input {
                (40_u64, 5_u64)
            } else {
                (3_000_u64, 20_u64)
            };
            let req_input = input_per_cycle.saturating_mul(cycles);
            let produced_output = output_per_cycle.saturating_mul(cycles);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony found for character {char_id} on planet {planet_id}"),
                    });
                };
                let current_input = col.extracted_commodities.get(&input_type_id).copied().unwrap_or(0);
                if current_input < req_input {
                    return Err(BridgeRpcError {
                        code: "InsufficientInputCommodities".into(),
                        message: format!(
                            "Colony storage has {current_input}x type {input_type_id}, but {req_input}x is required for {cycles} processor cycles"
                        ),
                    });
                }
                *col.extracted_commodities.entry(input_type_id).or_insert(0) -= req_input;
                *col.extracted_commodities.entry(output_type_id).or_insert(0) += produced_output;
                let total_output = col.extracted_commodities[&output_type_id];

                Ok((req_input, produced_output, total_output))
            });

            match res {
                Ok((consumed, produced, total)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("inputTypeID", PyRep::Integer(input_type_id as i64)),
                    ("inputQuantityConsumed", PyRep::Integer(consumed as i64)),
                    ("outputTypeID", PyRep::Integer(output_type_id as i64)),
                    ("outputQuantityProduced", PyRep::Integer(produced as i64)),
                    ("totalStoredOutput", PyRep::Integer(total as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "UserLaunchCommodities") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserLaunchCommodities requires planetID".into(),
                }));
            };
            let Some(type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRoutingParameters".into(),
                    message: "UserLaunchCommodities requires commodity typeID".into(),
                }));
            };
            let type_id = type_id_raw as TypeId;
            let qty = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
            let destination = args.get(3).and_then(|a| a.as_str()).unwrap_or("Storage");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony found for character {char_id} on planet {planet_id}"),
                    });
                };
                let available = col.extracted_commodities.get(&type_id).copied().unwrap_or(0);
                if available < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientCommoditiesForRouting".into(),
                        message: format!("Colony has {available} of type {type_id}, cannot route {qty}"),
                    });
                }
                let mut export_tax_isk = 0.0_f64;
                if destination == "CustomsOffice" {
                    let customs_lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&2497))
                        .map(|s| s.level)
                        .unwrap_or(0);
                    let unit_base = ctx
                        .sde
                        .types
                        .get(&type_id)
                        .map(|t| t.base_price.max(400.0))
                        .unwrap_or(400.0);
                    export_tax_isk = compute_poco_export_tax(
                        unit_base * (qty as f64),
                        0.10,
                        customs_lvl,
                        0.05,
                    );
                    let tax_cents = IskCents::from_f64_isk(export_tax_isk);
                    if tax_cents.0 > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(tax_cents).unwrap_or(IskCents(0));
                            let bal_after = ch.balance;
                            snap.append_journal(
                                char_id,
                                "CustomOfficeTax",
                                tax_cents,
                                bal_after,
                                format!("POCO export tax for {qty}x type {type_id} on planet {planet_id}"),
                            );
                        }
                    }
                }
                if destination == "Hangar" || destination == "CustomsOffice" {
                    if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                        *col_mut.extracted_commodities.entry(type_id).or_insert(0) -= qty;
                    }
                    let delivery_loc = session.station_id.or(session.structure_id).unwrap_or(col.planet_id);
                    snap.next_entity_id += 1;
                    let nid = snap.next_entity_id;
                    snap.items.insert(
                        nid,
                        InventoryItemRecord {
                            item_id: nid,
                            type_id,
                            owner_id: char_id,
                            location_id: delivery_loc,
                            flag: InventoryFlag::Hangar,
                            quantity: qty as i32,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
                Ok((qty, export_tax_isk))
            });

            match res {
                Ok((routed, export_tax_isk)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("commodityTypeID", PyRep::Integer(type_id as i64)),
                    ("routedQuantity", PyRep::Integer(routed as i64)),
                    ("destination", PyRep::String(destination.to_string())),
                    ("exportTaxISK", PyRep::Real(export_tax_isk)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
