//! `cosmetics_events` — Domain 12: Ship SKINs, Paragon Hub, SKINR Studio, Events, & Living World Director (`§3.10`, `§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
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
        // Living World NPC Ecology & Supply-Chain Convoy Interdiction (`taleMgr` / `E2E-17`)
        ("taleMgr", "MaterializeSystemLOD2") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(session_sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "UnknownSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            if let Some(arg_sys) = args.first().and_then(|a| a.as_i64()) {
                let Ok(req_sys_id) = u32::try_from(arg_sys) else {
                    return Some(Err(BridgeRpcError {
                        code: "UnknownSolarSystem".into(),
                        message: format!("Invalid solar system ID {arg_sys}"),
                    }));
                };
                if !ctx.sde.solar_systems.contains_key(&req_sys_id) {
                    return Some(Err(BridgeRpcError {
                        code: "UnknownSolarSystem".into(),
                        message: format!("Solar system {req_sys_id} does not exist in SDE"),
                    }));
                }
                if req_sys_id != session_sys_id {
                    return Some(Err(BridgeRpcError {
                        code: "CannotMaterializeRemoteSystem".into(),
                        message: format!(
                            "Cannot materialize LOD 2 in remote solar system {req_sys_id} while in system {session_sys_id}"
                        ),
                    }));
                }
            }
            if !ctx.sde.solar_systems.contains_key(&session_sys_id) {
                return Some(Err(BridgeRpcError {
                    code: "UnknownSolarSystem".into(),
                    message: format!("Solar system {session_sys_id} does not exist in SDE"),
                }));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "taleMgr.MaterializeSystemLOD2 requires GM role; normal players trigger LOD 2 automatically on system entry or undock".into(),
                }));
            }
            let count = ctx
                .living_world
                .materialize_system_lod2(session_sys_id, &ctx.systems, &ctx.sde)
                .await;
            Some(Ok(PyRep::Integer(count as i64)))
        }

        ("taleMgr", "InterdictConvoy") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "taleMgr.InterdictConvoy requires GM role; normal players must destroy convoys in space via combat".into(),
                }));
            }
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot interdict a space convoy while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let Some(convoy_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "InterdictConvoy requires convoyID".into(),
                }));
            };
            let Some(convoy) = ctx.living_world.get_convoy(convoy_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ConvoyNotFound".into(),
                    message: format!("Convoy {convoy_id} does not exist"),
                }));
            };
            if session.solar_system_id != Some(convoy.current_system_id) {
                return Some(Err(BridgeRpcError {
                    code: "ConvoyWrongSystem".into(),
                    message: format!(
                        "Convoy {convoy_id} is in system {}, but attacker is in {:?}",
                        convoy.current_system_id, session.solar_system_id
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(convoy.current_system_id);
            let Some((_, ship_ball)) = (match sys_handle.get_ball_snapshot(ship_id).await {
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
                    message: "Attacker ship is not in space".into(),
                }));
            };

            let Some((_, convoy_ball)) = (match sys_handle.get_ball_snapshot(convoy_id).await {
                Ok(v) => v,
                Err(e) => {
                    return Some(Err(BridgeRpcError {
                        code: "DestinyQueryFailed".into(),
                        message: e,
                    }))
                }
            }) else {
                return Some(Err(BridgeRpcError {
                    code: "ConvoyNotMaterializedOnGrid".into(),
                    message: format!(
                        "Convoy {convoy_id} is not materialized on grid in Ballpark"
                    ),
                }));
            };

            let dist = ship_ball.position.distance(convoy_ball.position);
            if dist > 30_000.0 {
                return Some(Err(BridgeRpcError {
                    code: "OutsideConvoyInterdictionRange".into(),
                    message: format!(
                        "Ship is {:.1} m from convoy {convoy_id} (must be <= 30000 m)",
                        dist
                    ),
                }));
            }

            let _ = sys_handle.remove_ball(convoy_id).await;
            match ctx
                .living_world
                .interdict_convoy(convoy_id, char_id, &ctx.db)
            {
                Ok((wreck_item_id, loot_item_id, spiked_ask)) => {
                    let _ = sys_handle
                        .spawn_ball_with_combat(
                            Ball {
                                id: wreck_item_id,
                                type_id: 26557,
                                position: convoy_ball.position,
                                velocity: DVec3::ZERO,
                                max_velocity: 0.0,
                                inertia: 1.0,
                                mass: 10_000.0,
                                radius: 50.0,
                                speed_fraction: 0.0,
                                mode: MovementMode::Stop,
                            },
                            CombatState {
                                shield_hp: 0.0,
                                max_shield_hp: 0.0,
                                armor_hp: 0.0,
                                max_armor_hp: 0.0,
                                structure_hp: 500.0,
                                max_structure_hp: 500.0,
                                ..CombatState::default()
                            },
                        )
                        .await;
                    Some(Ok(PyRep::keyval([
                        ("convoyID", PyRep::Integer(convoy_id)),
                        ("wreckItemID", PyRep::Integer(wreck_item_id)),
                        ("lootItemID", PyRep::Integer(loot_item_id)),
                        ("newDestinationAskISK", PyRep::Real(spiked_ask.to_wire_f64())),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InterdictionFailed".into(),
                    message: e,
                })),
            }
        }

        ("shipCosmeticsMgr", "GiveSkin") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "shipCosmeticsMgr.GiveSkin requires GM role".into(),
                }));
            }
            let Some(raw_skin) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: "GiveSkin requires skinID".into(),
                }));
            };
            let Ok(skin_id) = u32::try_from(raw_skin) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!("Invalid SKIN ID {raw_skin}"),
                }));
            };
            if !ctx.sde.skins.contains_key(&skin_id) && !ctx.sde.skin_licenses.contains_key(&skin_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!("SKIN or skin license {skin_id} does not exist in SDE"),
                }));
            }
            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                let list = snap.licensed_skins.entry(char_id).or_default();
                if !list.contains(&skin_id) {
                    list.push(skin_id);
                }
                if let Some(lic) = ctx.sde.skin_licenses.get(&skin_id) {
                    if !list.contains(&lic.skin_id) {
                        list.push(lic.skin_id);
                    }
                }
                Ok::<_, std::convert::Infallible>(())
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("skinID", PyRep::Integer(skin_id as i64)),
                ("licensed", PyRep::Bool(true)),
            ])))
        }

        ("shipCosmeticsMgr", "ActivateSkinLicense") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(raw_skin_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: "ActivateSkinLicense requires skin license typeID".into(),
                }));
            };
            let Ok(requested_skin_id) = u32::try_from(raw_skin_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!("Invalid SKIN license typeID {raw_skin_id}"),
                }));
            };
            let Some(skin_type) = ctx.sde.types.get(&requested_skin_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!("Type {requested_skin_id} not found in SDE"),
                }));
            };
            if skin_type.category_id != 91 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!(
                        "Type {requested_skin_id} (category {}) is not a Permanent SKIN license (category 91)",
                        skin_type.category_id
                    ),
                }));
            }
            let Some(lic_info) = ctx.sde.skin_licenses.get(&requested_skin_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!("Type {requested_skin_id} is not mapped to any SKIN in SDE skin_licenses"),
                }));
            };
            if !ctx.sde.skins.contains_key(&lic_info.skin_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidSkinLicenseType".into(),
                    message: format!("SKIN ID {} mapped by license {requested_skin_id} does not exist in SDE skins", lic_info.skin_id),
                }));
            }
            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let license_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.type_id == requested_skin_id
                            && i.quantity >= 1
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(item_id) = license_item_id else {
                    return Err(BridgeRpcError {
                        code: "SkinLicenseItemNotFound".into(),
                        message: format!(
                            "Character {char_id} does not own SKIN license item (typeID {requested_skin_id}) at current station/structure or active ship"
                        ),
                    });
                };

                if let Some(item) = snap.items.get_mut(&item_id) {
                    if item.quantity > 1 {
                        item.quantity -= 1;
                        item.version += 1;
                    } else {
                        snap.items.remove(&item_id);
                    }
                }

                let list = snap.licensed_skins.entry(char_id).or_default();
                if !list.contains(&requested_skin_id) {
                    list.push(requested_skin_id);
                }
                if !list.contains(&lic_info.skin_id) {
                    list.push(lic_info.skin_id);
                }
                Ok(requested_skin_id)
            });

            match res {
                Ok(skin_id) => Some(Ok(PyRep::keyval([
                    ("characterID", PyRep::Integer(char_id)),
                    ("skinID", PyRep::Integer(skin_id as i64)),
                    ("skinHashID", PyRep::Integer(skin_id as i64)),
                    ("licensed", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("shipCosmeticsMgr", "ApplySkinToShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(raw_ship_type) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidShipTypeForSkin".into(),
                    message: "ApplySkinToShip requires shipTypeID".into(),
                }));
            };
            let Ok(ship_type_id) = u32::try_from(raw_ship_type) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidShipTypeForSkin".into(),
                    message: format!("Invalid ship typeID {raw_ship_type}"),
                }));
            };
            if !ctx
                .sde
                .types
                .get(&ship_type_id)
                .map(|t| t.category_id == 6)
                .unwrap_or(false)
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidShipTypeForSkin".into(),
                    message: format!("Type {ship_type_id} is not a valid ship type in SDE"),
                }));
            }

            // Resolve requested SKIN strictly to an SDE category 91 SKIN type_id or skin_id
            let (target_skin_id, resolved_skin_id, design_name) = match args.get(1) {
                Some(PyRep::Integer(id)) => {
                    let Ok(sid) = u32::try_from(*id) else {
                        return Some(Err(BridgeRpcError {
                            code: "SkinNotLicensed".into(),
                            message: format!("Invalid SKIN ID {id}"),
                        }));
                    };
                    if let Some(lic) = ctx.sde.skin_licenses.get(&sid) {
                        let name = ctx.sde.types.get(&sid).map(|t| t.name.clone()).unwrap_or_else(|| format!("SKIN {sid}"));
                        (sid, lic.skin_id, name)
                    } else if let Some(skin) = ctx.sde.skins.get(&sid) {
                        (sid, sid, skin.internal_name.clone())
                    } else {
                        let is_cat91 = ctx.sde.types.get(&sid).filter(|t| t.category_id == 91);
                        if let Some(st) = is_cat91 {
                            (sid, sid, st.name.clone())
                        } else {
                            return Some(Err(BridgeRpcError {
                                code: "SkinNotLicensed".into(),
                                message: format!("SKIN ID {sid} is not a valid SDE SKIN"),
                            }));
                        }
                    }
                }
                Some(PyRep::String(name)) => {
                    let found = ctx
                        .sde
                        .types
                        .values()
                        .find(|t| t.category_id == 91 && t.name.eq_ignore_ascii_case(name));
                    let Some(st) = found else {
                        return Some(Err(BridgeRpcError {
                            code: "SkinNotLicensed".into(),
                            message: format!(
                                "Character {char_id} has not activated a license for SKIN '{name}'"
                            ),
                        }));
                    };
                    let skin_id = ctx.sde.skin_licenses.get(&st.type_id).map(|l| l.skin_id).unwrap_or(st.type_id);
                    (st.type_id, skin_id, st.name.clone())
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "SkinNotLicensed".into(),
                        message: "ApplySkinToShip requires a valid SKIN ID or design name".into(),
                    }));
                }
            };

            if let Some(skin_info) = ctx.sde.skins.get(&resolved_skin_id) {
                if !skin_info.types.is_empty() && !skin_info.types.contains(&ship_type_id) {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidShipTypeForSkin".into(),
                        message: format!(
                            "SKIN '{}' (id {resolved_skin_id}) cannot be applied to ship type {ship_type_id}",
                            skin_info.internal_name
                        ),
                    }));
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let active_ship = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                let ship_item = snap.items.get(&active_ship).ok_or_else(|| BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Active ship {active_ship} not found"),
                })?;
                if ship_item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!("Ship {active_ship} is not owned by character {char_id}"),
                    });
                }
                if ship_item.type_id != ship_type_id {
                    return Err(BridgeRpcError {
                        code: "ShipTypeMismatch".into(),
                        message: format!(
                            "Active ship {active_ship} has type {}, cannot apply skin for type {ship_type_id}",
                            ship_item.type_id
                        ),
                    });
                }
                let is_licensed = snap
                    .licensed_skins
                    .get(&char_id)
                    .map(|list| list.contains(&target_skin_id) || list.contains(&resolved_skin_id))
                    .unwrap_or(false);
                if !is_licensed {
                    return Err(BridgeRpcError {
                        code: "SkinNotLicensed".into(),
                        message: format!(
                            "Character {char_id} has not activated a license for SKIN '{design_name}' ({target_skin_id})"
                        ),
                    });
                }
                snap.applied_ship_skins.insert(active_ship, target_skin_id);
                Ok((active_ship, target_skin_id))
            });
            match res {
                Ok((active_ship, skin_id)) => Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(active_ship)),
                    ("shipTypeID", PyRep::Integer(ship_type_id as i64)),
                    ("skinID", PyRep::Integer(skin_id as i64)),
                    ("designName", PyRep::String(design_name)),
                    ("sequenced", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        _ => None,
    }
}
