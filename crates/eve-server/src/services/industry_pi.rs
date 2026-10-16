//! `industry_pi` — Domain 8: Manufacturing, ME/TE Research, Invention, Reprocessing, Compression, & PI (`§3.7`).

#![forbid(unsafe_code)]

use crate::formulas::{
    compute_invention_probability, compute_manufacturing_material_qty,
    compute_manufacturing_time_seconds, compute_pi_link_cost, compute_poco_export_tax,
    compute_reprocessing_yield_rate,
};
use crate::router::ServerContext;
use eve_db::{IndustryJobRecord, InventoryFlag, InventoryItemRecord, PlanetaryColonyRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

const ATTR_BLUEPRINT_ME: u16 = 1112;
const ATTR_BLUEPRINT_TE: u16 = 1113;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("reprocessingSvc", "Reprocess") | ("reprocessingSvc", "SetSessionCheck") => {
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
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_012);
            let yield_rate = compute_reprocessing_yield_rate(0.50, 5, 5, 5, 0.0);

            let res = ctx.db.with_mut_snapshot(|snap| {
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
                let ore_qty = item.quantity;
                snap.items.remove(&item_id);

                let out_trit = ((ore_qty as f64) * 4.0 * yield_rate).round().max(1.0) as i32;
                snap.next_entity_id += 1;
                let tid = snap.next_entity_id;
                snap.items.insert(
                    tid,
                    InventoryItemRecord {
                        item_id: tid,
                        type_id: 34, // Tritanium
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: out_trit,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                    },
                );
                Ok((tid, out_trit))
            });

            match res {
                Ok((trit_id, trit_qty)) => Some(Ok(PyRep::keyval([
                    ("yieldRate", PyRep::Real(yield_rate)),
                    ("tritaniumItemID", PyRep::Integer(trit_id)),
                    ("tritaniumQuantity", PyRep::Integer(trit_qty as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureCompressionMgr", "CompressItem")
        | ("structureCompressionMgr", "CompressItemInStructure")
        | ("inSpaceCompressionMgr", "CompressItemInSpace") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_012);
            let res = ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.owner_id == char_id {
                        if let Some(&comp_type) = ctx.sde.compressible_types.get(&item.type_id) {
                            item.type_id = comp_type;
                            item.version += 1;
                            return Some((item.item_id, item.type_id, item.quantity));
                        }
                    }
                }
                None
            });
            if let Some((iid, ctype, qty)) = res {
                Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(iid)),
                    ("compressedTypeID", PyRep::Integer(ctype as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                ])))
            } else {
                Some(Err(BridgeRpcError {
                    code: "CompressionFailed".into(),
                    message: "Item cannot be compressed".into(),
                }))
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
            let bp_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(687) as TypeId;
            let activity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let runs_raw = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1);

            if !(1..=1_000).contains(&runs_raw) || !matches!(activity_id, 1 | 3 | 4 | 5 | 8) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Runs must be in 1..=1000 and activityID in {1, 3, 4, 5, 8}".into(),
                }));
            }
            let runs = runs_raw as u32;

            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                }));
            };
            let product_type_id = bp.product_type_id;
            let base_time = bp.manufacturing_time_sec;
            let inv_prob = compute_invention_probability(0.30, 5, 5, 5, 1.0);

            let res = ctx.db.with_mut_snapshot(|snap| {
                // Verify character owns an unlocked blueprint item at `station_id`
                let bp_item = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == station_id
                            && i.type_id == bp_type_id
                            && i.quantity >= 1
                            && i.flag != InventoryFlag::Locked
                    })
                    .cloned();
                let Some(bp_rec) = bp_item else {
                    return Err(BridgeRpcError {
                        code: "BlueprintNotAvailable".into(),
                        message: format!(
                            "Character {char_id} does not have an unlocked blueprint {bp_type_id} at station {station_id}"
                        ),
                    });
                };
                let bp_iid = bp_rec.item_id;

                // Read ME/TE from the blueprint item (defaulting to 10/20 for seeded BPO)
                let me_level = bp_rec
                    .dynamic_attributes
                    .get(&ATTR_BLUEPRINT_ME)
                    .map(|v| (*v as i64).clamp(0, 10) as u8)
                    .unwrap_or(10);
                let te_level = bp_rec
                    .dynamic_attributes
                    .get(&ATTR_BLUEPRINT_TE)
                    .map(|v| (*v as i64).clamp(0, 20) as u8)
                    .unwrap_or(20);

                let duration_sec =
                    compute_manufacturing_time_seconds(runs, base_time, te_level, 5, 5, 1.0);
                let trit_req =
                    compute_manufacturing_material_qty(runs, 32_000, me_level, 1.0, 0.0);

                // For manufacturing (activity_id == 1), verify and consume BOM materials at `station_id`
                if activity_id == 1 {
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
                }

                // Lock blueprint while job is active
                if let Some(bp_mut) = snap.items.get_mut(&bp_iid) {
                    bp_mut.flag = InventoryFlag::Locked;
                    bp_mut.version += 1;
                }

                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;
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
                    },
                );
                Ok((jid, duration_sec, trit_req))
            });

            match res {
                Ok((job_id, duration_sec, trit_req)) => Some(Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("productTypeID", PyRep::Integer(product_type_id as i64)),
                    ("durationSeconds", PyRep::Integer(duration_sec as i64)),
                    ("tritaniumRequired", PyRep::Integer(trit_req as i64)),
                    ("inventionProbability", PyRep::Real(inv_prob)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("industryManager", "CompleteJob")
        | ("industryManager", "CompleteManyJobs")
        | ("ramProxy", "CompleteJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);

            let res = ctx.db.with_mut_snapshot(|snap| {
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
                if let Some(j_mut) = snap.industry_jobs.get_mut(&job_id) {
                    j_mut.status = "Delivered".into();
                }

                // Unlock blueprint item
                if let Some(bp_mut) = snap.items.get_mut(&job.blueprint_item_id) {
                    bp_mut.flag = InventoryFlag::Hangar;
                    if job.activity_id == 3 {
                        let cur_te = bp_mut
                            .dynamic_attributes
                            .get(&ATTR_BLUEPRINT_TE)
                            .copied()
                            .unwrap_or(0.0);
                        bp_mut
                            .dynamic_attributes
                            .insert(ATTR_BLUEPRINT_TE, (cur_te + 2.0).min(20.0));
                    } else if job.activity_id == 4 {
                        let cur_me = bp_mut
                            .dynamic_attributes
                            .get(&ATTR_BLUEPRINT_ME)
                            .copied()
                            .unwrap_or(0.0);
                        bp_mut
                            .dynamic_attributes
                            .insert(ATTR_BLUEPRINT_ME, (cur_me + 1.0).min(10.0));
                    }
                    bp_mut.version += 1;
                }

                // Only deliver manufactured/invented output for activity_id 1 or 8
                if job.activity_id == 1 || job.activity_id == 8 {
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
                        },
                    );
                    Ok((prod_item_id, job.product_type_id))
                } else {
                    Ok((job.blueprint_item_id, job.blueprint_type_id))
                }
            });

            match res {
                Ok((prod_item_id, prod_type)) => Some(Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("deliveredItemID", PyRep::Integer(prod_item_id)),
                    ("productTypeID", PyRep::Integer(prod_type as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        // Planetary Interaction (`planetMgr` / `planetOrbitalRegistryBroker`, `E2E-13`)
        ("planetMgr", "CreateColony")
        | ("planetMgr", "Flush")
        | ("planetMgr", "GMAddCommodity")
        | ("planetOrbitalRegistryBroker", "GetTaxRate")
        | ("planetOrbitalRegistryBroker", "GetSettingsInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let sys_id = session.solar_system_id.unwrap_or(30_000_142);
            let (link_pg, link_cpu) = compute_pi_link_cost(150.0, 1);
            let export_tax = compute_poco_export_tax(50_000.0, 0.10, 4, 0.05);

            ctx.db.with_mut_snapshot(|snap| {
                let col = snap
                    .planetary_colonies
                    .entry((char_id, planet_id))
                    .or_insert(PlanetaryColonyRecord {
                        planet_id,
                        owner_id: char_id,
                        solar_system_id: sys_id,
                        command_center_level: 4,
                        pins_count: 5,
                        extracted_commodities: HashMap::new(),
                    });
                *col.extracted_commodities.entry(3645).or_insert(0) += 200; // Water (P1)
            });

            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("producedTypeID", PyRep::Integer(3645)),
                ("producedQuantity", PyRep::Integer(200)),
                ("linkPowergrid", PyRep::Real(link_pg)),
                ("linkCPU", PyRep::Real(link_cpu)),
                ("pocoExportTaxISK", PyRep::Real(export_tax)),
            ])))
        }

        ("industryManager", "GetJobsByOwner")
        | ("industryManager", "GetJobCounts")
        | ("facilityManager", "GetFacilities")
        | ("blueprintManager", "GetBlueprintDataByOwner")
        | ("characterMiningLedger", "GetCharacterLogs")
        | ("moonExtractions", "GetExtractionsForCorp") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.industry_jobs
                    .values()
                    .filter(|j| j.installer_id == char_id)
                    .map(|j| {
                        vec![
                            PyRep::Integer(j.job_id),
                            PyRep::Integer(j.blueprint_type_id as i64),
                            PyRep::Integer(j.product_type_id as i64),
                            PyRep::Integer(j.runs as i64),
                            PyRep::String(j.status.clone()),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["jobID", "blueprintTypeID", "productTypeID", "runs", "status"],
                rows,
            )))
        }

        _ => None,
    }
}
