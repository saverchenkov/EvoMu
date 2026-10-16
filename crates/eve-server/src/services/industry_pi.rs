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

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("reprocessingSvc", "Reprocess") | ("reprocessingSvc", "GetOptionsForItemTypes") => {
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
                if item.owner_id != char_id || item.quantity <= 0 {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!("Item {item_id} is not owned by character {char_id}"),
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

        ("compressionMgr", "CompressItem") | ("compressionMgr", "CompressInSpace") => {
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

        ("industrySvc", "InstallJob") | ("ramProxy", "InstallJob") => {
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
            let me_raw = args.get(3).and_then(|a| a.as_i64()).unwrap_or(10);
            let te_raw = args.get(4).and_then(|a| a.as_i64()).unwrap_or(20);

            if runs_raw <= 0 || !(0..=10).contains(&me_raw) || !(0..=20).contains(&te_raw) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidIndustryJobParameters".into(),
                    message: "Runs must be > 0, ME in 0..=10, and TE in 0..=20".into(),
                }));
            }
            let runs = runs_raw as u32;
            let me_level = me_raw as u8;
            let te_level = te_raw as u8;

            let Some(bp) = ctx.sde.blueprints.get(&bp_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownBlueprint".into(),
                    message: format!("Blueprint type {bp_type_id} not found in SDE"),
                }));
            };
            let product_type_id = bp.product_type_id;
            let base_time = bp.manufacturing_time_sec;
            let duration_sec =
                compute_manufacturing_time_seconds(runs, base_time, te_level, 5, 5, 1.0);
            let trit_req = compute_manufacturing_material_qty(runs, 32_000, me_level, 1.0, 0.0);
            let inv_prob = compute_invention_probability(0.30, 5, 5, 5, 1.0);

            let res = ctx.db.with_mut_snapshot(|snap| {
                // Verify character owns the blueprint item
                let bp_item_id = snap
                    .items
                    .values()
                    .find(|i| i.owner_id == char_id && i.type_id == bp_type_id && i.quantity >= 1)
                    .map(|i| i.item_id);
                let Some(bp_iid) = bp_item_id else {
                    return Err(BridgeRpcError {
                        code: "BlueprintNotOwned".into(),
                        message: format!(
                            "Character {char_id} does not own blueprint type {bp_type_id}"
                        ),
                    });
                };

                // For manufacturing (activity_id == 1), verify and consume BOM materials
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
                                    && i.type_id == mat.type_id
                                    && i.quantity >= req_qty
                            })
                            .map(|i| i.item_id);
                        let Some(mid) = mat_item_id else {
                            return Err(BridgeRpcError {
                                code: "InsufficientManufacturingMaterials".into(),
                                message: format!(
                                    "Missing {req_qty}x material type {} for blueprint {bp_type_id}",
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
                Ok(jid)
            });

            match res {
                Ok(job_id) => Some(Ok(PyRep::keyval([
                    ("jobID", PyRep::Integer(job_id)),
                    ("productTypeID", PyRep::Integer(product_type_id as i64)),
                    ("durationSeconds", PyRep::Integer(duration_sec as i64)),
                    ("tritaniumRequired", PyRep::Integer(trit_req as i64)),
                    ("inventionProbability", PyRep::Real(inv_prob)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("industrySvc", "CompleteJob")
        | ("industrySvc", "DeliverJob")
        | ("ramProxy", "CompleteJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let station_id = session
                .station_id
                .or(session.structure_id)
                .unwrap_or(60_003_760);

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

                snap.next_entity_id += 1;
                let prod_item_id = snap.next_entity_id;
                snap.items.insert(
                    prod_item_id,
                    InventoryItemRecord {
                        item_id: prod_item_id,
                        type_id: job.product_type_id,
                        owner_id: char_id,
                        location_id: station_id,
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

        // Planetary Interaction (`planetMgr` / `planetBaseMgr` / `colony`, `E2E-13`)
        ("planetMgr", "CreateColony")
        | ("planetMgr", "GetPlanetsForChar")
        | ("planetMgr", "RunColonyCycle")
        | ("planetBaseMgr", "ExportCommodities") => {
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

        ("facilitySvc", "GetFacilities")
        | ("blueprintSvc", "GetBlueprintTypeCopy")
        | ("miningLedgerSvc", "GetCharacterLogs")
        | ("moonMining", "GetAutoMoonMiningBeaconData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            Some(Ok(PyRep::List(vec![])))
        }

        _ => None,
    }
}
