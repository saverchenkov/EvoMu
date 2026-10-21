//! `industry_pi` — Domain 8: Manufacturing, ME/TE Research, Invention, Reactions, Reprocessing, & Planetary Industry (`§3.7`).

// parity: plan:E-4, plan:E-6
// parity: feat:industry.blueprint-data, feat:industry.facilities, feat:industry.facility-tax
// parity: feat:industry.install-job, feat:industry.job-cost
// parity: feat:pi.planet-info, feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins, feat:pi.links

#![forbid(unsafe_code)]

#[path = "industry_facilities_bps.rs"]
pub mod industry_facilities_bps;
pub use industry_facilities_bps::*;

#[path = "industry_jobs.rs"]
pub mod industry_jobs;
pub use industry_jobs::*;

#[path = "pi_colonies.rs"]
pub mod pi_colonies;
pub use pi_colonies::*;

#[path = "pi_orbitals.rs"]
pub mod pi_orbitals;
pub use pi_orbitals::*;

use crate::formulas::compute_reprocessing_yield_rate;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{InvType, SdeRegistry, TypeId, TypeMaterialEntry};
use std::collections::HashMap;

/// Checks whether an Upwell structure has an online Reprocessing service module (`35899`, `35892`, `35894`,
/// or `group_id == 1322`) fitted in a service or module slot (`it.flag != InventoryFlag::Hangar`).
pub fn structure_has_online_reprocessing_service(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    st_type_id: TypeId,
) -> bool {
    let is_reproc_mod = |it: &InventoryItemRecord| -> bool {
        matches!(it.type_id, 35899 | 35892 | 35894)
            || sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1322)
    };
    let is_fitted_and_online = |it: &InventoryItemRecord| -> bool {
        it.flag != InventoryFlag::Hangar
            && (it.flag.is_service_slot() || matches!(it.flag as u16, 11..=34))
            && snap.module_online_states.get(&it.item_id).copied().unwrap_or(true)
    };
    let has_fitted_reproc = snap.items.values().any(|it| {
        it.location_id == structure_id && is_reproc_mod(it) && is_fitted_and_online(it)
    });
    if has_fitted_reproc {
        return true;
    }
    let has_any_reproc_item = snap
        .items
        .values()
        .any(|it| it.location_id == structure_id && is_reproc_mod(it));
    let is_refinery = sde
        .types
        .get(&st_type_id)
        .is_some_and(|t| t.group_id == 1406);
    is_refinery && !has_any_reproc_item
}

/// SDE dogma attribute IDs for Planetary Industry pins & harvesters.
pub const ATTR_HARVESTER_TYPE: u16 = 709;
pub const ATTR_PLANET_RESTRICTION: u16 = 1632;
pub const ATTR_EXPORT_TAX_MULTIPLIER: u16 = 1641;
pub const ATTR_PIN_EXTRACTION_QUANTITY: u16 = 1642;
pub const ATTR_PIN_CYCLE_TIME: u16 = 1643;

/// Resolves the raw P0 planetary resource (`harvesterType` attribute `709`), base pin extraction
/// cycle duration (`pinCycleTime` attribute `1643` = `300`s), base pin extraction quantity
/// (`pinExtractionQuantity` attribute `1642` = `1000` units), and R0 export tax base
/// (`exportTaxMultiplier` attribute `1641` = `5.0` ISK) for an SDE planet celestial (`group_id == 7`)
/// via SDE extractor pins (`group_id == 1026`, linked by `planetRestriction` attribute `1632`).
pub fn resolve_sde_planet_p0_extraction(
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

pub fn list_sde_planet_p0_resources(
    ctx: &ServerContext,
    solar_system_id: u32,
    planet_id: i64,
) -> Vec<TypeId> {
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

/// Computes the Upwell structure base reprocessing yield (`0.51` for Athanor `35835` via SDE
/// `35899` attribute `717` = `0.50` and `35835` attribute `2722` `strRefiningYieldBonus` = `+2.0%`;
/// `0.52` for Tatara `35836`; `0.50` default).
pub fn compute_structure_reprocessing_base_yield(sde: &SdeRegistry, st_type_id: TypeId) -> f64 {
    let service_base = sde
        .types
        .get(&35899)
        .and_then(|t| t.default_attributes.get(&717).copied())
        .unwrap_or(0.50);
    if st_type_id == 35836 {
        0.52
    } else if st_type_id == 35835 {
        let bonus_pct = sde
            .types
            .get(&st_type_id)
            .and_then(|t| t.default_attributes.get(&2722).copied())
            .unwrap_or(2.0);
        ((service_base * (1.0 + bonus_pct / 100.0)) * 10_000.0).round() / 10_000.0
    } else {
        service_base
    }
}

pub fn compute_structure_rigged_base_yield(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    structure_id: i64,
    inv_type: Option<&InvType>,
) -> f64 {
    let Some(st) = snap.structures.get(&structure_id) else {
        return 0.50;
    };
    let unrigged = compute_structure_reprocessing_base_yield(sde, st.type_id);
    let hull_mult = unrigged / 0.50;
    let st_rig_size = sde
        .types
        .get(&st.type_id)
        .and_then(|t| t.default_attributes.get(&1547).copied())
        .unwrap_or(if st.type_id == 35836 { 3.0 } else { 2.0 });
    let sec_attr_id: u16 = if st.solar_system_id >= 31_000_000 {
        2357
    } else {
        let sec = sde
            .solar_systems
            .get(&st.solar_system_id)
            .map(|s| crate::services::mining_sites::displayed_security(s.security_status))
            .unwrap_or(1.0);
        if sec <= 0.0 {
            2357
        } else if sec < 0.5 {
            2356
        } else {
            2355
        }
    };
    let mut best_rig_yield: Option<f64> = None;
    for it in snap.items.values() {
        if it.location_id != structure_id
            || !matches!(
                it.flag,
                InventoryFlag::RigSlot0 | InventoryFlag::RigSlot1 | InventoryFlag::RigSlot2
            )
        {
            continue;
        }
        let Some(rig_t) = sde.types.get(&it.type_id) else {
            continue;
        };
        if !matches!(rig_t.group_id, 1941..=1945) {
            continue;
        }
        let rig_size = rig_t.default_attributes.get(&1547).copied().unwrap_or(0.0);
        if (rig_size - st_rig_size).abs() > 0.1 {
            continue;
        }
        let matches_item = match inv_type {
            None => true,
            Some(ty) => {
                let is_ice = ty.group_id == 465 || ty.group_id == 4033;
                let is_moon = matches!(ty.group_id, 1884 | 1920..=1923);
                let is_ast =
                    (ty.category_id == 25 || ty.group_id == 4932) && !is_ice && !is_moon;
                match rig_t.group_id {
                    1941 => is_ast,
                    1942 => is_ice,
                    1943 => is_moon,
                    1944 | 1945 => is_ast || is_ice || is_moon,
                    _ => false,
                }
            }
        };
        if !matches_item {
            continue;
        }
        let Some(&rig_base) = rig_t.default_attributes.get(&717) else {
            continue;
        };
        let sec_mult = rig_t
            .default_attributes
            .get(&sec_attr_id)
            .copied()
            .unwrap_or(1.0);
        let candidate = rig_base * sec_mult * hull_mult;
        if best_rig_yield.is_none_or(|b| candidate > b) {
            best_rig_yield = Some(candidate);
        }
    }
    best_rig_yield.unwrap_or(unrigged)
}

/// Resolves reprocessing output materials for `inv_type` (`sde.type_materials`), including
/// Equinox `Prismaticite` (`90041`) / `Compressed Prismaticite` (`90307`) (`group_id == 4915`,
/// randomly collapsing into 1 of the 8 standard minerals weighted by equal ISK value from `sde.types`)
/// and `Unrefined <Mineral>` (`90283..=90298`, `group_id == 4932`, deterministically yielding the
/// target mineral from `sde.types`).
/// DERIVED: CCP stores Prismaticite & Unrefined Mineral outputs in `randomizedTypeMaterials` rather than
/// `sde_type_materials`; equal-ISK batch quantities (`524,288 ISK / base_price` per 100 units) are
/// derived from each SDE mineral's `base_price` (`34..=40`, `11399`).
fn resolve_reprocessable_materials(
    sde: &SdeRegistry,
    inv_type: &InvType,
    item_id: i64,
    next_entity_id: i64,
    deterministic_quote: bool,
) -> Option<Vec<TypeMaterialEntry>> {
    if let Some(mats) = sde.type_materials.get(&inv_type.type_id).filter(|m| !m.is_empty()) {
        return Some(mats.clone());
    }
    let mut standard_minerals: Vec<&InvType> = sde
        .types
        .values()
        .filter(|t| t.group_id == 18 && t.published && t.type_id <= 11399 && t.base_price > 0.0)
        .collect();
    standard_minerals.sort_by_key(|t| t.type_id);
    if standard_minerals.is_empty() {
        return None;
    }
    if inv_type.group_id == 4932 {
        let mineral_name = inv_type.name.strip_prefix("Unrefined ")?;
        let min_t = standard_minerals.into_iter().find(|t| t.name == mineral_name)?;
        let qty = (524_288.0 / min_t.base_price.max(2.0)).round().max(1.0) as u64;
        return Some(vec![TypeMaterialEntry {
            material_type_id: min_t.type_id,
            quantity: qty,
        }]);
    }
    if inv_type.group_id == 4915 || matches!(inv_type.type_id, 90041 | 90307) {
        let idx = if deterministic_quote {
            (item_id.unsigned_abs() as usize) % standard_minerals.len()
        } else {
            let mut seed = (item_id as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(next_entity_id as u64);
            let mut buf = [0u8; 8];
            if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
                use std::io::Read;
                if f.read_exact(&mut buf).is_ok() {
                    seed ^= u64::from_ne_bytes(buf);
                }
            }
            (seed as usize) % standard_minerals.len()
        };
        let min_t = standard_minerals[idx];
        let qty = (524_288.0 / min_t.base_price.max(2.0)).round().max(1.0) as u64;
        return Some(vec![TypeMaterialEntry {
            material_type_id: min_t.type_id,
            quantity: qty,
        }]);
    }
    None
}

pub fn compute_reprocessing_rates_for_item(
    sde: &SdeRegistry,
    snap: &WorldStateSnapshot,
    char_id: i64,
    station_id: i64,
    inv_type: &InvType,
) -> (f64, f64, f64) {
    let char_skills = snap.skills.get(&char_id);
    let reproc_lvl = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
    let reproc_eff_lvl = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
    let ore_proc_lvl = inv_type
        .default_attributes
        .get(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE)
        .and_then(|&skill_id_f| char_skills.and_then(|m| m.get(&(skill_id_f as TypeId))).map(|s| s.level))
        .unwrap_or(0);
    let scrap_lvl = char_skills.and_then(|m| m.get(&12196)).map(|s| s.level).unwrap_or(0);
    let is_ore_or_ice = inv_type.category_id == 25
        || inv_type.group_id == 465
        || inv_type.group_id == 4932
        || inv_type.default_attributes.contains_key(&eve_sde::attr_ids::REPROCESSING_SKILL_TYPE);
    let station_base_yield = if station_id >= 1_000_000_000_000 {
        if is_ore_or_ice {
            compute_structure_rigged_base_yield(sde, snap, station_id, Some(inv_type))
        } else {
            0.50
        }
    } else {
        sde.solar_systems
            .values()
            .flat_map(|s| s.stations.iter())
            .find(|st| st.station_id == station_id)
            .map(|st| st.reprocessing_efficiency.clamp(0.30, 0.50))
            .unwrap_or(0.50)
    };
    let implant_bonus = snap
        .active_implants
        .get(&char_id)
        .map(|implants| {
            implants
                .iter()
                .filter_map(|&imp_id| sde.types.get(&imp_id)?.default_attributes.get(&379))
                .map(|&bonus| bonus / 100.0)
                .sum::<f64>()
        })
        .unwrap_or(0.0);
    let yield_rate = if is_ore_or_ice {
        compute_reprocessing_yield_rate(
            station_base_yield,
            reproc_lvl,
            reproc_eff_lvl,
            ore_proc_lvl,
            implant_bonus,
        )
    } else {
        (station_base_yield * (1.0 + 0.02 * (scrap_lvl.min(5) as f64))).clamp(0.0, 0.55)
    };
    let station_tax_rate = if station_id >= 1_000_000_000_000 {
        0.0
    } else {
        let owner_corp_id = sde
            .solar_systems
            .values()
            .flat_map(|s| s.stations.iter())
            .find(|st| st.station_id == station_id)
            .map(|st| st.owner_corporation_id as i64);
        let effective_standing: f64 = owner_corp_id
            .map(|cid| {
                let ch_opt = snap.characters.get(&char_id);
                let personal_raw = ch_opt
                    .and_then(|ch| ch.standings.get(&cid).copied())
                    .unwrap_or(0.0);
                let conn_lvl = char_skills
                    .and_then(|m| m.get(&3359))
                    .map(|s| s.level.min(5))
                    .unwrap_or(0);
                let has_completed_mission_for_corp = snap.active_missions.values().any(|m| {
                    m.character_id == char_id
                        && m.state == "Completed"
                        && sde.agents.get(&m.agent_id).is_some_and(|a| a.corporation_id == cid)
                }) || snap
                    .standing_transactions
                    .get(&char_id)
                    .is_some_and(|txs| {
                        txs.iter()
                            .any(|tx| tx.from_id == cid && tx.event_type_id == 85 && tx.modification > 0.0)
                    });
                let personal_eff = if has_completed_mission_for_corp && personal_raw >= 0.0 && conn_lvl > 0 {
                    (personal_raw + (10.0 - personal_raw) * 0.04 * (conn_lvl as f64)).clamp(-10.0, 10.0)
                } else {
                    personal_raw
                };
                let player_corp_id = ch_opt.map(|ch| ch.corporation_id).unwrap_or(0);
                let corp_standing = if player_corp_id > 0 {
                    let direct_corp_st = snap
                        .characters
                        .get(&player_corp_id)
                        .and_then(|c| c.standings.get(&cid).copied());
                    direct_corp_st.unwrap_or_else(|| {
                        let mut sum = 0.0;
                        let mut cnt = 0usize;
                        for member in snap.characters.values().filter(|m| m.corporation_id == player_corp_id) {
                            if let Some(&st) = member.standings.get(&cid) {
                                sum += st;
                                cnt += 1;
                            }
                        }
                        if cnt > 0 { sum / (cnt as f64) } else { 0.0 }
                    })
                } else {
                    0.0
                };
                personal_eff.max(corp_standing).max(0.0)
            })
            .unwrap_or(0.0);
        (0.05_f64 - 0.0075_f64 * effective_standing).clamp(0.0, 0.05)
    };
    (station_base_yield, yield_rate, station_tax_rate)
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Some(res) =
        industry_facilities_bps::handle_industry_facilities_bps(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }
    if let Some(res) =
        industry_jobs::handle_industry_jobs(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }
    if let Some(res) =
        pi_colonies::handle_pi_colonies(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }
    if let Some(res) =
        pi_orbitals::handle_pi_orbitals(ctx, session, service, method, args, kwargs)
    {
        return Some(res);
    }

    // parity: plan:S-7
    match (service, method) {
        ("reprocessingSvc", "SetSessionCheck") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ok = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::Bool(ok)))
        }

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
                if station_id >= 1_000_000_000_000 {
                    let Some(st) = snap.structures.get(&station_id) else {
                        return Err(BridgeRpcError {
                            code: "StructureNotFound".into(),
                            message: format!("Structure {station_id} not found"),
                        });
                    };
                    if st.fuel_blocks == 0
                        || !structure_has_online_reprocessing_service(
                            &ctx.sde,
                            snap,
                            station_id,
                            st.type_id,
                        )
                    {
                        return Err(BridgeRpcError {
                            code: "ReprocessingServiceOffline".into(),
                            message: format!(
                                "Structure {station_id} does not have an online Reprocessing service"
                            ),
                        });
                    }
                }

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
                let Some(materials) = resolve_reprocessable_materials(
                    &ctx.sde,
                    inv_type,
                    item_id,
                    snap.next_entity_id,
                    false,
                ) else {
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

                let (station_base_yield, yield_rate, station_tax_rate) =
                    compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, inv_type);
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
                let mut gross_value_isk = 0.0_f64;
                for mat in &materials {
                    let unit_val = ctx
                        .sde
                        .types
                        .get(&mat.material_type_id)
                        .map(|t| t.base_price.max(4.0))
                        .unwrap_or(4.0);
                    gross_value_isk +=
                        (portions as f64) * (mat.quantity as f64) * yield_rate * unit_val;
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
                            ..Default::default()
                        },
                    );
                    outputs.push((mid, mat.material_type_id, out_qty));
                }

                let tax_isk = if station_tax_rate > 0.0 {
                    (gross_value_isk * station_tax_rate).max(1.0)
                } else {
                    0.0
                };
                if tax_isk > 0.0 {
                    let tax_cents = IskCents::from_f64_isk(tax_isk);
                    let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
                        let deduct = tax_cents.min(ch.balance);
                        ch.balance = ch.balance.checked_sub(deduct).unwrap_or(IskCents(0));
                        ch.balance
                    } else {
                        IskCents(0)
                    };
                    snap.append_journal(
                        char_id,
                        "ReprocessingTax",
                        tax_cents,
                        bal_after,
                        format!(
                            "Station reprocessing tax ({:.2}%) at station {station_id}",
                            station_tax_rate * 100.0
                        ),
                    );
                }

                Ok((outputs, station_base_yield, yield_rate, station_tax_rate, tax_isk, remainder))
            });

            match res {
                Ok((outputs, station_base_yield, yield_rate, station_tax_rate, tax_isk, remainder)) => {
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnReprocessingMaterialsChanged",
                        "character",
                        vec![PyRep::Integer(char_id), PyRep::Integer(station_id)],
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
                        ("stationBaseYield", PyRep::Real(station_base_yield)),
                        ("yieldRate", PyRep::Real(yield_rate)),
                        ("stationTaxRate", PyRep::Real(station_tax_rate)),
                        ("reprocessingTaxISK", PyRep::Real(tax_isk)),
                        ("remainderQuantity", PyRep::Integer(remainder as i64)),
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
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list)) => list.iter().filter_map(|a| a.as_i64()).collect(),
                Some(first) => first.as_i64().into_iter().collect(),
                None => Vec::new(),
            };
            if item_ids.is_empty() {
                let (station_base_yield, station_tax_rate, reproc_lvl, reproc_eff_lvl) =
                    ctx.db.with_snapshot(|snap| {
                        let dummy_type = ctx.sde.types.get(&1230);
                        let (mut base, _, tax) = dummy_type
                            .map(|t| compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, t))
                            .unwrap_or((0.50, 0.50, 0.0));
                        if station_id >= 1_000_000_000_000 {
                            base = snap
                                .structures
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
                                .unwrap_or(0.50);
                        }
                        let char_skills = snap.skills.get(&char_id);
                        let r = char_skills.and_then(|m| m.get(&3385)).map(|s| s.level).unwrap_or(0);
                        let re = char_skills.and_then(|m| m.get(&3389)).map(|s| s.level).unwrap_or(0);
                        (base, tax, r, re)
                    });
                return Some(Ok(PyRep::keyval([
                    ("stationBaseYield", PyRep::Real(station_base_yield)),
                    ("stationTaxRate", PyRep::Real(station_tax_rate)),
                    ("reprocessingSkillLevel", PyRep::Integer(reproc_lvl as i64)),
                    ("reprocessingEfficiencyLevel", PyRep::Integer(reproc_eff_lvl as i64)),
                ])));
            }
            let res = ctx.db.with_snapshot(|snap| {
                let mut quotes = Vec::new();
                for item_id in item_ids {
                    let Some(item) = snap.items.get(&item_id) else { continue };
                    let Some(inv_type) = ctx.sde.types.get(&item.type_id) else { continue };
                    let Some(materials) = resolve_reprocessable_materials(
                        &ctx.sde,
                        inv_type,
                        item_id,
                        snap.next_entity_id,
                        true,
                    ) else {
                        continue;
                    };
                    let (station_base_yield, yield_rate, station_tax_rate) =
                        compute_reprocessing_rates_for_item(&ctx.sde, snap, char_id, station_id, inv_type);
                    let net_yield_rate = yield_rate * (1.0 - station_tax_rate);
                    let portion_size = inv_type.portion_size.max(1) as i32;
                    let portions = item.quantity / portion_size;
                    let mut est_outputs = Vec::new();
                    for mat in &materials {
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
                        ("stationBaseYield", PyRep::Real(station_base_yield)),
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

        // parity: feat:pi.planet-info
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
                let net_state = pi_colonies::load_pi_network(snap, char_id, planet_id, cc_lvl);
                (col, net_state)
            });
            let (pins_rep, links_rep, routes_rep) = pi_colonies::format_pi_pins_links(&net);

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
            let num_bands =
                (2 + (planetology_lvl as usize) + 2 * (adv_planetology_lvl as usize)).clamp(2, 16);
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

        _ => None,
    }
}
