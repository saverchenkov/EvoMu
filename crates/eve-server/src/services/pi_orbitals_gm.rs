//! `pi_orbitals_gm` — Game Master debugging and administration RPCs for Planetary Industry (`E-6`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
pub use crate::services::industry_pi::pi_helpers::*;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub fn handle_pi_orbitals_gm(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let bound_planet = bound_entry.and_then(|b| (b.service == "planetMgr").then_some(b.object_id));

    match (service, method) {
        ("planetMgr", "GMAddCommodity") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMAddCommodity requires GM role".into(),
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMAddCommodity requires planetID".into(),
                }));
            };
            let (pin_id, type_id, qty) = if args.len() >= 4 {
                (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId,
                    args.get(3).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (
                    0,
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId,
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            };
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                    *col.extracted_commodities.entry(type_id).or_insert(0) += qty.max(0) as u64;
                    let cc_lvl = col.command_center_level;
                    let mut net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                    let target = if pin_id > 0 {
                        net.pins.iter_mut().find(|p| p.pin_id == pin_id)
                    } else {
                        net.pins.first_mut()
                    };
                    if let Some(p) = target {
                        *p.contents.entry(type_id).or_insert(0) += qty.max(0) as u64;
                        save_pi_network(snap, char_id, planet_id, &net);
                    }
                }
                let loc_id = if pin_id > 0 { pin_id } else { planet_id };
                let existing = snap.items.values_mut().find(|it| it.owner_id == char_id && it.location_id == loc_id && it.type_id == type_id);
                if let Some(it) = existing {
                    it.quantity = it.quantity.saturating_add(qty as i32);
                    it.version += 1;
                } else {
                    snap.next_entity_id += 1;
                    let iid = snap.next_entity_id;
                    snap.items.insert(iid, InventoryItemRecord {
                        item_id: iid,
                        type_id,
                        owner_id: char_id,
                        location_id: loc_id,
                        flag: InventoryFlag::None,
                        quantity: qty as i32,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    });
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("planetMgr", "GMConvertCommandCenter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMConvertCommandCenter requires GM role".into(),
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMConvertCommandCenter requires planetID".into(),
                }));
            };
            let (target_type_id, new_level) = if args.len() >= 3 {
                (
                    args.get(1).and_then(|a| a.as_i64()).map(|v| v as TypeId),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(5) as u8,
                )
            } else {
                (
                    None,
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(5) as u8,
                )
            };
            let ok = ctx.db.with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) else {
                    return false;
                };
                col.command_center_level = new_level;
                let mut net = load_pi_network(snap, char_id, planet_id, new_level);
                let (cpu, pg) = cc_level_cpu_pg(new_level);
                net.cpu_max = cpu;
                net.pg_max = pg;
                if let Some(ttid) = target_type_id {
                    if let Some(cc_pin) = net.pins.iter_mut().find(|p| is_command_center_pin(&ctx.sde, p.type_id)) {
                        cc_pin.type_id = ttid;
                    }
                }
                save_pi_network(snap, char_id, planet_id, &net);
                true
            });
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GMForceInstallProgram") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMForceInstallProgram requires GM role".into(),
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMForceInstallProgram requires planetID".into(),
                }));
            };
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res_tid = args.get(2).and_then(|a| a.as_i64()).unwrap_or(2268) as TypeId;
            let heads_cnt = args.get(3).and_then(|a| a.as_i64()).unwrap_or(3) as u8;
            let now_unix = ctx.now_unix();
            let ok = ctx.db.with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return false;
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let idx = net.pins.iter().position(|p| p.pin_id == pin_id)
                    .or_else(|| net.pins.iter().position(|p| p.type_id == 2848 || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false)))
                    .or_else(|| if !net.pins.is_empty() { Some(0) } else { None });
                if let Some(i) = idx {
                    let pin = &mut net.pins[i];
                    pin.extractor_resource_type_id = Some(res_tid);
                    pin.extractor_head_count = heads_cnt;
                    pin.extractor_cycle_seconds = 300;
                    pin.extractor_head_radius = 0.02;
                    pin.program_install_unix = now_unix;
                    save_pi_network(snap, char_id, planet_id, &net);
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GMVerifySimulation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMVerifySimulation requires GM role".into(),
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMVerifySimulation requires planetID".into(),
                }));
            };
            let (cpu_ok, pg_ok, links_ok) = ctx.db.with_snapshot(|snap| {
                let cc_lvl = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let c_ok = net.cpu_used <= net.cpu_max;
                let p_ok = net.pg_used <= net.pg_max;
                let l_ok = net.links.iter().all(|l| net.pins.iter().any(|p| p.pin_id == l.source_pin_id) && net.pins.iter().any(|p| p.pin_id == l.dest_pin_id));
                (c_ok, p_ok, l_ok)
            });
            let valid = cpu_ok && pg_ok && links_ok;
            Some(Ok(PyRep::keyval([
                ("valid", PyRep::Bool(valid)),
                ("cpuOk", PyRep::Bool(cpu_ok)),
                ("powergridOk", PyRep::Bool(pg_ok)),
                ("linksOk", PyRep::Bool(links_ok)),
            ])))
        }

        ("planetMgr", "GMGetLocalDistributionReport") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetLocalDistributionReport requires GM role".into(),
                }));
            }
            let (planet_id_opt, theta_rep, phi_rep) = match bound_planet {
                Some(bp) => (Some(bp), args.first(), args.get(1)),
                None => (args.first().and_then(|a| a.as_i64()), args.get(1), args.get(2)),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMGetLocalDistributionReport requires planetID".into(),
                }));
            };
            let (theta, phi) = parse_theta_phi(theta_rep, phi_rep);
            let (pins_count, report_dict) = ctx.db.with_snapshot(|snap| {
                let p_cnt = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.pins_count as i64).unwrap_or(0);
                let sys_id = ctx.sde.solar_systems.values().find(|s| s.planet_ids.contains(&planet_id)).map(|s| s.solar_system_id).unwrap_or(30000142);
                let p0_res = crate::services::industry_pi::list_sde_planet_p0_resources(ctx, sys_id, planet_id);
                let mut dist: Vec<(PyRep, PyRep)> = p0_res.into_iter().map(|tid| {
                    let richness = compute_planet_resource_density(ctx, snap, planet_id, tid, theta, phi);
                    (PyRep::Integer(tid as i64), PyRep::Real(richness))
                }).collect();
                dist.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
                (p_cnt, PyRep::Dict(dist))
            });
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("characterID", PyRep::Integer(char_id)),
                ("pinsCount", PyRep::Integer(pins_count)),
                ("report", report_dict),
            ])))
        }

        ("planetMgr", "GMGetSynchedServerState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetSynchedServerState requires GM role".into(),
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMGetSynchedServerState requires planetID".into(),
                }));
            };
            let rep = ctx.db.with_snapshot(|snap| {
                let count = snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count() as i64;
                let col = snap.planetary_colonies.get(&(char_id, planet_id));
                let cc_lvl = col.map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let (pins_rep, links_rep, routes_rep) = format_pi_pins_links(&net);
                PyRep::keyval([
                    ("colonyCount", PyRep::Integer(count)),
                    ("planetID", PyRep::Integer(planet_id)),
                    ("lastTick", PyRep::Integer(col.map(|c| c.last_extracted_unix as i64).unwrap_or(0))),
                    ("pins", PyRep::List(pins_rep)),
                    ("links", PyRep::List(links_rep)),
                    ("routes", PyRep::List(routes_rep)),
                ])
            });
            Some(Ok(rep))
        }

        ("planetMgr", "GMRunDepletionSim") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMRunDepletionSim requires GM role".into(),
                }));
            }
            let (planet_id_opt, duration_opt) = match bound_planet {
                Some(bp) => (Some(bp), args.first()),
                None => (args.first().and_then(|a| a.as_i64()), args.get(1)),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMRunDepletionSim requires planetID".into(),
                }));
            };
            let duration_hours = duration_opt.and_then(|a| a.as_i64()).unwrap_or(24);
            let rep = ctx.db.with_mut_snapshot(|snap| {
                let count = snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count() as i64;
                let col = snap.planetary_colonies.get(&(char_id, planet_id));
                let cc_lvl = col.map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let (base_rate, curve) = simulate_planet_depletion(snap, planet_id, char_id, &net, duration_hours);
                let pins_history: Vec<PyRep> = net
                    .pins
                    .iter()
                    .filter(|p| p.extractor_head_count > 0 || p.type_id == 2848)
                    .map(|p| {
                        PyRep::keyval([
                            ("pinID", PyRep::Integer(p.pin_id)),
                            ("typeID", PyRep::Integer(p.type_id as i64)),
                            ("headCount", PyRep::Integer(p.extractor_head_count as i64)),
                            ("cycleTimeSeconds", PyRep::Integer(p.extractor_cycle_seconds as i64)),
                            ("resourceTypeID", PyRep::Integer(p.extractor_resource_type_id.unwrap_or(2268) as i64)),
                            ("installedUnix", PyRep::Integer(p.program_install_unix as i64)),
                        ])
                    })
                    .collect();
                PyRep::keyval([
                    ("colonyCount", PyRep::Integer(count)),
                    ("planetID", PyRep::Integer(planet_id)),
                    ("durationHours", PyRep::Integer(duration_hours)),
                    ("totalDuration", PyRep::Integer(duration_hours * 3600)),
                    ("depletionRate", PyRep::Real(base_rate)),
                    ("depletionCurve", PyRep::List(curve.into_iter().map(PyRep::Real).collect())),
                    ("pins", PyRep::List(pins_history)),
                ])
            });
            Some(Ok(rep))
        }

        _ => None,
    }
}
