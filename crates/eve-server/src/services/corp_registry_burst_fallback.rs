//! Secure fallback handler for fleet Command Burst (`group 1770`) `dogmaIM.Activate` / `Deactivate`
//! when the active ship ball is spawned in space without a full hull `InventoryItemRecord` (`#477`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

pub(super) async fn handle_fleet_burst_fallback(
    ctx: &ServerContext,
    session: &SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "dogmaIM"
        || !matches!(method, "Activate" | "Deactivate")
        || session.fleet_id.is_none()
    {
        return None;
    }
    let module_id = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0)?;
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let active_ship_opt = session.ship_id.filter(|&id| id > 0);
    if let Some(sid) = active_ship_opt {
        if ctx.db.with_snapshot(|s| s.structures.contains_key(&sid))
            || ctx.get_or_rebuild_ship_fitting(char_id, sid).is_ok()
        {
            // Defer to full Upwell / tactical_modules / fitting_module_cycles handling.
            return None;
        }
    }

    let is_burst_module = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&module_id)
            .and_then(|it| ctx.sde.types.get(&it.type_id))
            .is_some_and(|t| t.group_id == 1770)
    });
    if !is_burst_module {
        return None;
    }

    if session.station_id.is_some() || session.structure_id.is_some() {
        return Some(Err(err(
            "ShipDocked",
            "Cannot activate or deactivate command burst modules while docked",
        )));
    }
    let Some(active_ship_id) = active_ship_opt else {
        return Some(Err(err("NoActiveShip", "Session has no active ship")));
    };
    let Some(sys_id) = session.solar_system_id.filter(|&id| id > 0) else {
        return Some(Err(err(
            "NoSolarSystem",
            "Session has no active solarSystemID",
        )));
    };

    let burst_check = ctx.db.with_snapshot(|snap| -> Result<(Option<u32>, f64), BridgeRpcError> {
        if let Some(ship_item) = snap.items.get(&active_ship_id) {
            if ship_item.owner_id != char_id {
                return Err(err(
                    "ShipOwnershipViolation",
                    "Active ship does not belong to caller",
                ));
            }
        }
        let Some(mod_item) = snap.items.get(&module_id) else {
            return Err(err("ModuleNotFound", "Command burst module not found"));
        };
        if mod_item.owner_id != char_id || mod_item.location_id != active_ship_id {
            return Err(err(
                "ModuleNotOwnedByShip",
                "Command burst module is not owned by caller and fitted to active ship",
            ));
        }
        if snap.is_item_locked_in_contract_or_job(module_id)
            || !(11..=34).contains(&(mod_item.flag as u16))
        {
            return Err(err(
                "ModuleNotFitted",
                "Command burst module must be fitted in a high, medium, or low rack slot",
            ));
        }
        if method == "Activate"
            && !snap
                .module_online_states
                .get(&module_id)
                .copied()
                .unwrap_or(true)
        {
            return Err(err(
                "ModuleOffline",
                "Command burst module must be online to activate",
            ));
        }
        let Some(mod_type) = ctx.sde.types.get(&mod_item.type_id) else {
            return Err(err("ModuleNotFound", "Command burst module SDE type unknown"));
        };
        let charge_tid = snap
            .items
            .values()
            .find(|i| {
                i.owner_id == char_id
                    && i.location_id == active_ship_id
                    && i.item_id != module_id
                    && i.flag == mod_item.flag
                    && i.quantity > 0
            })
            .map(|i| i.type_id);
        let aoe_range = mod_type
            .default_attributes
            .get(&54)
            .copied()
            .unwrap_or(15_000.0)
            .max(15_000.0);
        Ok((charge_tid, aoe_range))
    });

    let (charge_tid_opt, aoe_range) = match burst_check {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };

    let sys = ctx.systems.get_or_spawn(sys_id);
    let Some((_, booster_ball)) = sys.get_ball_snapshot(active_ship_id).await.ok().flatten() else {
        return Some(Err(err(
            "ShipNotInSpace",
            "Active ship ball is not spawned in space",
        )));
    };

    if method == "Deactivate" {
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&module_id);
            snap.module_active_charges.remove(&module_id);
        });
        return Some(Ok(PyRep::keyval([
            ("moduleID", PyRep::Integer(module_id)),
            ("active", PyRep::Bool(false)),
        ])));
    }

    let fid = session.fleet_id.unwrap_or(0);
    let candidates: Vec<(i64, i64)> = ctx.db.with_snapshot(|snap| {
        let mut out = vec![(char_id, active_ship_id)];
        if let Some(fl) = snap.fleets.get(&fid).filter(|fl| fl.members.contains(&char_id)) {
            for &m_id in &fl.members {
                if m_id != char_id
                    && crate::services::fleet_social::is_in_fleet_boost_scope(fl, char_id, m_id)
                {
                    if let Some(ch) = snap.characters.get(&m_id) {
                        if ch.solar_system_id == sys_id
                            && ch.station_id.is_none()
                            && ch.structure_id.is_none()
                            && ch.active_ship_id > 0
                        {
                            out.push((m_id, ch.active_ship_id));
                        }
                    }
                }
            }
        }
        out
    });

    let booster_pos = booster_ball.position;
    let mut affected_members = Vec::new();
    let mut affected_ships = Vec::new();
    for (mcid, msid) in candidates {
        let in_range = if msid == active_ship_id {
            true
        } else if let Ok(Some((_, mb))) = sys.get_ball_snapshot(msid).await {
            (mb.position - booster_pos).length() <= aoe_range
        } else {
            false
        };
        if in_range {
            affected_members.push(PyRep::Integer(mcid));
            affected_ships.push(PyRep::Integer(msid));
        }
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.insert(module_id);
        if let Some(ctid) = charge_tid_opt {
            snap.module_active_charges.insert(module_id, ctid);
        }
    });

    Some(Ok(PyRep::keyval([
        ("moduleID", PyRep::Integer(module_id)),
        ("active", PyRep::Bool(true)),
        (
            "chargeTypeID",
            charge_tid_opt
                .map(|c| PyRep::Integer(c as i64))
                .unwrap_or(PyRep::None),
        ),
        ("affectedMemberIDs", PyRep::List(affected_members)),
        ("affectedShipIDs", PyRep::List(affected_ships)),
    ])))
}

#[cfg(test)]
#[path = "corp_registry_burst_fallback_edges_tests.rs"]
mod corp_registry_burst_fallback_edges_tests;
