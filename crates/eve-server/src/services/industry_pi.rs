//! `industry_pi` — Domain 8: Manufacturing, ME/TE Research, Invention, Reprocessing, Compression, & PI (`§3.7`).

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

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
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

                let yield_rate = compute_reprocessing_yield_rate(
                    0.50,
                    reproc_lvl,
                    reproc_eff_lvl,
                    ore_proc_lvl,
                    0.0,
                );
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
                    if item.owner_id == char_id
                        && reachable
                        && item.flag != InventoryFlag::Locked
                        && item.quantity >= 100
                    {
                        if let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) {
                            let orig_type = item.type_id;
                            let orig_loc = item.location_id;
                            let orig_flag = item.flag;
                            let rem = item.quantity % 100;
                            item.type_id = comp_type;
                            item.quantity /= 100;
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

        ("industryManager", "InstallJob") | ("ramProxy", "InstallJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Installing an industry job requires being docked".into(),
                }));
            };
            let Some(raw_bp_type_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "InstallJob requires an explicit blueprint typeID".into(),
                }));
            };
            let raw_activity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1);
            let runs_raw = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);
            let (Ok(bp_type_id), Ok(activity_id)) = (
                TypeId::try_from(raw_bp_type_id),
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
                let sci1_lvl = skill_lvl(11448); // Minmatar Starship Engineering
                let sci2_lvl = skill_lvl(11452); // Mechanical Engineering
                let enc_lvl = skill_lvl(21791); // Minmatar Encryption Methods

                // Match an unlocked blueprint item at `station_id` respecting BPO vs BPC rules:
                // - Activity 3 (TE), 4 (ME), 5 (Copying): require a BPO (`!i.is_blueprint_copy`)
                // - Activity 8 (Invention): require a BPC (`i.is_blueprint_copy && runs_remaining >= 1`)
                // - Activity 1 (Manufacturing): allow either a BPO or a BPC with `runs_remaining >= runs`
                let matching_bps: Vec<InventoryItemRecord> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == station_id
                            && i.type_id == bp_type_id
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
                            "Character {char_id} does not have an unlocked blueprint {bp_type_id} at station {station_id}"
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
                                    "No BPO or BPC with at least {runs} remaining runs available at station {station_id}"
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

                let duration_sec = match activity_id {
                    1 => compute_manufacturing_time_seconds(
                        runs,
                        bp.manufacturing_time_sec,
                        te_level,
                        industry_lvl,
                        adv_industry_lvl,
                        1.0,
                    ),
                    3 => (bp.research_te_time_sec.max(1) as u64).saturating_mul(runs as u64),
                    4 => (bp.research_me_time_sec.max(1) as u64).saturating_mul(runs as u64),
                    5 => (bp.copy_time_sec.max(1) as u64).saturating_mul(runs as u64),
                    8 => (bp.invention_time_sec.max(1) as u64).saturating_mul(runs as u64),
                    _ => 600,
                };
                let trit_req =
                    compute_manufacturing_material_qty(runs, 32_000, me_level, 1.0, 0.0);
                let inv_prob = compute_invention_probability(
                    bp.invention_base_probability,
                    sci1_lvl,
                    sci2_lvl,
                    enc_lvl,
                    1.0,
                );

                // For manufacturing (activity_id == 1), verify and consume BOM materials at `station_id`
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
                            1.0,
                            0.0,
                        ) as i32;
                        let mat_item_id = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == char_id
                                    && i.location_id == station_id
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
                                    "Missing {req_qty}x material type {} in station {station_id} hangar",
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
                    // Invention requires and consumes `runs` units of BOTH Datacore types:
                    // 20424 (Datacore - Mechanical Engineering) and 20172 (Datacore - Minmatar Starship Engineering)
                    let req_dc = runs as i32;
                    for dc_type in [20424_u32, 20172_u32] {
                        let dc_item_id = snap
                            .items
                            .values()
                            .find(|i| {
                                i.owner_id == char_id
                                    && i.location_id == station_id
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
                                    "Missing {req_dc}x Datacore (type {dc_type}) in station {station_id} hangar"
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
                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;

                // Deduct facility tax (10% base tax on estimated job cost: 10,000 ISK * runs)
                let base_job_cost = 10_000.0 * (runs as f64);
                let facility_tax_isk = base_job_cost * 0.10;
                let facility_tax_cents = IskCents::from_f64_isk(facility_tax_isk);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.balance < facility_tax_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!(
                                "Insufficient funds for industry facility tax ({:.2} ISK required, {:.2} available)",
                                facility_tax_isk, ch.balance.to_wire_f64()
                            ),
                        });
                    }
                    ch.balance = ch.balance.checked_sub(facility_tax_cents).unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "IndustryFacilityTax",
                        facility_tax_cents,
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
                Ok((jid, duration_sec, trit_req, inv_prob, facility_tax_isk))
            });

            match res {
                Ok((job_id, duration_sec, trit_req, inv_prob, facility_tax_isk)) => Some(Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("productTypeID", PyRep::Integer(product_type_id as i64)),
                    ("durationSeconds", PyRep::Integer(duration_sec as i64)),
                    ("tritaniumRequired", PyRep::Integer(trit_req as i64)),
                    ("inventionProbability", PyRep::Real(inv_prob)),
                    ("facilityTaxISK", PyRep::Real(facility_tax_isk)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("industryManager", "GetJobsByOwner")
        | ("ramProxy", "GetJobsByOwner")
        | ("industryManager", "GetJobs") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let jobs = ctx.db.with_snapshot(|snap| {
                snap.industry_jobs
                    .values()
                    .filter(|j| j.installer_id == char_id)
                    .map(|j| {
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
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(jobs)))
        }

        ("industryManager", "CompleteJob") | ("ramProxy", "CompleteJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
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

                // Verify the blueprint item still exists, belongs to installer, and is Locked at job.facility_id
                let bp_valid = snap
                    .items
                    .get(&job.blueprint_item_id)
                    .map(|bp| {
                        bp.owner_id == char_id
                            && bp.location_id == job.facility_id
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
                    // Invention consumes `job.runs` from the input T1 BPC (removing the BPC only when remaining runs <= 0)
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
                    // Deliver distinct T2 BPC (Wolf Blueprint 11372, 1 run remaining, base ME = 2, TE = 4)
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

                // Unlock blueprint item for non-invention jobs (Manufacturing, ME/TE research, Copying)
                let mut parent_dyn_attrs = HashMap::new();
                let mut remove_consumed_bpc = false;
                if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
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
                            location_id: job.facility_id,
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
                    // Copying delivers a distinct BPC item with `is_blueprint_copy = true` and `blueprint_runs_remaining = Some(job.runs)`
                    snap.next_entity_id += 1;
                    let copy_item_id = snap.next_entity_id;
                    snap.items.insert(
                        copy_item_id,
                        InventoryItemRecord {
                            item_id: copy_item_id,
                            type_id: job.blueprint_type_id,
                            owner_id: char_id,
                            location_id: job.facility_id,
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

        // Planetary Interaction (`planetMgr`, `E2E-13`)
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
            let colony_opt = ctx
                .db
                .with_snapshot(|snap| snap.planetary_colonies.get(&(char_id, planet_id)).cloned());
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
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
            ])))
        }

        ("planetMgr", "GetMyColonies") => {
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

        ("planetMgr", "CreateColony") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.len() > 1 {
                return Some(Err(BridgeRpcError {
                    code: "ClientSchematicOverrideForbidden".into(),
                    message: "CreateColony only accepts (planetID) and forbids client-supplied overrides".into(),
                }));
            }
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "CreateColony requires an explicit integer planetID".into(),
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
            let (_p0_type_id, _cycle_secs, p0_qty_per_pin, p0_unit_tax_value) =
                match resolve_sde_planet_p0_extraction(ctx, sys_id, planet_id, None) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
            let (link_pg, link_cpu) = compute_pi_link_cost(150.0, 1);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if snap.planetary_colonies.contains_key(&(char_id, planet_id)) {
                    return Err(BridgeRpcError {
                        code: "ColonyAlreadyExists".into(),
                        message: format!(
                            "Character {char_id} already has a colony on planet {planet_id}"
                        ),
                    });
                }
                let char_colony_count = snap
                    .planetary_colonies
                    .keys()
                    .filter(|(cid, _)| *cid == char_id)
                    .count();
                // EVE mechanics: 1 base colony + 1 per level of Interplanetary Consolidation (skill 2495, up to 6 total)
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
                // Derive Command Center upgrade level from Command Center Upgrades (skill 2505, defaulting to level 1 base CC when untrained)
                let cc_level = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&2505))
                    .map(|s| s.level.clamp(1, 5))
                    .unwrap_or(1);
                let pins_count = (cc_level as u32) + 1;
                let produced_qty =
                    p0_qty_per_pin.saturating_mul(u64::from((pins_count - 1).max(1)));
                let tax_base = p0_unit_tax_value * (produced_qty as f64);
                let export_tax = compute_poco_export_tax(tax_base, 0.10, cc_level, 0.05);
                snap.planetary_colonies.insert(
                    (char_id, planet_id),
                    PlanetaryColonyRecord {
                        planet_id,
                        owner_id: char_id,
                        solar_system_id: sys_id,
                        command_center_level: cc_level,
                        pins_count,
                        extracted_commodities: HashMap::new(),
                        last_extracted_unix: 0,
                    },
                );
                Ok((cc_level, pins_count, export_tax))
            });

            match res {
                Ok((cc_level, pins_count, export_tax)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("commandCenterLevel", PyRep::Integer(cc_level as i64)),
                    ("pinsCount", PyRep::Integer(pins_count as i64)),
                    ("linkPowergrid", PyRep::Real(link_pg)),
                    ("linkCPU", PyRep::Real(link_cpu)),
                    ("pocoExportTaxISK", PyRep::Real(export_tax)),
                ]))),
                Err(e) => Some(Err(e)),
            }
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

        ("planetMgr", "UpgradeCommandCenter") | ("planetMgr", "UpgradeColony") => {
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
                // Verify Command Center Upgrades skill (2505)
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

                let upgrade_cost_isk = (target_level as f64) * 250_000.0;
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

                let col_mut = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony exists");
                col_mut.command_center_level = target_level;
                col_mut.pins_count = (target_level as u32) + 1;
                let new_pins = col_mut.pins_count;

                Ok((target_level, new_pins, upgrade_cost_isk))
            });

            match res {
                Ok((new_level, new_pins, cost)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("commandCenterLevel", PyRep::Integer(new_level as i64)),
                    ("pinsCount", PyRep::Integer(new_pins as i64)),
                    ("upgradeCostISK", PyRep::Real(cost)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "ProcessCommodity") | ("planetMgr", "ConvertCommodities") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "ProcessCommodity requires an explicit integer planetID".into(),
                }));
            };
            let Some(input_type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProcessorParameters".into(),
                    message: "ProcessCommodity requires input commodity typeID".into(),
                }));
            };
            let Some(output_type_id_raw) = args.get(2).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProcessorParameters".into(),
                    message: "ProcessCommodity requires output commodity typeID".into(),
                }));
            };
            let input_type_id = input_type_id_raw as TypeId;
            let output_type_id = output_type_id_raw as TypeId;
            let cycles = args.get(3).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u64;

            // Basic processor conversion ratio: 3,000 P0 -> 20 P1 per cycle
            // Advanced processor conversion ratio: 40 P1 -> 5 P2 per cycle
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

        ("planetMgr", "RouteCommodity") | ("planetMgr", "TransferCommodity") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "RouteCommodity requires planetID".into(),
                }));
            };
            let Some(type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRoutingParameters".into(),
                    message: "RouteCommodity requires commodity typeID".into(),
                }));
            };
            let type_id = type_id_raw as TypeId;
            let qty = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
            let destination = args.get(3).and_then(|a| a.as_str()).unwrap_or("Storage");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) else {
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
                if destination == "Hangar" || destination == "CustomsOffice" {
                    *col.extracted_commodities.entry(type_id).or_insert(0) -= qty;
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
                Ok(qty)
            });

            match res {
                Ok(routed) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("commodityTypeID", PyRep::Integer(type_id as i64)),
                    ("routedQuantity", PyRep::Integer(routed as i64)),
                    ("destination", PyRep::String(destination.to_string())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
