#![forbid(unsafe_code)]
//! `ship.StoreVessel` orchestration, in-space tackle/immobilization/module/lock guards,
//! space-departure cleanup, and Dogma hull kinematics evaluation (`#497`).

use super::*;
use crate::services::inventory_fitting::fitting_containers_ships;

/// Evaluates effective `(max_velocity, inertia, mass, radius)` for a ship hull from its
/// `ShipFittingEngine` Dogma attributes (`MAX_VELOCITY = 37`, `AGILITY = 70`, `MASS = 4`),
/// falling back to SDE `default_attributes` / `TypeInfo` fields when unavailable.
pub(super) fn compute_ship_ball_kinematics(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    type_id: u32,
) -> (f64, f64, f64, f64) {
    let t = ctx.sde.types.get(&type_id);
    let mut max_velocity = t
        .and_then(|ti| ti.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(100.0);
    let mut inertia = t
        .and_then(|ti| ti.default_attributes.get(&eve_sde::attr_ids::AGILITY).copied())
        .filter(|&v| v > 0.0)
        .unwrap_or(1.0);
    let mut mass = t.map(|ti| ti.mass).filter(|&v| v > 0.0).unwrap_or(1.0);
    let radius = t.map(|ti| ti.radius).filter(|&v| v > 0.0).unwrap_or(50.0);

    if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let mut eff_attrs: HashMap<u16, f64> = [
            eve_sde::attr_ids::MAX_VELOCITY,
            eve_sde::attr_ids::AGILITY,
            eve_sde::attr_ids::MASS,
        ]
        .into_iter()
        .filter(|&aid| {
            fit.ship.base_attributes.contains_key(&aid)
                || ctx.sde.attributes.contains_key(&aid)
                || fit.ship.modifiers.iter().any(|m| m.target_attribute_id == aid)
        })
        .map(|aid| (aid, fit.ship.get_attribute(aid, &ctx.sde)))
        .collect();
        crate::services::inventory_fitting::fitting_scanners_superweapons::apply_active_dbuffs_and_system_effects_to_ship_attrs(
            ctx,
            ship_id,
            &mut eff_attrs,
        );
        if let Some(&v) = eff_attrs.get(&eve_sde::attr_ids::MAX_VELOCITY) {
            if v > 0.0 {
                max_velocity = v;
            }
        }
        if let Some(&ag) = eff_attrs.get(&eve_sde::attr_ids::AGILITY) {
            if ag > 0.0 {
                inertia = ag;
            }
        }
        if let Some(&m) = eff_attrs.get(&eve_sde::attr_ids::MASS) {
            if m > 0.0 {
                mass = m;
            }
        }
    }

    (max_velocity, inertia, mass, radius)
}

/// Validates that storing `target_ship_id` into `host_ship_id` in space cannot bypass warp
/// scramble, interdiction/ESS bubbles, mid-warp movement, navigation/immobilization locks,
/// active modules/cloaks, or incoming target locks on either vessel (`#497`).
pub(super) async fn validate_in_space_ship_store_allowed(
    ctx: &ServerContext,
    sys_id: u32,
    sys: &crate::system_actor::SystemHandle,
    active_ship_id: i64,
    target_ship_id: i64,
    host_ship_id: i64,
) -> Result<(), BridgeRpcError> {
    let mut warp_check_ids = vec![target_ship_id, host_ship_id];
    if active_ship_id != 0 && !warp_check_ids.contains(&active_ship_id) {
        warp_check_ids.push(active_ship_id);
    }
    for &sid in &warp_check_ids {
        if let Ok(Some((_, b))) = sys.get_ball_snapshot(sid).await {
            if matches!(b.mode, eve_destiny::MovementMode::Warp { .. }) {
                return Err(err(
                    "CantStoreInWarp",
                    "Cannot store a ship in a Ship Maintenance Bay while in warp",
                ));
            }
        }
        crate::services::space_destiny::space_nav_travel::ensure_not_mjd_spooling(sys, sid)
            .await?;
    }

    for &sid in &warp_check_ids {
        if sys.is_ball_scrambled(sid).await.unwrap_or(false) {
            return Err(err(
                "WarpScrambled",
                "Cannot store ship while warp scrambled or inside a warp disruption bubble",
            ));
        }
        crate::services::pve_ess_reserve_banks::check_ess_intrusion_bubble_restriction(
            ctx,
            sys_id,
            sid,
            "store ship",
        )
        .await?;
        crate::services::tactical_modules::check_ship_navigation_lock(ctx, sid, "store ship")?;
        crate::services::space_navigation_advanced::check_immobilizing_module_active(
            ctx,
            sid,
            "store ship",
        )?;

        let is_gate_cloaked = {
            let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
            bp.combat_states
                .get(&sid)
                .is_some_and(|cs| cs.gate_cloak_remaining_ticks > 0)
        };
        if is_gate_cloaked
            || crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
                ctx, sys, sid,
            )
            .await
        {
            return Err(err(
                "CannotStoreCloakedShip",
                "Cannot store a ship in a Ship Maintenance Bay while cloaked",
            ));
        }

        let has_active_cycles = sys
            .actor_state
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .active_module_cycles
            .iter()
            .any(|c| c.ship_id == sid);
        let has_active_mod_state = ctx.db.with_snapshot(|snap| {
            snap.items
                .values()
                .any(|it| it.location_id == sid && snap.module_active_states.contains(&it.item_id))
        });
        if has_active_cycles || has_active_mod_state {
            return Err(err(
                "CannotStoreShipWithActiveModules",
                "Cannot store a ship in a Ship Maintenance Bay while modules are active",
            ));
        }

        let is_targeted = {
            let bp = sys.ballpark.read().unwrap_or_else(|e| e.into_inner());
            bp.target_locks
                .iter()
                .any(|(&locker_id, locks)| {
                    locker_id != sid && locks.iter().any(|l| l.target_id == sid)
                })
        };
        if is_targeted {
            return Err(err(
                "CannotStoreTargetedShip",
                "Cannot store a ship while it is targeted by another vessel",
            ));
        }
    }

    Ok(())
}

/// Flushes active modules, projected EWAR, drones/fighters, cyno beacons/portals, active module
/// cycles, and target locks when a ship leaves space or is vacated (`#497`).
pub(super) async fn cleanup_ship_leaving_space(
    ctx: &ServerContext,
    sys_id: u32,
    sys: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
) {
    crate::services::space_destiny::space_warp_jump::deactivate_ship_modules_on_leave_space(
        ctx, char_id, ship_id,
    );
    let _ = crate::services::notify_emitters_space_dungeons::flush_ship_all_ewar_sync(
        ctx,
        char_id,
        Some(sys_id),
        ship_id,
    );
    crate::sp6_lifecycle::abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);
    crate::services::inventory_fitting::fitting_cyno_portal::on_cyno_ship_destroyed(ctx, ship_id);
    let cycle_mods: Vec<i64> = sys
        .actor_state
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .active_module_cycles
        .iter()
        .filter(|c| c.ship_id == ship_id)
        .map(|c| c.module_id)
        .collect();
    for mid in cycle_mods {
        sys.try_remove_module_cycle(ship_id, mid);
    }
    let _ = sys.clear_target_locks(ship_id).await;
}

pub(super) async fn handle_store_vessel(
    ctx: &ServerContext,
    session: &mut SessionState,
    char_id: i64,
    dock_id: Option<i64>,
    supplied_password: Option<&str>,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (target_ship_id, host_ship_id) = if args.len() >= 2 {
        (args[0].as_i64().unwrap_or(0), args[1].as_i64().unwrap_or(0))
    } else {
        (
            session.ship_id.unwrap_or(0),
            args.first().and_then(|a| a.as_i64()).unwrap_or(0),
        )
    };
    if target_ship_id == 0 || host_ship_id == 0 {
        return Err(err(
            "InvalidArgument",
            "StoreVessel requires valid target and host IDs",
        ));
    }
    if let Some(dock) = dock_id {
        let caller_corp = session.corporation_id.unwrap_or(0);
        return ctx.db.try_with_mut_snapshot(|snap| {
            if target_ship_id == host_ship_id {
                return Err(err("UserError", "CannotStoreSelf"));
            }
            if Some(target_ship_id) == session.ship_id
                || snap.is_structure_controller_saved_ship(target_ship_id)
                || snap
                    .characters
                    .values()
                    .any(|c| c.active_ship_id == target_ship_id)
            {
                return Err(err("UserError", "CannotStoreActiveShip"));
            }
            let Some(target) = snap.items.get(&target_ship_id) else {
                return Err(err("ShipNotFound", format!("Target ship {target_ship_id} not found")));
            };
            let Some(host) = snap.items.get(&host_ship_id) else {
                return Err(err("ShipNotFound", format!("Host ship {host_ship_id} not found")));
            };
            if target.location_id != dock || host.location_id != dock {
                return Err(err("UserError", "ShipNotInSameHangar"));
            }
            fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, target)?;
            fitting_containers_ships::check_item_lock_impound_asset_safety_in_snap(snap, host)?;
            let t_type = ctx
                .sde
                .types
                .get(&target.type_id)
                .ok_or_else(|| err("InvalidType", "Unknown type"))?;
            if t_type.category_id != 6 || target.type_id == 670 {
                return Err(err("UserError", "OnlyShipsCanBeStoredInSMA"));
            }
            let check_docked_ship_access = |item: &eve_db::InventoryItemRecord,
                                            label: &str,
                                            require_station_mgr_for_corp: bool|
             -> Result<(), BridgeRpcError> {
                let is_corp = caller_corp > 0 && item.owner_id == caller_corp;
                if item.owner_id != char_id && !is_corp {
                    return Err(err(
                        "OwnershipViolation",
                        format!("Cannot store into or from another player's docked {label}"),
                    ));
                }
                if is_corp {
                    let flag_u16 = item.flag as u16;
                    if !(116..=122).contains(&flag_u16) {
                        return Err(err(
                            "InvalidLocation",
                            format!("Corporation-owned {label} must be in CorpSAG1..7"),
                        ));
                    }
                    let (is_ceo, snap_roles) =
                        crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap(
                            snap,
                            caller_corp,
                            char_id,
                            Some(dock),
                        );
                    let effective_roles = snap_roles | session.corp_role;
                    let is_dir_or_ceo = is_ceo
                        || eve_db::corp_roles::has_role(
                            effective_roles,
                            eve_db::corp_roles::ROLE_DIRECTOR,
                        );
                    let div_take_role = 1u64 << (13 + (flag_u16 - 116) as u32);
                    if !is_dir_or_ceo
                        && !eve_db::corp_roles::has_role(effective_roles, div_take_role)
                    {
                        return Err(err(
                            "CorpRoleDenied",
                            format!(
                                "Character {char_id} lacks corporation hangar division {} take role for {label}",
                                flag_u16 - 115
                            ),
                        ));
                    }
                    if require_station_mgr_for_corp
                        && !is_dir_or_ceo
                        && !eve_db::corp_roles::has_role(
                            effective_roles,
                            eve_db::corp_roles::ROLE_STATION_MANAGER,
                        )
                    {
                        return Err(err(
                            "CorpRoleDenied",
                            "Corporation-owned ship storage requires Director or Station Manager role",
                        ));
                    }
                } else if item.flag != InventoryFlag::Hangar {
                    return Err(err(
                        "InvalidLocation",
                        format!("Personal {label} must be in station or structure Hangar"),
                    ));
                }
                Ok(())
            };
            check_docked_ship_access(target, "target ship", true)?;
            check_docked_ship_access(host, "host ship", false)?;
            if !target.singleton {
                return Err(err(
                    "OnlyAssembledShipsInSMA",
                    "Only assembled ships can be stored in a Ship Maintenance Bay",
                ));
            }
            if ship_has_non_charge_cargo(ctx, snap, target_ship_id) {
                return Err(err(
                    "ShipHasNonChargeCargo",
                    "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo",
                ));
            }
            if !fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
                snap,
                char_id,
                host_ship_id,
                host.owner_id,
                true,
                supplied_password,
            ) {
                return Err(err(
                    "SMAAccessDenied",
                    "Ship Maintenance Bay access denied by ship configuration",
                ));
            }
            let dest_flag = select_and_validate_bay_dest_flag(
                ctx,
                snap,
                target,
                t_type.group_id,
                host,
                target_ship_id,
                host_ship_id,
            )?;
            if let Some(target_mut) = snap.items.get_mut(&target_ship_id) {
                target_mut.location_id = host_ship_id;
                target_mut.flag = dest_flag;
                target_mut.version += 1;
            }
            Ok(PyRep::keyval([
                ("storedShipID", PyRep::Integer(target_ship_id)),
                ("hostShipID", PyRep::Integer(host_ship_id)),
                ("flagID", PyRep::Integer(dest_flag as i64)),
            ]))
        });
    }

    let active_ship_id = session.ship_id.unwrap_or(0);
    if target_ship_id == host_ship_id {
        return Err(err("UserError", "CannotStoreShipInItself"));
    }
    let sys_id = session
        .solar_system_id
        .ok_or_else(|| err("NotInSpace", "StoreVessel in space requires solarSystemID"))?;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let caller_pos = sys
        .get_ball_snapshot(active_ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position);
    let host_pos = sys
        .get_ball_snapshot(host_ship_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position);
    let (Some(cp), Some(hp)) = (caller_pos, host_pos) else {
        return Err(err(
            "ShipNotInSpace",
            format!("Host {host_ship_id} or active ship {active_ship_id} is not on grid"),
        ));
    };
    let caller_host_dist = cp.distance(hp);
    if caller_host_dist > BAY_ACCESS_RANGE_M {
        return Err(err(
            "OutOfRange",
            format!(
                "Bay host {host_ship_id} is {caller_host_dist:.0} m away from caller (must be <= {BAY_ACCESS_RANGE_M} m)"
            ),
        ));
    }
    check_pos_forcefield_coverage(ctx, sys_id, char_id, active_ship_id, &[cp, hp]).await?;
    crate::services::combat_crimewatch::check_crimewatch_action_restriction(
        ctx,
        char_id,
        target_ship_id,
        sys_id,
        "store_ship",
        ctx.now_unix(),
    )?;

    if target_ship_id != active_ship_id {
        let target_pos = sys
            .get_ball_snapshot(target_ship_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position);
        let Some(tp) = target_pos else {
            return Err(err(
                "ShipNotInSpace",
                format!("Target ship {target_ship_id} is not on grid"),
            ));
        };
        let th_dist = tp.distance(hp);
        if th_dist > BAY_ACCESS_RANGE_M {
            return Err(err(
                "OutOfRange",
                format!(
                    "Target ship is {th_dist:.0} m away from host (must be <= {BAY_ACCESS_RANGE_M} m)"
                ),
            ));
        }
        let ct_dist = cp.distance(tp);
        if ct_dist > BAY_ACCESS_RANGE_M {
            return Err(err(
                "OutOfRange",
                format!(
                    "Target ship is {ct_dist:.0} m away from caller (must be <= {BAY_ACCESS_RANGE_M} m)"
                ),
            ));
        }
        check_pos_forcefield_coverage(ctx, sys_id, char_id, active_ship_id, &[cp, hp, tp]).await?;
    }

    validate_in_space_ship_store_allowed(
        ctx,
        sys_id,
        &sys,
        active_ship_id,
        target_ship_id,
        host_ship_id,
    )
    .await?;

    if target_ship_id == active_ship_id {
        let pod_pos = cp + DVec3::new(100.0, 0.0, 0.0);
        let (capsule_id, res_flag_id) = ctx.db.try_with_mut_snapshot(|snap| {
            let dest_flag = validate_and_select_in_space_store_snap(
                ctx,
                snap,
                char_id,
                sys_id,
                target_ship_id,
                host_ship_id,
                supplied_password,
                true,
            )?;
            if let Some(target_mut) = snap.items.get_mut(&target_ship_id) {
                target_mut.location_id = host_ship_id;
                target_mut.flag = dest_flag;
                target_mut.version += 1;
            }
            snap.item_positions.remove(&target_ship_id);
            let existing_cap = snap
                .items
                .values()
                .find(|it| {
                    it.type_id == 670
                        && it.owner_id == char_id
                        && it.location_id == sys_id as i64
                        && !snap.characters.values().any(|c| c.active_ship_id == it.item_id)
                })
                .map(|it| it.item_id);
            let cap_id = if let Some(cid) = existing_cap {
                if let Some(cap) = snap.items.get_mut(&cid) {
                    cap.location_id = sys_id as i64;
                    cap.flag = InventoryFlag::Pilot;
                    cap.singleton = true;
                    cap.version += 1;
                }
                cid
            } else {
                snap.next_entity_id += 1;
                let cid = snap.next_entity_id;
                snap.items.insert(
                    cid,
                    eve_db::InventoryItemRecord {
                        item_id: cid,
                        type_id: 670,
                        owner_id: char_id,
                        location_id: sys_id as i64,
                        flag: InventoryFlag::Pilot,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
                cid
            };
            snap.item_positions.insert(cap_id, [pod_pos.x, pod_pos.y, pod_pos.z]);
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.active_ship_id = cap_id;
            }
            Ok((cap_id, dest_flag as i64))
        })?;
        cleanup_ship_leaving_space(ctx, sys_id, &sys, char_id, target_ship_id).await;
        let _ = sys.remove_ball(target_ship_id).await;
        let cap_t = ctx.sde.types.get(&670);
        let cap_mass = cap_t.map(|t| t.mass).unwrap_or(16_000.0);
        let cap_radius = cap_t.map(|t| t.radius).unwrap_or(22.0);
        let cap_speed = cap_t
            .and_then(|t| t.default_attributes.get(&37).copied())
            .unwrap_or(350.0);
        let cap_combat = ctx.build_ship_combat_state(char_id, capsule_id, 670);
        let _ = sys
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: capsule_id,
                    type_id: 670,
                    position: pod_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: cap_speed,
                    inertia: 1.0,
                    mass: cap_mass,
                    radius: cap_radius,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                cap_combat,
                eve_destiny::SlimItemMeta {
                    group_id: 29,
                    category_id: 6,
                    owner_id: char_id,
                    char_id: Some(char_id),
                    corp_id: session.corporation_id.unwrap_or(-1),
                    alliance_id: session.alliance_id.unwrap_or(-1),
                    security_status: 0.0,
                    is_cloaked: false,
                    ..Default::default()
                },
            )
            .await;
        session.ship_id = Some(capsule_id);
        ctx.invalidate_ship_fitting(target_ship_id);
        ctx.invalidate_ship_fitting(capsule_id);
        let changes = vec![(
            PyRep::String("shipid".into()),
            PyRep::Tuple(vec![PyRep::Integer(target_ship_id), PyRep::Integer(capsule_id)]),
        )];
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnSessionChanged",
            "clientID",
            vec![
                PyRep::Bool(false),
                PyRep::Integer(session.session_id as i64),
                PyRep::Dict(changes),
            ],
        );
        Ok(PyRep::keyval([
            ("storedShipID", PyRep::Integer(target_ship_id)),
            ("hostShipID", PyRep::Integer(host_ship_id)),
            ("capsuleID", PyRep::Integer(capsule_id)),
            ("flagID", PyRep::Integer(res_flag_id)),
        ]))
    } else {
        let res_flag = ctx.db.try_with_mut_snapshot(|snap| {
            let dest_flag = validate_and_select_in_space_store_snap(
                ctx,
                snap,
                char_id,
                sys_id,
                target_ship_id,
                host_ship_id,
                supplied_password,
                false,
            )?;
            if let Some(target_mut) = snap.items.get_mut(&target_ship_id) {
                target_mut.location_id = host_ship_id;
                target_mut.flag = dest_flag;
                target_mut.version += 1;
            }
            snap.item_positions.remove(&target_ship_id);
            Ok(dest_flag as i64)
        })?;
        cleanup_ship_leaving_space(ctx, sys_id, &sys, char_id, target_ship_id).await;
        let _ = sys.remove_ball(target_ship_id).await;
        ctx.invalidate_ship_fitting(target_ship_id);
        Ok(PyRep::keyval([
            ("storedShipID", PyRep::Integer(target_ship_id)),
            ("hostShipID", PyRep::Integer(host_ship_id)),
            ("flagID", PyRep::Integer(res_flag)),
        ]))
    }
}

fn ship_has_non_charge_cargo(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    ship_id: i64,
) -> bool {
    snap.items.values().any(|child| {
        if child.location_id != ship_id || child.quantity <= 0 {
            return false;
        }
        let cf = child.flag as u16;
        if matches!(cf, 11..=34 | 87 | 92..=94 | 125..=132 | 158) {
            return false;
        }
        let c_cat = ctx.sde.types.get(&child.type_id).map(|t| t.category_id).unwrap_or(0);
        c_cat != 8
    })
}

fn select_and_validate_bay_dest_flag(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    target: &eve_db::InventoryItemRecord,
    target_group_id: u32,
    host: &eve_db::InventoryItemRecord,
    target_ship_id: i64,
    host_ship_id: i64,
) -> Result<InventoryFlag, BridgeRpcError> {
    let Some(host_type) = ctx.sde.types.get(&host.type_id) else {
        return Err(err("TypeNotFound", "Host ship type not found"));
    };
    if host_type.category_id != 6 {
        return Err(err("CannotStoreCargoInSMA", "Host is not a ship"));
    }
    let sma_cap = host_type.default_attributes.get(&908).copied().unwrap_or(0.0);
    let feb_cap = host_type.default_attributes.get(&3020).copied().unwrap_or(0.0);
    if sma_cap <= 0.0 && feb_cap <= 0.0 {
        return Err(err(
            "NoShipMaintenanceBay",
            format!(
                "Ship {} does not have a Ship Maintenance Bay or Frigate Escape Bay",
                host_type.name
            ),
        ));
    }
    let is_frigate = matches!(
        target_group_id,
        25 | 324 | 830 | 831 | 834 | 893 | 1283 | 1527 | 2001
    );
    let store_in_feb = feb_cap > 0.0 && (sma_cap <= 0.0 || is_frigate);
    if store_in_feb {
        if !is_frigate {
            return Err(err("InvalidFrigateGroup", "Frigate Escape Bay only accepts frigates"));
        }
        let existing_feb = snap.items.values().any(|i| {
            i.location_id == host_ship_id
                && matches!(i.flag as u16, 177 | 179)
                && i.item_id != target_ship_id
        });
        if existing_feb {
            return Err(err(
                "CapacityExceeded",
                "Frigate Escape Bay can hold at most 1 assembled frigate",
            ));
        }
        Ok(InventoryFlag::FrigateEscapeBay)
    } else {
        let target_vol = fitting_containers_ships::effective_item_unit_volume(&ctx.sde, target)
            * (target.quantity as f64);
        let used_sma: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == host_ship_id
                    && matches!(i.flag as u16, 90 | 138)
                    && i.item_id != target_ship_id
            })
            .map(|i| {
                fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i)
                    * (i.quantity as f64)
            })
            .sum();
        if used_sma + target_vol > sma_cap + 1e-4 {
            return Err(err(
                "CapacityExceeded",
                format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded"),
            ));
        }
        Ok(InventoryFlag::ShipHangar)
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_and_select_in_space_store_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    sys_id: u32,
    target_ship_id: i64,
    host_ship_id: i64,
    supplied_password: Option<&str>,
    is_active_ship: bool,
) -> Result<InventoryFlag, BridgeRpcError> {
    let Some(target) = snap.items.get(&target_ship_id) else {
        return Err(err("ShipNotFound", format!("Target ship {target_ship_id} not found")));
    };
    if target.owner_id != char_id {
        return Err(err("OwnershipViolation", "Cannot store another player's ship"));
    }
    if target.type_id == 23 {
        return Err(err("UserError", "Cargo Containers cannot be scooped"));
    }
    if target.location_id != sys_id as i64 {
        return Err(err(
            "ObjectNotInSpace",
            format!("Target ship {target_ship_id} is not in space"),
        ));
    }
    if !is_active_ship
        && (snap.is_structure_controller_saved_ship(target_ship_id)
            || snap.characters.values().any(|c| c.active_ship_id == target_ship_id))
    {
        return Err(err("ShipPiloted", "Cannot store a piloted ship"));
    }
    if target.type_id == 670 {
        return Err(err(
            "CannotStoreCapsuleInSMA",
            "Capsules cannot be stored in a Ship Maintenance Bay",
        ));
    }
    let Some(target_type) = ctx.sde.types.get(&target.type_id) else {
        return Err(err("TypeNotFound", "Target ship type not found"));
    };
    if target_type.category_id != 6 {
        return Err(err(
            "CannotStoreCargoInSMA",
            "Only ships can be stored in a Ship Maintenance Bay",
        ));
    }
    if !target.singleton {
        return Err(err(
            "OnlyAssembledShipsInSMA",
            "Only assembled ships can be stored in a Ship Maintenance Bay",
        ));
    }
    if ship_has_non_charge_cargo(ctx, snap, target_ship_id) {
        return Err(err(
            "ShipHasNonChargeCargo",
            "Cannot store ship in Ship Maintenance Bay while it carries cargo other than charges/ammo",
        ));
    }
    let Some(host) = snap.items.get(&host_ship_id) else {
        return Err(err("ShipNotFound", format!("Host ship {host_ship_id} not found")));
    };
    if host.location_id != sys_id as i64 {
        return Err(err(
            "ObjectNotInSpace",
            format!("Host ship {host_ship_id} is not in space"),
        ));
    }
    if !fitting_containers_ships::can_character_access_ship_bay_with_password_snap(
        snap,
        char_id,
        host_ship_id,
        host.owner_id,
        true,
        supplied_password,
    ) {
        return Err(err(
            "SMAAccessDenied",
            "Ship Maintenance Bay access denied by ship configuration",
        ));
    }
    select_and_validate_bay_dest_flag(
        ctx,
        snap,
        target,
        target_type.group_id,
        host,
        target_ship_id,
        host_ship_id,
    )
}
