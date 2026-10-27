//! Starbase POS forcefield coverage and entry validation for ship bays and movement.
#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::inventory_fitting::fitting_containers_ships;
use eve_proto::BridgeRpcError;
use glam::DVec3;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub fn can_enter_forcefield(ctx: &ServerContext, char_id: i64, ship_id: i64, tower_id: i64) -> bool {
    ctx.db.with_snapshot(|snap| {
        let owner_id = snap
            .items
            .get(&tower_id)
            .map(|i| i.owner_id)
            .or_else(|| snap.structures.get(&tower_id).map(|s| s.owner_corp_id));
        let Some(owner_id) = owner_id else { return false; };
        let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
        let tower_corp_id = if snap.corporations.contains_key(&owner_id) {
            owner_id
        } else {
            snap.characters
                .get(&owner_id)
                .map(|c| c.corporation_id)
                .unwrap_or(owner_id)
        };
        if owner_id == char_id || (char_corp != 0 && (owner_id == char_corp || tower_corp_id == char_corp)) {
            return true;
        }
        let Some(settings) = snap.starbase_settings.get(&tower_id) else {
            return false;
        };
        let tower_alliance_id = snap
            .characters
            .get(&owner_id)
            .and_then(|c| c.alliance_id)
            .or_else(|| snap.corporations.get(&tower_corp_id).and_then(|c| c.alliance_id))
            .unwrap_or(0);
        let char_alliance = snap.characters.get(&char_id).and_then(|c| c.alliance_id).unwrap_or(0);
        if (settings.allow_corporation_members && tower_corp_id != 0 && tower_corp_id == char_corp)
            || (settings.allow_alliance_members && tower_alliance_id != 0 && tower_alliance_id == char_alliance)
        {
            return true;
        }
        if let Some(ref pw) = settings.password {
            if !pw.is_empty() {
                let cs = fitting_containers_ships::read_container_ship_state_snap(snap);
                let ship_pw = cs
                    .ship_passwords
                    .get(&ship_id)
                    .map(|s| s.as_str())
                    .or_else(|| cs.ship_configs.get(&ship_id).and_then(|c| c.password.as_deref()));
                return ship_pw == Some(pw.as_str());
            }
        }
        false
    })
}

pub async fn check_pos_forcefield_coverage(
    ctx: &ServerContext,
    sys_id: u32,
    char_id: i64,
    ship_id: i64,
    positions: &[DVec3],
) -> Result<(), BridgeRpcError> {
    let towers = ctx.db.with_snapshot(|snap| {
        let mut list = Vec::new();
        for item in snap.items.values() {
            if item.location_id == sys_id as i64
                && ctx.sde.types.get(&item.type_id).is_some_and(|t| t.group_id == 365)
                && snap.starbase_settings.contains_key(&item.item_id)
            {
                let rad = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .and_then(|t| t.default_attributes.get(&680).copied())
                    .unwrap_or(35_000.0);
                list.push((
                    item.item_id,
                    rad,
                    snap.item_positions
                        .get(&item.item_id)
                        .map(|p| DVec3::new(p[0], p[1], p[2])),
                ));
            }
        }
        for st in snap.structures.values() {
            if st.solar_system_id == sys_id
                && ctx.sde.types.get(&st.type_id).is_some_and(|t| t.group_id == 365)
                && snap.starbase_settings.contains_key(&st.structure_id)
            {
                let rad = ctx
                    .sde
                    .types
                    .get(&st.type_id)
                    .and_then(|t| t.default_attributes.get(&680).copied())
                    .unwrap_or(35_000.0);
                list.push((
                    st.structure_id,
                    rad,
                    Some(DVec3::new(st.position[0], st.position[1], st.position[2])),
                ));
            }
        }
        list
    });
    let sys = ctx.systems.get_or_spawn(sys_id);
    for (tower_id, shield_rad, mut pos_opt) in towers {
        if pos_opt.is_none() {
            pos_opt = sys
                .get_ball_snapshot(tower_id)
                .await
                .ok()
                .flatten()
                .map(|(_, b)| b.position);
        }
        let Some(tower_pos) = pos_opt else { continue; };
        for &pos in positions {
            if pos.distance(tower_pos) <= shield_rad && !can_enter_forcefield(ctx, char_id, ship_id, tower_id) {
                return Err(err(
                    "ForcefieldAccessDenied",
                    "Forcefield access denied by starbase settings",
                ));
            }
        }
    }
    Ok(())
}

pub fn is_deployable_item(sde: &eve_sde::SdeRegistry, type_id: u32) -> bool {
    let Some(t) = sde.types.get(&type_id) else {
        return false;
    };
    if t.category_id == 18 || t.category_id == 6 || t.category_id == 65 || t.group_id == 186 {
        return false;
    }
    t.category_id == 22 || matches!(t.group_id, 361 | 1246 | 1250 | 1273 | 1274 | 1275 | 340 | 448 | 649)
}

pub fn is_infrastructure_item(sde: &eve_sde::SdeRegistry, type_id: u32) -> bool {
    let Some(t) = sde.types.get(&type_id) else {
        return false;
    };
    t.category_id == 22 || matches!(t.group_id, 1246 | 361 | 1250 | 1275)
}

pub fn is_mobile_depot(sde: &eve_sde::SdeRegistry, type_id: u32) -> bool {
    let Some(t) = sde.types.get(&type_id) else {
        return false;
    };
    t.group_id == 1246 || t.name.to_lowercase().contains("mobile depot")
}

/// Client `shipConfigSvc` configuration keys (V24 `ship.GetShipConfiguration` / `ship.ConfigureShip`).
pub const CFG_FLEET_HANGAR_FLEET: &str = "FleetHangar_AllowFleetAccess";
pub const CFG_FLEET_HANGAR_CORP: &str = "FleetHangar_AllowCorpAccess";
pub const CFG_SMB_FLEET: &str = "SMB_AllowFleetAccess";
pub const CFG_SMB_CORP: &str = "SMB_AllowCorpAccess";

pub(crate) fn config_rep(cfg: &crate::services::inventory_fitting::ShipAccessConfig) -> eve_proto::PyRep {
    eve_proto::PyRep::Dict(vec![
        (eve_proto::PyRep::String(CFG_FLEET_HANGAR_FLEET.into()), eve_proto::PyRep::Bool(cfg.allow_fleet_hangar)),
        (eve_proto::PyRep::String(CFG_FLEET_HANGAR_CORP.into()), eve_proto::PyRep::Bool(cfg.allow_corp_hangar)),
        (eve_proto::PyRep::String(CFG_SMB_FLEET.into()), eve_proto::PyRep::Bool(cfg.allow_fleet_smb)),
        (eve_proto::PyRep::String(CFG_SMB_CORP.into()), eve_proto::PyRep::Bool(cfg.allow_corp_smb)),
    ])
}

pub fn ship_configuration(
    ctx: &ServerContext,
    session: &eve_machonet::SessionState,
    method: &str,
    args: &[eve_proto::PyRep],
) -> Result<eve_proto::PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let (ship_id, password_arg) = if method == "SetShipPassword" {
        if args.len() == 1 {
            (session.ship_id.unwrap_or(0), args.first())
        } else if args.len() >= 2 {
            if let Some(sid) = args[0].as_i64() {
                (sid, args.get(1))
            } else {
                (session.ship_id.unwrap_or(0), args.first())
            }
        } else {
            (session.ship_id.unwrap_or(0), None)
        }
    } else {
        let sid = args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0);
        (sid, None)
    };
    let owner = ctx.db.with_snapshot(|snap| snap.items.get(&ship_id).map(|i| i.owner_id));
    let Some(owner) = owner else {
        return Err(err("ShipNotFound", format!("Ship {ship_id} not found")));
    };
    let mut st = fitting_containers_ships::read_container_ship_state(ctx);
    if method == "GetShipConfiguration" {
        return Ok(config_rep(&st.ship_configs.get(&ship_id).cloned().unwrap_or_default()));
    }
    if owner != char_id {
        return Err(err("OwnershipViolation", format!("Ship {ship_id} is not owned by character {char_id}")));
    }
    let cfg = st.ship_configs.entry(ship_id).or_default();
    if method == "SetShipPassword" {
        let pw_opt = password_arg.and_then(|a| a.as_str()).filter(|p| !p.is_empty()).map(str::to_string);
        if args.len() >= 2 {
            cfg.password = pw_opt.clone();
        }
        if let Some(ref pw) = pw_opt {
            st.ship_passwords.insert(ship_id, pw.clone());
        } else {
            st.ship_passwords.remove(&ship_id);
        }
        fitting_containers_ships::save_container_ship_state(ctx, &st);
        return Ok(eve_proto::PyRep::None);
    }
    let dict_entries = args.get(1).or_else(|| args.first()).and_then(|a| match a {
        eve_proto::PyRep::Dict(entries) => Some(entries),
        _ => None,
    });
    if let Some(entries) = dict_entries {
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
    let rep = config_rep(cfg);
    fitting_containers_ships::save_container_ship_state(ctx, &st);
    Ok(rep)
}
