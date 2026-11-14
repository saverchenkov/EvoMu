//! `station_structure_handlers` — Helper routines for orbital and station structure operations.

use crate::router::ServerContext;
use crate::services::station_structure::{
    structure_ansiblex_navigation,
    structure_lifecycle,
    structure_lifecycle as life,
    structure_lifecycle_rpc::has_structure_manager_role,
    structure_upwell_ops::{self, load_ext_state, StructureExtendedState},
};
use eve_db::WorldStateSnapshot;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

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
            client_error: None,
        });
    }
    let sys_id = session.solar_system_id.ok_or_else(|| BridgeRpcError {
        code: "NoSolarSystem".into(),
        message: "Character is not in an active solar system".into(),
        client_error: None,
    })?;
    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Character has no active ship in space".into(),
        client_error: None,
    })?;
    let ship_pos = ctx
        .systems
        .get_ball_position_sync(sys_id, ship_id)
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship must be spawned in space to online an orbital".into(),
            client_error: None,
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
                client_error: None,
            });
        }
        let req_roles = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
        let (is_ceo, roles) =
            snap.effective_corp_roles_at(char_corp, char_id, None, session.corp_role, ctx.now_unix());
        let has_role = session.has_gm_role()
            || is_ceo
            || (roles & req_roles) != 0;
        if !has_role {
            return Err(BridgeRpcError {
                code: "CrpAccessDenied".into(),
                message: "Upgrading or onlining an orbital gantry requires Director or Station Manager role".into(),
                client_error: None,
            });
        }
        crate::services::structure_anchoring_rules::validate_pochven_customs_office_standing(
            snap, char_id, sys_id, &ctx.sde,
        )?;

        let (cur_tid, planet_id) = {
            let it = snap.items.get(&orbital_id).ok_or_else(|| BridgeRpcError {
                code: "OrbitalNotFound".into(),
                message: format!("Orbital {orbital_id} not found"),
                client_error: None,
            })?;
            if it.type_id != 3962 {
                return Err(BridgeRpcError {
                    code: "InvalidOrbitalGantry".into(),
                    message: format!(
                        "Target item {orbital_id} (type {}) is not an upgradable Customs Office Gantry",
                        it.type_id
                    ),
                    client_error: None,
                });
            }
            if it.owner_id != char_corp && it.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "OrbitalOwnershipDenied".into(),
                    message: format!("Corporation {char_corp} does not own orbital {orbital_id}"),
                    client_error: None,
                });
            }
            let st = snap.structures.get(&orbital_id).ok_or_else(|| BridgeRpcError {
                code: "OrbitalNotAnchored".into(),
                message: format!("Orbital {orbital_id} is not an anchored structure in space"),
                client_error: None,
            })?;
            if st.type_id != 3962 {
                return Err(BridgeRpcError {
                    code: "OrbitalAlreadyUpgraded".into(),
                    message: format!("Orbital {orbital_id} is not an un-upgraded gantry"),
                    client_error: None,
                });
            }
            if st.owner_corp_id != char_corp {
                return Err(BridgeRpcError {
                    code: "OrbitalOwnershipDenied".into(),
                    message: format!("Corporation {char_corp} does not own orbital structure {orbital_id}"),
                    client_error: None,
                });
            }
            if st.solar_system_id != sys_id {
                return Err(BridgeRpcError {
                    code: "OrbitalWrongSystem".into(),
                    message: format!(
                        "Orbital {orbital_id} is in solar system {}, not {sys_id}",
                        st.solar_system_id
                    ),
                    client_error: None,
                });
            }
            if matches!(st.state.as_str(), "Destroyed" | "Unanchored") {
                return Err(BridgeRpcError {
                    code: "InvalidOrbitalState".into(),
                    message: format!("Orbital {orbital_id} is in invalid state '{}'", st.state),
                    client_error: None,
                });
            }
            let dist = ship_pos.distance(glam::DVec3::from_array(st.position));
            if dist > 10_000.0 {
                return Err(BridgeRpcError {
                    code: "OrbitalOutOfRange".into(),
                    message: format!(
                        "Ship {ship_id} is {dist:.0}m from orbital {orbital_id} (max 10,000m)"
                    ),
                    client_error: None,
                });
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
                client_error: None,
            })?;
            if !is_valid_upgrade_item(u) {
                return Err(BridgeRpcError {
                    code: "CustomsOfficeItemRequired".into(),
                    message: format!(
                        "Upgrade item {arg_iid} must be an unlocked type {target_tid} owned by caller in active ship cargo"
                    ),
                    client_error: None,
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
                client_error: None,
            });
        };
        if let Some(u_owner) = snap.items.get(&up_iid).map(|u| u.owner_id) {
            snap.consume_exact_item_quantity(
                up_iid,
                1,
                eve_db::ConsumeExactOptions::for_owner_exact((u_owner == char_id).then_some(char_id), u_owner),
            )
            .map_err(|e| err("CustomsOfficeItemLocked", e.to_string()))?;
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
        crate::services::industry_pi::save_orbital_settings(snap, orbital_id, &cfg);
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
            client_error: None,
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
            client_error: None,
        });
    }
    let item_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| BridgeRpcError {
        code: "MissingArgument".into(),
        message: "AnchorOrbital requires an orbital itemID".into(),
        client_error: None,
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
            client_error: None,
        });
    }
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "NotInSpace".into(),
            message: "Must be undocked in space to anchor an orbital structure".into(),
            client_error: None,
        });
    }

    let sys_sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
    if sys_sec >= 0.45 && sys_id < 31_000_000 {
        return Err(BridgeRpcError {
            code: "CannotAnchorOrbitalInHighSec".into(),
            message: format!("Customs Office Gantry cannot be anchored in high-sec system {sys_id}"),
            client_error: None,
        });
    }

    ctx.db.with_snapshot(|snap| {
        crate::services::structure_anchoring_rules::validate_pochven_customs_office_standing(
            snap, char_id, sys_id, &ctx.sde,
        )
    })?;

    let char_corp = ctx.db.with_snapshot(|snap| -> Result<i64, BridgeRpcError> {
        let corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .or(session.corporation_id)
            .unwrap_or(0);
        if corp_id < 98_000_000 || !snap.corporations.contains_key(&corp_id) {
            return Err(BridgeRpcError {
                code: "CrpAccessDenied".into(),
                message: format!("Character {char_id} is in NPC corporation {corp_id}; only player corporations may anchor orbitals"),
                client_error: None,
            });
        }
        let req_roles = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
        let (is_ceo, roles) =
            snap.effective_corp_roles_at(corp_id, char_id, None, session.corp_role, ctx.now_unix());
        let has_role = session.has_gm_role()
            || is_ceo
            || (roles & req_roles) != 0;
        if !has_role {
            return Err(BridgeRpcError {
                code: "CrpAccessDenied".into(),
                message: "Anchoring an orbital requires Director or Station Manager corporation role".into(),
                client_error: None,
            });
        }
        Ok(corp_id)
    })?;

    let ship_id = session.ship_id.ok_or_else(|| BridgeRpcError {
        code: "NoActiveShip".into(),
        message: "Anchoring an orbital requires an active ship in space".into(),
        client_error: None,
    })?;
    let ship_pos = ctx
        .systems
        .get_ball_position_sync(sys_id, ship_id)
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: "Active ship must be spawned in space to anchor an orbital".into(),
            client_error: None,
        })?;

    let planets_in_sys: Vec<_> = ctx
        .sde
        .celestials_in_system(sys_id)
        .into_iter()
        .filter(|c| c.group_id == 7)
        .collect();
    let target_planet = if let Some(explicit_pid) = args.get(1).and_then(|a| a.as_i64()) {
        planets_in_sys
            .into_iter()
            .find(|c| c.celestial_id == explicit_pid)
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidOrbitalPlanet".into(),
                message: format!("Celestial {explicit_pid} is not a planet in solar system {sys_id}"),
                client_error: None,
            })?
    } else {
        planets_in_sys
            .into_iter()
            .min_by(|a, b| {
                (a.position - ship_pos)
                    .length_squared()
                    .partial_cmp(&(b.position - ship_pos).length_squared())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidOrbitalPlanet".into(),
                message: format!("Solar system {sys_id} has no anchorable planets"),
                client_error: None,
            })?
    };
    let dist = (ship_pos - target_planet.position).length();
    let max_dist = target_planet.radius + 100_000_000.0;
    if dist > max_dist {
        return Err(BridgeRpcError {
            code: "OrbitalOutOfRange".into(),
            message: format!(
                "Ship is {dist:.0}m from planet {} (must be within {max_dist:.0}m to anchor orbital)",
                target_planet.celestial_id
            ),
            client_error: None,
        });
    }
    let planet_id = target_planet.celestial_id;

    let (tid, pid) = ctx.db.try_with_mut_snapshot(|snap| {
        if snap.structures.contains_key(&item_id) {
            return Err(BridgeRpcError {
                code: "AlreadyAnchored".into(),
                message: format!("Orbital structure {item_id} is already anchored in space"),
                client_error: None,
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
                client_error: None,
            });
        }
        let it = snap.items.get_mut(&item_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Gantry item {item_id} not found"),
            client_error: None,
        })?;
        if it.owner_id != char_corp && it.owner_id != char_id {
            return Err(BridgeRpcError {
                code: "NotItemOwner".into(),
                message: format!("Character {char_id} does not own item {item_id}"),
                client_error: None,
            });
        }
        if it.location_id != ship_id || it.flag != eve_db::InventoryFlag::Cargo {
            return Err(BridgeRpcError {
                code: "ItemNotInCargo".into(),
                message: format!("Gantry item {item_id} must be in the cargo hold of active ship {ship_id}"),
                client_error: None,
            });
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
                position: ship_pos.to_array(),
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

#[allow(clippy::too_many_arguments)]
fn build_structure_dir_entry(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    session: &SessionState,
    ext: &StructureExtendedState,
    st: &eve_db::UpwellStructureRecord,
    now: u64,
    char_id: i64,
    caller_corp: i64,
    has_acl_access: bool,
) -> PyRep {
    let can_view_services = caller_corp == st.owner_corp_id || has_acl_access;
    let online_types = if can_view_services {
        life::online_service_type_ids(ctx, snap, st.structure_id)
    } else {
        Vec::new()
    };
    let services: Vec<(PyRep, PyRep)> = online_types
        .iter()
        .filter_map(|&tid| structure_upwell_ops::service_id_for_module_type(tid).map(|svc_id| (PyRep::Integer(svc_id), PyRep::Integer(1))))
        .collect();
    let power = life::power_state(ctx, snap, ext, st, now);
    let (hour, day) = snap
        .structure_vulnerability_windows
        .get(&st.structure_id)
        .copied()
        .or_else(|| snap.corp_governance.get(&st.owner_corp_id).map(|g| (u64::from(g.reinforce_hour), 5)))
        .unwrap_or((18, 5));
    let is_corp_manager = caller_corp == st.owner_corp_id && has_structure_manager_role(snap, session, char_id, caller_corp);
    let hacked_reveal = ext.hacked_reinforce_reveals.get(&st.structure_id).is_some_and(|s| s.contains(&char_id));
    let can_see_reinforce = caller_corp == st.owner_corp_id || hacked_reveal;
    let can_see_pending = is_corp_manager || hacked_reveal;
    let pending = ext.pending_reinforce_schedule.get(&st.structure_id).copied();
    let upkeep_state = match power {
        life::PowerState::FullPower => 1_i64,
        life::PowerState::LowPower | life::PowerState::NotOperational => 2_i64,
        life::PowerState::Abandoned => 3_i64,
    };
    let mut war_ids: Vec<i64> = snap
        .wars
        .values()
        .filter(|w| {
            w.active
                && snap.war_state.wars.get(&w.war_id).is_some_and(|meta| {
                    meta.war_hq_id == Some(st.structure_id)
                        && meta.finish_at_unix.is_none_or(|f| f > now)
                })
        })
        .map(|w| w.war_id)
        .collect();
    war_ids.sort_unstable();
    let fuel_expires = if is_corp_manager {
        life::estimated_fuel_expires_unix(ctx, snap, st, now)
            .map(|t| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(t)))
            .unwrap_or(PyRep::None)
    } else {
        PyRep::None
    };
    let timer_end = if st.reinforced_until_unix > now {
        PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(st.reinforced_until_unix))
    } else {
        PyRep::None
    };
    let unanchoring = if can_see_reinforce {
        ext.unanchoring_until_unix
            .get(&st.structure_id)
            .copied()
            .filter(|&u| u > now)
            .map(|u| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(u)))
            .unwrap_or(PyRep::None)
    } else {
        PyRep::None
    };
    let liquid_ozone_qty = if is_corp_manager
        && st.type_id == structure_ansiblex_navigation::ANSIBLEX_TYPE_ID
    {
        let qty: i64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == st.structure_id
                    && super::structure_acl_defense_rules::is_structure_fuel_flag(i.flag)
                    && i.type_id == structure_ansiblex_navigation::LIQUID_OZONE_TYPE_ID
                    && i.quantity > 0
            })
            .map(|i| i64::from(i.quantity))
            .fold(0_i64, i64::saturating_add);
        PyRep::Integer(qty)
    } else {
        PyRep::None
    };
    let r_hour = if can_see_reinforce { PyRep::Integer(hour as i64) } else { PyRep::None };
    let r_day = if can_see_reinforce { PyRep::Integer(day as i64) } else { PyRep::None };
    let nr_hour = if can_see_pending { pending.map_or(PyRep::None, |(nh, _, _)| PyRep::Integer(nh as i64)) } else { PyRep::None };
    let nr_day = if can_see_pending { pending.map_or(PyRep::None, |(_, nd, _)| PyRep::Integer(nd as i64)) } else { PyRep::None };
    let nr_apply_ft = if can_see_pending {
        pending.map_or(PyRep::None, |(_, _, at)| PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(at)))
    } else {
        PyRep::None
    };
    PyRep::keyval([
        ("structureID", PyRep::Integer(st.structure_id)),
        ("typeID", PyRep::Integer(i64::from(st.type_id))),
        ("ownerID", PyRep::Integer(st.owner_corp_id)),
        ("solarSystemID", PyRep::Integer(i64::from(st.solar_system_id))),
        ("itemName", PyRep::String(st.name.clone())),
        ("state", PyRep::String(st.state.clone())),
        ("upkeepState", PyRep::Integer(upkeep_state)),
        ("powerState", PyRep::String(power.as_str().into())),
        ("profileID", if is_corp_manager { ext.structure_profile_id.get(&st.structure_id).copied().map(PyRep::Integer).unwrap_or(PyRep::None) } else { PyRep::None }),
        ("fuelBlocks", if is_corp_manager { PyRep::Integer(i64::from(st.fuel_blocks)) } else { PyRep::None }),
        ("fuelExpires", fuel_expires),
        ("timerEnd", timer_end),
        ("unanchoring", unanchoring),
        ("liquidOzoneQty", liquid_ozone_qty),
        ("wars", PyRep::List(war_ids.into_iter().map(PyRep::Integer).collect())),
        ("services", PyRep::Dict(services)),
        ("onlineServiceTypeIDs", PyRep::List(online_types.into_iter().map(|t| PyRep::Integer(i64::from(t))).collect())),
        ("inSpace", PyRep::Bool(st.state != life::STATE_DESTROYED)),
        ("reinforceHour", r_hour.clone()),
        ("reinforceWeekday", r_day.clone()),
        ("nextReinforceHour", nr_hour.clone()),
        ("nextReinforceWeekday", nr_day.clone()),
        ("applyAfterUnix", if can_see_pending { pending.map_or(PyRep::None, |(_, _, at)| PyRep::Integer(at as i64)) } else { PyRep::None }),
        ("reinforce_hour", r_hour),
        ("reinforce_weekday", r_day),
        ("next_reinforce_hour", nr_hour),
        ("next_reinforce_weekday", nr_day),
        ("next_reinforce_apply", nr_apply_ft),
    ])
}

pub(crate) fn handle_get_structures(ctx: &ServerContext, session: &mut SessionState, corp_only: bool) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    structure_upwell_ops::sync_pending_profiles_in_ctx(ctx);
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        let ext = load_ext_state(snap);
        let mut list: Vec<(&eve_db::UpwellStructureRecord, bool)> = snap
            .structures
            .values()
            .filter_map(|s| {
                if s.state == life::STATE_DESTROYED {
                    return None;
                }
                let has_acl = structure_upwell_ops::evaluate_character_structure_access_in_snap_at(snap, s.structure_id, char_id, Some(now));
                if s.owner_corp_id == caller_corp || (!corp_only && has_acl) {
                    Some((s, has_acl))
                } else {
                    None
                }
            })
            .collect();
        list.sort_by_key(|(s, _)| s.structure_id);
        Ok(PyRep::Dict(
            list.into_iter()
                .map(|(st, has_acl)| (PyRep::Integer(st.structure_id), build_structure_dir_entry(ctx, snap, session, &ext, st, now, char_id, caller_corp, has_acl)))
                .collect(),
        ))
    })
}

pub(crate) fn handle_get_structure_info(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = args.first().and_then(|a| a.as_i64()).or(session.structure_id).ok_or_else(|| err("MissingStructureId", "GetStructureInfo requires structureID"))?;
    structure_upwell_ops::sync_pending_profiles_in_ctx(ctx);
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
        let Some(st) = snap
            .structures
            .get(&structure_id)
            .filter(|s| s.state != life::STATE_DESTROYED)
        else {
            return Ok(PyRep::None);
        };
        let has_acl = structure_upwell_ops::evaluate_character_structure_access_in_snap_at(snap, structure_id, char_id, Some(now));
        if caller_corp != st.owner_corp_id && session.solar_system_id != Some(st.solar_system_id) && !has_acl {
            return Err(err("StructureAccessDenied", format!("Character {char_id} lacks access to structure {structure_id}")));
        }
        let ext = load_ext_state(snap);
        Ok(build_structure_dir_entry(ctx, snap, session, &ext, st, now, char_id, caller_corp, has_acl))
    })
}

/// Enforces V24 station service standing gates loaded from `res:/staticdata/stationstandingsrestrictions.fsdbinary`.
pub fn check_station_service_standing(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    raw_service_id: i64,
    service_id: u32,
    owner_corp: i64,
    owner_fac: Option<i64>,
) -> Result<(), BridgeRpcError> {
    let restrictions = ctx
        .sde
        .load_cfsd_nested("res:/staticdata/stationstandingsrestrictions.fsdbinary")
        .map_err(|e| BridgeRpcError {
            code: "StaticDataUnavailable".into(),
            message: format!("Failed to load station standings restrictions: {e}"),
            client_error: None,
        })?;

    let service_key = service_id.to_string();
    let corp_key = owner_corp.to_string();
    let gate = if let Some(entry) = restrictions.get(&corp_key) {
        entry
            .get("services")
            .and_then(|svcs| svcs.get(&service_key))
            .and_then(|v| v.as_f64())
            .map(|min_st| (owner_corp, min_st))
    } else {
        owner_fac.and_then(|fid| {
            restrictions
                .get(&fid.to_string())
                .and_then(|entry| entry.get("services"))
                .and_then(|svcs| svcs.get(&service_key))
                .and_then(|v| v.as_f64())
                .map(|min_st| (fid, min_st))
        })
    };

    let Some((gate_entity_id, min_standing)) = gate else {
        return Ok(());
    };

    let eff_standing = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id);
        let raw = ch
            .and_then(|c| c.standings.get(&gate_entity_id).copied())
            .unwrap_or(0.0);
        let has_entry = ch.is_some_and(|c| c.standings.contains_key(&gate_entity_id));
        let (eff, _, _, _) =
            crate::services::agents_missions::agents_standings_arcs::compute_effective_standing_breakdown(
                ctx,
                snap,
                char_id,
                gate_entity_id,
                raw,
                has_entry,
            );
        eff
    });

    if eff_standing + 1e-9 < min_standing {
        return Err(BridgeRpcError {
            code: "StandingTooLowForStationService".into(),
            message: format!(
                "Character {char_id} standing {eff_standing:.2} with {gate_entity_id} is below required {min_standing:.2} for service {raw_service_id} at station {station_id}"
            ),
            client_error: None,
        });
    }
    Ok(())
}

/// Validates Upwell structure docking eligibility, state, profile/group/standing ACL, and V24 hull
/// size matrix (`feat:structures.docking-acl`), returning the structure's position in space.
pub(crate) fn resolve_upwell_dock_target(
    ctx: &ServerContext,
    dock_id: i64,
    sys_id: u32,
    char_id: i64,
    ship_type_id: u32,
    ship_group_id: u32,
) -> Result<glam::DVec3, BridgeRpcError> {
    structure_lifecycle::sync_all_structures(ctx);
    let Some(st) = ctx.db.with_snapshot(|snap| snap.structures.get(&dock_id).cloned()) else {
        return Err(err("StructureNotFound", format!("Structure {dock_id} does not exist")));
    };
    if st.solar_system_id != sys_id {
        return Err(err(
            "DockTargetWrongSystem",
            format!("Structure {dock_id} is in system {}, but pilot is in {sys_id}", st.solar_system_id),
        ));
    }
    if st.state == "Destroyed" {
        return Err(err("StructureDestroyed", format!("Structure {dock_id} is destroyed")));
    }
    let st_group_id = ctx.sde.types.get(&st.type_id).map(|t| t.group_id).unwrap_or(0);
    // Allowlist dockable Upwell structure classes only (1657=Citadel, 1404=Engineering Complex,
    // 1406=Refinery) plus Havoc Pirate Insurgency FOBs (group 4644, types 79172/78260, gated
    // to the matching pirate militia by `check_docking_allowed`); deny NPC Stronghold FOBs
    // (46363/46364), FW hubs (32226), FLEX, orbital skyhooks, Metenox, and depots (#569).
    let is_pirate_insurgency_fob =
        st_group_id == 4644 || matches!(st.type_id, 79_172 | 78_260);
    if is_pirate_insurgency_fob {
        crate::services::fw_gameplay::check_docking_allowed(ctx, char_id, sys_id, dock_id, true)?;
    }
    let is_dockable_upwell_group =
        matches!(st_group_id, 1657 | 1404 | 1406) || is_pirate_insurgency_fob;
    let is_explicit_non_dockable_type = matches!(
        st.type_id,
        35_840 | 35_841 | 37_534 | 81_826 | 2_233 | 3_962 | 46_363 | 46_364 | 32_226
    );
    if !is_dockable_upwell_group || is_explicit_non_dockable_type {
        return Err(err(
            "StructureDoesNotAllowDocking",
            format!("Structure {} (type {}, group {}) does not allow ship docking", dock_id, st.type_id, st_group_id),
        ));
    }
    if matches!(st.state.as_str(), "Unanchored" | "AnchorVulnerable" | "Anchoring") {
        return Err(err(
            "StructureNotDockable",
            format!("Structure {dock_id} is in state '{}' and cannot be docked at", st.state),
        ));
    }
    if let Err(e) = ctx.db.with_snapshot(|snap| {
        super::structure_acl_defense_rules::verify_structure_docking_acl(ctx, snap, dock_id, char_id)
    }) {
        return Err(e);
    }
    // Freighters (513) and Jump Freighters (902) dock at standard Upwell hulls, while
    // `ballparkCommon/docking.pyj:CanDock` (`res:/staticdata/graphiclocations.fsdbinary`)
    // governs directional undock locator categories and Pirate FOB freighter exclusions.
    // parity: sde:res:/staticdata/graphiclocations.fsdbinary
    let ship_type_id = if ctx.sde.types.get(&ship_type_id).map(|t| t.group_id) == Some(ship_group_id) {
        ship_type_id
    } else {
        ctx.db
            .with_snapshot(|s| {
                s.characters
                    .get(&char_id)
                    .and_then(|c| s.items.get(&c.active_ship_id))
                    .map(|i| i.type_id)
            })
            .unwrap_or(ship_type_id)
    };
    let is_capital_ship = matches!(ship_group_id, 30 | 485 | 547 | 659 | 883 | 1538 | 4594);
    let is_supercapital_ship = matches!(ship_group_id, 30 | 659);
    let is_rorqual = ship_type_id == 28_352 || ship_group_id == 883;
    let hull_class_allowed = match st.type_id {
        35_834 | 40_340 => true,
        35_833 | 47_512..=47_516 | 35_827 => !is_supercapital_ship,
        35_836 => !is_capital_ship || is_rorqual,
        35_832 | 35_825 | 35_826 | 35_835 => !is_capital_ship,
        _ => match structure_lifecycle::structure_profile(ctx, st.type_id).map(|p| p.hull_class) {
            Some(structure_lifecycle::HullClass::XLarge) if st_group_id == 1657 => true,
            Some(structure_lifecycle::HullClass::XLarge) => !is_supercapital_ship,
            Some(structure_lifecycle::HullClass::Large) if st_group_id == 1657 => !is_supercapital_ship,
            _ => !is_capital_ship,
        },
    };
    let hull_allowed = hull_class_allowed
        && ctx
            .sde
            .can_dock_by_graphic_locations(st.type_id, ship_type_id)
            .unwrap_or(true);
    if !hull_allowed {
        return Err(err(
            "ShipTooLargeForStructure",
            format!(
                "Ship type {} (group {}) is too large to dock at structure {} (type {})",
                ship_type_id, ship_group_id, dock_id, st.type_id
            ),
        ));
    }
    Ok(glam::DVec3::from_array(st.position))
}

pub fn load_station_standings_restrictions(sde: &eve_sde::SdeRegistry) -> HashMap<i64, HashMap<u32, f64>> {
    let mut out = HashMap::new();
    if let Ok(table) = sde.load_cfsd_nested("res:/staticdata/stationstandingsrestrictions.fsdbinary") {
        for (entity_str, val) in table {
            let Ok(entity_id) = entity_str.parse::<i64>() else { continue };
            let mut svc_map = HashMap::new();
            if let Some(svcs) = val.get("services").and_then(|v| v.as_object()) {
                for (sid_str, req_val) in svcs {
                    if let (Ok(sid), Some(req)) = (sid_str.parse::<u32>(), req_val.as_f64()) {
                        svc_map.insert(sid, req);
                    }
                }
            }
            out.insert(entity_id, svc_map);
        }
    }
    out
}

pub fn effective_standing_with_skills(raw: f64, diplomacy_lvl: u8, connections_lvl: u8) -> f64 {
    if raw < -1e-9 {
        (raw + (10.0 - raw) * 0.04 * (diplomacy_lvl.min(5) as f64)).clamp(-10.0, 10.0)
    } else if raw > 1e-9 {
        (raw + (10.0 - raw) * 0.04 * (connections_lvl.min(5) as f64)).clamp(-10.0, 10.0)
    } else {
        0.0
    }
}

pub fn validate_npc_station_docking_gate(ctx: &ServerContext, char_id: i64, station_id: i64) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    if station_id == 60_015_187 {
        let char_war_fac = ctx.db.with_snapshot(|s| s.characters.get(&char_id).and_then(|c| c.war_faction_id));
        if matches!(char_war_fac, Some(500_001..=500_004)) {
            return Err(BridgeRpcError::user_error("FulcrumRefusesEmpireMilitia"));
        }
        let weapons_active = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states
            .get(&char_id)
            .is_some_and(|s| s.is_weapon_timer_active(now_unix));
        let retribution_active = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.disapproval_flags.get(&char_id).copied().is_some_and(|exp| now_unix < exp)
        });
        if weapons_active || retribution_active {
            return Err(BridgeRpcError::user_error("FulcrumRefusesTimerActive"));
        }
    }
    if let Some(st) = ctx.sde.stations.get(&station_id) {
        let restrictions = load_station_standings_restrictions(&ctx.sde);
        let req_opt = restrictions
            .get(&st.owner_corporation_id)
            .and_then(|m| m.get(&22).copied())
            .or_else(|| st.owner_faction_id.and_then(|fid| restrictions.get(&fid)?.get(&22).copied()));
        if let Some(req_st) = req_opt {
            let eff = ctx.db.with_snapshot(|snap| {
                let sk = snap.skills.get(&char_id);
                let dip = sk.and_then(|m| m.get(&3357)).map(|s| s.level).unwrap_or(0);
                let con = sk.and_then(|m| m.get(&3359)).map(|s| s.level).unwrap_or(0);
                let ch = snap.characters.get(&char_id);
                let entity = if restrictions.contains_key(&st.owner_corporation_id) {
                    st.owner_corporation_id
                } else {
                    st.owner_faction_id.unwrap_or(st.owner_corporation_id)
                };
                let raw = ch.and_then(|c| c.standings.get(&entity).copied()).unwrap_or(0.0);
                effective_standing_with_skills(raw, dip, con)
            });
            if eff + 1e-9 < req_st {
                return Err(BridgeRpcError {
                    code: "StandingTooLowForStationService".into(),
                    message: format!("Standing {eff:.2} is below required {req_st:.2} to dock at station {station_id}"),
                    client_error: None,
                });
            }
        }
    }
    Ok(())
}

fn paired_station_service_ids(service_id: u32) -> &'static [u32] {
    match service_id {
        1..=3 => &[1, 2, 3],
        5 | 6 => &[5, 6],
        14 | 15 => &[14, 15],
        _ => &[],
    }
}

pub fn evaluate_station_service_standing_gate(
    ctx: &ServerContext,
    char_id: i64,
    station_id: i64,
    raw_service_id: u32,
) -> Result<PyRep, BridgeRpcError> {
    let service_id = if raw_service_id.is_power_of_two() && raw_service_id >= 16 {
        raw_service_id.trailing_zeros() + 1
    } else {
        raw_service_id
    };
    let (owner_corp, owner_fac) = if let Some(st) = ctx.sde.stations.get(&station_id) {
        let offered = crate::services::station_structure::npc_station_offers_service(
            &ctx.sde,
            station_id,
            service_id as i64,
        ) || paired_station_service_ids(service_id).iter().any(|&alt| {
            crate::services::station_structure::npc_station_offers_service(
                &ctx.sde,
                station_id,
                alt as i64,
            )
        });
        if !offered {
            return Err(err(
                "StationServiceUnavailable",
                format!("Station {station_id} does not offer service {service_id}"),
            ));
        }
        (st.owner_corporation_id, st.owner_faction_id)
    } else if let Some(struct_info) = ctx.db.with_snapshot(|s| s.structures.get(&station_id).map(|st| (st.owner_corp_id, None))) {
        struct_info
    } else {
        return Err(err("StationNotFound", format!("Station {station_id} not found in SDE")));
    };

    if station_id == 60_015_187 && matches!(service_id, 23 | 24) {
        let ok = ctx.db.with_snapshot(|snap| {
            let Some(ch) = snap.characters.get(&char_id) else { return false };
            if service_id == 23 {
                snap.corporations.get(&ch.corporation_id).is_some_and(|corp| {
                    snap.characters.get(&corp.ceo_id).and_then(|ceo| ceo.war_faction_id).is_some_and(|f| matches!(f, 500_010 | 500_011))
                        || snap.fw_runtime.corp_war_factions.get(&ch.corporation_id).copied().is_some_and(|f| matches!(f, 500_010 | 500_011))
                })
            } else {
                matches!(ch.war_faction_id, Some(500_010 | 500_011))
                    || snap.fw_runtime.direct_enlisted_chars.get(&char_id).copied().is_some_and(|f| matches!(f, 500_010 | 500_011))
            }
        });
        if !ok {
            return Err(err(
                "StandingTooLowForStationService",
                format!("Service {service_id} at The Fulcrum requires pirate insurgency alignment"),
            ));
        }
    }

    let restrictions = load_station_standings_restrictions(&ctx.sde);
    let fsd_gate = restrictions
        .get(&owner_corp)
        .and_then(|m| m.get(&service_id).map(|&req| (owner_corp, req)))
        .or_else(|| owner_fac.and_then(|fid| restrictions.get(&fid)?.get(&service_id).map(|&req| (fid, req))));

    let Some((gate_entity, req_standing)) = fsd_gate else {
        return Ok(PyRep::Bool(true));
    };

    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id);
        let target_cid = if service_id == 23 && ch.is_some_and(|c| snap.corporations.contains_key(&c.corporation_id)) {
            let cid = ch.map(|c| c.corporation_id).unwrap_or(0);
            snap.corporations.get(&cid).map(|cp| cp.ceo_id).unwrap_or(char_id)
        } else {
            char_id
        };
        let target_ch = snap.characters.get(&target_cid);
        let raw = target_ch
            .and_then(|c| c.standings.get(&gate_entity).copied())
            .unwrap_or(0.0);
        let has_entry = target_ch.is_some_and(|c| c.standings.contains_key(&gate_entity));
        let (eff, _, _, _) =
            crate::services::agents_missions::agents_standings_arcs::compute_effective_standing_breakdown(
                ctx,
                snap,
                target_cid,
                gate_entity,
                raw,
                has_entry,
            );
        if eff + 1e-9 < req_standing {
            return Err(err(
                "StandingTooLowForStationService",
                format!("Effective standing {eff:.2} toward {gate_entity} is below FSD required {req_standing:.2} for service {service_id} at station {station_id}"),
            ));
        }
        Ok(PyRep::Bool(true))
    })
}

/// Computes Fulcrum alignment speed inertia multiplier (`feat:structures.fulcrum-alignment-services`).
/// Pirate-enlisted pilots (500010 Angel / 500011 Guristas) at The Fulcrum (60015187) or in Zarzakh (30100000)
/// receive the Deathless alignment speed buff (reducing inertia by 20%, 0.80x multiplier).
pub fn fulcrum_alignment_inertia_multiplier(
    snap: &WorldStateSnapshot,
    char_id: i64,
    station_id: Option<i64>,
    sys_id: u32,
) -> f64 {
    if station_id == Some(60_015_187) || sys_id == 30_100_000 {
        let is_pirate = snap.characters.get(&char_id).and_then(|c| c.war_faction_id).is_some_and(|wf| matches!(wf, 500_010 | 500_011))
            || snap.fw_runtime.direct_enlisted_chars.get(&char_id).copied().is_some_and(|wf| matches!(wf, 500_010 | 500_011));
        if is_pirate {
            return 0.80;
        }
    }
    1.0
}

#[cfg(test)] #[path = "station_structure_handlers_tests.rs"]
mod station_structure_handlers_tests;

#[path = "station_structure_handlers_cov_unit_tests.rs"]
#[cfg(test)]
mod station_structure_handlers_cov_unit_tests;

