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
#[path = "structure_ansiblex_navigation.rs"]
pub mod structure_ansiblex_navigation;
#[path = "structure_lifecycle_rpc.rs"]
pub mod structure_lifecycle_rpc;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // Upwell lifecycle timers are evaluated lazily before any structure RPC is served.
    if service.starts_with("structure") {
        structure_lifecycle::sync_all_structures(ctx);
    }
    match (service, method) {
        ("stationSvc", "GetAllianceSystems") | ("stationSvc", "GetSystemsForAlliance") => {
            let aid = args.first().and_then(|a| a.as_i64()).or(session.alliance_id).unwrap_or(0);
            let systems = ctx.db.with_snapshot(|snap| {
                let member_corps: std::collections::HashSet<i64> = snap
                    .corporations
                    .values()
                    .filter(|c| c.alliance_id == Some(aid))
                    .map(|c| c.corporation_id)
                    .collect();
                let mut sys_set: std::collections::BTreeSet<i64> = snap
                    .system_states
                    .values()
                    .filter(|s| s.sov_alliance_id == Some(aid))
                    .map(|s| s.solar_system_id as i64)
                    .collect();
                for st in snap.structures.values() {
                    if member_corps.contains(&st.owner_corp_id) {
                        sys_set.insert(st.solar_system_id as i64);
                    }
                }
                sys_set.into_iter().map(PyRep::Integer).collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(systems)))
        }

        ("stationSvc", "GetStationsForOwner") => {
            let owner_id = args.first().and_then(|a| a.as_i64()).unwrap_or(1000035);
            let mut stations: Vec<PyRep> = ctx
                .sde
                .stations
                .values()
                .filter(|st| st.owner_corporation_id == owner_id || st.owner_faction_id == Some(owner_id))
                .take(50)
                .map(|st| {
                    PyRep::keyval([
                        ("stationID", PyRep::Integer(st.station_id)),
                        ("solarSystemID", PyRep::Integer(st.solar_system_id as i64)),
                        ("corporationID", PyRep::Integer(st.owner_corporation_id)),
                    ])
                })
                .collect();
            let struct_stations: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                snap.structures
                    .values()
                    .filter(|st| st.owner_corp_id == owner_id)
                    .map(|st| {
                        PyRep::keyval([
                            ("stationID", PyRep::Integer(st.structure_id)),
                            ("solarSystemID", PyRep::Integer(st.solar_system_id as i64)),
                            ("corporationID", PyRep::Integer(st.owner_corp_id)),
                        ])
                    })
                    .collect()
            });
            stations.extend(struct_stations);
            Some(Ok(PyRep::List(stations)))
        }

        ("stationSvc", "GetSolarSystem") | ("station", "GetSolarSystem") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let first_arg = args.first().and_then(|a| a.as_i64());
            let (sys_id, is_station_lookup) = match first_arg {
                Some(id) if id >= 60_000_000 => {
                    let st = ctx.sde.stations.get(&id).map(|s| s.solar_system_id).or_else(|| {
                        ctx.db.with_snapshot(|snap| snap.structures.get(&id).map(|s| s.solar_system_id))
                    });
                    match st {
                        Some(s) => (s, true),
                        None => {
                            return Some(Err(BridgeRpcError {
                                code: "StationNotFound".into(),
                                message: format!("Station or structure {id} not found"),
                            }));
                        }
                    }
                }
                Some(id) if id > 0 => (id as u32, false),
                _ => match session.solar_system_id {
                    Some(s) => (s, false),
                    None => {
                        return Some(Err(BridgeRpcError {
                            code: "NoSolarSystem".into(),
                            message: "No solarSystemID argument or active session solar system".into(),
                        }))
                    }
                },
            };
            if is_station_lookup {
                Some(Ok(PyRep::Integer(sys_id as i64)))
            } else if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
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
                    .map(|o| o.services.iter().fold(0i64, |acc, &sid| if sid > 0 && sid <= 64 { acc | (1i64 << (sid - 1)) } else { acc }))
                    .unwrap_or(0);
                let services = op
                    .map(|o| o.services.iter().map(|&sid| PyRep::Integer(i64::from(sid))).collect())
                    .unwrap_or_default();
                let hq_services: Vec<PyRep> = crate::services::fw_gameplay::get_war_hq_services(sys_id)
                    .map(|(_, svcs)| svcs.into_iter().map(|s| PyRep::String(s.into())).collect())
                    .unwrap_or_default();
                let is_war_hq = !hq_services.is_empty();
                Some(Ok(PyRep::keyval([
                    ("stationID", PyRep::Integer(st.station_id)),
                    ("stationName", PyRep::String(st.name)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                    ("stationTypeID", PyRep::Integer(i64::from(st.station_type_id))),
                    ("ownerID", PyRep::Integer(st.owner_corporation_id)),
                    ("operationID", PyRep::Integer(i64::from(st.operation_id))),
                    ("serviceMask", PyRep::Integer(service_mask)),
                    ("services", PyRep::List(services)),
                    ("isWarHQ", PyRep::Bool(is_war_hq)),
                    ("warHQServices", PyRep::List(hq_services)),
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
            // `StationSvc.SetStationItemBits(bits)` builds
            // `Row(['ownerID', 'itemID', 'operationID', 'stationTypeID'], bits)`
            // (`eve/client/script/ui/station/base.pyj:684-685`); the row needs exactly these 4 values.
            Some(Ok(PyRep::Tuple(vec![
                PyRep::Integer(st.owner_corporation_id),
                PyRep::Integer(st.station_id),
                PyRep::Integer(i64::from(st.operation_id)),
                PyRep::Integer(i64::from(st.station_type_id)),
            ])))
        }

        ("station", "GetGuests") => {
            let (_, caller_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let station_id = match args.first().and_then(|a| a.as_i64()).or(session.station_id) {
                Some(sid) => sid,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "NotDockedInStation".into(),
                        message: "station.GetGuests requires being docked in a station".into(),
                    }));
                }
            };
            let rows = ctx.db.with_snapshot(|snap| {
                let mut guests: Vec<_> = snap
                    .characters
                    .values()
                    .filter(|c| c.station_id == Some(station_id) && (c.character_id == caller_id || ctx.is_character_online(c.character_id, c.user_id)))
                    .map(|c| (c.character_id, c.corporation_id, c.alliance_id, c.war_faction_id))
                    .collect();
                guests.sort_by_key(|g| g.0);
                let opt = |v: Option<i64>| v.map(PyRep::Integer).unwrap_or(PyRep::None);
                guests
                    .into_iter()
                    .map(|(cid, corp, ally, fac)| {
                        PyRep::Tuple(vec![
                            PyRep::Integer(cid),
                            PyRep::Integer(corp),
                            opt(ally),
                            opt(fac),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("corpStationMgr", "DoStandingCheckForStationService") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let raw_service_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            if raw_service_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "StationServiceUnavailable".into(),
                    message: format!("Invalid station service ID {raw_service_id}"),
                }));
            }
            let Some(station_id) = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or(session.station_id)
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MissingStationId".into(),
                    message: "DoStandingCheckForStationService requires a stationID argument or active session stationID".into(),
                }));
            };
            let Ok(raw_u32) = u32::try_from(raw_service_id) else {
                return Some(Err(BridgeRpcError {
                    code: "StationServiceUnavailable".into(),
                    message: format!("Invalid station service ID {raw_service_id}"),
                }));
            };
            const MAX_STATION_SERVICE_ID: u32 = 27;
            let (service_id, owner_corp, owner_fac) = if let Some(st) = ctx.sde.stations.get(&station_id) {
                let op_svcs = ctx
                    .sde
                    .station_operations
                    .get(&st.operation_id)
                    .map(|op| op.services.as_slice())
                    .unwrap_or(&[]);
                let resolved_sid = if raw_u32 <= MAX_STATION_SERVICE_ID {
                    if op_svcs.contains(&raw_u32) {
                        Some(raw_u32)
                    } else if raw_u32 == 16 && op_svcs.contains(&5) {
                        // Legacy EVE appConst.stationServiceReprocessingPlant bitmask (1 << 4 = 16 -> service 5)
                        Some(5)
                    } else {
                        None
                    }
                } else if raw_u32.is_power_of_two() && raw_u32 <= (1 << (MAX_STATION_SERVICE_ID - 1)) {
                    let bit_sid = raw_u32.trailing_zeros() + 1;
                    op_svcs.contains(&bit_sid).then_some(bit_sid)
                } else {
                    None
                };
                let Some(sid) = resolved_sid else {
                    return Some(Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Station {station_id} does not offer service {raw_service_id}"),
                    }));
                };
                (sid, st.owner_corporation_id, st.owner_faction_id)
            } else if let Some(struct_info) = ctx.db.with_snapshot(|s| s.structures.get(&station_id).map(|st| (st.owner_corp_id, None))) {
                let sid = if raw_u32 <= MAX_STATION_SERVICE_ID {
                    raw_u32
                } else if raw_u32.is_power_of_two() && raw_u32 <= (1 << (MAX_STATION_SERVICE_ID - 1)) {
                    raw_u32.trailing_zeros() + 1
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Invalid station service ID {raw_service_id}"),
                    }));
                };
                (sid, struct_info.0, struct_info.1)
            } else {
                return Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {station_id} not found in SDE"),
                }));
            };
            if let Err(err) = crate::services::station_structure_handlers::check_station_service_standing(
                ctx,
                char_id,
                station_id,
                raw_service_id,
                service_id,
                owner_corp,
                owner_fac,
            ) {
                return Some(Err(err));
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("stationSvc", "Undock") | ("structureDocking", "Undock") | ("ship", "Undock") => {
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

            if kwargs
                .get("onlineModules")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
            {
                ctx.db.with_mut_snapshot(|snap| {
                    let mod_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|it| {
                            it.location_id == ship.item_id
                                && it.owner_id == char_id
                                && (11..=34).contains(&(it.flag as i32))
                        })
                        .map(|it| it.item_id)
                        .collect();
                    for mid in mod_ids {
                        snap.module_online_states.insert(mid, true);
                    }
                });
            }
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

            session.station_id = None;
            session.structure_id = None;
            ctx.invalidate_bound_objects_for_session_transition(char_id, ch.solar_system_id, None, "undock");
            let sess_rep = PyRep::keyval([
                ("userid", session.user_id.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
                ("charid", PyRep::Integer(char_id)),
                ("corpid", session.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::Integer(ch.corporation_id))),
                ("allianceid", session.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("shipid", session.ship_id.map(PyRep::Integer).unwrap_or(PyRep::Integer(ship.item_id))),
                ("stationid", PyRep::None),
                ("structureid", PyRep::None),
                ("solarsystemid2", session.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)),
                ("solarsystemid", session.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)),
                ("locationid", session.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)),
                ("constellationid", session.constellation_id.map(|c| PyRep::Integer(c as i64)).unwrap_or(PyRep::None)),
                ("regionid", session.region_id.map(|r| PyRep::Integer(r as i64)).unwrap_or(PyRep::None)),
            ]);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnSessionChanged",
                "charid",
                vec![
                    PyRep::Bool(false),
                    sess_rep,
                    PyRep::Dict(vec![(
                        PyRep::String(if station_id >= 1_000_000_000_000 { "structureid" } else { "stationid" }.into()),
                        PyRep::Tuple(vec![PyRep::Integer(station_id), PyRep::None]),
                    )]),
                ],
            );
            if station_id >= 1_000_000_000_000 {
                ctx.notify(
                    crate::router::NotificationTarget::Station(station_id),
                    "OnCharacterLeftStructure",
                    "stationid",
                    vec![PyRep::Integer(char_id)],
                );
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnCharacterLeftStructure",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            let invuln_end_ft = ctx.now_blueos_filetime_ticks() + 300_000_000;
            ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(char_id as u32, &["undockFromStation"], 1, ctx.now_unix(), &ctx.quasar_gateway);
            ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnInvulunOnUndockingUpdated",
                "charid",
                vec![
                    PyRep::Integer(ship.item_id),
                    PyRep::Integer(invuln_end_ft),
                    PyRep::Integer(30_000),
                ],
            );
            let beyonce_bound_id =
                match ctx.bind_object(session, "beyonce", ch.solar_system_id as i64) {
                    Ok(b) => b,
                    Err(e) => return Some(Err(e)),
                };
            let _ = crate::services::combat_npc_police::check_and_spawn_faction_police(ctx, ch.solar_system_id).await;
            // Entering space: send the pilot the SKIN states and structure paintworks around them.
            crate::services::quasar_cosmetics_live::publish_arrival_cosmetics(&ctx.quasar_gateway, &ctx.db, &ctx.sde, char_id, ctx.now_unix());
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
            if args.get(1).and_then(|a| a.as_i64()).is_some_and(|req| req != ship_id) {
                return Some(Err(BridgeRpcError { code: "InvalidShipId".into(), message: "Requested shipID does not match active ship".into() }));
            }
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let is_structure = service == "structureDocking"
                || dock_id >= 1_000_000_000_000
                || ctx.db.with_snapshot(|snap| snap.structures.contains_key(&dock_id));

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

            let now_unix = ctx.now_unix();
            if let Some(active_timer) = crate::services::space_destiny::check_session_change_timer_active(ctx, char_id, now_unix, None) {
                return Some(Err(BridgeRpcError {
                    code: "SessionChangeInProgress".into(),
                    message: format!(
                        "Cannot dock while 10s session-change timer ({}) is active",
                        active_timer.reason
                    ),
                }));
            }
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            let ship_type_id = ctx.db.with_snapshot(|s| s.items.get(&ship_id).map(|i| i.type_id)).unwrap_or(ball.type_id);
            let ship_group_id = ctx.sde.types.get(&ship_type_id).map(|t| t.group_id).unwrap_or(0);
            if let Err(e) = crate::services::space_navigation_advanced::check_immobilizing_module_active(ctx, ship_id, "dock") {
                return Some(Err(e));
            }
            if let Err(e) = crate::services::space_navigation_advanced::validate_npc_station_docking(ship_group_id, !is_structure) {
                return Some(Err(e));
            }
            let is_capsule = ship_type_id == 670 || ship_group_id == 29;
            {
                let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
                if let Some(cw_state) = cw.states.get(&char_id) {
                    if cw_state.is_weapon_timer_active(now_unix) {
                        return Some(Err(BridgeRpcError {
                            code: "WeaponsTimerActive".into(),
                            message: "Cannot dock while a 60s Weapons Timer is active".into(),
                        }));
                    }
                    if sys_sec >= 0.45 && cw_state.is_criminal(now_unix) && !is_capsule {
                        return Some(Err(BridgeRpcError {
                            code: "CantDockGlobalCriminalFlag".into(),
                            message: "Cannot dock a non-capsule ship in high-sec while a Criminal Timer is active".into(),
                        }));
                    }
                }
            }

            if crate::services::combat_crimewatch::combat_npc_police::is_outlaw_ship_docking_refused(ctx, sys_id, char_id, ship_id) {
                return Some(Err(BridgeRpcError {
                    code: "FactionPoliceRefusedDocking".into(),
                    message: "Faction police refuse docking permissions to outlaw ships in high-security space".into(),
                }));
            }

            if let Err(e) = crate::services::tactical_modules::check_ship_navigation_lock(ctx, ship_id, "dock") {
                return Some(Err(e));
            }
            if let Err(e) = crate::services::fw_gameplay::check_docking_allowed(ctx, char_id, sys_id, dock_id, is_structure) {
                return Some(Err(e));
            }

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
                if matches!(st.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring") {
                    return Some(Err(BridgeRpcError {
                        code: "StructureNotDockable".into(),
                        message: format!("Structure {dock_id} is in state '{}' and cannot be docked at", st.state),
                    }));
                }
                let acl_denied = ctx.db.with_snapshot(|snap| {
                    !structure_upwell_ops::evaluate_character_structure_access(
                        ctx, snap, dock_id, char_id,
                    )
                });
                if acl_denied {
                    return Some(Err(BridgeRpcError {
                        code: "StructureDockingAccessDenied".into(),
                        message: format!(
                            "Character {char_id} is denied docking access by structure {dock_id} ACL"
                        ),
                    }));
                }
                let is_medium_structure = matches!(st.type_id, 35832 | 35825 | 35835);
                let is_large_structure = matches!(st.type_id, 35833 | 35826 | 35836);
                let is_capital_ship =
                    matches!(ship_group_id, 30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594);
                let is_supercapital_ship = matches!(ship_group_id, 30 | 659);
                if (is_medium_structure && is_capital_ship)
                    || (is_large_structure && is_supercapital_ship)
                {
                    return Some(Err(BridgeRpcError {
                        code: "ShipTooLargeForStructure".into(),
                        message: format!(
                            "Ship type {} (group {}) is too large to dock at structure {} (type {})",
                            ball.type_id, ship_group_id, dock_id, st.type_id
                        ),
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

            if matches!(ball.mode, MovementMode::Warp { .. }) {
                return Some(Err(BridgeRpcError {
                    code: "ShipInWarp".into(),
                    message: "Cannot dock while ship is still in warp".into(),
                }));
            }
            if is_structure && handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
                return Some(Err(BridgeRpcError { code: "ShipWarpScrambled".into(), message: "Cannot dock at an Upwell structure while ship is warp scrambled".into() }));
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
                    let active_mods: Vec<i64> = ctx.db.with_mut_snapshot(|snap| {
                        let mods: Vec<i64> = snap
                            .items
                            .values()
                            .filter(|it| {
                                it.location_id == ship_id
                                    && snap.module_active_states.contains(&it.item_id)
                            })
                            .map(|it| it.item_id)
                            .collect();
                        for mid in &mods {
                            snap.module_active_states.remove(mid);
                        }
                        mods
                    });
                    let _ = handle.accept_docking(ship_id, dock_id, target_pos).await;
                    for mid in active_mods {
                        ctx.notify_godma_ship_effect(
                            crate::router::NotificationTarget::Character(char_id),
                            mid,
                            10,
                            false,
                            0,
                            0,
                        );
                    }
                    let prev_sys = session.solar_system_id.unwrap_or(0);
                    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, prev_sys);
                    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                        rt.authorized_shipping_storage.retain(|&(_, c)| c != char_id);
                    });
                    if is_structure {
                        session.structure_id = Some(dock_id);
                        session.station_id = None;
                    } else {
                        session.station_id = Some(dock_id);
                        session.structure_id = None;
                    }
                    crate::services::space_destiny::record_session_change_timer(
                        ctx, char_id, now_unix, sys_id, 0, "dock",
                    );
                    let sess_rep = PyRep::keyval([
                        ("userid", session.user_id.map(|u| PyRep::Integer(u as i64)).unwrap_or(PyRep::None)),
                        ("charid", PyRep::Integer(char_id)),
                        ("corpid", session.corporation_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("allianceid", session.alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("shipid", session.ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("stationid", session.station_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("structureid", session.structure_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                        ("solarsystemid2", session.solar_system_id.map(|s| PyRep::Integer(s as i64)).unwrap_or(PyRep::None)),
                        ("solarsystemid", PyRep::None),
                        ("locationid", PyRep::Integer(dock_id)),
                        ("constellationid", session.constellation_id.map(|c| PyRep::Integer(c as i64)).unwrap_or(PyRep::None)),
                        ("regionid", session.region_id.map(|r| PyRep::Integer(r as i64)).unwrap_or(PyRep::None)),
                    ]);
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnSessionChanged",
                        "charid",
                        vec![
                            PyRep::Bool(false),
                            sess_rep,
                            PyRep::Dict(vec![(
                                PyRep::String(if is_structure { "structureid" } else { "stationid" }.into()),
                                PyRep::Tuple(vec![PyRep::None, PyRep::Integer(dock_id)]),
                            )]),
                        ],
                    );
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnDockingAccepted",
                        "charid",
                        vec![PyRep::Integer(dock_id)],
                    );
                    ctx.notify(
                        crate::router::NotificationTarget::Character(char_id),
                        "OnDockingFinished",
                        "charid",
                        vec![PyRep::Integer(dock_id)],
                    );
                    if is_structure {
                        let (corp_id, ally_id, fac_id, st_type_id) = ctx.db.with_snapshot(|snap| {
                            let c = snap.characters.get(&char_id);
                            let corp = c.map(|ch| ch.corporation_id).unwrap_or(0);
                            let ally = c.and_then(|ch| ch.alliance_id);
                            let fac = c.and_then(|ch| ch.war_faction_id);
                            let st_tid = snap.structures.get(&dock_id).map(|s| s.type_id).unwrap_or(35832);
                            (corp, ally, fac, st_tid)
                        });
                        ctx.notify(
                            crate::router::NotificationTarget::Station(dock_id),
                            "OnCharacterEnteredStructure",
                            "stationid",
                            vec![
                                PyRep::Integer(char_id),
                                PyRep::Integer(corp_id),
                                ally_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                                fac_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ],
                        );
                        let hv_state = PyRep::keyval([
                            ("characterID", PyRep::Integer(char_id)),
                            ("structureID", PyRep::Integer(dock_id)),
                            ("structureTypeID", PyRep::Integer(i64::from(st_type_id))),
                            ("shipID", PyRep::Integer(ship_id)),
                        ]);
                        ctx.notify(
                            crate::router::NotificationTarget::Character(char_id),
                            "OnHangarViewStateUpdated",
                            "charid",
                            vec![PyRep::Integer(dock_id), hv_state],
                        );
                    }
                    ctx.quasar_gateway.on_session_changed(session, now_unix);
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

        // parity: feat:modules.mobile-warp-disruptor, feat:structures.mobile-depot, feat:structures.mobile-tractor, feat:structures.mobile-deploy-restrictions
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
            if item_type.category_id != 22 && !item_type.effect_ids.contains(&649) {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotAnchorable".into(),
                    message: format!("Item {} (type {}) cannot be anchored in space", item_id, item.type_id),
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
                return Some(Err(BridgeRpcError { code: "AnchoringDisallowedInTheraOrShattered".into(), message: format!("Anchoring starbases or deployables is disallowed in Thera or shattered wormhole system {sys_id}") }));
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
            let already_anchored = ctx.db.with_snapshot(|snap| {
                if snap.sp6_lifecycle.space_object_meta.get(&item_id).is_some_and(|m| m.anchor_start_unix.is_some() || m.anchor_completed) {
                    return true;
                }
                let st = crate::services::mobile_deployables::read_deployable_state_snap(snap);
                st.deployables.get(&item_id).is_some_and(|r| !r.is_unanchored)
            });
            if already_anchored {
                return Some(Err(BridgeRpcError {
                    code: "StructureAlreadyAnchored".into(),
                    message: format!("Structure {item_id} is already anchored in space; unanchor and scoop first"),
                }));
            }
            let is_mobile = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, item.type_id);
            if is_mobile && item.location_id != ship_id {
                return Some(Err(BridgeRpcError {
                    code: "InvalidItemLocation".into(),
                    message: format!("Mobile deployable {item_id} must be deployed from ship cargo"),
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
            let corr5 = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id) >= 5;
            if item_type.group_id == 361 && (sys_sec >= 0.45 || (sys_sec > 0.0 && !corr5)) {
                return Some(Err(BridgeRpcError {
                    code: "CantAnchorInEmpire".into(),
                    message: format!(
                        "Mobile Warp Disruptor cannot be anchored in high-sec or low-sec system {sys_id} (security {sys_sec:.2} > 0.0)"
                    ),
                }));
            }
            if matches!(item_type.group_id, 12 | 340 | 448 | 649) && item_type.type_id != 23 && sys_sec >= 0.75 {
                return Some(Err(BridgeRpcError {
                    code: "CantAnchorInHighSec08".into(),
                    message: format!(
                        "Secure container cannot be anchored in high-sec system {sys_id} with security >= 0.8 (security {sys_sec:.2})"
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

            let is_mobile = crate::services::mobile_deployables::is_mobile_structure(&ctx.sde, item.type_id);
            if is_mobile {
                if let Err(e) = crate::services::mobile_deployables::check_mobile_deployment_restrictions(
                    ctx,
                    session,
                    item_id,
                    item.type_id,
                    item_type.group_id,
                    sys_id,
                    anchor_pos,
                ) {
                    return Some(Err(e));
                }
            }

            let is_warp_disruptor = item_type.group_id == 361;
            let bubble_radius = if is_warp_disruptor {
                item_type
                    .default_attributes
                    .get(&104)
                    .or_else(|| item_type.default_attributes.get(&103))
                    .copied()
                    .unwrap_or(20_000.0)
            } else {
                0.0
            };
            let anchoring_delay_s = (item_type
                .default_attributes
                .get(&556)
                .copied()
                .unwrap_or_else(|| {
                    if item_type.group_id == crate::services::mobile_deployables::GROUP_MOBILE_TRACTOR {
                        10_000.0
                    } else {
                        60_000.0
                    }
                })
                / 1_000.0)
                .max(1.0);
            let now_unix = ctx.now_unix();

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.get_mut(&item_id) {
                    it.location_id = sys_id as i64;
                    it.flag = InventoryFlag::Hangar;
                    it.singleton = true;
                }
                crate::sp6_lifecycle::record_anchor_meta_in_snap(
                    snap,
                    item_id,
                    now_unix,
                    anchoring_delay_s,
                    is_warp_disruptor.then_some(bubble_radius),
                );
                snap.item_positions
                    .insert(item_id, [anchor_pos.x, anchor_pos.y, anchor_pos.z]);
            });

            let is_secure_container = item_type.capacity > 0.0 && !is_warp_disruptor && !is_mobile;
            if is_secure_container {
                crate::services::combat_crimewatch::register_container_lifetime(
                    ctx,
                    item_id,
                    sys_id,
                    item.type_id,
                    char_id,
                    now_unix,
                    false,
                );
            }
            if is_warp_disruptor {
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
            } else {
                let preserved_damage = if is_mobile {
                    crate::services::mobile_deployables::register_mobile_deployable(
                        ctx,
                        item_id,
                        item.type_id,
                        item_type.group_id,
                        char_id,
                        sys_id,
                        anchoring_delay_s.ceil() as u64,
                        anchor_pos,
                    )
                } else {
                    None
                };
                let mut combat_state = ctx.build_type_combat_state(item.type_id);
                if let Some(dmg) = preserved_damage {
                    combat_state.shield_hp = dmg.shield_hp;
                    combat_state.armor_hp = dmg.armor_hp;
                    combat_state.structure_hp = dmg.structure_hp;
                }
                if let Err(e) = sys_handle
                    .spawn_ball_with_combat(
                        eve_destiny::Ball {
                            id: item_id,
                            type_id: item.type_id,
                            position: anchor_pos,
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: item_type.mass.max(10_000.0),
                            radius: item_type.radius.max(30.0),
                            speed_fraction: 0.0,
                            mode: eve_destiny::MovementMode::Stop,
                        },
                        combat_state,
                    )
                    .await
                {
                    return Some(Err(BridgeRpcError {
                        code: "SystemActorError".into(),
                        message: e,
                    }));
                }
            }

            let despawn_unix = if is_secure_container {
                now_unix.saturating_add(crate::sp6_lifecycle::SECURE_CONTAINER_DECAY_SECS)
            } else if is_warp_disruptor {
                now_unix.saturating_add(crate::sp6_lifecycle::sde_mobile_warp_disruptor_decay_secs(
                    &ctx.sde,
                    item.type_id,
                ))
            } else if item_type.group_id == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
                now_unix.saturating_add(crate::services::mobile_deployables::DEPOT_LIFETIME_SECS)
            } else if matches!(item_type.group_id, crate::services::mobile_deployables::GROUP_MOBILE_TRACTOR | crate::services::mobile_deployables::GROUP_MOBILE_MICRO_JUMP_UNIT) {
                now_unix.saturating_add(crate::services::mobile_deployables::MTU_LIFETIME_SECS)
            } else if item_type.group_id == crate::services::mobile_deployables::GROUP_MOBILE_CYNO_BEACON {
                if item.type_id == crate::services::mobile_deployables::TYPE_COVERT_MOBILE_CYNO_BEACON {
                    now_unix.saturating_add(crate::services::mobile_deployables::COVERT_CYNO_BEACON_DURATION_SECS)
                } else {
                    now_unix.saturating_add(crate::services::mobile_deployables::CYNO_BEACON_DURATION_SECS)
                }
            } else {
                0
            };

            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(item_id)),
                ("typeID", PyRep::Integer(item.type_id as i64)),
                ("groupID", PyRep::Integer(item_type.group_id as i64)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("state", PyRep::String("Anchoring".into())),
                ("anchoringDelaySeconds", PyRep::Real(anchoring_delay_s)),
                ("bubbleRadius", PyRep::Real(bubble_radius)),
                ("despawnUnix", PyRep::Integer(despawn_unix as i64)),
            ])))
        }

        _ => {
            if service == "posMgr" && method == "AnchorOrbital" {
                return Some(crate::services::station_structure_handlers::handle_anchor_orbital(
                    ctx, session, args,
                ));
            }
            if service == "posMgr" && method == "OnlineOrbital" {
                let res = crate::services::station_structure_handlers::handle_upgrade_gantry(
                    ctx, session, args,
                );
                return Some(res);
            }
            if service == "structure" && method == "JettisonStructureFuel" {
                return Some(structure_lifecycle_rpc::handle_jettison_structure_fuel(ctx, session, args).await);
            }
            if let Some(res) = super::structure_acl_defense_rules::handle(ctx, session, service, method, args, kwargs) {
                return Some(res);
            }
            structure_upwell_ops::handle(ctx, session, service, method, args, kwargs).await
        }
    }
}

/// Returns whether an NPC station's operation provides the given numeric station service ID (`sde.station_operations`).
pub fn npc_station_offers_service(sde: &eve_sde::SdeRegistry, station_id: i64, service_id: i64) -> bool {
    let Some(station) = sde.stations.get(&station_id) else {
        return false;
    };
    let Ok(sid) = u32::try_from(service_id) else {
        return false;
    };
    sde.station_operations
        .get(&station.operation_id)
        .map(|op| op.services.contains(&sid))
        .unwrap_or(false)
}

