//! `fitting_ship_bays` — boarding ships stored in a ship maintenance bay (`ship.BoardStoredShip`).

#![forbid(unsafe_code)]

use super::*;

/// Ship maintenance bay flags: `invFlagShipHangar` (90, used by `ship.StoreVessel`) and
/// `SpecializedShipHold` (138).
fn is_ship_bay_flag(flag: u16) -> bool {
    matches!(flag, 90 | 138)
}

/// Maximum distance (m) between the pilot's current ship and the bay host for in-space access —
/// the same range `ship.Board` uses for boarding in space.
const BAY_ACCESS_RANGE_M: f64 = 2_500.0;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}

/// `ship.BoardStoredShip(hostID, storedShipID)`: board an owned, assembled ship held in the ship
/// maintenance bay of a ship (in space or docked) or of the structure the pilot is docked in.
pub(crate) async fn board_stored_ship(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (host_arg, stored_id) = if args.len() >= 2 {
        (args[0].as_i64().unwrap_or(0), args[1].as_i64().unwrap_or(0))
    } else {
        (0, args.first().and_then(|a| a.as_i64()).unwrap_or(0))
    };
    let dock_id = session.station_id.or(session.structure_id);
    let custom_flag = container_ship_state()
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .custom_item_flags
        .get(&stored_id)
        .copied();
    let (stored, host_type, host_loc) = ctx.db.with_snapshot(|snap| {
        let stored = snap.items.get(&stored_id).cloned();
        let host = stored.as_ref().and_then(|s| snap.items.get(&s.location_id).cloned());
        (stored, host.as_ref().map(|h| h.type_id), host.map(|h| h.location_id))
    });
    let stored = stored.ok_or_else(|| err("ShipNotFound", format!("Stored ship {stored_id} not found")))?;
    let is_ship = ctx.sde.types.get(&stored.type_id).is_some_and(|t| t.category_id == 6);
    if !is_ship || !stored.singleton {
        return Err(err("ShipNotBoardable", format!("Item {stored_id} is not an assembled ship")));
    }
    if stored.owner_id != char_id {
        return Err(err("OwnershipViolation", format!("Ship {stored_id} is not owned by character {char_id}")));
    }
    let host_id = if host_arg != 0 { host_arg } else { stored.location_id };
    let in_bay = is_ship_bay_flag(stored.flag as u16) || custom_flag.is_some_and(is_ship_bay_flag);
    if stored.location_id != host_id || !in_bay {
        return Err(err(
            "ShipNotInMaintenanceBay",
            format!("Ship {stored_id} is not stored in the ship maintenance bay of {host_id}"),
        ));
    }
    let host_is_ship = host_type.and_then(|t| ctx.sde.types.get(&t)).is_some_and(|t| t.category_id == 6);
    let host_is_structure = ctx.db.with_snapshot(|s| s.structures.contains_key(&host_id));
    if !host_is_ship && !host_is_structure {
        return Err(err("ShipNotInMaintenanceBay", format!("{host_id} has no ship maintenance bay")));
    }
    let prev_ship_id = session.ship_id;
    let mut launch_pos = None;
    match dock_id {
        Some(dock) => {
            if !(host_id == dock || host_loc == Some(dock)) {
                return Err(err("ShipOutOfBoardingRange", format!("Bay host {host_id} is not at your dock {dock}")));
            }
        }
        None => {
            let sys_id = session.solar_system_id.ok_or_else(|| err("NoSolarSystem", "Session has no solarSystemID"))?;
            let cur = prev_ship_id.ok_or_else(|| err("NoActiveShip", "No active ship in space"))?;
            let sys = ctx.systems.get_or_spawn(sys_id);
            let (Ok(Some((_, cb))), Ok(Some((_, hb)))) =
                (sys.get_ball_snapshot(cur).await, sys.get_ball_snapshot(host_id).await)
            else {
                return Err(err("ShipNotInSpace", format!("Bay host {host_id} is not on grid")));
            };
            let dist = cb.position.distance(hb.position);
            if dist > BAY_ACCESS_RANGE_M {
                return Err(err(
                    "ShipOutOfBoardingRange",
                    format!("Bay host {host_id} is {dist:.0} m away (must be <= {BAY_ACCESS_RANGE_M} m)"),
                ));
            }
            launch_pos = Some(hb.position + DVec3::new(hb.radius + 300.0, 0.0, 0.0));
        }
    }

    let capsule_group = ctx.sde.groups.values().find(|g| g.name == "Capsule").map(|g| g.group_id);
    let removed_capsule = ctx.db.with_mut_snapshot(|snap| {
        if let Some(s) = snap.items.get_mut(&stored_id) {
            s.location_id = dock_id.or(session.solar_system_id.map(|v| v as i64)).unwrap_or(s.location_id);
            s.flag = if dock_id.is_some() { InventoryFlag::Hangar } else { InventoryFlag::None };
            s.version += 1;
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = stored_id;
        }
        // In space the capsule transfers into the new hull; an abandoned hull stays where it is.
        let prev_is_capsule = prev_ship_id
            .and_then(|p| snap.items.get(&p))
            .and_then(|i| ctx.sde.types.get(&i.type_id))
            .is_some_and(|t| Some(t.group_id) == capsule_group);
        if dock_id.is_none() && prev_is_capsule {
            prev_ship_id.and_then(|p| snap.items.remove(&p)).map(|i| i.item_id)
        } else {
            None
        }
    });
    container_ship_state().write().unwrap_or_else(|e| e.into_inner()).custom_item_flags.remove(&stored_id);
    if let (Some(pos), Some(sys_id)) = (launch_pos, session.solar_system_id) {
        let sys = ctx.systems.get_or_spawn(sys_id);
        if let Some(cap) = removed_capsule {
            let _ = sys.remove_ball(cap).await;
        }
        let t = ctx.sde.types.get(&stored.type_id);
        let ball = eve_destiny::Ball {
            id: stored_id,
            type_id: stored.type_id,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: t.map(|t| t.mass).unwrap_or(1.0),
            radius: t.map(|t| t.radius).unwrap_or(50.0),
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        };
        let cs = ctx.build_ship_combat_state(char_id, stored_id, stored.type_id);
        let meta = ctx.build_ship_slim_meta(char_id, stored.type_id);
        let _ = sys.spawn_ball_with_meta(ball, cs, meta).await;
    }
    session.ship_id = Some(stored_id);
    ctx.invalidate_ship_fitting(stored_id);
    // `ProcessActiveShipChanged` is derived by the V24 client (`DogmaLocation.MakeShipActive`)
    // from the `shipid` session change; the server never sends it.
    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(stored_id)),
        ("typeID", PyRep::Integer(stored.type_id as i64)),
        ("hostID", PyRep::Integer(host_id)),
        ("oldShipID", prev_ship_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
    ]))
}

/// Client `shipConfigSvc` configuration keys (V24 `ship.GetShipConfiguration` / `ship.ConfigureShip`).
const CFG_FLEET_HANGAR_FLEET: &str = "FleetHangar_AllowFleetAccess";
const CFG_FLEET_HANGAR_CORP: &str = "FleetHangar_AllowCorpAccess";
const CFG_SMB_FLEET: &str = "SMB_AllowFleetAccess";
const CFG_SMB_CORP: &str = "SMB_AllowCorpAccess";

fn config_rep(cfg: &ShipAccessConfig) -> PyRep {
    PyRep::Dict(vec![
        (PyRep::String(CFG_FLEET_HANGAR_FLEET.into()), PyRep::Bool(cfg.allow_fleet_hangar)),
        (PyRep::String(CFG_FLEET_HANGAR_CORP.into()), PyRep::Bool(cfg.allow_corp_hangar)),
        (PyRep::String(CFG_SMB_FLEET.into()), PyRep::Bool(cfg.allow_fleet_smb)),
        (PyRep::String(CFG_SMB_CORP.into()), PyRep::Bool(cfg.allow_corp_smb)),
    ])
}

/// `ship.GetShipConfiguration(shipID)`, `ship.ConfigureShip(shipID, {key: value})` and
/// `ship.SetShipPassword(shipID, password)`. The access flags gate fleet/corp use of the ship's
/// Fleet Hangar and Ship Maintenance Bay (enforced in `validate_container_and_bay_move`).
pub(crate) fn ship_configuration(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let ship_id = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
    let owner = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.owner_id));
    let Some(owner) = owner else {
        return Err(err("ShipNotFound", format!("Ship {ship_id} not found")));
    };
    let mut st = container_ship_state().write().unwrap_or_else(|e| e.into_inner());
    if method == "GetShipConfiguration" {
        return Ok(config_rep(&st.ship_configs.get(&ship_id).cloned().unwrap_or_default()));
    }
    if owner != char_id {
        return Err(err("OwnershipViolation", format!("Ship {ship_id} is not owned by character {char_id}")));
    }
    let cfg = st.ship_configs.entry(ship_id).or_default();
    if method == "SetShipPassword" {
        cfg.password = args.get(1).and_then(|a| a.as_str()).filter(|p| !p.is_empty()).map(str::to_string);
        return Ok(config_rep(cfg));
    }
    if let Some(PyRep::Dict(entries)) = args.get(1) {
        for (k, v) in entries {
            let (Some(key), Some(val)) = (k.as_str(), v.as_bool().or_else(|| v.as_i64().map(|i| i != 0))) else {
                continue;
            };
            match key {
                CFG_FLEET_HANGAR_FLEET => cfg.allow_fleet_hangar = val,
                CFG_FLEET_HANGAR_CORP => cfg.allow_corp_hangar = val,
                CFG_SMB_FLEET => cfg.allow_fleet_smb = val,
                CFG_SMB_CORP => cfg.allow_corp_smb = val,
                other => return Err(err("InvalidArgument", format!("Unknown ship configuration key {other}"))),
            }
        }
    }
    Ok(config_rep(cfg))
}
