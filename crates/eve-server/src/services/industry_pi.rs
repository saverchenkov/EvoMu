//! `industry_pi` — Domain 8: Manufacturing, ME/TE Research, Invention, Reactions, Reprocessing, & Planetary Industry (`§3.7`).

// parity: plan:E-4, plan:E-6
// parity: feat:industry.blueprint-data, feat:industry.facilities, feat:industry.facility-tax
// parity: feat:industry.install-job, feat:industry.job-cost
// parity: feat:pi.planet-info, feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins, feat:pi.links

#![forbid(unsafe_code)]

#[path = "industry_facilities_bps.rs"]
pub mod industry_facilities_bps;
pub use industry_facilities_bps::*;

#[path = "industry_rigs.rs"]
pub mod industry_rigs;
#[allow(unused_imports)]
pub use industry_rigs::*;

#[path = "industry_jobs.rs"]
pub mod industry_jobs;
pub use industry_jobs::*;

#[path = "pi_resource_helpers.rs"]
pub mod pi_resource_helpers;
pub use pi_resource_helpers::*;

#[path = "pi_helpers.rs"]
pub mod pi_helpers;
pub use pi_helpers::*;

#[path = "pi_colonies.rs"]
pub mod pi_colonies;
pub use pi_colonies::*;

#[path = "pi_orbitals.rs"]
pub mod pi_orbitals;
pub use pi_orbitals::*;

#[path = "pi_orbitals_gm.rs"]
pub mod pi_orbitals_gm;
pub use pi_orbitals_gm::*;

#[path = "pi_orbital_registry.rs"]
pub mod pi_orbital_registry;
pub use pi_orbital_registry::*;

use crate::router::ServerContext;
use eve_db::IndustryJobRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::{HashMap, HashSet, VecDeque};

pub fn solar_system_jump_distance(ctx: &ServerContext, from_sys: u32, to_sys: u32) -> Option<u32> {
    if from_sys == to_sys {
        return Some(0);
    }
    let mut visited = HashSet::from([from_sys]);
    let mut queue = VecDeque::from([(from_sys, 0_u32)]);
    while let Some((curr, dist)) = queue.pop_front() {
        if dist >= 35 {
            continue;
        }
        for jump in ctx.sde.jumps.values() {
            let neighbor_opt = if jump.from_solar_system_id == curr {
                Some(jump.to_solar_system_id)
            } else if jump.to_solar_system_id == curr {
                Some(jump.from_solar_system_id)
            } else {
                None
            };
            if let Some(neighbor) = neighbor_opt {
                if neighbor == to_sys {
                    return Some(dist + 1);
                }
                if visited.insert(neighbor) {
                    queue.push_back((neighbor, dist + 1));
                }
            }
        }
    }
    None
}

pub fn resolve_decryptor_modifiers(
    ctx: &ServerContext,
    decryptor_type_id: TypeId,
) -> Result<(f64, f64, f64, i32), BridgeRpcError> {
    let Some(dtype) = ctx.sde.types.get(&decryptor_type_id) else {
        return Err(BridgeRpcError {
            code: "InvalidDecryptor".into(),
            message: format!("Decryptor type {decryptor_type_id} not found in SDE"),
        });
    };
    if dtype.group_id != 1304 {
        return Err(BridgeRpcError {
            code: "InvalidDecryptor".into(),
            message: format!("Type {decryptor_type_id} is not a Decryptor (group 1304)"),
        });
    }
    let prob_mult = dtype.default_attributes.get(&1112).copied().filter(|&v| v > 0.0).unwrap_or(1.0);
    let me_mod = dtype.default_attributes.get(&1113).copied().unwrap_or(0.0);
    let te_mod = dtype.default_attributes.get(&1114).copied().unwrap_or(0.0);
    let runs_mod = dtype.default_attributes.get(&1124).copied().unwrap_or(0.0).round() as i32;
    Ok((prob_mult, me_mod, te_mod, runs_mod))
}

pub fn format_industry_job_pyrep(j: &IndustryJobRecord, meta: &IndustryJobCostMeta) -> PyRep {
    let owner_id = if meta.owner_id > 0 { meta.owner_id } else { j.installer_id };
    PyRep::keyval([
        ("jobID", PyRep::Integer(j.job_id)),
        ("installerID", PyRep::Integer(j.installer_id)),
        ("ownerID", PyRep::Integer(owner_id)),
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
        ("alphaCloneTaxISK", PyRep::Real(meta.alpha_clone_tax_isk)),
        ("totalJobCostISK", PyRep::Real(meta.total_cost_isk)),
    ])
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

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
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
        pi_colonies::handle_pi_colonies(ctx, session, service, method, args, kwargs, bound_entry)
    {
        return Some(res);
    }
    if let Some(res) =
        pi_orbital_registry::handle_pi_orbital_registry(ctx, session, service, method, args, kwargs, bound_entry)
    {
        return Some(res);
    }
    if let Some(res) =
        pi_orbitals::handle_pi_orbitals(ctx, session, service, method, args, kwargs, bound_entry)
    {
        return Some(res);
    }

    let bound_planet = bound_entry.and_then(|b| (b.service == "planetMgr").then_some(b.object_id));

    match (service, method) {
        // parity: feat:pi.planet-info
        ("planetMgr", "GetPlanetInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetPlanetInfo requires an explicit integer planetID or bound planetMgr moniker".into(),
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
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetPlanetResourceInfo requires an explicit integer planetID or bound planetMgr moniker".into(),
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
            // V24 `ClientPlanet.GetResourceData` (clientPlanet.pyj) sends ONE `utillib.KeyVal` built by
            // `CreateResourceInfo` (resourceTypeID, updateTime, oldBand, planetology, advancedPlanetology,
            // remoteSensing, proximity, newBand) and reads `shData.data` / `.numBands` / `.proximity` back.
            let info_field = |k: &str| -> Option<i64> {
                match args.first() {
                    Some(PyRep::KeyVal(kv)) => kv.get(k).and_then(|v| v.as_i64()),
                    Some(PyRep::Dict(entries)) => entries
                        .iter()
                        .find(|(ek, _)| ek.as_str() == Some(k))
                        .and_then(|(_, ev)| ev.as_i64()),
                    _ => None,
                }
            };
            let is_info = matches!(args.first(), Some(PyRep::KeyVal(_)) | Some(PyRep::Dict(_)));
            let (planet_id_opt, res_type_opt) = if is_info {
                (info_field("planetID").or(bound_planet), info_field("resourceTypeID"))
            } else if let Some(bp) = bound_planet {
                (Some(bp), args.first().and_then(|a| a.as_i64()))
            } else {
                (args.first().and_then(|a| a.as_i64()), args.get(1).and_then(|a| a.as_i64()))
            };
            let requested_band = info_field("newBand");
            let requested_proximity = info_field("proximity");
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetResourceData requires planetID or bound planetMgr moniker".into(),
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
            let (col_depletion, gm_depletion) = ctx.db.with_snapshot(|snap| {
                let mut colony_extracted_qty = 0_u64;
                for col in snap.planetary_colonies.values() {
                    if col.planet_id == planet_id {
                        colony_extracted_qty += col
                            .extracted_commodities
                            .get(&req_res_tid)
                            .copied()
                            .unwrap_or(0);
                    }
                }
                let c_dep = ((colony_extracted_qty as f32) / 100_000.0).min(0.40);
                let depl_state = load_planet_depletion(snap, planet_id);
                let g_dep = (depl_state
                    .resource_depletion
                    .get(&req_res_tid)
                    .copied()
                    .unwrap_or(0.0) as f32)
                    .max(depl_state.gm_depletion_rate as f32);
                (c_dep, g_dep)
            });
            let total_depletion = (col_depletion + gm_depletion).min(0.85);

            // DERIVED: server-side band cap from the pilot's Planetology / Advanced Planetology levels
            // (mirrors the client's `minBand + planetology + 2 * advancedPlanetology` rule); the client's
            // requested `newBand` is honoured up to this cap.
            let skill_cap =
                (2 + (planetology_lvl as usize) + 2 * (adv_planetology_lvl as usize)).clamp(2, 16);
            let num_bands = requested_band
                .map(|b| (b.max(1) as usize).min(skill_cap))
                .unwrap_or(skill_cap);
            // DERIVED: deterministic harmonic resource-band samples (no CCP resource map is available);
            // seeded by planet/resource/band index and scaled down by colony + GM depletion.
            let mut sh_bytes = Vec::with_capacity(num_bands * num_bands * 4);
            for b in 0..(num_bands * num_bands) {
                let base_sample = ((planet_id as u64)
                    .wrapping_mul(31)
                    .wrapping_add((req_res_tid as u64).wrapping_mul(17))
                    .wrapping_add(b as u64)
                    % 100) as f32
                    / 100.0;
                let sample = (base_sample * (1.0 - total_depletion)).max(0.01);
                sh_bytes.extend_from_slice(&sample.to_le_bytes());
            }
            // Client reads exactly `data`, `numBands`, `proximity` (clientPlanet.pyj GetResourceData).
            Some(Ok(PyRep::keyval([
                ("data", PyRep::Bytes(sh_bytes)),
                ("numBands", PyRep::Integer(num_bands as i64)),
                ("proximity", requested_proximity.map(PyRep::Integer).unwrap_or(PyRep::None)),
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

pub fn extractor_overlap_factor(heads: &[(i64, f64, f64)], head_radius: f64) -> f64 {
    if heads.len() <= 1 {
        return 1.0;
    }
    let r = head_radius.clamp(0.005, 0.25);
    let mut penalty = 0.0_f64;
    for i in 0..heads.len() {
        for j in (i + 1)..heads.len() {
            let d = ((heads[i].1 - heads[j].1).hypot(heads[i].2 - heads[j].2)).max(0.0);
            if d < 2.0 * r {
                penalty += 0.35 * (1.0 - d / (2.0 * r));
            }
        }
    }
    (1.0 - penalty).clamp(0.25, 1.0)
}

pub fn pi_schematic_recipe(
    sde: &eve_sde::SdeRegistry,
    schem_id: u32,
) -> Option<(u64, Vec<(TypeId, u64)>, TypeId, u64)> {
    let info = sde.planet_schematics.get(&schem_id)?;
    let mut inputs: Vec<(TypeId, u64)> = info
        .types
        .iter()
        .filter(|t| t.is_input)
        .map(|t| (t.type_id, t.quantity))
        .collect();
    inputs.sort_by_key(|&(tid, _)| tid);
    let out = info.types.iter().find(|t| !t.is_input)?;
    if inputs.is_empty() || info.cycle_time == 0 {
        return None;
    }
    Some((info.cycle_time, inputs, out.type_id, out.quantity))
}

pub(crate) fn restore_blueprint_flag(bp_owner_id: i64, meta: &IndustryJobCostMeta) -> eve_db::InventoryFlag {
    use eve_db::InventoryFlag;
    match meta.blueprint_original_flag {
        Some(f)
            if matches!(
                f,
                InventoryFlag::CorpSAG1
                    | InventoryFlag::CorpSAG2
                    | InventoryFlag::CorpSAG3
                    | InventoryFlag::CorpSAG4
                    | InventoryFlag::CorpSAG5
                    | InventoryFlag::CorpSAG6
                    | InventoryFlag::CorpSAG7
            ) =>
        {
            f
        }
        _ if meta.use_corp && bp_owner_id == meta.owner_id => InventoryFlag::CorpSAG1,
        Some(f) if f != InventoryFlag::Locked && f != InventoryFlag::None => f,
        _ => InventoryFlag::Hangar,
    }
}

pub(crate) fn emit_industry_job(
    ctx: &ServerContext,
    char_id: i64,
    owner_id: i64,
    job_id: i64,
    bp_iid: i64,
    status: i64,
    runs: i64,
) {
    use crate::router::NotificationTarget;
    let eff_owner = if owner_id > 0 { owner_id } else { char_id };
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnIndustryJob",
        "character",
        vec![
            PyRep::Integer(job_id),
            PyRep::Integer(eff_owner),
            PyRep::Integer(bp_iid),
            PyRep::Integer(char_id),
            PyRep::Integer(status),
            PyRep::Integer(runs),
        ],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnBlueprintsUpdated",
        "ownerID",
        vec![PyRep::Integer(eff_owner)],
    );
}

pub(crate) fn handle_industry_job_queries(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("industryManager", "GetJobCounts") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let counts = ctx.db.with_mut_snapshot(|snap| {
                sync_industry_job_pause_states(snap, now_unix);
                let char_skills = snap.skills.get(&char_id);
                let skill_lvl = |sid: TypeId| -> u8 {
                    char_skills.and_then(|m| m.get(&sid)).map(|s| s.level).unwrap_or(0)
                };
                let mfg_max = 1 + (skill_lvl(3387) as i64) + (skill_lvl(24625) as i64);
                let sci_max = 1 + (skill_lvl(3406) as i64) + (skill_lvl(24624) as i64);
                let react_max = 1 + (skill_lvl(45748) as i64) + (skill_lvl(45749) as i64);
                let mfg_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| j.installer_id == char_id && matches!(j.status.as_str(), "Active" | "Paused") && j.activity_id == 1)
                    .count() as i64;
                let sci_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| {
                        j.installer_id == char_id
                            && matches!(j.status.as_str(), "Active" | "Paused")
                            && matches!(j.activity_id, 3 | 4 | 5 | 8)
                    })
                    .count() as i64;
                let react_used = snap
                    .industry_jobs
                    .values()
                    .filter(|j| j.installer_id == char_id && matches!(j.status.as_str(), "Active" | "Paused") && j.activity_id == 9)
                    .count() as i64;
                PyRep::keyval([
                    ("manufacturingUsed", PyRep::Integer(mfg_used)),
                    ("manufacturingMax", PyRep::Integer(mfg_max)),
                    ("researchUsed", PyRep::Integer(sci_used)),
                    ("researchMax", PyRep::Integer(sci_max)),
                    ("reactionsUsed", PyRep::Integer(react_used)),
                    ("reactionsMax", PyRep::Integer(react_max)),
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
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                sync_industry_job_pause_states(snap, now_unix);
                let Some(j) = snap.industry_jobs.get(&job_id) else {
                    return Err(BridgeRpcError {
                        code: "JobNotFound".into(),
                        message: format!("Industry job {job_id} not found"),
                    });
                };
                let meta = load_job_cost_meta(snap, j.job_id).unwrap_or_default();
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if j.installer_id != char_id && !(meta.use_corp && meta.owner_id == char_corp) {
                    return Err(BridgeRpcError {
                        code: "JobOwnershipViolation".into(),
                        message: format!("Industry job {job_id} does not belong to character {char_id}"),
                    });
                }
                Ok(format_industry_job_pyrep(j, &meta))
            });
            Some(res)
        }

        ("industryManager", "GetJobsByOwner") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let req_owner_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("ownerID").and_then(|v| v.as_i64()))
                .unwrap_or(char_id);
            let caller_corp_id = session
                .corporation_id
                .or_else(|| {
                    ctx.db
                        .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                })
                .unwrap_or(0);
            if req_owner_id != char_id && req_owner_id != caller_corp_id && !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "OwnerAccessDenied".into(),
                    message: format!(
                        "Character {char_id} is not authorized to query industry jobs for owner {req_owner_id}"
                    ),
                }));
            }
            let now_unix = ctx.now_unix();
            let jobs = ctx.db.with_mut_snapshot(|snap| {
                sync_industry_job_pause_states(snap, now_unix);
                snap.industry_jobs
                    .values()
                    .filter(|j| {
                        let meta = load_job_cost_meta(snap, j.job_id).unwrap_or_default();
                        if req_owner_id == char_id {
                            j.installer_id == char_id
                        } else {
                            meta.owner_id == req_owner_id || j.installer_id == req_owner_id
                        }
                    })
                    .map(|j| {
                        let meta = load_job_cost_meta(snap, j.job_id).unwrap_or_default();
                        format_industry_job_pyrep(j, &meta)
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(jobs)))
        }

        ("industryMonitor", "ConnectJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(j) = snap.industry_jobs.get(&job_id).cloned() else {
                    return Err(BridgeRpcError { code: "JobNotFound".into(), message: format!("Industry job {job_id} not found") });
                };
                let meta = load_job_cost_meta(snap, job_id).unwrap_or_default();
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if j.installer_id != char_id {
                    let corp = snap.corporations.get(&char_corp);
                    let caller_roles = corp.and_then(|c| c.member_roles.get(&char_id).copied()).unwrap_or(session.corp_role);
                    let has_role = corp.is_some_and(|c| c.ceo_id == char_id)
                        || (caller_roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_FACTORY_MANAGER) != 0);
                    let is_corp_job = meta.use_corp && meta.owner_id == char_corp && char_corp >= 2_000_000;
                    if !is_corp_job || !has_role {
                        return Err(BridgeRpcError { code: "CrpAccessDenied".into(), message: format!("Industry job {job_id} does not belong to character {char_id} or caller lacks Director/Factory Manager role") });
                    }
                }
                let nid = -8_950_000_000_000_i64 - char_id;
                let mut set: std::collections::BTreeSet<i64> = snap.owner_notes.get(&nid).and_then(|n| serde_json::from_str(&n.note).ok()).unwrap_or_default();
                set.insert(job_id);
                snap.owner_notes.insert(nid, eve_db::OwnerNoteRecord {
                    note_id: nid, character_id: char_id, item_id: job_id, label: "industry_monitor".into(),
                    note: serde_json::to_string(&set).unwrap_or_default(), timestamp_unix: 0,
                });
                Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(j.job_id)),
                    ("installerID", PyRep::Integer(j.installer_id)),
                    ("status", PyRep::String(j.status)),
                    ("connected", PyRep::Bool(true)),
                    ("subscriberCharID", PyRep::Integer(char_id)),
                ]))
            });
            Some(res)
        }

        ("industryMonitor", "DisconnectJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(j) = snap.industry_jobs.get(&job_id) else {
                    return Err(BridgeRpcError { code: "JobNotFound".into(), message: format!("Industry job {job_id} not found") });
                };
                let meta = load_job_cost_meta(snap, job_id).unwrap_or_default();
                let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                if j.installer_id != char_id {
                    let corp = snap.corporations.get(&char_corp);
                    let caller_roles = corp.and_then(|c| c.member_roles.get(&char_id).copied()).unwrap_or(session.corp_role);
                    let has_role = corp.is_some_and(|c| c.ceo_id == char_id)
                        || (caller_roles & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_FACTORY_MANAGER) != 0);
                    let is_corp_job = meta.use_corp && meta.owner_id == char_corp && char_corp >= 2_000_000;
                    if !is_corp_job || !has_role {
                        return Err(BridgeRpcError { code: "CrpAccessDenied".into(), message: format!("Industry job {job_id} does not belong to character {char_id} or caller lacks Director/Factory Manager role") });
                    }
                }
                let nid = -8_950_000_000_000_i64 - char_id;
                let mut set: std::collections::BTreeSet<i64> = snap.owner_notes.get(&nid).and_then(|n| serde_json::from_str(&n.note).ok()).unwrap_or_default();
                let was_subscribed = set.remove(&job_id);
                snap.owner_notes.insert(nid, eve_db::OwnerNoteRecord {
                    note_id: nid, character_id: char_id, item_id: job_id, label: "industry_monitor".into(),
                    note: serde_json::to_string(&set).unwrap_or_default(), timestamp_unix: 0,
                });
                Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(j.job_id)),
                    ("connected", PyRep::Bool(false)),
                    ("unsubscribed", PyRep::Bool(was_subscribed)),
                    ("subscriberCharID", PyRep::Integer(char_id)),
                ]))
            });
            Some(res)
        }

        _ => None,
    }
}

/// Authoritative V24.01 FW I-Hub tier facility tax/broker-fee multiplier from
/// `eve/common/script/util/facwarCommon.pyj` (`BENEFITS_BY_LEVEL` `BENEFIT_MARKETREDUCTION` /
/// `BENEFIT_INDUSTRYCOST` = `[0, 10, 20, 30, 40, 50]`%, `GetAdjustedFeePercentage = fee * (1 - 0.1 * level)`).
pub fn compute_facility_tax_bonus_for_tier(base_fee: f64, tier: u8) -> f64 {
    let discount = (f64::from(tier) * 0.10).min(0.50);
    base_fee * (1.0 - discount)
}
