//! `station_structure` — Domain 4: Stations, Docking/Undocking, Upwell Structures, Tethering, & POS (`§3.3`).

#![forbid(unsafe_code)]

use crate::formulas::clamp_upwell_structure_damage;
use crate::router::ServerContext;
use eve_db::UpwellStructureRecord;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::attr_ids;
use glam::DVec3;
use std::collections::HashMap;

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("stationSvc", "GetSolarSystem")
        | ("station", "GetSolarSystem")
        | ("stationSvc", "GetStation")
        | ("station", "GetStation") => {
            let _ = match ServerContext::require_auth(session) {
                Ok(u) => u,
                Err(e) => return Some(Err(e)),
            };
            let sys_id = match args.first().and_then(|a| a.as_i64()) {
                Some(id) => id as u32,
                None => match session.solar_system_id {
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
                Some(Ok(PyRep::None))
            }
        }

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

            let spawn_pos = ctx
                .sde
                .solar_systems
                .get(&ch.solar_system_id)
                .and_then(|sys| sys.stations.iter().find(|s| s.station_id == station_id))
                .map(|st| st.position + DVec3::new(1_500.0, 0.0, 0.0))
                .unwrap_or_else(|| DVec3::new(100_000.0, 0.0, 0.0));

            let ship_type = ctx.sde.types.get(&ship.type_id);
            let max_vel = ship_type
                .and_then(|t| t.default_attributes.get(&attr_ids::MAX_VELOCITY))
                .copied()
                .unwrap_or(365.0);
            let inertia = ship_type
                .and_then(|t| t.default_attributes.get(&attr_ids::AGILITY))
                .copied()
                .unwrap_or(3.45);
            let mass = ship_type.map(|t| t.mass).unwrap_or(1_067_000.0);
            let radius = ship_type.map(|t| t.radius).unwrap_or(37.0);

            let ball = Ball {
                id: ship.item_id,
                type_id: ship.type_id,
                position: spawn_pos,
                velocity: DVec3::new(max_vel * 0.5, 0.0, 0.0),
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 1.0,
                mode: MovementMode::GotoDirection {
                    direction: DVec3::X,
                },
            };

            let sys_handle = ctx.systems.get_or_spawn(ch.solar_system_id);
            let tick = match sys_handle.spawn_ball(ball).await {
                Ok(t) => t,
                Err(e) => {
                    ctx.db.rollback_undock(char_id, station_id, ship.item_id);
                    return Some(Err(BridgeRpcError {
                        code: "SystemActorError".into(),
                        message: e,
                    }));
                }
            };

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
            ])))
        }

        ("stationSvc", "Dock") | ("structureDocking", "Dock") | ("beyonce", "CmdDock") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let dock_id = args.first().and_then(|a| a.as_i64()).unwrap_or(60_003_760);
            let is_structure = service == "structureDocking" || dock_id >= 1_000_000_000_000;
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };

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
                DVec3::from_array(st.position)
            } else {
                let st_opt = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .and_then(|sys| sys.stations.iter().find(|s| s.station_id == dock_id))
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

            if let Some(ship_id) = session.ship_id {
                let handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ball))) = handle.get_ball_snapshot(ship_id).await {
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
                }
                let _ = handle.remove_ball(ship_id).await;
            }

            match ctx.db.dock_character(char_id, dock_id, is_structure) {
                Ok(_) => {
                    if is_structure {
                        session.structure_id = Some(dock_id);
                        session.station_id = None;
                    } else {
                        session.station_id = Some(dock_id);
                        session.structure_id = None;
                    }
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

        ("structureDeployment", "Anchor") | ("structureDeployment", "AnchorStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(35832) as u32;
            let name = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Astrahus Citadel");
            let sys_id = session.solar_system_id.unwrap_or(30_000_144);
            let corp_id = session.corporation_id.unwrap_or(1_000_125);

            let res = ctx.db.with_mut_snapshot(|snap| {
                // Verify and consume 1 structure item (`type_id`) and 1 Upwell Quantum Core (`56201`) owned by `char_id`
                let struct_item_id = snap
                    .items
                    .values()
                    .find(|i| i.owner_id == char_id && i.type_id == type_id && i.quantity >= 1)
                    .map(|i| i.item_id);
                let core_item_id = snap
                    .items
                    .values()
                    .find(|i| i.owner_id == char_id && i.type_id == 56201 && i.quantity >= 1)
                    .map(|i| i.item_id);

                let (Some(sid), Some(cid)) = (struct_item_id, core_item_id) else {
                    return Err(BridgeRpcError {
                        code: "MissingStructureOrCore".into(),
                        message: format!(
                            "Anchoring requires owning structure type {type_id} and Upwell Quantum Core (56201)"
                        ),
                    });
                };

                for consume_id in [sid, cid] {
                    if let Some(item) = snap.items.get_mut(&consume_id) {
                        if item.quantity > 1 {
                            item.quantity -= 1;
                            item.version += 1;
                        } else {
                            snap.items.remove(&consume_id);
                        }
                    }
                }

                snap.next_entity_id += 1;
                let id = 1_000_000_000_000 + snap.next_entity_id;
                snap.structures.insert(
                    id,
                    UpwellStructureRecord {
                        structure_id: id,
                        type_id,
                        owner_corp_id: corp_id,
                        solar_system_id: sys_id,
                        name: name.to_string(),
                        position: [50_000.0, 0.0, 0.0],
                        state: "ShieldVulnerable".into(),
                        quantum_core_installed: true,
                        fuel_blocks: 1_000,
                    },
                );
                Ok(id)
            });

            match res {
                Ok(structure_id) => Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(structure_id)),
                    ("state", PyRep::String("ShieldVulnerable".into())),
                    ("quantumCoreInstalled", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureControl", "AttackStructure") | ("structureService", "DamageStructure") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let structure_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(1_000_200_000_001);
            let incoming_dmg = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(25_000.0)
                .max(0.0);
            let clamped = clamp_upwell_structure_damage(incoming_dmg, 5_000.0, 1.0);
            let caller_sys = session.solar_system_id.unwrap_or(0);

            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(st) = snap.structures.get_mut(&structure_id) else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} does not exist"),
                    });
                };
                if st.solar_system_id != caller_sys {
                    return Err(BridgeRpcError {
                        code: "StructureWrongSystem".into(),
                        message: format!(
                            "Structure {structure_id} is in system {}, but caller is in {caller_sys}",
                            st.solar_system_id
                        ),
                    });
                }
                st.state = "ArmorReinforced".into();
                Ok(())
            });

            match res {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(structure_id)),
                    ("appliedDamage", PyRep::Real(clamped)),
                    ("state", PyRep::String("ArmorReinforced".into())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureBrowser", "GetStructures")
        | ("structureSettings", "GetCharacterStructures")
        | ("structureGuests", "GetGuests")
        | ("posMgr", "GetControlTowerFuelRequirements") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.structures
                    .values()
                    .map(|st| {
                        vec![
                            PyRep::Integer(st.structure_id),
                            PyRep::Integer(st.type_id as i64),
                            PyRep::Integer(st.solar_system_id as i64),
                            PyRep::String(st.name.clone()),
                            PyRep::String(st.state.clone()),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["structureID", "typeID", "solarSystemID", "name", "state"],
                rows,
            )))
        }

        _ => None,
    }
}
