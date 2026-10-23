//! `pi_colonies` — Command centers, pins, links, routes, extractor programs, schematics, & offline simulation (`E-6`).

// parity: plan:E-6
// parity: feat:pi.command-center, feat:pi.colony-limit, feat:pi.place-pins
// parity: feat:pi.links, feat:pi.routes, feat:pi.extractor-program
// parity: feat:pi.factory-cycles, feat:pi.storage-overflow, feat:pi.colony-simulation
// parity: feat:pi.pi-skills, feat:pi.pi-notifications

#![forbid(unsafe_code)]

use crate::formulas::compute_pi_link_cost;
use crate::router::ServerContext;
use crate::services::industry_pi::{ATTR_PLANET_RESTRICTION, resolve_sde_planet_p0_extraction};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, PlanetaryColonyRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub use crate::services::industry_pi::pi_helpers::*;

pub fn handle_pi_colonies(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("planetMgr", "GetColony") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetColony requires an explicit integer planetID".into(),
                }));
            };
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(mut col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No planetary colony found on planet {planet_id}"),
                    });
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let baseline = if col.last_extracted_unix > 0 { col.last_extracted_unix } else { net.last_simulated_unix };
                let elapsed = if baseline > 0 && now_unix > baseline {
                    now_unix.saturating_sub(baseline)
                } else {
                    0
                };
                let (sim_extracted, sim_processed) = if elapsed > 0 {
                    let r = advance_colony_simulation(ctx, &mut col, &mut net, elapsed, now_unix);
                    snap.planetary_colonies.insert((char_id, planet_id), col.clone());
                    save_pi_network(snap, char_id, planet_id, &net);
                    r
                } else {
                    (0, 0)
                };
                let (pins_rep, links_rep, routes_rep) = format_pi_pins_links(&net);
                let stored_entries: Vec<(PyRep, PyRep)> = col
                    .extracted_commodities
                    .iter()
                    .map(|(&tid, &q)| (PyRep::Integer(tid as i64), PyRep::Integer(q as i64)))
                    .collect();
                Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)), ("ownerID", PyRep::Integer(char_id)),
                    ("solarSystemID", PyRep::Integer(col.solar_system_id as i64)),
                    ("commandCenterLevel", PyRep::Integer(col.command_center_level as i64)),
                    ("pinsCount", PyRep::Integer(col.pins_count as i64)),
                    ("cpuUsed", PyRep::Real(net.cpu_used)), ("cpuMax", PyRep::Real(net.cpu_max)),
                    ("powergridUsed", PyRep::Real(net.pg_used)), ("powergridMax", PyRep::Real(net.pg_max)),
                    ("simulatedExtractedQuantity", PyRep::Integer(sim_extracted as i64)),
                    ("simulatedProcessedQuantity", PyRep::Integer(sim_processed as i64)),
                    ("storedCommodities", PyRep::Dict(stored_entries)),
                    ("pins", PyRep::List(pins_rep)), ("links", PyRep::List(links_rep)), ("routes", PyRep::List(routes_rep)),
                ]))
            });
            Some(res)
        }

        ("planetMgr", "GetFullNetworkForOwner") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id_opt = args.first().and_then(|a| a.as_i64());
            let res = ctx.db.with_snapshot(|snap| {
                let mut all_pins = Vec::new();
                let mut all_links = Vec::new();
                let mut all_routes = Vec::new();
                let mut rep_pid = 0i64;
                let mut rep_cc_lvl = 0u8;
                for (&(cid, pid), col) in &snap.planetary_colonies {
                    if cid != char_id {
                        continue;
                    }
                    if let Some(target) = planet_id_opt {
                        if pid != target {
                            continue;
                        }
                    }
                    rep_pid = pid;
                    rep_cc_lvl = rep_cc_lvl.max(col.command_center_level);
                    let net = load_pi_network(snap, char_id, pid, col.command_center_level);
                    let (p_rep, l_rep, r_rep) = format_pi_pins_links(&net);
                    all_pins.extend(p_rep);
                    all_links.extend(l_rep);
                    all_routes.extend(r_rep);
                }
                if let Some(target) = planet_id_opt {
                    rep_pid = target;
                }
                Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(rep_pid)),
                    ("ownerID", PyRep::Integer(char_id)),
                    ("commandCenterLevel", PyRep::Integer(rep_cc_lvl as i64)),
                    ("pins", PyRep::List(all_pins)),
                    ("links", PyRep::List(all_links)),
                    ("routes", PyRep::List(all_routes)),
                ]))
            });
            Some(res)
        }

        ("planetMgr", "GetCommandPinsForPlanet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetCommandPinsForPlanet requires an explicit integer planetID".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Ok(PyRep::List(Vec::new()));
                };
                let net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let cmd_pins: Vec<PyRep> = net
                    .pins
                    .iter()
                    .filter(|p| {
                        is_command_center_type(ctx, p.type_id)
                            || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1027 || t.group_id == 1026).unwrap_or(false)
                    })
                    .map(|p| {
                        PyRep::keyval([
                            ("pinID", PyRep::Integer(p.pin_id)),
                            ("typeID", PyRep::Integer(p.type_id as i64)),
                            ("latitude", PyRep::Real(p.latitude)),
                            ("longitude", PyRep::Real(p.longitude)),
                            ("cpuUsage", PyRep::Real(p.cpu_usage)),
                            ("powergridUsage", PyRep::Real(p.pg_usage)),
                            ("level", PyRep::Integer(col.command_center_level as i64)),
                        ])
                    })
                    .collect();
                Ok(PyRep::List(cmd_pins))
            });
            Some(res)
        }

        ("planetMgr", "GetExtractorsForPlanet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetExtractorsForPlanet requires an explicit integer planetID".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Ok(PyRep::List(Vec::new()));
                };
                let net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let ext_pins: Vec<PyRep> = net
                    .pins
                    .iter()
                    .filter(|p| {
                        p.type_id == 2848
                            || p.extractor_head_count > 0
                            || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false)
                    })
                    .map(|p| {
                        PyRep::keyval([
                            ("pinID", PyRep::Integer(p.pin_id)),
                            ("typeID", PyRep::Integer(p.type_id as i64)),
                            ("latitude", PyRep::Real(p.latitude)),
                            ("longitude", PyRep::Real(p.longitude)),
                            ("cpuUsage", PyRep::Real(p.cpu_usage)),
                            ("powergridUsage", PyRep::Real(p.pg_usage)),
                            ("headCount", PyRep::Integer(p.extractor_head_count as i64)),
                            ("cycleTimeSeconds", PyRep::Integer(p.extractor_cycle_seconds as i64)),
                        ])
                    })
                    .collect();
                Ok(PyRep::List(ext_pins))
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
                let cycle_secs = if pin.extractor_cycle_seconds > 0 {
                    pin.extractor_cycle_seconds as i64
                } else if (65..=135).contains(&schem_id) {
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
                    ("headCount", PyRep::Integer(pin.extractor_head_count as i64)),
                    ("resourceTypeID", PyRep::Integer(pin.extractor_resource_type_id.unwrap_or(0) as i64)),
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
                snap.planetary_colony_layouts.remove(&(char_id, planet_id));
                Ok(PyRep::Bool(true))
            });
            // No notification: the V24 client calls `UserAbandonPlanet` from
            // `ClientPlanet._SubmitChanges` and itself scatters
            // `OnPlanetCommandCenterDeployedOrRemoved` via `planetSvc`.
            Some(res)
        }

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
                    message: "UserUpdateNetwork only accepts (planetID, [commandStream])".into(),
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
                    message: format!("Planet ID {planet_id} is outside 40000000..50000000"),
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
                    message: format!("Planet {planet_id} is not in solar system {sys_id}"),
                }));
            }
            let _ = match resolve_sde_planet_p0_extraction(ctx, sys_id, planet_id, None) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_restriction_type_id = ctx
                .sde
                .celestials_in_system(sys_id)
                .into_iter()
                .find(|c| c.celestial_id == planet_id && c.group_id == 7)
                .and_then(|cel| {
                    ctx.sde
                        .types
                        .get(&cel.type_id)
                        .map(|pt| pt.variation_parent_type_id.unwrap_or(pt.type_id))
                        .or(Some(cel.type_id))
                })
                .unwrap_or(0);
            let Some(commands) = cmd_list_opt.filter(|c| !c.is_empty()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandStream".into(),
                    message: "UserUpdateNetwork requires a non-empty commandStream".into(),
                }));
            };

            let active_ship_id = session.ship_id;
            let is_docked = session.station_id.is_some() || session.structure_id.is_some();
            let now_unix = ctx.now_unix();

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
                                    1 => vec![PyRep::Integer(1), kv.get("pinID").cloned().unwrap_or(PyRep::Integer(0)), kv.get("typeID").cloned().unwrap_or(PyRep::Integer(0)), kv.get("latitude").cloned().unwrap_or(PyRep::Real(0.0)), kv.get("longitude").cloned().unwrap_or(PyRep::Real(0.0))],
                                    3 | 5 => vec![PyRep::Integer(cid), kv.get("endpoint1").or_else(|| kv.get("sourcePinID")).cloned().unwrap_or(PyRep::Integer(0)), kv.get("endpoint2").or_else(|| kv.get("destPinID")).cloned().unwrap_or(PyRep::Integer(0)), kv.get("level").cloned().unwrap_or(PyRep::Integer(0))],
                                    9 => vec![PyRep::Integer(9), kv.get("pinID").cloned().unwrap_or(PyRep::Integer(1)), kv.get("level").or_else(|| kv.get("targetLevel")).cloned().unwrap_or(PyRep::Integer(1))],
                                    10 => vec![PyRep::Integer(10), kv.get("pinID").cloned().unwrap_or(PyRep::Integer(0)), kv.get("resourceTypeID").cloned().unwrap_or(PyRep::Integer(0)), kv.get("cycleTimeSeconds").cloned().unwrap_or(PyRep::Integer(300)), kv.get("headCount").cloned().unwrap_or(PyRep::Integer(1))],
                                    _ => vec![PyRep::Integer(cid)],
                                }
                            }
                            _ => {
                                return Err(BridgeRpcError {
                                    code: "InvalidCommandStream".into(),
                                    message: "Each command must be a tuple, list, or KeyVal".into(),
                                });
                            }
                        };
                        let cmd_id = els.first().and_then(|v| v.as_i64()).unwrap_or(0);

                        match cmd_id {
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
                                if !lat.is_finite() || !lon.is_finite() || !(-half_pi..=half_pi).contains(&lat) || !(-full_pi..=full_pi).contains(&lon) {
                                    return Err(BridgeRpcError {
                                        code: "InvalidPinCoordinates".into(),
                                        message: "Pin coordinates must be finite radians".into(),
                                    });
                                }

                                if is_command_center_type(ctx, pin_type_id) {
                                    if is_docked {
                                        return Err(BridgeRpcError {
                                            code: "CannotPlaceCommandCenterWhileDocked".into(),
                                            message: "Cannot deploy a Command Center while docked".into(),
                                        });
                                    }
                                    let Some(ship_id) = active_ship_id else {
                                        return Err(BridgeRpcError {
                                            code: "CommandCenterItemRequired".into(),
                                            message: "Deploying a Command Center requires an active ship".into(),
                                        });
                                    };
                                    if snap.planetary_colonies.contains_key(&(char_id, planet_id)) {
                                        return Err(BridgeRpcError {
                                            code: "ColonyAlreadyExists".into(),
                                            message: format!("Colony already exists on planet {planet_id}"),
                                        });
                                    }
                                    let char_colony_count = snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count();
                                    let ipc_level = snap.skills.get(&char_id).and_then(|m| m.get(&2495)).map(|s| s.level.clamp(0, 5)).unwrap_or(0);
                                    if char_colony_count >= 1 + (ipc_level as usize) {
                                        return Err(BridgeRpcError {
                                            code: "MaxColoniesReached".into(),
                                            message: "Maximum planetary colonies reached".into(),
                                        });
                                    }
                                    if let Some(cc_sde_type) = ctx.sde.types.get(&pin_type_id) {
                                        if let Some(&req_pt) = cc_sde_type.default_attributes.get(&ATTR_PLANET_RESTRICTION) {
                                            let req_id = req_pt as u32;
                                            if req_id > 0 && planet_restriction_type_id > 0 && req_id != planet_restriction_type_id {
                                                return Err(BridgeRpcError {
                                                    code: "IncompatibleCommandCenterPlanetType".into(),
                                                    message: format!("Command Center type {pin_type_id} requires planet type {req_id}"),
                                                });
                                            }
                                        }
                                    }
                                    if pin_id_raw > 0 && client_to_server_pin_id.contains_key(&pin_id_raw) {
                                        return Err(BridgeRpcError {
                                            code: "DuplicatePinID".into(),
                                            message: format!("Duplicate pin ID {pin_id_raw}"),
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
                                                && matches!(it.flag, InventoryFlag::Cargo | InventoryFlag::SpecializedCommandCenterHold)
                                        })
                                        .map(|it| it.item_id)
                                        .min();
                                    let Some(cc_item_id) = matching_cc_item_id else {
                                        return Err(BridgeRpcError {
                                            code: "CommandCenterItemRequired".into(),
                                            message: "Command Center item required in ship cargo".into(),
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
                                            ..Default::default()
                                        }],
                                        cpu_max,
                                        pg_max,
                                        last_simulated_unix: now_unix,
                                        ..Default::default()
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
                                            message: format!("No colony on planet {planet_id}"),
                                        });
                                    };
                                    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                    if pin_id_raw > 0 && (client_to_server_pin_id.contains_key(&pin_id_raw) || net.pins.iter().any(|p| p.pin_id == pin_id_raw)) {
                                        return Err(BridgeRpcError {
                                            code: "DuplicatePinID".into(),
                                            message: format!("Pin ID {pin_id_raw} already exists"),
                                        });
                                    }
                                    let (pin_cpu, pin_pg, pin_cost_isk) = resolve_pi_pin_stats(ctx, pin_type_id);
                                    if net.cpu_used + pin_cpu > net.cpu_max + 1e-6 || net.pg_used + pin_pg > net.pg_max + 1e-6 {
                                        return Err(BridgeRpcError {
                                            code: "InsufficientColonyCpuOrPowergrid".into(),
                                            message: "Placing pin exceeds colony CPU or Powergrid".into(),
                                        });
                                    }
                                    let cost_cents = IskCents::from_f64_isk(pin_cost_isk);
                                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                                        if ch.balance < cost_cents {
                                            return Err(BridgeRpcError {
                                                code: "InsufficientFunds".into(),
                                                message: "Insufficient ISK to construct planetary pin".into(),
                                            });
                                        }
                                        ch.balance = ch.balance.checked_sub(cost_cents).unwrap_or(IskCents(0));
                                        let bal_after = ch.balance;
                                        snap.append_journal(
                                            char_id,
                                            "PlanetPinConstruction",
                                            cost_cents,
                                            bal_after,
                                            format!("Constructed planetary pin type {pin_type_id}"),
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
                                        cpu_usage: pin_cpu,
                                        pg_usage: pin_pg,
                                        ..Default::default()
                                    });
                                    if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                                        col_mut.pins_count = net.pins.len() as u32;
                                    }
                                    save_pi_network(snap, char_id, planet_id, &net);
                                }
                            }

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
                                        message: format!("Pin {pin_id} not found"),
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

                            3 => {
                                let src_raw = extract_pi_pin_id(els.get(1));
                                let dst_raw = extract_pi_pin_id(els.get(2));
                                let src_id = client_to_server_pin_id.get(&src_raw).copied().unwrap_or(src_raw);
                                let dst_id = client_to_server_pin_id.get(&dst_raw).copied().unwrap_or(dst_raw);
                                let level_raw = els.get(3).and_then(|v| v.as_i64()).unwrap_or(0);
                                if src_id <= 0 || dst_id <= 0 || src_id == dst_id || !(0..=10).contains(&level_raw) {
                                    return Err(BridgeRpcError {
                                        code: "InvalidLinkParameters".into(),
                                        message: "CreateLink requires distinct positive pinIDs and level 0..=10".into(),
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
                                        message: format!("Source pin {src_id} not found"),
                                    });
                                };
                                let Some(p2) = net.pins.iter().find(|p| p.pin_id == dst_id).cloned() else {
                                    return Err(BridgeRpcError {
                                        code: "PinNotFound".into(),
                                        message: format!("Destination pin {dst_id} not found"),
                                    });
                                };
                                if net.links.iter().any(|l| {
                                    (l.source_pin_id == src_id && l.dest_pin_id == dst_id)
                                        || (l.source_pin_id == dst_id && l.dest_pin_id == src_id)
                                }) {
                                    return Err(BridgeRpcError {
                                        code: "LinkAlreadyExists".into(),
                                        message: "Link already exists".into(),
                                    });
                                }
                                let cos_dsigma = (p1.latitude.sin() * p2.latitude.sin()
                                    + p1.latitude.cos() * p2.latitude.cos() * (p1.longitude - p2.longitude).cos())
                                .clamp(-1.0, 1.0);
                                let length_km = (5_000.0 * cos_dsigma.acos()).max(1.0);
                                let (link_pg, link_cpu) = compute_pi_link_cost(length_km, level);
                                if net.cpu_used + link_cpu > net.cpu_max + 1e-6 || net.pg_used + link_pg > net.pg_max + 1e-6 {
                                    return Err(BridgeRpcError {
                                        code: "InsufficientColonyCpuOrPowergrid".into(),
                                        message: "Link exceeds colony CPU or Powergrid".into(),
                                    });
                                }
                                let link_cost_isk = (2_500.0 + 10.0 * length_km) * 2.0_f64.powi(level as i32);
                                let link_cost_cents = IskCents::from_f64_isk(link_cost_isk);
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    if ch.balance < link_cost_cents {
                                        return Err(BridgeRpcError {
                                            code: "InsufficientFunds".into(),
                                            message: "Insufficient ISK to build planetary link".into(),
                                        });
                                    }
                                    ch.balance = ch.balance.checked_sub(link_cost_cents).unwrap_or(IskCents(0));
                                }
                                total_isk_spent += link_cost_isk;
                                net.cpu_used += link_cpu;
                                net.pg_used += link_pg;
                                net.links.push(PiLinkState {
                                    source_pin_id: src_id,
                                    dest_pin_id: dst_id,
                                    level,
                                    length_km,
                                    cpu_usage: link_cpu,
                                    pg_usage: link_pg,
                                    bandwidth: 1_250.0 * 2.0_f64.powi(level as i32),
                                });
                                save_pi_network(snap, char_id, planet_id, &net);
                            }

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
                                        message: "Link not found".into(),
                                    });
                                };
                                let removed = net.links.remove(idx);
                                net.cpu_used = (net.cpu_used - removed.cpu_usage).max(0.0);
                                net.pg_used = (net.pg_used - removed.pg_usage).max(0.0);
                                save_pi_network(snap, char_id, planet_id, &net);
                            }

                            5 => {
                                let src_raw = extract_pi_pin_id(els.get(1));
                                let dst_raw = extract_pi_pin_id(els.get(2));
                                let src_id = client_to_server_pin_id.get(&src_raw).copied().unwrap_or(src_raw);
                                let dst_id = client_to_server_pin_id.get(&dst_raw).copied().unwrap_or(dst_raw);
                                let new_lvl_raw = els.get(3).and_then(|v| v.as_i64()).unwrap_or(0);
                                if !(1..=10).contains(&new_lvl_raw) {
                                    return Err(BridgeRpcError {
                                        code: "InvalidLinkLevel".into(),
                                        message: "Link upgrade level must be in 1..=10".into(),
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
                                        message: "Link not found".into(),
                                    });
                                };
                                let old_link = net.links[idx].clone();
                                if new_level <= old_link.level {
                                    return Err(BridgeRpcError {
                                        code: "InvalidLinkLevel".into(),
                                        message: "New link level must exceed current level".into(),
                                    });
                                }
                                let (new_pg, new_cpu) = compute_pi_link_cost(old_link.length_km, new_level);
                                let delta_cpu = (new_cpu - old_link.cpu_usage).max(0.0);
                                let delta_pg = (new_pg - old_link.pg_usage).max(0.0);
                                if net.cpu_used + delta_cpu > net.cpu_max + 1e-6 || net.pg_used + delta_pg > net.pg_max + 1e-6 {
                                    return Err(BridgeRpcError {
                                        code: "InsufficientColonyCpuOrPowergrid".into(),
                                        message: "Upgrading link exceeds colony CPU or Powergrid".into(),
                                    });
                                }
                                let upgrade_cost_isk = (2_500.0 + 10.0 * old_link.length_km) * 2.0_f64.powi(new_level as i32);
                                let upgrade_cents = IskCents::from_f64_isk(upgrade_cost_isk);
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    if ch.balance < upgrade_cents {
                                        return Err(BridgeRpcError {
                                            code: "InsufficientFunds".into(),
                                            message: "Insufficient ISK to upgrade link".into(),
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
                                let unit_vol = ctx.sde.types.get(&ctype).map(|t| t.volume.max(0.01)).unwrap_or(0.38);
                                let route_bw = (qty as f64) * unit_vol;
                                for win in path_ids.windows(2) {
                                    let (u, v) = (win[0], win[1]);
                                    if let Some(link) = net.links.iter().find(|l| {
                                        (l.source_pin_id == u && l.dest_pin_id == v)
                                            || (l.source_pin_id == v && l.dest_pin_id == u)
                                    }) {
                                        if route_bw > link.bandwidth + 1e-6 {
                                            return Err(BridgeRpcError {
                                                code: "InsufficientLinkBandwidth".into(),
                                                message: "Route exceeds link bandwidth".into(),
                                            });
                                        }
                                    }
                                }
                                net.routes.push(PiRouteState {
                                    route_id,
                                    path: path_ids,
                                    commodity_type_id: ctype,
                                    quantity: qty,
                                });
                                save_pi_network(snap, char_id, planet_id, &net);
                            }

                            7 if els.len() == 2 => {
                                let route_id = els.get(1).and_then(|v| v.as_i64()).unwrap_or(0);
                                if let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() {
                                    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                    net.routes.retain(|r| r.route_id != route_id);
                                    save_pi_network(snap, char_id, planet_id, &net);
                                }
                            }

                            7 | 8 => {
                                let pin_id_raw = extract_pi_pin_id(els.get(1));
                                let pin_id = client_to_server_pin_id.get(&pin_id_raw).copied().unwrap_or(pin_id_raw);
                                let schem_raw = els.get(2).and_then(|v| v.as_i64()).unwrap_or(0);
                                if !(1..=200).contains(&schem_raw) {
                                    return Err(BridgeRpcError {
                                        code: "InvalidSchematic".into(),
                                        message: format!("Invalid planetary schematic ID {schem_raw}"),
                                    });
                                }
                                if let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() {
                                    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                    if let Some(p) = net.pins.iter_mut().find(|p| p.pin_id == pin_id) {
                                        let is_factory = ctx.sde.types.get(&p.type_id).is_some_and(|t| t.group_id == 1028);
                                        if !is_factory {
                                            return Err(BridgeRpcError {
                                                code: "InvalidSchematic".into(),
                                                message: format!("Pin {pin_id} (type {}) is not a processor facility", p.type_id),
                                            });
                                        }
                                        p.schematic_id = Some(schem_raw as u32);
                                    }
                                    save_pi_network(snap, char_id, planet_id, &net);
                                }
                            }

                            9 => {
                                let target_lvl_raw = if els.len() >= 3 {
                                    els.get(2).and_then(|v| v.as_i64()).unwrap_or(0)
                                } else {
                                    els.get(1).and_then(|v| v.as_i64()).unwrap_or(0)
                                };
                                if !(1..=5).contains(&target_lvl_raw) {
                                    return Err(BridgeRpcError {
                                        code: "InvalidCommandCenterLevel".into(),
                                        message: "Target Command Center level must be in 1..=5".into(),
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
                                        message: "Target level must exceed current level".into(),
                                    });
                                }
                                let cc_skill_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&2505)).map(|s| s.level).unwrap_or(0);
                                if cc_skill_lvl < target_level {
                                    return Err(BridgeRpcError {
                                        code: "MissingSkillForCommandCenterUpgrade".into(),
                                        message: "Command Center Upgrades skill (2505) insufficient".into(),
                                    });
                                }
                                let upgrade_cost_isk = cc_upgrade_cost_isk(col.command_center_level, target_level);
                                let upgrade_cost_cents = IskCents::from_f64_isk(upgrade_cost_isk);
                                if let Some(ch) = snap.characters.get_mut(&char_id) {
                                    if ch.balance < upgrade_cost_cents {
                                        return Err(BridgeRpcError {
                                            code: "InsufficientFunds".into(),
                                            message: "Insufficient funds for command center upgrade".into(),
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

                            10 => {
                                let pin_id_raw = extract_pi_pin_id(els.get(1));
                                let pin_id = client_to_server_pin_id.get(&pin_id_raw).copied().unwrap_or(pin_id_raw);
                                let res_tid = els.get(2).and_then(|v| v.as_i64()).unwrap_or(0) as TypeId;
                                if res_tid > 0 {
                                    let _ = resolve_sde_planet_p0_extraction(ctx, sys_id, planet_id, Some(res_tid))?;
                                }
                                let head_radius = els.get(3).and_then(|v| v.as_f64()).filter(|&r| r > 0.0 && r < 1.0).unwrap_or(0.02);
                                let cyc_secs = els.get(3).and_then(|v| v.as_i64()).unwrap_or(300).max(60) as u64;
                                let parsed_heads: Option<Vec<(i64, f64, f64)>> = els.get(5).or_else(|| els.get(4)).and_then(|rep| match rep {
                                    PyRep::List(list) | PyRep::Tuple(list) => Some(
                                        list.iter()
                                            .enumerate()
                                            .filter_map(|(idx, h)| match h {
                                                PyRep::Tuple(t) | PyRep::List(t) if t.len() >= 3 => Some((
                                                    t[0].as_i64().unwrap_or(idx as i64),
                                                    t[1].as_f64().unwrap_or(0.0),
                                                    t[2].as_f64().unwrap_or(0.0),
                                                )),
                                                PyRep::Tuple(t) | PyRep::List(t) if t.len() == 2 => Some((
                                                    idx as i64,
                                                    t[0].as_f64().unwrap_or(0.0),
                                                    t[1].as_f64().unwrap_or(0.0),
                                                )),
                                                _ => None,
                                            })
                                            .collect(),
                                    ),
                                    _ => None,
                                });
                                let head_cnt_raw = parsed_heads
                                    .as_ref()
                                    .map(|h| h.len() as i64)
                                    .or_else(|| els.get(4).and_then(|v| v.as_i64()))
                                    .unwrap_or(1);
                                if !(1..=10).contains(&head_cnt_raw) {
                                    return Err(BridgeRpcError {
                                        code: "InvalidExtractorHeadCount".into(),
                                        message: "Extractor head count must be in 1..=10".into(),
                                    });
                                }
                                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                    return Err(BridgeRpcError {
                                        code: "ColonyNotFound".into(),
                                        message: format!("No colony on planet {planet_id}"),
                                    });
                                };
                                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == pin_id) else {
                                    return Err(BridgeRpcError {
                                        code: "PinNotFound".into(),
                                        message: format!("Extractor pin {pin_id} not found"),
                                    });
                                };
                                if let Some(heads) = parsed_heads {
                                    for &(_, hlat, hlon) in &heads {
                                        if (hlat - pin.latitude).hypot(hlon - pin.longitude) > 0.50 {
                                            return Err(BridgeRpcError {
                                                code: "ExtractorHeadOutOfRange".into(),
                                                message: "Extractor head exceeds maximum extraction radius from ECU".into(),
                                            });
                                        }
                                    }
                                    pin.extractor_heads = heads;
                                }
                                pin.extractor_head_radius = head_radius;
                                pin.extractor_resource_type_id = Some(if res_tid > 0 { res_tid } else { 2268 });
                                pin.extractor_cycle_seconds = cyc_secs;
                                pin.extractor_head_count = head_cnt_raw as u8;
                                pin.program_install_unix = now_unix;
                                if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                                    if col_mut.last_extracted_unix == 0 {
                                        col_mut.last_extracted_unix = now_unix;
                                    }
                                }
                                save_pi_network(snap, char_id, planet_id, &net);
                            }

                            14 | 15 => {
                                let pin_id_raw = extract_pi_pin_id(els.get(1));
                                let pin_id = client_to_server_pin_id.get(&pin_id_raw).copied().unwrap_or(pin_id_raw);
                                let head_id = els.get(2).and_then(|v| v.as_i64()).unwrap_or(0);
                                let hlat = els.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let hlon = els.get(4).and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                                    return Err(BridgeRpcError {
                                        code: "ColonyNotFound".into(),
                                        message: format!("No colony on planet {planet_id}"),
                                    });
                                };
                                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                                let Some(pin) = net.pins.iter_mut().find(|p| p.pin_id == pin_id) else {
                                    return Err(BridgeRpcError {
                                        code: "PinNotFound".into(),
                                        message: format!("Extractor pin {pin_id} not found"),
                                    });
                                };
                                if (hlat - pin.latitude).hypot(hlon - pin.longitude) > 0.50 {
                                    return Err(BridgeRpcError {
                                        code: "ExtractorHeadOutOfRange".into(),
                                        message: "Extractor head exceeds maximum extraction radius from ECU".into(),
                                    });
                                }
                                if let Some(existing) = pin.extractor_heads.iter_mut().find(|h| h.0 == head_id) {
                                    existing.1 = hlat;
                                    existing.2 = hlon;
                                } else {
                                    if pin.extractor_heads.len() >= 10 {
                                        return Err(BridgeRpcError {
                                            code: "InvalidExtractorHeadCount".into(),
                                            message: "Extractor head count must be in 1..=10".into(),
                                        });
                                    }
                                    pin.extractor_heads.push((head_id, hlat, hlon));
                                }
                                pin.extractor_head_count = pin.extractor_heads.len() as u8;
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
                            Some(col) => { snap.planetary_colonies.insert((char_id, planet_id), col); }
                            None => { snap.planetary_colonies.remove(&(char_id, planet_id)); }
                        }
                        match prev_note {
                            Some(n) => { snap.owner_notes.insert(net_nid, n); }
                            None => { snap.owner_notes.remove(&net_nid); }
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
                let (pins_rep, links_rep, _) = format_pi_pins_links(&net);
                Ok((
                    net.pins.len(),
                    PyRep::keyval([
                        ("planetID", PyRep::Integer(planet_id)), ("currentLevel", PyRep::Integer(col.command_center_level as i64)),
                        ("commandCenterLevel", PyRep::Integer(col.command_center_level as i64)), ("pinsCount", PyRep::Integer(net.pins.len() as i64)),
                        ("linksCount", PyRep::Integer(net.links.len() as i64)), ("cpuUsed", PyRep::Real(net.cpu_used)),
                        ("cpuMax", PyRep::Real(net.cpu_max)), ("cpuCapacity", PyRep::Integer(net.cpu_max.round() as i64)),
                        ("powerUsed", PyRep::Real(net.pg_used)), ("powergridUsed", PyRep::Real(net.pg_used)),
                        ("powergridMax", PyRep::Real(net.pg_max)), ("powerCapacity", PyRep::Integer(net.pg_max.round() as i64)),
                        ("totalCostISK", PyRep::Real(total_isk_spent)), ("pins", PyRep::List(pins_rep)), ("links", PyRep::List(links_rep)),
                    ]),
                ))
            });

            match res {
                // No notification: after this RPC returns, the V24 client's
                // `ClientPlanet._SubmitChanges` scatters `OnPlanetChangesSubmitted`, and
                // `planetSvc.OnPlanetChangesSubmitted` derives `OnColonyPinCountUpdated`.
                Ok((_pin_cnt, rep)) => Some(Ok(rep)),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
