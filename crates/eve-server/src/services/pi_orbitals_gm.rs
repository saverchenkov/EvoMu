//! `pi_orbitals_gm` — Game Master debugging and administration RPCs for Planetary Industry (`E-6`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
pub use crate::services::industry_pi::pi_helpers::*;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ItemQty, ItemQtyError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

/// Resolves target planet, solar system, and P0 resource type for GM planet RPCs.
fn resolve_gm_planet_and_resource(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    session: &SessionState,
    char_id: i64,
    explicit_planet: Option<i64>,
    res_tid_arg: Option<TypeId>,
) -> Result<(i64, u32, TypeId), BridgeRpcError> {
    let invalid_planet = |message: String| BridgeRpcError {
        code: "InvalidPlanetID".into(),
        message,
        client_error: None,
    };
    let planet_id = if let Some(pid) = explicit_planet {
        let known = snap.planetary_colonies.values().any(|c| c.planet_id == pid)
            || ctx.sde.solar_systems.values().any(|s| s.planet_ids.contains(&pid));
        if !known {
            return Err(invalid_planet(format!("Planet {pid} not found")));
        }
        pid
    } else {
        snap.planetary_colonies
            .keys()
            .filter(|(cid, _)| *cid == char_id)
            .map(|(_, pid)| *pid)
            .min()
            .or_else(|| {
                let target_sys_id = session.solar_system_id.or_else(|| {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.solar_system_id)
                        .filter(|&sid| sid > 0)
                });
                target_sys_id
                    .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                    .and_then(|s| s.planet_ids.first().copied())
            })
            .or_else(|| {
                ctx.sde
                    .solar_systems
                    .values()
                    .filter_map(|s| {
                        s.planet_ids
                            .iter()
                            .copied()
                            .min()
                            .map(|pid| (s.solar_system_id, pid))
                    })
                    .min_by_key(|&(sys_id, _)| sys_id)
                    .map(|(_, pid)| pid)
            })
            .ok_or_else(|| invalid_planet("No planet found for GM planet command".into()))?
    };
    let sys_id = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.planet_ids.contains(&planet_id))
        .min_by_key(|s| s.solar_system_id)
        .map(|s| s.solar_system_id)
        .or_else(|| {
            snap.planetary_colonies
                .values()
                .filter(|c| c.planet_id == planet_id && c.solar_system_id > 0)
                .min_by_key(|c| (c.solar_system_id, c.owner_id))
                .map(|c| c.solar_system_id)
        })
        .or(session.solar_system_id)
        .or_else(|| {
            snap.characters
                .get(&char_id)
                .map(|c| c.solar_system_id)
                .filter(|&sid| sid > 0)
        })
        .ok_or_else(|| invalid_planet(format!("Solar system not found for planet {planet_id}")))?;
    let resource_type_id = match res_tid_arg {
        Some(tid) => tid,
        None => crate::services::industry_pi::list_sde_planet_p0_resources(ctx, sys_id, planet_id)
            .into_iter()
            .next()
            .ok_or_else(|| {
                BridgeRpcError::custom_info(format!(
                    "No P0 resources available on planet {planet_id}"
                ))
            })?,
    };
    Ok((planet_id, sys_id, resource_type_id))
}

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a crate::router::BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(std::future::ready(handle_pi_orbitals_gm(
        ctx,
        session,
        service,
        method,
        args,
        kwargs,
        bound_entry,
    )))
}

pub(crate) const RPCS: &[RpcEntry] = &[
    rpc_entry("planetMgr", "GMAddCommodity", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMConvertCommandCenter", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMForceInstallProgram", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMVerifySimulation", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMGetLocalDistributionReport", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMGetSynchedServerState", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMRunDepletionSim", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMCreateNuggetLayer", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("planetMgr", "GMGetCompleteResource", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];

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
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMAddCommodity requires GM role".into(),
                    client_error: None,
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMAddCommodity requires planetID".into(),
                    client_error: None,
                }));
            };
            let (pin_id, type_id_raw, qty_raw) = if args.len() >= 4 {
                (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(2).and_then(|a| a.as_i64()),
                    args.get(3).and_then(|a| a.as_i64()),
                )
            } else {
                (
                    0,
                    args.get(1).and_then(|a| a.as_i64()),
                    args.get(2).and_then(|a| a.as_i64()),
                )
            };
            let Some(type_id) = type_id_raw.and_then(|v| TypeId::try_from(v).ok()).filter(|&t| t > 0) else {
                return Some(Err(BridgeRpcError::invalid_quantity(
                    "planetMgr.GMAddCommodity requires positive commodity typeID",
                )));
            };
            let qty = match ItemQty::try_from_i64(qty_raw.unwrap_or(0)) {
                Ok(q) => q,
                Err(e) => return Some(Err(e.into_rpc_error("InvalidQuantity"))),
            };
            let stack_delta = match qty.try_into_stack_i32() {
                Ok(v) => v,
                Err(e) => return Some(Err(e.into_rpc_error("InvalidQuantity"))),
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                    let next_ext = col
                        .extracted_commodities
                        .get(&type_id)
                        .copied()
                        .unwrap_or(0)
                        .checked_add(qty.as_u64())
                        .ok_or_else(|| ItemQtyError::Overflow.into_rpc_error("InvalidQuantity"))?;
                    col.extracted_commodities.insert(type_id, next_ext);
                    let cc_lvl = col.command_center_level;
                    let mut net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                    let target = if pin_id > 0 {
                        net.pins.iter_mut().find(|p| p.pin_id == pin_id)
                    } else {
                        net.pins.first_mut()
                    };
                    if let Some(p) = target {
                        let next_content = p
                            .contents
                            .get(&type_id)
                            .copied()
                            .unwrap_or(0)
                            .checked_add(qty.as_u64())
                            .ok_or_else(|| ItemQtyError::Overflow.into_rpc_error("InvalidQuantity"))?;
                        p.contents.insert(type_id, next_content);
                        save_pi_network(snap, char_id, planet_id, &net);
                    }
                }
                let loc_id = if pin_id > 0 { pin_id } else { planet_id };
                let existing = snap.items.values_mut().find(|it| it.owner_id == char_id && it.location_id == loc_id && it.type_id == type_id);
                if let Some(it) = existing {
                    it.quantity = it
                        .quantity
                        .checked_add(stack_delta)
                        .ok_or_else(|| ItemQtyError::Overflow.into_rpc_error("InvalidQuantity"))?;
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
                        quantity: stack_delta,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    });
                }
                Ok::<(), BridgeRpcError>(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "GMConvertCommandCenter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMConvertCommandCenter requires GM role".into(),
                    client_error: None,
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMConvertCommandCenter requires planetID".into(),
                    client_error: None,
                }));
            };
            let (target_type_id, new_level) = if args.len() >= 3 {
                (
                    args.get(1)
                        .and_then(|a| a.as_i64())
                        .and_then(|v| TypeId::try_from(v).ok()),
                    u8::try_from(
                        args.get(2)
                            .and_then(|a| a.as_i64())
                            .unwrap_or(5)
                            .clamp(0, i64::from(u8::MAX)),
                    )
                    .unwrap_or(5),
                )
            } else {
                (
                    None,
                    u8::try_from(
                        args.get(1)
                            .and_then(|a| a.as_i64())
                            .unwrap_or(5)
                            .clamp(0, i64::from(u8::MAX)),
                    )
                    .unwrap_or(5),
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
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMForceInstallProgram requires GM role".into(),
                    client_error: None,
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMForceInstallProgram requires planetID".into(),
                    client_error: None,
                }));
            };
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res_tid = args
                .get(2)
                .and_then(|a| a.as_i64())
                .and_then(|v| TypeId::try_from(v).ok())
                .filter(|&v| v > 0)
                .unwrap_or(2268);
            let heads_cnt = u8::try_from(
                args.get(3)
                    .and_then(|a| a.as_i64())
                    .unwrap_or(3)
                    .clamp(0, i64::from(u8::MAX)),
            )
            .unwrap_or(3);
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
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMVerifySimulation requires GM role".into(),
                    client_error: None,
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMVerifySimulation requires planetID".into(),
                    client_error: None,
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
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetLocalDistributionReport requires GM role".into(),
                    client_error: None,
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
                    client_error: None,
                }));
            };
            let Some(sys_id) = ctx
                .sde
                .solar_systems
                .values()
                .filter(|s| s.planet_ids.contains(&planet_id))
                .min_by_key(|s| s.solar_system_id)
                .map(|s| s.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: format!("Planet {planet_id} not found in SDE"),
                    client_error: None,
                }));
            };
            let (theta, phi) = parse_theta_phi(theta_rep, phi_rep);
            let (pins_count, report_dict) = ctx.db.with_snapshot(|snap| {
                let p_cnt = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| i64::from(c.pins_count)).unwrap_or(0);
                let p0_res = crate::services::industry_pi::list_sde_planet_p0_resources(ctx, sys_id, planet_id);
                let mut dist: Vec<(PyRep, PyRep)> = p0_res.into_iter().map(|tid| {
                    let richness = compute_planet_resource_density(ctx, snap, planet_id, tid, theta, phi);
                    (PyRep::Integer(i64::from(tid)), PyRep::Real(richness))
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
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetSynchedServerState requires GM role".into(),
                    client_error: None,
                }));
            }
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetMgr.GMGetSynchedServerState requires planetID".into(),
                    client_error: None,
                }));
            };
            let rep = ctx.db.with_snapshot(|snap| {
                let count = i64::try_from(snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count()).unwrap_or(i64::MAX);
                let col = snap.planetary_colonies.get(&(char_id, planet_id));
                let cc_lvl = col.map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let (pins_rep, links_rep, routes_rep) = format_pi_pins_links(&net);
                PyRep::keyval([
                    ("colonyCount", PyRep::Integer(count)),
                    ("planetID", PyRep::Integer(planet_id)),
                    ("lastTick", PyRep::Integer(col.and_then(|c| i64::try_from(c.last_extracted_unix).ok()).unwrap_or(0))),
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
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMRunDepletionSim requires GM role".into(),
                    client_error: None,
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
                    client_error: None,
                }));
            };
            let duration_hours = duration_opt.and_then(|a| a.as_i64()).unwrap_or(24);
            let rep = ctx.db.with_mut_snapshot(|snap| {
                let count = i64::try_from(snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count()).unwrap_or(i64::MAX);
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
                            ("typeID", PyRep::Integer(i64::from(p.type_id))),
                            ("headCount", PyRep::Integer(i64::from(p.extractor_head_count))),
                            ("cycleTimeSeconds", PyRep::Integer(i64::try_from(p.extractor_cycle_seconds).unwrap_or(i64::MAX))),
                            ("resourceTypeID", PyRep::Integer(i64::from(p.extractor_resource_type_id.unwrap_or(2268)))),
                            ("installedUnix", PyRep::Integer(i64::try_from(p.program_install_unix).unwrap_or(i64::MAX))),
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

        ("planetMgr", "GMCreateNuggetLayer") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMCreateNuggetLayer requires GM role".into(),
                    client_error: None,
                }));
            }
            let (explicit_planet, arg_offset) = match bound_planet {
                Some(bp) => (Some(bp), 0usize),
                None => (args.first().and_then(|a| a.as_i64()), 1usize),
            };
            let res_tid_arg = args
                .get(arg_offset)
                .and_then(|a| a.as_i64())
                .filter(|&v| v > 0)
                .and_then(|v| TypeId::try_from(v).ok());
            let (lat, lon) = parse_theta_phi(args.get(arg_offset + 1), args.get(arg_offset + 2));
            let radius = args
                .get(arg_offset + 3)
                .and_then(|a| a.as_f64())
                .filter(|&r| r > 0.0 && r.is_finite())
                .unwrap_or(0.35);
            let boost = args
                .get(arg_offset + 4)
                .and_then(|a| a.as_f64())
                .filter(|&b| b > 0.0 && b.is_finite())
                .unwrap_or(0.25);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (planet_id, _sys_id, resource_type_id) = resolve_gm_planet_and_resource(
                    ctx,
                    snap,
                    session,
                    char_id,
                    explicit_planet,
                    res_tid_arg,
                )?;
                let mut depl = load_planet_depletion(snap, planet_id);
                depl.nugget_layers.push(PiNuggetLayer {
                    resource_type_id,
                    latitude: lat,
                    longitude: lon,
                    radius,
                    boost,
                });
                if depl.nugget_layers.len() > 64 {
                    let excess = depl.nugget_layers.len() - 64;
                    depl.nugget_layers.drain(0..excess);
                }
                save_planet_depletion(snap, &depl);
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::None)),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "GMGetCompleteResource") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !crate::router::ServerContext::has_canonical_gm_role(session) {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetCompleteResource requires GM role".into(),
                    client_error: None,
                }));
            }
            let (explicit_planet, res_arg) = match bound_planet {
                Some(bp) => (Some(bp), args.first()),
                None => (args.first().and_then(|a| a.as_i64()), args.get(1)),
            };
            let res_tid_arg = res_arg
                .and_then(|a| a.as_i64())
                .filter(|&v| v > 0)
                .and_then(|v| TypeId::try_from(v).ok());
            let res = ctx.db.with_snapshot(|snap| {
                let (planet_id, _sys_id, resource_type_id) = resolve_gm_planet_and_resource(
                    ctx,
                    snap,
                    session,
                    char_id,
                    explicit_planet,
                    res_tid_arg,
                )?;
                let data = compute_planet_complete_resource_sh_bytes(
                    ctx,
                    snap,
                    planet_id,
                    resource_type_id,
                );
                Ok((data, planet_id, resource_type_id))
            });
            match res {
                Ok((data, planet_id, resource_type_id)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("resourceTypeID", PyRep::Integer(i64::from(resource_type_id))),
                    ("data", PyRep::Bytes(data)),
                    ("numBands", PyRep::Integer(3)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}

#[cfg(test)]
#[path = "pi_orbitals_gm_tests.rs"]
mod pi_orbitals_gm_tests;

#[cfg(test)]
mod qty_tests {
    use super::*;
    use crate::router::ServerContext;
    use eve_db::{InventoryFlag, InventoryItemRecord, PlanetaryColonyRecord, WorldRepository};
    use eve_sde::SdeRegistry;
    use std::collections::HashMap;
    use std::sync::Arc;

    #[test]
    fn gm_add_commodity_rejects_invalid_qty_and_rolls_back_stack_overflow() {
        let sde = Arc::new(SdeRegistry::with_canonical_seed());
        let db = Arc::new(WorldRepository::with_demo_seed());
        let ctx = ServerContext::with_sde_and_repository(sde, db);
        let char_id = 90_000_001;
        let planet_id = 40_009_082;
        let pin_id = 501;

        ctx.db.with_mut_snapshot(|snap| {
            snap.planetary_colonies.insert(
                (char_id, planet_id),
                PlanetaryColonyRecord {
                    planet_id,
                    owner_id: char_id,
                    solar_system_id: 30_000_142,
                    command_center_level: 1,
                    pins_count: 1,
                    extracted_commodities: HashMap::new(),
                    last_extracted_unix: 1000,
                },
            );
            let net = PiColonyNetworkState {
                cpu_max: 7057.0,
                pg_max: 9000.0,
                pins: vec![PiPinState {
                    pin_id,
                    type_id: 2254,
                    ..Default::default()
                }],
                ..Default::default()
            };
            save_pi_network(snap, char_id, planet_id, &net);
            snap.items.insert(
                939_801,
                InventoryItemRecord {
                    item_id: 939_801,
                    type_id: 2268,
                    owner_id: char_id,
                    location_id: pin_id,
                    flag: InventoryFlag::None,
                    quantity: i32::MAX - 3,
                    ..Default::default()
                },
            );
        });

        let mut gm = SessionState {
            user_id: Some(1),
            character_id: Some(char_id),
            role: eve_db::service_roles::ROLE_GMH,
            ..Default::default()
        };
        let kw = HashMap::new();

        for bad_qty in [0_i64, -5, i64::from(i32::MAX) + 1] {
            let err = handle_pi_orbitals_gm(
                &ctx,
                &mut gm,
                "planetMgr",
                "GMAddCommodity",
                &[
                    PyRep::Integer(planet_id),
                    PyRep::Integer(pin_id),
                    PyRep::Integer(2268),
                    PyRep::Integer(bad_qty),
                ],
                &kw,
                None,
            )
            .unwrap()
            .unwrap_err();
            assert_eq!(err.code, "InvalidQuantity");
        }

        let wrapped_tid = i64::from(u32::MAX) + 2269;
        let tid_err = handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMAddCommodity",
            &[
                PyRep::Integer(planet_id),
                PyRep::Integer(pin_id),
                PyRep::Integer(wrapped_tid),
                PyRep::Integer(5),
            ],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(tid_err.code, "InvalidQuantity");

        let overflow_err = handle_pi_orbitals_gm(
            &ctx,
            &mut gm,
            "planetMgr",
            "GMAddCommodity",
            &[
                PyRep::Integer(planet_id),
                PyRep::Integer(pin_id),
                PyRep::Integer(2268),
                PyRep::Integer(10),
            ],
            &kw,
            None,
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(overflow_err.code, "InvalidQuantity");

        ctx.db.with_snapshot(|snap| {
            assert_eq!(snap.items.get(&939_801).map(|i| i.quantity), Some(i32::MAX - 3));
            assert!(snap.planetary_colonies[&(char_id, planet_id)]
                .extracted_commodities
                .is_empty());
        });
    }
}

