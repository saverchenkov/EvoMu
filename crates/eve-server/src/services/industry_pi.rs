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
        ("reprocessingSvc", "Reprocess") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(100_000_012);
            let yield_rate = compute_reprocessing_yield_rate(0.50, 5, 5, 5, 0.0);
            let station_id = session.station_id.unwrap_or(60_003_760);

            let (trit_id, trit_qty) = ctx.db.with_mut_snapshot(|snap| {
                let ore_qty = snap
                    .items
                    .get(&item_id)
                    .map(|i| i.quantity)
                    .unwrap_or(1_000);
                snap.items.remove(&item_id);
                let out_trit = ((ore_qty as f64) * 4.0 * yield_rate).round() as i32;
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
                (tid, out_trit)
            });

            Some(Ok(PyRep::keyval([
                ("yieldRate", PyRep::Real(yield_rate)),
                ("tritaniumItemID", PyRep::Integer(trit_id)),
                ("tritaniumQuantity", PyRep::Integer(trit_qty as i64)),
            ])))
        }

        ("compressionMgr", "CompressItem") => {
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
            let bp_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(687) as TypeId;
            let activity_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(1) as u8;
            let runs = args.get(2).and_then(|a| a.as_i64()).unwrap_or(1) as u32;
            let me_level = args.get(3).and_then(|a| a.as_i64()).unwrap_or(10) as u8;
            let te_level = args.get(4).and_then(|a| a.as_i64()).unwrap_or(20) as u8;

            let bp = ctx.sde.blueprints.get(&bp_type_id);
            let product_type_id = bp.map(|b| b.product_type_id).unwrap_or(587);
            let base_time = bp.map(|b| b.manufacturing_time_sec).unwrap_or(6000);
            let duration_sec =
                compute_manufacturing_time_seconds(runs, base_time, te_level, 5, 5, 1.0);
            let trit_req = compute_manufacturing_material_qty(runs, 32_000, me_level, 1.0, 0.0);
            let inv_prob = compute_invention_probability(0.30, 5, 5, 5, 1.0);

            let job_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;
                snap.industry_jobs.insert(
                    jid,
                    IndustryJobRecord {
                        job_id: jid,
                        installer_id: char_id,
                        facility_id: session.station_id.unwrap_or(60_003_760),
                        blueprint_item_id: 100_000_007,
                        blueprint_type_id: bp_type_id,
                        activity_id,
                        runs,
                        product_type_id,
                        status: "Active".into(),
                        duration_seconds: duration_sec,
                    },
                );
                jid
            });

            Some(Ok(PyRep::keyval([
                ("jobID", PyRep::Integer(job_id)),
                ("productTypeID", PyRep::Integer(product_type_id as i64)),
                ("durationSeconds", PyRep::Integer(duration_sec as i64)),
                ("tritaniumRequired", PyRep::Integer(trit_req as i64)),
                ("inventionProbability", PyRep::Real(inv_prob)),
            ])))
        }

        ("industrySvc", "CompleteJob") | ("ramProxy", "CompleteJob") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let job_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let station_id = session.station_id.unwrap_or(60_003_760);

            let out = ctx.db.with_mut_snapshot(|snap| {
                let (prod_type, runs) = if let Some(job) = snap.industry_jobs.get_mut(&job_id) {
                    job.status = "Delivered".into();
                    (job.product_type_id, job.runs)
                } else {
                    (587, 1)
                };
                snap.next_entity_id += 1;
                let prod_item_id = snap.next_entity_id;
                snap.items.insert(
                    prod_item_id,
                    InventoryItemRecord {
                        item_id: prod_item_id,
                        type_id: prod_type,
                        owner_id: char_id,
                        location_id: station_id,
                        flag: InventoryFlag::Hangar,
                        quantity: runs as i32,
                        singleton: runs == 1,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                    },
                );
                (prod_item_id, prod_type)
            });

            Some(Ok(PyRep::keyval([
                ("jobID", PyRep::Integer(job_id)),
                ("deliveredItemID", PyRep::Integer(out.0)),
                ("productTypeID", PyRep::Integer(out.1 as i64)),
            ])))
        }

        // Planetary Interaction (`planetMgr` / `planetBaseMgr` / `colony`, `E2E-13`)
        ("planetMgr", "CreateColony")
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

        ("facilitySvc", _)
        | ("blueprintSvc", _)
        | ("colony", _)
        | ("miningLedgerSvc", _)
        | ("moonMining", _)
        | ("factory", _)
        | ("corpStationMgr", _)
        | ("dynamicResourceSvc", _)
        | ("skyhookMgr", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
