//! `station_structure` — Domain 4: Stations, Docking/Undocking, Upwell Structures, Tethering, & POS (`§3.3`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::InventoryFlag;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use std::collections::HashMap;

#[path = "structure_upwell_ops.rs"]
pub mod structure_upwell_ops;
#[path = "structure_lifecycle.rs"]
pub mod structure_lifecycle;
#[path = "structure_lifecycle_rpc.rs"]
pub mod structure_lifecycle_rpc;

/// Guest row `(characterID, corporationID, allianceID, warFactionID)` broadcast with
/// `OnCharNowInStation` / `OnCharNoLongerInStation` (same shape as `station.GetGuests`).
fn station_guest_row(ctx: &ServerContext, char_id: i64) -> PyRep {
    ctx.db.with_snapshot(|snap| {
        let c = snap.characters.get(&char_id);
        PyRep::Tuple(vec![
            PyRep::Integer(char_id),
            c.map(|c| PyRep::Integer(c.corporation_id)).unwrap_or(PyRep::None),
            c.and_then(|c| c.alliance_id).map(PyRep::Integer).unwrap_or(PyRep::None),
            c.and_then(|c| c.war_faction_id).map(PyRep::Integer).unwrap_or(PyRep::None),
        ])
    })
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // Upwell lifecycle timers are evaluated lazily before any structure RPC is served.
    if service.starts_with("structure") {
        structure_lifecycle::sync_all_structures(ctx);
    }
    match (service, method) {
        ("stationSvc", "GetAllianceSystems") | ("stationSvc", "GetSystemsForAlliance") => {
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            let systems = ctx.db.with_snapshot(|snap| {
                snap.system_states.values()
                    .filter(|s| s.sov_alliance_id == Some(aid))
                    .map(|s| PyRep::Integer(s.solar_system_id as i64))
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(systems)))
        }

        ("stationSvc", "GetStationsForOwner") => {
            let owner_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1000035);
            let stations: Vec<PyRep> = ctx.sde.stations.values()
                .filter(|st| st.owner_corporation_id == owner_id)
                .take(50)
                .map(|st| {
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(st.station_id)),
                        ("solarSystemID", PyRep::Integer(st.solar_system_id as i64)),
                        ("corporationID", PyRep::Integer(st.owner_corporation_id)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::List(stations)))
        }

        ("stationSvc", "GetSolarSystem") | ("station", "GetSolarSystem") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) if id < 60_000_000 => id as u32,
                _ => match session.solar_system_id {
                    Some(s) => s,
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "NoSolarSystem".into(),
                            message: "No solarSystemID argument or active session solar system"
                                .into(),
                        }))
                    }
                },
            };
            if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
                Some(Ok(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(sys.solar_system_id as i64)),
                    ("solarSystemName", PyRep::String(sys.name.clone())),
                    ("constellationID", PyRep::Integer(sys.constellation_id as i64)),
                    ("regionID", PyRep::Integer(sys.region_id as i64)),
                    ("security", PyRep::Real(sys.security_status)),
                ])))
            } else {
                Some(Err(BridgeRpcError {
                    code: "SolarSystemNotFound".into(),
                    message: format!("Solar system {sys_id} not found in SDE"),
                }))
            }
        }

        ("stationSvc", "GetStation") | ("station", "GetStation") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.station_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MissingStationId".into(),
                    message: "GetStation requires a stationID argument or active session stationID"
                        .into(),
                }));
            };
            let found = ctx
                .sde
                .stations
                .get(&station_id)
                .map(|st| (st.solar_system_id, st.clone()));
            if let Some((sys_id, st)) = found {
                let op = ctx.sde.station_operations.get(&st.operation_id);
                let service_mask = op
                    .map(|o| o.services.iter().fold(0i64, |acc, &sid| if sid < 64 { acc | (1i64 << sid) } else { acc }))
                    .unwrap_or(0);
                let services = op
                    .map(|o| o.services.iter().map(|&sid| PyRep::Integer(i64::from(sid))).collect())
                    .unwrap_or_default();
                Some(Ok(PyRep::keyval([
                    ("stationID", PyRep::Integer(st.station_id)),
                    ("stationName", PyRep::String(st.name)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("stationTypeID", PyRep::Integer(i64::from(st.station_type_id))),
                    ("ownerID", PyRep::Integer(st.owner_corporation_id)),
                    ("operationID", PyRep::Integer(i64::from(st.operation_id))),
                    ("serviceMask", PyRep::Integer(service_mask)),
                    ("services", PyRep::List(services)),
                    ("reprocessingEfficiency", PyRep::Real(st.reprocessing_efficiency)),
                    ("reprocessingStationsTake", PyRep::Real(st.reprocessing_stations_take)),
                ])))
            } else {
                Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {station_id} not found in SDE"),
                }))
            }
        }

        ("stationSvc", "GetStationItemBits") | ("station", "GetStationItemBits") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = args.first().and_then(|a| a.as_i64()).or(session.station_id) else {
                return Some(Err(BridgeRpcError {
                    code: "MissingStationId".into(),
                    message: "GetStationItemBits requires stationID".into(),
                }));
            };
            let Some(st) = ctx.sde.stations.get(&station_id) else {
                return Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {station_id} not found in SDE"),
                }));
            };
            let op_id = st.operation_id;
            let mask = ctx
                .sde
                .station_operations
                .get(&st.operation_id)
                .map(|o| o.services.iter().fold(0i64, |acc, &sid| if sid < 64 { acc | (1i64 << sid) } else { acc }))
                .unwrap_or(0);
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::Integer(st.owner_corporation_id),
                PyRep::Integer(st.station_id),
                PyRep::Integer(mask),
                PyRep::Integer(i64::from(op_id)),
            ])))
        }

        ("corpStationMgr", "DoStandingCheckForStationService") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let service_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0) as u32;
            let station_id = args.get(1).and_then(|a| a.as_i64()).or(session.station_id).unwrap_or(60_003_760);
            let Some(st) = ctx.sde.stations.get(&station_id) else {
                return Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {station_id} not found in SDE"),
                }));
            };
            if service_id > 0 {
                let has_service = ctx
                    .sde
                    .station_operations
                    .get(&st.operation_id)
                    .map(|op| op.services.contains(&service_id))
                    .unwrap_or(false);
                if !has_service {
                    return Some(Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Station {station_id} does not offer service {service_id}"),
                    }));
                }
            }
            let (corp_st, fac_st) = ctx.db.with_snapshot(|snap| {
                let ch = snap.characters.get(&char_id);
                let cs = ch.and_then(|c| c.standings.get(&st.owner_corporation_id).copied()).unwrap_or(0.0);
                let fs = ch.and_then(|c| st.owner_faction_id.and_then(|fid| c.standings.get(&fid).copied())).unwrap_or(0.0);
                (cs, fs)
            });
            let min_standing = if service_id == 512 { 0.0 } else { -2.0 };
            if corp_st < min_standing || fac_st < -2.0 {
                return Some(Err(BridgeRpcError {
                    code: "StandingTooLowForStationService".into(),
                    message: format!(
                        "Character {char_id} standing (corp {corp_st:.2}, faction {fac_st:.2}) is below required {min_standing:.1} for service {service_id} at station {station_id}"
                    ),
                }));
            }
            Some(Ok(PyRep::Bool(true)))
        }

        // parity: feat:navigation.undock
        ("stationSvc", "Undock") | ("structureDocking", "Undock") => {
            let (user_id, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (station_id, ch, ship) = match ctx.db.undock_character(user_id, char_id) {
                Ok(t) => t,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "UndockFailed".into(),
                        message: e.to_string(),
                    }))
                }
            };

            ctx.invalidate_ship_fitting(ship.item_id);
            let Some(ship_type) = ctx.sde.types.get(&ship.type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownShipType".into(),
                    message: format!("Active ship type {} not found in SDE", ship.type_id),
                }));
            };

            let (spawn_pos, undock_dir) = if station_id >= 1_000_000_000_000 {
                ctx.db
                    .with_snapshot(|snap| {
                        snap.structures.get(&station_id).map(|st| {
                            let st_pos = DVec3::from_array(st.position);
                            let dir = if st_pos.length_squared() > 1.0 {
                                st_pos.normalize()
                            } else {
                                DVec3::X
                            };
                            let st_radius = ctx
                                .sde
                                .types
                                .get(&st.type_id)
                                .map(|t| t.radius)
                                .unwrap_or(5_000.0);
                            let offset = (st_radius * 0.15 + ship_type.radius).clamp(500.0, 2_000.0);
                            (st_pos + dir * offset, dir)
                        })
                    })
                    .unwrap_or_else(|| (DVec3::new(51_500.0, 0.0, 0.0), DVec3::X))
            } else {
                ctx.sde
                    .stations
                    .get(&station_id)
                    .map(|st| {
                        let dir = st
                            .orbit_id
                            .and_then(|oid| {
                                ctx.sde
                                    .celestials_in_system(ch.solar_system_id)
                                    .into_iter()
                                    .find(|c| c.celestial_id == oid)
                            })
                            .and_then(|cel| {
                                let delta = st.position - cel.position;
                                (delta.length_squared() > 1.0).then(|| delta.normalize())
                            })
                            .or_else(|| {
                                (st.position.length_squared() > 1.0)
                                    .then(|| st.position.normalize())
                            })
                            .unwrap_or(DVec3::X);
                        let st_radius = ctx
                            .sde
                            .types
                            .get(&st.station_type_id)
                            .map(|t| t.radius)
                            .unwrap_or(30_000.0);
                        let offset = (st_radius * 0.015 + ship_type.radius).clamp(500.0, 2_000.0);
                        (st.position + dir * offset, dir)
                    })
                    .unwrap_or_else(|| (DVec3::new(100_000.0, 0.0, 0.0), DVec3::X))
            };

            let max_vel = ctx
                .get_or_rebuild_ship_fitting(char_id, ship.item_id)
                .ok()
                .map(|f| f.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
                .or_else(|| {
                    ship_type
                        .default_attributes
                        .get(&attr_ids::MAX_VELOCITY)
                        .copied()
                })
                .unwrap_or(0.0);
            let inertia = ship_type
                .default_attributes
                .get(&attr_ids::AGILITY)
                .copied()
                .unwrap_or(1.0);
            let mass = ship_type.mass;
            let radius = ship_type.radius;

            let ball = Ball {
                id: ship.item_id,
                type_id: ship.type_id,
                position: spawn_pos,
                velocity: undock_dir * (max_vel * 0.5),
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 1.0,
                mode: MovementMode::GotoDirection {
                    direction: undock_dir,
                },
            };

            let mut combat_state = ctx.build_ship_combat_state(char_id, ship.item_id, ship.type_id);
            combat_state.undock_invulnerable_remaining_ticks = 30;
            let slim_meta = ctx.build_ship_slim_meta(char_id, ship.type_id);
            let sys_handle = ctx.systems.get_or_spawn(ch.solar_system_id);
            let tick = match sys_handle
                .spawn_ball_with_meta(ball, combat_state, slim_meta)
                .await
            {
                Ok(t) => t,
                Err(e) => {
                    ctx.db.rollback_undock(char_id, station_id, ship.item_id);
                    return Some(Err(BridgeRpcError {
                        code: "SystemActorError".into(),
                        message: e,
                    }));
                }
            };
            if ctx.feature_flags.enable_living_world {
                let _ = ctx
                    .living_world
                    .materialize_system_lod2(ch.solar_system_id, &ctx.systems, &ctx.sde)
                    .await;
            }
            let _ = sys_handle.push_set_state(ship.item_id).await;

            // The undocked character is no longer a guest, so only remaining guests receive this.
            let guest_row = station_guest_row(ctx, char_id);
            ctx.notify(
                crate::router::NotificationTarget::Station(station_id),
                "OnCharNoLongerInStation",
                "charid",
                vec![guest_row],
            );

            session.station_id = None;
            session.structure_id = None;
            let beyonce_bound_id =
                match ctx.bind_object(session, "beyonce", ch.solar_system_id as i64) {
                    Ok(b) => b,
                    Err(e) => return Some(Err(e)),
                };
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(ch.solar_system_id as i64)),
                ("shipID", PyRep::Integer(ship.item_id)),
                ("beyonceBoundID", PyRep::Integer(beyonce_bound_id as i64)),
                ("spawnTick", PyRep::Integer(tick as i64)),
                ("undockInvulnerabilityTicks", PyRep::Integer(30)),
                (
                    "undockDirection",
                    PyRep::Tuple(vec![
                        PyRep::Real(undock_dir.x),
                        PyRep::Real(undock_dir.y),
                        PyRep::Real(undock_dir.z),
                    ]),
                ),
            ])))
        }

        ("stationSvc", "Dock") | ("structureDocking", "Dock") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "AlreadyDocked".into(),
                    message: "Character is already docked in a station or structure".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Cannot dock without an active ship in space".into(),
                }));
            };
            let Some(dock_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "MissingLocationId".into(),
                    message: "Dock requires an explicit station or structure ID argument".into(),
                }));
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let is_structure = service == "structureDocking" || dock_id >= 1_000_000_000_000;

            let target_pos = if is_structure {
                let st_opt = ctx
                    .db
                    .with_snapshot(|snap| snap.structures.get(&dock_id).cloned());
                let Some(st) = st_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {dock_id} does not exist"),
                    }));
                };
                if st.solar_system_id != sys_id {
                    return Some(Err(BridgeRpcError {
                        code: "DockTargetWrongSystem".into(),
                        message: format!(
                            "Structure {dock_id} is in system {}, but pilot is in {sys_id}",
                            st.solar_system_id
                        ),
                    }));
                }
                if st.state == "Destroyed" {
                    return Some(Err(BridgeRpcError {
                        code: "StructureDestroyed".into(),
                        message: format!("Structure {dock_id} is destroyed"),
                    }));
                }
                DVec3::from_array(st.position)
            } else {
                let st_opt = ctx
                    .sde
                    .stations
                    .get(&dock_id)
                    .filter(|st| st.solar_system_id == sys_id)
                    .cloned();
                let Some(st) = st_opt else {
                    return Some(Err(BridgeRpcError {
                        code: "DockTargetWrongSystem".into(),
                        message: format!(
                            "Station {dock_id} does not exist in current solar system {sys_id}"
                        ),
                    }));
                };
                st.position
            };

            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ball)) = (match handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is not spawned in solar system {sys_id} Ballpark"),
                }));
            };

            if matches!(ball.mode, MovementMode::Warp { .. }) {
                return Some(Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot dock while ship is still in warp".into(),
                }));
            }
            let dist = ball.position.distance(target_pos);
            if dist > 2_500.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideDockingPerimeter".into(),
                    message: format!(
                        "Ship is {:.1} m from docking perimeter (must be <= 2500 m)",
                        dist
                    ),
                }));
            }
            match ctx.db.dock_character(char_id, dock_id, is_structure) {
                Ok(_) => {
                    let _ = handle.remove_ball(ship_id).await;
                    if is_structure {
                        session.structure_id = Some(dock_id);
                        session.station_id = None;
                    } else {
                        session.station_id = Some(dock_id);
                        session.structure_id = None;
                    }
                    let guest_row = station_guest_row(ctx, char_id);
                    ctx.notify(
                        crate::router::NotificationTarget::Station(dock_id),
                        "OnCharNowInStation",
                        "charid",
                        vec![guest_row],
                    );
                    Some(Ok(PyRep::keyval([
                        ("locationID", PyRep::Integer(dock_id)),
                        ("isStructure", PyRep::Bool(is_structure)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "DockFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("structureDeployment", "Anchor") => Some(structure_lifecycle_rpc::handle_anchor(ctx, session, args).await),

        // rpc-lint: allow pre-existing handler (not a V24 client name) kept for tests/structures.rs and regressions (S-7); now fuels per SDE service fuel groups
        ("structureControl", "FuelStructure") => Some(structure_lifecycle_rpc::handle_fuel(ctx, session, args)),

        // rpc-lint: allow S-7 in action_plan.md names AttackStructure (proximity/state-machine checks); now resolved by structure_lifecycle
        ("structureControl", "AttackStructure") => Some(structure_lifecycle_rpc::handle_attack(ctx, session, args).await),

        // parity: feat:modules.mobile-warp-disruptor
        ("posMgr", "AnchorStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "MissingArgument".into(),
                    message: "AnchorStructure requires an itemID".into(),
                }));
            };
            let item = match ctx.db.with_snapshot(|snap| snap.items.get(&item_id).cloned()) {
                Some(it) => it,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "StarbaseRemoved".into(),
                        message: "Legacy POS starbases and control towers are superseded by Upwell structures in V24.01".into(),
                    }));
                }
            };
            let Some(item_type) = ctx.sde.types.get(&item.type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "StarbaseRemoved".into(),
                    message: "Legacy POS starbases and control towers are superseded by Upwell structures in V24.01".into(),
                }));
            };
            if item_type.group_id == 365 || item_type.category_id == 23 {
                return Some(Err(BridgeRpcError {
                    code: "StarbaseRemoved".into(),
                    message: "Legacy POS starbases and control towers are superseded by Upwell structures in V24.01".into(),
                }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let (_, is_thera, is_shattered, _) =
                crate::services::space_destiny::system_wormhole_profile(sys_id);
            if is_thera || is_shattered {
                return Some(Err(BridgeRpcError {
                    code: "AnchoringDisallowedInTheraOrShattered".into(),
                    message: format!(
                        "Anchoring starbases or deployables is disallowed in Thera or shattered wormhole system {sys_id}"
                    ),
                }));
            }
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "NotInSpace".into(),
                    message: "Cannot anchor a structure while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Cannot anchor a structure without an active ship in space".into(),
                }));
            };
            if item.owner_id != char_id {
                return Some(Err(BridgeRpcError {
                    code: "NotItemOwner".into(),
                    message: format!("Character {char_id} does not own item {item_id}"),
                }));
            }
            if item.location_id != ship_id && item.location_id != sys_id as i64 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidItemLocation".into(),
                    message: format!(
                        "Item {item_id} must be in active ship cargo or unanchored in space in system {sys_id}"
                    ),
                }));
            }

            let skill_levels = ctx.character_skill_levels(char_id);
            for &(skill_attr, lvl_attr) in &[
                (182u16, 277u16),
                (183, 278),
                (184, 279),
                (1285, 1286),
                (1289, 1290),
                (1291, 1292),
            ] {
                if let Some(&req_skill_f) = item_type.default_attributes.get(&skill_attr) {
                    let req_skill_id = req_skill_f as u32;
                    let req_lvl = item_type
                        .default_attributes
                        .get(&lvl_attr)
                        .copied()
                        .unwrap_or(1.0) as u8;
                    let have_lvl = skill_levels.get(&req_skill_id).copied().unwrap_or(0);
                    if req_skill_id > 0 && have_lvl < req_lvl {
                        return Some(Err(BridgeRpcError {
                            code: "SkillMissing".into(),
                            message: format!(
                                "Character {char_id} lacks required skill {req_skill_id} level {req_lvl} (has {have_lvl})"
                            ),
                        }));
                    }
                }
            }

            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if item_type.group_id == 361 && sys_sec > 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "CantAnchorInEmpire".into(),
                    message: format!(
                        "Mobile Warp Disruptor cannot be anchored in high-sec or low-sec system {sys_id} (security {sys_sec:.2} > 0.0)"
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "SystemActorError".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: "Active ship is not spawned in space".into(),
                }));
            };

            let requested_pos = args.get(1).and_then(|p| match p {
                PyRep::Tuple(coords) | PyRep::List(coords) if coords.len() == 3 => {
                    Some(DVec3::new(
                        coords[0].as_f64()?,
                        coords[1].as_f64()?,
                        coords[2].as_f64()?,
                    ))
                }
                _ => None,
            });
            let anchor_pos = match requested_pos {
                Some(p) if (p - ship_ball.position).length() <= 5_000.0 => p,
                _ => ship_ball.position + DVec3::new(250.0, 0.0, 0.0),
            };

            let bubble_radius = item_type
                .default_attributes
                .get(&103)
                .copied()
                .unwrap_or(5_000.0);
            let anchoring_delay_s = (item_type
                .default_attributes
                .get(&556)
                .copied()
                .unwrap_or(120_000.0)
                / 1_000.0)
                .max(1.0);

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.get_mut(&item_id) {
                    it.location_id = sys_id as i64;
                    it.flag = InventoryFlag::Hangar;
                    it.singleton = true;
                }
            });

            if let Err(e) = sys_handle
                .schedule_mobile_warp_disruptor(
                    item_id,
                    item.type_id,
                    anchor_pos,
                    bubble_radius,
                    anchoring_delay_s,
                )
                .await
            {
                return Some(Err(BridgeRpcError {
                    code: "SystemActorError".into(),
                    message: e,
                }));
            }

            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(item_id)),
                ("typeID", PyRep::Integer(item.type_id as i64)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("state", PyRep::String("Anchoring".into())),
                ("anchoringDelaySeconds", PyRep::Real(anchoring_delay_s)),
                ("bubbleRadius", PyRep::Real(bubble_radius)),
            ])))
        }

        _ => {
            if service == "posMgr" && method == "AnchorOrbital" {
                let (_, char_id) = match ServerContext::require_character(session) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let Some(sys_id) = session.solar_system_id else {
                    return Some(Err(BridgeRpcError {
                        code: "NoSolarSystem".into(),
                        message: "Session has no active solarSystemID".into(),
                    }));
                };
                let (_, is_thera, is_shattered, _) =
                    crate::services::space_destiny::system_wormhole_profile(sys_id);
                if is_thera || is_shattered {
                    return Some(Err(BridgeRpcError {
                        code: "AnchoringDisallowedInTheraOrShattered".into(),
                        message: format!(
                            "Anchoring starbases or orbitals is disallowed in Thera or shattered wormhole system {sys_id}"
                        ),
                    }));
                }
                let region_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(0);
                if region_id == structure_lifecycle::POCHVEN_REGION_ID {
                    if session.station_id.is_some() || session.structure_id.is_some() {
                        return Some(Err(BridgeRpcError {
                            code: "NotInSpace".into(),
                            message: "Must be in space to anchor an orbital structure".into(),
                        }));
                    }
                    let trig_standing = ctx.db.with_snapshot(|snap| {
                        snap.characters
                            .get(&char_id)
                            .and_then(|c| c.standings.get(&500_026).copied())
                            .unwrap_or(0.0)
                    });
                    if trig_standing < 1.5 {
                        return Some(Err(BridgeRpcError {
                            code: "InsufficientTriglavianStanding".into(),
                            message: format!(
                                "Anchoring orbitals in Pochven requires at least 1.5 standing with the Triglavian Collective (500026); pilot has {trig_standing:.2}"
                            ),
                        }));
                    }
                    if let Some(item_id) = args.first().and_then(|a| a.as_i64()) {
                        let res = ctx.db.try_with_mut_snapshot(|snap| {
                            let it = snap.items.get_mut(&item_id).ok_or_else(|| BridgeRpcError {
                                code: "ItemNotFound".into(),
                                message: format!("Orbital item {item_id} not found"),
                            })?;
                            if it.owner_id != char_id {
                                return Err(BridgeRpcError {
                                    code: "NotItemOwner".into(),
                                    message: format!("Character {char_id} does not own item {item_id}"),
                                });
                            }
                            let tid = it.type_id;
                            it.location_id = i64::from(sys_id);
                            it.flag = InventoryFlag::None;
                            it.singleton = true;
                            Ok(tid)
                        });
                        return match res {
                            Ok(tid) => Some(Ok(PyRep::keyval([
                                ("itemID", PyRep::Integer(item_id)),
                                ("typeID", PyRep::Integer(i64::from(tid))),
                                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                                ("state", PyRep::String("Anchored".into())),
                            ]))),
                            Err(e) => Some(Err(e)),
                        };
                    }
                }
            }
            structure_upwell_ops::handle(ctx, session, service, method, args, _kwargs).await
        }
    }
}
