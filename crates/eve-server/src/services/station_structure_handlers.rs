//! `station_structure_handlers` — Helper routines for orbital and station structure operations.

use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub fn handle_upgrade_gantry(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "MustBeInSpace".into(),
            message: "Character must be undocked in space to upgrade or online an orbital gantry".into(),
        });
    }
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Character is not in an active solar system".into(),
    })?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Character has no active ship in space".into(),
    })?;
    let orbital_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let upgrade_item_arg = args.get(1).and_then(|a| a.as_i64());

    let (tid, pid) = ctx.db.try_with_mut_snapshot(|snap| {
        let char_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if char_corp <= 0 {
            return Err(BridgeRpcError {
                code: "CrpAccessDenied".into(),
                message: "Character has no valid corporation".into(),
            });
        }
        let req_roles = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
        let has_role = session.has_gm_role()
            || (session.corp_role & req_roles) != 0
            || snap.corporations.get(&char_corp).is_some_and(|c| {
                c.ceo_id == char_id
                    || c.member_roles.get(&char_id).is_some_and(|&r| (r & req_roles) != 0)
            })
            || snap
                .corp_role_assignments
                .get(&(char_corp, char_id))
                .is_some_and(|a| (a.roles & req_roles) != 0);
        if !has_role {
            return Err(BridgeRpcError {
                code: "CrpAccessDenied".into(),
                message: "Upgrading or onlining an orbital gantry requires Director or Station Manager role".into(),
            });
        }

        let (cur_tid, planet_id) = {
            let it = snap.items.get(&orbital_id).ok_or_else(|| BridgeRpcError {
                code: "OrbitalNotFound".into(),
                message: format!("Orbital {orbital_id} not found"),
            })?;
            if it.type_id != 3962 {
                return Err(BridgeRpcError {
                    code: "InvalidOrbitalGantry".into(),
                    message: format!(
                        "Target item {orbital_id} (type {}) is not an upgradable Customs Office Gantry",
                        it.type_id
                    ),
                });
            }
            if it.owner_id != char_corp && it.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "OrbitalOwnershipDenied".into(),
                    message: format!("Corporation {char_corp} does not own orbital {orbital_id}"),
                });
            }
            let st = snap.structures.get(&orbital_id).ok_or_else(|| BridgeRpcError {
                code: "OrbitalNotAnchored".into(),
                message: format!("Orbital {orbital_id} is not an anchored structure in space"),
            })?;
            if st.type_id != 3962 {
                return Err(BridgeRpcError {
                    code: "OrbitalAlreadyUpgraded".into(),
                    message: format!("Orbital {orbital_id} is not an un-upgraded gantry"),
                });
            }
            if st.owner_corp_id != char_corp {
                return Err(BridgeRpcError {
                    code: "OrbitalOwnershipDenied".into(),
                    message: format!("Corporation {char_corp} does not own orbital structure {orbital_id}"),
                });
            }
            if st.solar_system_id != sys_id {
                return Err(BridgeRpcError {
                    code: "OrbitalWrongSystem".into(),
                    message: format!(
                        "Orbital {orbital_id} is in solar system {}, not {sys_id}",
                        st.solar_system_id
                    ),
                });
            }
            if matches!(st.state.as_str(), "Destroyed" | "Unanchored") {
                return Err(BridgeRpcError {
                    code: "InvalidOrbitalState".into(),
                    message: format!("Orbital {orbital_id} is in invalid state '{}'", st.state),
                });
            }
            if let Some(ship_pos) = ctx.systems.get_ball_position_sync(sys_id, ship_id) {
                let dist = ship_pos.distance(glam::DVec3::from_array(st.position));
                if dist > 10_000.0 {
                    return Err(BridgeRpcError {
                        code: "OrbitalOutOfRange".into(),
                        message: format!(
                            "Ship {ship_id} is {dist:.0}m from orbital {orbital_id} (max 10,000m)"
                        ),
                    });
                }
            }
            (it.type_id, it.location_id)
        };
        let target_tid = ctx
            .sde
            .types
            .get(&cur_tid)
            .and_then(|t| t.default_attributes.get(&1771).copied())
            .map(|v| v as u32)
            .unwrap_or(2233);
        let is_valid_upgrade_item = |u: &eve_db::InventoryItemRecord| -> bool {
            (u.type_id == target_tid || u.type_id == 2233 || u.type_id == 3963)
                && u.location_id == ship_id
                && u.flag == eve_db::InventoryFlag::Cargo
                && (u.owner_id == char_id || u.owner_id == char_corp)
                && u.quantity >= 1
                && u.flag != eve_db::InventoryFlag::Locked
                && !snap.is_item_locked_in_contract_or_job(u.item_id)
        };
        let up_item_opt = if let Some(arg_iid) = upgrade_item_arg.filter(|&id| id != planet_id) {
            let u = snap.items.get(&arg_iid).ok_or_else(|| BridgeRpcError {
                code: "CustomsOfficeItemRequired".into(),
                message: format!("Upgrade item {arg_iid} not found"),
            })?;
            if !is_valid_upgrade_item(u) {
                return Err(BridgeRpcError {
                    code: "CustomsOfficeItemRequired".into(),
                    message: format!(
                        "Upgrade item {arg_iid} must be an unlocked type {target_tid} owned by caller in active ship cargo"
                    ),
                });
            }
            Some(arg_iid)
        } else {
            snap.items
                .values()
                .find(|u| is_valid_upgrade_item(u))
                .map(|u| u.item_id)
        };
        let Some(up_iid) = up_item_opt else {
            return Err(BridgeRpcError {
                code: "CustomsOfficeItemRequired".into(),
                message: format!("Upgrading gantry requires item type {target_tid} in active ship cargo"),
            });
        };
        if let Some(u) = snap.items.get_mut(&up_iid) {
            if u.quantity > 1 {
                u.quantity -= 1;
            } else {
                snap.items.remove(&up_iid);
            }
        }
        if let Some(it) = snap.items.get_mut(&orbital_id) {
            it.type_id = target_tid;
        }
        if let Some(st) = snap.structures.get_mut(&orbital_id) {
            st.type_id = target_tid;
            st.state = "ShieldVulnerable".into();
            st.shield_hp = 10_000_000.0;
            st.armor_hp = 2_500_000.0;
            st.hull_hp = 2_000_000.0;
        }
        let cfg = crate::services::industry_pi::OrbitalCustomsSettings {
            planet_id,
            orbital_type_id: target_tid,
            owner_corp_id: char_corp,
            ..Default::default()
        };
        crate::services::industry_pi::save_orbital_settings(snap, planet_id, &cfg);
        Ok((target_tid, planet_id))
    })?;

    Ok(PyRep::keyval([
        ("orbitalID", PyRep::Integer(orbital_id)),
        ("planetID", PyRep::Integer(pid)),
        ("typeID", PyRep::Integer(i64::from(tid))),
        ("state", PyRep::String("ShieldVulnerable".into())),
    ]))
}

pub fn handle_anchor_orbital(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(sys_id) = session.solar_system_id else {
        return Err(BridgeRpcError {
            code: "NoSolarSystem".into(),
            message: "Session has no active solarSystemID".into(),
        });
    };
    let (_, is_thera, is_shattered, _) =
        crate::services::space_destiny::system_wormhole_profile(sys_id);
    if is_thera || is_shattered {
        return Err(BridgeRpcError {
            code: "AnchoringDisallowedInTheraOrShattered".into(),
            message: format!(
                "Anchoring starbases or orbitals is disallowed in Thera or shattered wormhole system {sys_id}"
            ),
        });
    }
    let region_id = ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id).unwrap_or(0);
    if region_id == structure_lifecycle::POCHVEN_REGION_ID {
        if session.station_id.is_some() || session.structure_id.is_some() {
            return Err(BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Must be in space to anchor an orbital structure".into(),
            });
        }
        let trig_standing = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .and_then(|c| c.standings.get(&500_026).copied())
                .unwrap_or(0.0)
        });
        if trig_standing < 1.5 {
            return Err(BridgeRpcError {
                code: "InsufficientTriglavianStanding".into(),
                message: format!(
                    "Anchoring orbitals in Pochven requires at least 1.5 standing with the Triglavian Collective (500026); pilot has {trig_standing:.2}"
                ),
            });
        }
    }
    let item_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "MissingArgument".into(),
        message: "AnchorOrbital requires an orbital itemID".into(),
    })?;

    let is_gantry = ctx.db.with_snapshot(|snap| {
        snap.items.get(&item_id).is_some_and(|it| {
            it.type_id == 3962
                || ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 1106)
        })
    });
    if !is_gantry {
        return Err(BridgeRpcError {
            code: "StarbaseRemoved".into(),
            message: "Legacy POS starbases and control towers are superseded by Upwell structures in V24.01".into(),
        });
    }

    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
    if sys_sec >= 0.45 && sys_id < 31_000_000 {
        return Err(BridgeRpcError {
            code: "CannotAnchorOrbitalInHighSec".into(),
            message: format!("Customs Office Gantry cannot be anchored in high-sec system {sys_id}"),
        });
    }

    let ship_pos = session.ship_id.and_then(|sid| ctx.systems.get_ball_position_sync(sys_id, sid));
    let nearest_planet = ship_pos.and_then(|sp| {
        ctx.sde.celestials_in_system(sys_id).into_iter().filter(|c| c.group_id == 7).min_by(|a, b| {
            (a.position - sp).length_squared().partial_cmp(&(b.position - sp).length_squared()).unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    let planet_id = args.get(1).and_then(|a| a.as_i64())
        .or_else(|| nearest_planet.as_ref().map(|p| p.celestial_id))
        .or_else(|| ctx.sde.solar_systems.get(&sys_id).and_then(|s| s.planet_ids.first().copied()))
        .unwrap_or(40_000_001);
    if let (Some(sp), Some(np)) = (ship_pos, nearest_planet) {
        if args.get(1).is_none() || args.get(1).and_then(|a| a.as_i64()) == Some(np.celestial_id) {
            let dist = (sp - np.position).length();
            let max_dist = np.radius + 100_000_000.0;
            if dist > max_dist {
                return Err(BridgeRpcError {
                    code: "OrbitalOutOfRange".into(),
                    message: format!("Ship is {dist:.0}m from planet {} (must be within {max_dist:.0}m to anchor orbital)", np.celestial_id),
                });
            }
        }
    }

    let (tid, pid) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.structures.contains_key(&item_id) {
            return Err(BridgeRpcError {
                code: "AlreadyAnchored".into(),
                message: format!("Orbital structure {item_id} is already anchored in space"),
            });
        }
        let tag = format!("Orbital:{planet_id}");
        if snap.structures.values().any(|st| {
            st.solar_system_id == sys_id
                && st.state != "Destroyed"
                && st.name == tag
        }) {
            return Err(BridgeRpcError {
                code: "PlanetAlreadyHasOrbital".into(),
                message: format!("Planet {planet_id} already has an anchored orbital"),
            });
        }
        let char_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(98_000_001);
        let it = snap.items.get_mut(&item_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Gantry item {item_id} not found"),
        })?;
        if it.owner_id != char_id && it.owner_id != char_corp {
            return Err(BridgeRpcError {
                code: "NotItemOwner".into(),
                message: format!("Character {char_id} does not own item {item_id}"),
            });
        }
        if let Some(active_ship) = session.ship_id {
            if it.location_id != active_ship || it.flag != eve_db::InventoryFlag::Cargo {
                return Err(BridgeRpcError {
                    code: "ItemNotInCargo".into(),
                    message: format!("Gantry item {item_id} must be in the cargo hold of active ship {active_ship}"),
                });
            }
        }
        let tid = it.type_id;
        it.location_id = planet_id;
        it.flag = eve_db::InventoryFlag::None;
        it.singleton = true;
        snap.structures.insert(
            item_id,
            eve_db::UpwellStructureRecord {
                structure_id: item_id,
                type_id: tid,
                owner_corp_id: char_corp,
                solar_system_id: sys_id,
                name: tag,
                position: ctx
                    .systems
                    .get_ball_position_sync(sys_id, session.ship_id.unwrap_or(0))
                    .map(|p| p.to_array())
                    .unwrap_or([200_000.0, 0.0, 0.0]),
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 1_000,
                shield_hp: 500_000.0,
                armor_hp: 500_000.0,
                hull_hp: 300_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
                self_destruct_at_unix: None,
            },
        );
        Ok((tid, planet_id))
    })?;

    Ok(PyRep::keyval([
        ("itemID", PyRep::Integer(item_id)),
        ("orbitalID", PyRep::Integer(item_id)),
        ("planetID", PyRep::Integer(pid)),
        ("typeID", PyRep::Integer(i64::from(tid))),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("state", PyRep::String("Anchored".into())),
    ]))
}
