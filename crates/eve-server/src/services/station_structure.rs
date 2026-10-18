//! `station_structure` — Domain 4: Stations, Docking/Undocking, Upwell Structures, Tethering, & POS (`§3.3`).

#![forbid(unsafe_code)]

use crate::formulas::clamp_upwell_structure_damage;
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, UpwellStructureRecord};
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
            let found = ctx.sde.solar_systems.values().find_map(|sys| {
                sys.stations
                    .iter()
                    .find(|st| st.station_id == station_id)
                    .map(|st| (sys.solar_system_id, st.clone()))
            });
            if let Some((sys_id, st)) = found {
                Some(Ok(PyRep::keyval([
                    ("stationID", PyRep::Integer(st.station_id)),
                    ("stationName", PyRep::String(st.name)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ])))
            } else {
                Some(Err(BridgeRpcError {
                    code: "StationNotFound".into(),
                    message: format!("Station {station_id} not found in SDE"),
                }))
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

            let spawn_pos = if station_id >= 1_000_000_000_000 {
                ctx.db
                    .with_snapshot(|snap| {
                        snap.structures
                            .get(&station_id)
                            .map(|st| DVec3::from_array(st.position) + DVec3::new(1_500.0, 0.0, 0.0))
                    })
                    .unwrap_or_else(|| DVec3::new(51_500.0, 0.0, 0.0))
            } else {
                ctx.sde
                    .solar_systems
                    .get(&ch.solar_system_id)
                    .and_then(|sys| sys.stations.iter().find(|s| s.station_id == station_id))
                    .map(|st| st.position + DVec3::new(1_500.0, 0.0, 0.0))
                    .unwrap_or_else(|| DVec3::new(100_000.0, 0.0, 0.0))
            };

            ctx.invalidate_ship_fitting(ship.item_id);
            let Some(ship_type) = ctx.sde.types.get(&ship.type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownShipType".into(),
                    message: format!("Active ship type {} not found in SDE", ship.type_id),
                }));
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

            let combat_state = ctx.build_ship_combat_state(char_id, ship.item_id, ship.type_id);
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

        ("structureDeployment", "Anchor") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Must be undocked in space to anchor an Upwell structure".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Anchoring requires an active ship in space".into(),
                }));
            };
            let raw_type_id = args.first().and_then(|a| a.as_i64()).unwrap_or(35832);
            let Ok(type_id) = u32::try_from(raw_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidStructureType".into(),
                    message: format!("Invalid structure type ID {raw_type_id}"),
                }));
            };
            let is_upwell_structure = ctx
                .sde
                .types
                .get(&type_id)
                .map(|t| t.category_id == 65)
                .unwrap_or(false);
            if !is_upwell_structure {
                return Some(Err(BridgeRpcError {
                    code: "InvalidStructureType".into(),
                    message: format!(
                        "Type {type_id} is not an anchorable Upwell structure (category 65)"
                    ),
                }));
            }
            let name = args
                .get(1)
                .and_then(|a| a.as_str())
                .unwrap_or("Astrahus Citadel");
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solarSystemID".into(),
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if type_id == 35840 && sys_sec >= 0.45 {
                return Some(Err(BridgeRpcError {
                    code: "CannotAnchorCynoBeaconInHighSec".into(),
                    message: format!(
                        "Pharolux Cyno Beacon (35840) cannot be anchored in high-security solar system {sys_id} (security {sys_sec:.2} >= 0.45)"
                    ),
                }));
            }

            let handle = ctx.systems.get_or_spawn(sys_id);
            let Some((_, ship_ball)) = (match handle.get_ball_snapshot(ship_id).await {
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
                    message: format!("Active ship {ship_id} is not spawned in space in system {sys_id}"),
                }));
            };
            let anchor_pos = ship_ball.position.to_array();

            let (sde_shield_hp, sde_armor_hp, sde_hull_hp) = ctx
                .sde
                .types
                .get(&type_id)
                .map(|t| {
                    (
                        t.default_attributes
                            .get(&eve_sde::attr_ids::SHIELD_CAPACITY)
                            .copied()
                            .unwrap_or(5_000.0)
                            .max(1.0),
                        t.default_attributes
                            .get(&eve_sde::attr_ids::ARMOR_HP)
                            .copied()
                            .unwrap_or(5_000.0)
                            .max(1.0),
                        t.default_attributes
                            .get(&eve_sde::attr_ids::HP_STRUCT)
                            .copied()
                            .unwrap_or(5_000.0)
                            .max(1.0),
                    )
                })
                .unwrap_or((5_000.0, 5_000.0, 5_000.0));

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                if corp_id < 98_000_000 || !snap.corporations.contains_key(&corp_id) {
                    return Err(BridgeRpcError {
                        code: "NpcCorpCannotAnchorStructure".into(),
                        message: format!(
                            "Character {char_id} is in NPC corporation {corp_id}; only player corporations (>= 98000000) may anchor Upwell structures"
                        ),
                    });
                }
                // Verify and consume 1 structure item (`type_id`) and 1 Upwell Quantum Core (`56201`) from active ship (`ship_id`)
                let struct_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.type_id == type_id
                            && i.quantity >= 1
                            && i.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.item_id);
                let core_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.type_id == 56201
                            && i.quantity >= 1
                            && i.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                    })
                    .map(|i| i.item_id);

                let (Some(sid), Some(cid)) = (struct_item_id, core_item_id) else {
                    return Err(BridgeRpcError {
                        code: "MissingStructureOrCore".into(),
                        message: format!(
                            "Anchoring requires structure type {type_id} and Upwell Quantum Core (56201) in active ship {ship_id} cargo"
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
                let now_unix = ctx.now_unix();
                snap.structures.insert(
                    id,
                    UpwellStructureRecord {
                        structure_id: id,
                        type_id,
                        owner_corp_id: corp_id,
                        solar_system_id: sys_id,
                        name: name.to_string(),
                        position: anchor_pos,
                        state: "ShieldVulnerable".into(),
                        quantum_core_installed: true,
                        fuel_blocks: 0,
                        shield_hp: sde_shield_hp,
                        armor_hp: sde_armor_hp,
                        hull_hp: sde_hull_hp,
                        reinforced_until_unix: 0,
                        last_damage_tick: 0,
                        last_damage_unix: now_unix,
                    },
                );
                Ok(id)
            });

            match res {
                Ok(structure_id) => Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(structure_id)),
                    ("state", PyRep::String("ShieldVulnerable".into())),
                    ("quantumCoreInstalled", PyRep::Bool(true)),
                    ("fuelBlocks", PyRep::Integer(0)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureControl", "FuelStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let structure_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.structure_id)
                .unwrap_or(0);
            let raw_blocks = args.get(1).and_then(|a| a.as_i64()).unwrap_or(10);
            let Ok(blocks_i32) = i32::try_from(raw_blocks) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFuelQuantity".into(),
                    message: format!("Invalid fuel block count {raw_blocks}"),
                }));
            };
            if blocks_i32 <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidFuelQuantity".into(),
                    message: "Fuel block count must be strictly positive".into(),
                }));
            }
            let blocks_to_load = blocks_i32 as u32;

            let caller_sys = session.solar_system_id.unwrap_or(0);
            let active_ship_id = session.ship_id;
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let caller_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                let Some((st_sys, st_owner_corp)) = snap
                    .structures
                    .get(&structure_id)
                    .map(|s| (s.solar_system_id, s.owner_corp_id))
                else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} does not exist"),
                    });
                };
                if st_owner_corp != caller_corp_id {
                    return Err(BridgeRpcError {
                        code: "StructureOwnershipViolation".into(),
                        message: format!(
                            "Only members of owning corporation {st_owner_corp} may fuel structure {structure_id}"
                        ),
                    });
                }
                if st_sys != caller_sys {
                    return Err(BridgeRpcError {
                        code: "StructureWrongSystem".into(),
                        message: format!(
                            "Structure {structure_id} is in system {st_sys}, but caller is in {caller_sys}"
                        ),
                    });
                }
                let fuel_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.type_id == 4247
                            && i.quantity >= blocks_i32
                            && i.flag != InventoryFlag::Locked
                            && !snap.is_item_locked_in_contract_or_job(i.item_id)
                            && (i.location_id == structure_id
                                || active_ship_id == Some(i.location_id))
                    })
                    .map(|i| i.item_id);
                let Some(fid) = fuel_item_id else {
                    return Err(BridgeRpcError {
                        code: "InsufficientFuelBlocks".into(),
                        message: format!(
                            "Fueling requires {blocks_to_load} Helium Fuel Block(s) (typeID 4247) in structure {structure_id} hangar or active ship cargo"
                        ),
                    });
                };

                let st = snap.structures.get_mut(&structure_id).unwrap();
                if st.state == "Destroyed" {
                    return Err(BridgeRpcError {
                        code: "StructureDestroyed".into(),
                        message: format!("Structure {structure_id} is destroyed"),
                    });
                }

                if let Some(item) = snap.items.get_mut(&fid) {
                    if item.quantity > blocks_i32 {
                        item.quantity -= blocks_i32;
                        item.version += 1;
                    } else {
                        snap.items.remove(&fid);
                    }
                }

                let st = snap.structures.get_mut(&structure_id).unwrap();
                st.fuel_blocks = st.fuel_blocks.saturating_add(blocks_to_load);
                Ok(st.fuel_blocks)
            });

            match res {
                Ok(fuel_blocks) => Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(structure_id)),
                    ("fuelBlocks", PyRep::Integer(fuel_blocks as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("structureControl", "AttackStructure") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot attack an Upwell structure while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(structure_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "MissingStructureId".into(),
                    message: "AttackStructure requires an explicit structureID argument".into(),
                }));
            };
            let caller_sys = session.solar_system_id.unwrap_or(0);

            // Derive siege DPS strictly from the attacker's ShipFittingEngine-validated online high-slot weapons (never raw flags or target HP)
            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let mut fitted_weapons: Vec<(u16, u32)> = fit
                .modules
                .values()
                .filter(|m| {
                    (27..=34).contains(&m.flag_id)
                        && m.state != eve_dogma::ModuleState::Offline
                        && ctx
                            .sde
                            .types
                            .get(&m.type_id)
                            .map(|t| {
                                matches!(
                                    t.group_id,
                                    53 | 55 | 74 | 506..=511 | 1986 | 4060 | 4500 | 4805 | 4807
                                )
                            })
                            .unwrap_or(false)
                })
                .map(|m| (m.flag_id, m.type_id))
                .collect();
            fitted_weapons.sort_by_key(|&(flag_id, wtid)| (flag_id, wtid));
            if fitted_weapons.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "NoWeaponFitted".into(),
                    message: format!("Active ship {ship_id} has no fitted high-slot weapon"),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(caller_sys);
            let Some((current_tick, ship_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is not spawned in space"),
                }));
            };

            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&caller_sys)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let mut total_raw_dps = 0.0_f64;
                let mut min_cycle_sec = f64::MAX;
                let mut armed_charge_item_ids: Vec<i64> = Vec::with_capacity(fitted_weapons.len());
                let mut skill_err: Option<BridgeRpcError> = None;

                for &(flag_id, wtid) in &fitted_weapons {
                    let Some(wt) = ctx.sde.types.get(&wtid) else {
                        continue;
                    };
                    let loaded_charge_opt = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == ship_id
                                && (i.flag as u16) == flag_id
                                && i.quantity > 0
                                && ctx
                                    .sde
                                    .types
                                    .get(&i.type_id)
                                    .map(|ct| {
                                        ServerContext::is_charge_compatible_with_weapon(wt, ct)
                                    })
                                    .unwrap_or(false)
                        })
                        .map(|i| (i.item_id, i.type_id));
                    let Some((cid, ctid)) = loaded_charge_opt else {
                        continue;
                    };
                    if let Err(e) =
                        ctx.check_type_skill_prerequisites_in_snapshot(snap, char_id, ctid)
                    {
                        if skill_err.is_none() {
                            skill_err = Some(e);
                        }
                        continue;
                    }
                    let Some(ct) = ctx.sde.types.get(&ctid) else {
                        continue;
                    };
                    let speed_ms = wt.default_attributes.get(&51).copied().unwrap_or(3000.0);
                    let cycle_sec = (speed_ms / 1000.0).max(1.0);
                    let mult = wt.default_attributes.get(&64).copied().unwrap_or(1.0);
                    let em = ct.default_attributes.get(&114).copied().unwrap_or(0.0);
                    let exp = ct.default_attributes.get(&116).copied().unwrap_or(0.0);
                    let kin = ct.default_attributes.get(&117).copied().unwrap_or(0.0);
                    let therm = ct.default_attributes.get(&118).copied().unwrap_or(0.0);
                    let volley = (em + exp + kin + therm) * mult;
                    if volley > 0.0 {
                        total_raw_dps += volley / cycle_sec;
                        min_cycle_sec = min_cycle_sec.min(cycle_sec);
                        armed_charge_item_ids.push(cid);
                    }
                }

                if total_raw_dps <= 0.0 || armed_charge_item_ids.is_empty() {
                    if let Some(e) = skill_err {
                        return Err(e);
                    }
                    return Err(BridgeRpcError {
                        code: "NoAmmoLoaded".into(),
                        message: format!(
                            "Active ship {ship_id} has fitted high-slot weapons but no compatible charge loaded in any weapon slot (flags 27..=34)"
                        ),
                    });
                }
                if !min_cycle_sec.is_finite() {
                    min_cycle_sec = 3.0;
                }

                let caller_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                let Some(st_owner_corp_id) =
                    snap.structures.get(&structure_id).map(|s| s.owner_corp_id)
                else {
                    return Err(BridgeRpcError {
                        code: "StructureNotFound".into(),
                        message: format!("Structure {structure_id} does not exist"),
                    });
                };
                let has_active_war = snap.wars.values().any(|w| {
                    w.active
                        && now_unix >= w.started_unix.saturating_add(86_400)
                        && ((w.aggressor_corp_id == caller_corp_id
                            && w.defender_corp_id == st_owner_corp_id)
                            || (w.aggressor_corp_id == st_owner_corp_id
                                && w.defender_corp_id == caller_corp_id))
                });

                let (st_sys, st_owner, st_state, st_reinf, st_last_tick, st_pos_arr, st_type_id) = {
                    let Some(st) = snap.structures.get(&structure_id) else {
                        return Err(BridgeRpcError {
                            code: "StructureNotFound".into(),
                            message: format!("Structure {structure_id} does not exist"),
                        });
                    };
                    (
                        st.solar_system_id,
                        st.owner_corp_id,
                        st.state.clone(),
                        st.reinforced_until_unix,
                        st.last_damage_tick,
                        st.position,
                        st.type_id,
                    )
                };
                if st_sys != caller_sys {
                    return Err(BridgeRpcError {
                        code: "StructureWrongSystem".into(),
                        message: format!(
                            "Structure {structure_id} is in system {st_sys}, but caller is in {caller_sys}"
                        ),
                    });
                };
                if st_owner == caller_corp_id {
                    return Err(BridgeRpcError {
                        code: "CannotAttackFriendlyStructure".into(),
                        message: "Cannot attack a structure owned by your own corporation".into(),
                    });
                }
                if sys_sec >= 0.45 && !has_active_war {
                    return Err(BridgeRpcError {
                        code: "IllegalHighSecStructureAttack".into(),
                        message: "Cannot attack an Upwell structure in high-security space without an active war declaration (24h warmup complete)".into(),
                    });
                }
                if st_state == "Destroyed" {
                    return Err(BridgeRpcError {
                        code: "StructureAlreadyDestroyed".into(),
                        message: format!("Structure {structure_id} is already destroyed"),
                    });
                }
                if now_unix < st_reinf {
                    return Err(BridgeRpcError {
                        code: "StructureReinforcedInvulnerable".into(),
                        message: format!(
                            "Structure {structure_id} is reinforced and invulnerable until {st_reinf}"
                        ),
                    });
                }
                let cycle_ticks = (min_cycle_sec.ceil() as u64).max(1);
                let effective_tick = current_tick.max(1);
                if st_last_tick > 0
                    && effective_tick < st_last_tick.saturating_add(cycle_ticks)
                {
                    return Err(BridgeRpcError {
                        code: "WeaponCycling".into(),
                        message: format!(
                            "Weapons are cycling against structure {structure_id} until tick {} (current tick {effective_tick})",
                            st_last_tick.saturating_add(cycle_ticks)
                        ),
                    });
                }
                let st_pos = DVec3::from_array(st_pos_arr);
                let dist = ship_ball.position.distance(st_pos);
                if dist > 50_000.0 {
                    return Err(BridgeRpcError {
                        code: "StructureOutOfRange".into(),
                        message: format!(
                            "Ship is {:.1} m from structure {structure_id} (must be <= 50000 m)",
                            dist
                        ),
                    });
                }

                // Consume 1 round of loaded charge per fired weapon inside the same transactional snapshot
                for cid in &armed_charge_item_ids {
                    if let Some(ch_item) = snap.items.get_mut(cid) {
                        if ch_item.quantity > 1 {
                            ch_item.quantity -= 1;
                            ch_item.version += 1;
                        } else {
                            snap.items.remove(cid);
                        }
                    }
                }

                // Upwell structure damage cap: 5,000 DPS for Medium structures (Astrahus 35832, Athanor 35835, Raitaru 35825),
                // 15,000 DPS for Large structures, 75,000 DPS for XL structures.
                // Apply strictly one weapon cycle (min_cycle_sec) of volley damage per call.
                let damage_cap_dps = match st_type_id {
                    35832 | 35835 | 35825 => 5_000.0,
                    35833 | 35836 | 35826 => 15_000.0,
                    _ => 75_000.0,
                };
                let clamped = clamp_upwell_structure_damage(
                    total_raw_dps * min_cycle_sec,
                    damage_cap_dps,
                    min_cycle_sec,
                );

                let st = snap.structures.get_mut(&structure_id).unwrap();
                st.last_damage_tick = effective_tick;
                st.last_damage_unix = now_unix;
                let mut dropped_core_id = 0i64;
                match st.state.as_str() {
                    "ShieldVulnerable" => {
                        st.shield_hp = (st.shield_hp - clamped).max(0.0);
                        if st.shield_hp <= 0.0 {
                            st.state = "ArmorReinforced".into();
                            st.reinforced_until_unix = now_unix + 86_400;
                        }
                    }
                    "ArmorReinforced" => {
                        st.armor_hp = (st.armor_hp - clamped).max(0.0);
                        if st.armor_hp <= 0.0 {
                            st.state = "HullReinforced".into();
                            st.reinforced_until_unix = now_unix + 172_800;
                        }
                    }
                    "HullReinforced" => {
                        st.hull_hp = (st.hull_hp - clamped).max(0.0);
                        if st.hull_hp <= 0.0 {
                            st.state = "Destroyed".into();
                            if st.quantum_core_installed {
                                st.quantum_core_installed = false;
                                let sys_loc = st.solar_system_id as i64;
                                snap.next_entity_id += 1;
                                dropped_core_id = snap.next_entity_id;
                                snap.items.insert(
                                    dropped_core_id,
                                    InventoryItemRecord {
                                        item_id: dropped_core_id,
                                        type_id: 56201, // Astrahus Upwell Quantum Core
                                        owner_id: char_id,
                                        location_id: sys_loc,
                                        flag: InventoryFlag::Hangar,
                                        quantity: 1,
                                        singleton: true,
                                        version: 1,
                                        dynamic_attributes: HashMap::new(),
                                        source_mutaplasmid_id: None,
                                        insured_payout_isk: 0.0,
                                        is_blueprint_copy: false,
                                        blueprint_runs_remaining: None,
                                    },
                                );
                            }
                        }
                    }
                    _ => {}
                }
                Ok((
                    st.state.clone(),
                    st.shield_hp,
                    st.armor_hp,
                    st.hull_hp,
                    st.reinforced_until_unix,
                    dropped_core_id,
                    clamped,
                ))
            });

            match res {
                Ok((
                    new_state,
                    shield_hp,
                    armor_hp,
                    hull_hp,
                    reinforced_until,
                    dropped_core_id,
                    clamped,
                )) => Some(Ok(PyRep::keyval([
                    ("structureID", PyRep::Integer(structure_id)),
                    ("appliedDamage", PyRep::Real(clamped)),
                    ("state", PyRep::String(new_state)),
                    ("shieldHP", PyRep::Real(shield_hp)),
                    ("armorHP", PyRep::Real(armor_hp)),
                    ("hullHP", PyRep::Real(hull_hp)),
                    ("reinforcedUntilUnix", PyRep::Integer(reinforced_until as i64)),
                    ("droppedQuantumCoreItemID", PyRep::Integer(dropped_core_id)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
